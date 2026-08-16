//! The write direction: turning a record's values back into bytes.
//!
//! # Why a reader needs an encoder
//!
//! Encoding is not only for writing files. Callers use it while *reading*:
//! `len(record.dumps())` is a common way to learn how many bytes a
//! null-terminated structure occupied, so the caller can seek past it. The LNK
//! reader does exactly that. So this module is on a correctness path even for
//! consumers that never write anything.
//!
//! # Fields are placed, not concatenated
//!
//! For a fixed-size record every field is written **at its plan offset**, not
//! appended. Two things fall out of that, and both are wrong if you append
//! instead:
//!
//! * A union's members overlap. `_INDEX_ENTRY` declares two eight-byte members
//!   at offset zero, so appending them yields a sixteen-byte record where
//!   upstream yields eight.
//! * Bitfields share a storage unit. Every field of one unit has the same
//!   offset, so placement handles them without a special case in the loop.
//!
//! A record whose size depends on its content has no precomputed offsets, so
//! there fields genuinely do follow one another.
//!
//! # Null terminators are part of what the record occupied
//!
//! `char d[]` stops at a NUL and that NUL is inside the record, so it is written
//! back. `char d[n]` does not have one. The distinction is
//! [`CompiledPlan::is_null_terminated`] and getting it wrong grows or shrinks
//! every record that has such a field.

use pyo3::prelude::*;

use cstruct_rs2_core::decode;
use cstruct_rs2_core::plan::{ArrayDimension, CompiledPlan, FieldOp, Op, DYNAMIC_OFFSET};
use cstruct_rs2_core::schema::Endian;

use crate::errors::{internal, to_py_err, typed_err};
use crate::types::record::Record;

/// Encode a whole record to bytes.
///
/// Returns a `Vec` rather than a Python object so a nested record can be
/// concatenated into its parent without round-tripping through the interpreter.
pub fn record_to_bytes(slf: &Bound<'_, Record>) -> PyResult<Vec<u8>> {
    record_to_bytes_with_endian(slf, None)
}

/// Encode one member into a static union buffer.
///
/// Value construction and assignment choose the first member supplied by the
/// caller, as upstream does, then decode this buffer once to refresh every
/// overlapping view. Ordinary union reads do not call this helper.
pub(crate) fn encode_union_member(
    py: Python<'_>,
    plan: &CompiledPlan,
    index: usize,
    value: &Bound<'_, PyAny>,
    endian: Endian,
) -> PyResult<Vec<u8>> {
    let size = plan.total_size.ok_or_else(|| {
        pyo3::exceptions::PyNotImplementedError::new_err(
            "Initializing a dynamic union is not yet supported",
        )
    })?;
    let field = plan
        .fields
        .get(index)
        .ok_or_else(|| internal("union member index is out of range"))?;
    let mut out = vec![0u8; size as usize];
    let window = plan.bit_windows.get(index).copied().unwrap_or_default();
    if window.is_bitfield() {
        let raw = integer_bits(py, value)? as u64;
        let mask = if window.width >= 64 {
            u64::MAX
        } else {
            (1u64 << window.width) - 1
        };
        let bits = (raw & mask) << window.shift;
        let mut encoded = Vec::with_capacity(field.size as usize);
        put_uint_bytes(
            &mut encoded,
            u128::from(bits),
            field.size as usize,
            matches!(endian, Endian::Big),
        );
        place(&mut out, true, field.offset as usize, &encoded);
        return Ok(out);
    }

    let mut encoded = Vec::new();
    encode_field(py, plan, index, field, value, endian, &mut encoded)?;
    place(&mut out, true, field.offset as usize, &encoded);
    Ok(out)
}

/// Encode a whole record, optionally overriding its compiled byte order.
///
/// The compiled plan remains immutable and decode-hot bit windows remain
/// precomputed.  A writer override is cold and derives its bit shifts while it
/// walks the existing plan, preserving type identity and avoiding a second
/// schema or serializer.
pub fn record_to_bytes_with_endian(
    slf: &Bound<'_, Record>,
    endian: Option<Endian>,
) -> PyResult<Vec<u8>> {
    let py = slf.py();
    let plan = Record::encoding_plan(slf);
    if plan.is_union && plan.total_size.is_none() {
        return Err(pyo3::exceptions::PyNotImplementedError::new_err(
            "Writing dynamic unions is not yet supported",
        ));
    }
    let endian = endian.unwrap_or(plan.endian);

    // A record is written into a pre-sized buffer only when every field has a
    // resolved offset, which means a fixed total size and no dynamic field.
    let fixed = plan
        .total_size
        .filter(|_| !plan.fields.iter().any(|field| field.dynamic));
    let mut out = match fixed {
        Some(size) => vec![0u8; size as usize],
        None => Vec::with_capacity(64),
    };

    let mut unit = BitUnit::default();

    for (index, field) in plan.fields.iter().enumerate() {
        let window = plan.bit_windows.get(index).copied().unwrap_or_default();

        // A content-sized aligned record has no compile-time offsets after its
        // first dynamic field. Apply the immutable per-field boundary here,
        // on the uncommon writer path, before opening a bitfield unit or
        // encoding a normal field.
        if fixed.is_none()
            && plan.aligned
            && !plan.is_union
            && (!window.is_bitfield() || window.opens_unit)
        {
            let step = plan
                .field_alignments
                .get(index)
                .copied()
                .unwrap_or(1)
                .max(1) as usize;
            let padding = out.len().wrapping_neg() & (step - 1);
            out.resize(out.len() + padding, 0);
        }

        if window.is_bitfield() {
            if window.opens_unit {
                unit.flush(&mut out, fixed.is_some(), endian);
                let at = position(&out, fixed, field);
                unit.open(field.size as usize, at);
            }
            let raw = match Record::field_value_at(slf, index)? {
                Some(value) => integer_bits(py, &value)?,
                // A bitfield with no value contributes zero bits rather than
                // shifting the fields after it.
                None => 0,
            };
            unit.add(raw as u64, window.width, endian);
            continue;
        }

        unit.flush(&mut out, fixed.is_some(), endian);

        let Some(mut value) = Record::field_value_at(slf, index)? else {
            continue;
        };
        if value.is_none() {
            let Some(default) = Record::field_default_at(slf, index)? else {
                continue;
            };
            value = default;
        }
        let mut encoded = Vec::new();
        encode_field(py, &plan, index, field, &value, endian, &mut encoded)?;
        let at = position(&out, fixed, field);
        place(&mut out, fixed.is_some(), at, &encoded);
    }

    unit.flush(&mut out, fixed.is_some(), endian);
    if fixed.is_none() && plan.aligned && !plan.is_union {
        let step = plan.alignment.max(1) as usize;
        let padding = out.len().wrapping_neg() & (step - 1);
        out.resize(out.len() + padding, 0);
    }
    Ok(out)
}

/// Write a record with the source implementation's field-call and count rules.
///
/// Bytes are produced first by [`record_to_bytes_with_endian`], so validation,
/// null termination, nesting, union placement, and bit packing still have one
/// implementation.  The small emission pass matters only for file-like objects
/// that accept fewer bytes than requested: current dissect.cstruct writes fixed
/// record fields separately and calculates its return value from ``tell()``.
/// Ordinary buffered files receive the same bytes and count either way.
pub fn write_record_to_stream<'py>(
    slf: &Bound<'py, Record>,
    stream: &Bound<'py, PyAny>,
    endian: Option<Endian>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = slf.py();
    let plan = Record::encoding_plan(slf);
    let raw = record_to_bytes_with_endian(slf, endian)?;

    // A dynamic record has no stable field offsets to slice.  Production
    // writers currently use fixed structures; retain one checked write for the
    // dynamic form instead of guessing spans from a mutated value.
    if plan.total_size.is_none() || plan.is_union {
        return stream.call_method1(
            pyo3::intern!(py, "write"),
            (pyo3::types::PyBytes::new(py, &raw),),
        );
    }

    let start: i64 = stream.call_method0(pyo3::intern!(py, "tell"))?.extract()?;
    let mut count = 0i64;
    for (index, field) in plan.fields.iter().enumerate() {
        let window = plan.bit_windows.get(index).copied().unwrap_or_default();
        if window.is_bitfield() && !window.opens_unit {
            continue;
        }
        if field.offset == DYNAMIC_OFFSET {
            return Err(internal("a fixed record contains a dynamic field offset"));
        }

        let current: i64 = stream.call_method0(pyo3::intern!(py, "tell"))?.extract()?;
        let target = start + i64::from(field.offset);
        let field_start = if current < target {
            let padding = usize::try_from(target - current)
                .map_err(|_| internal("record write padding does not fit usize"))?;
            stream.call_method1(
                pyo3::intern!(py, "write"),
                (pyo3::types::PyBytes::new(py, &vec![0; padding]),),
            )?;
            // Source assigns the declared offset even when the padding write
            // was short; that detail determines its observable short-write
            // return value.
            target
        } else {
            current
        };

        let offset = field.offset as usize;
        let size = field.size as usize;
        let end = offset
            .checked_add(size)
            .ok_or_else(|| internal("record field slice overflowed"))?;
        let encoded = raw
            .get(offset..end)
            .ok_or_else(|| internal("record field slice exceeds encoded bytes"))?;
        stream.call_method1(
            pyo3::intern!(py, "write"),
            (pyo3::types::PyBytes::new(py, encoded),),
        )?;

        // BitBuffer writes are flushed outside Structure._write's byte-count
        // accumulation in current dissect.cstruct.
        if !window.is_bitfield() {
            let after: i64 = stream.call_method0(pyo3::intern!(py, "tell"))?.extract()?;
            count += after - field_start;
        }
    }

    if plan.aligned {
        let position: i64 = stream.call_method0(pyo3::intern!(py, "tell"))?.extract()?;
        let alignment = i64::from(plan.alignment.max(1));
        let padding = usize::try_from((-position) & (alignment - 1))
            .map_err(|_| internal("tail padding does not fit usize"))?;
        stream.call_method1(
            pyo3::intern!(py, "write"),
            (pyo3::types::PyBytes::new(py, &vec![0; padding]),),
        )?;
    }

    Ok(count.into_pyobject(py).map(Bound::into_any)?)
}

/// Where one field's bytes go: its plan offset, or the end of what is written.
fn position(out: &[u8], fixed: Option<u32>, field: &FieldOp) -> usize {
    if fixed.is_some() {
        field.offset as usize
    } else {
        out.len()
    }
}

/// One bitfield storage unit under construction.
///
/// Bitfields cannot be encoded one at a time, because several of them share a
/// single integer in the record. So they are accumulated here and written out
/// when the unit closes — which is either at the next field that opens a new
/// unit, the next non-bitfield, or the end of the record.
///
/// Held as a small state machine rather than four loose locals so that "is a
/// unit currently open" is one question with one answer.
#[derive(Default)]
struct BitUnit {
    bits: u64,
    width: usize,
    used: u8,
    at: usize,
    open: bool,
}

impl BitUnit {
    /// Start a new unit of `width` bytes, to be written at `at`.
    fn open(&mut self, width: usize, at: usize) {
        self.bits = 0;
        self.width = width;
        self.used = 0;
        self.at = at;
        self.open = true;
    }

    /// Fold one field into the open unit in declaration order.
    fn add(&mut self, raw: u64, width: u8, endian: Endian) {
        let mask = if width >= 64 {
            u64::MAX
        } else {
            (1u64 << width) - 1
        };
        let shift = match endian {
            Endian::Little => self.used,
            Endian::Big => (self.width as u8 * 8) - self.used - width,
        };
        self.bits |= (raw & mask) << shift;
        self.used += width;
    }

    /// Write the unit out if one is open, and close it. A no-op otherwise.
    fn flush(&mut self, out: &mut Vec<u8>, fixed: bool, endian: Endian) {
        if !self.open {
            return;
        }
        let mut encoded = Vec::with_capacity(self.width);
        put_uint_bytes(
            &mut encoded,
            u128::from(self.bits),
            self.width,
            matches!(endian, Endian::Big),
        );
        place(out, fixed, self.at, &encoded);
        *self = Self::default();
    }
}

/// Put one field's encoded bytes where they belong.
///
/// For a fixed record that is its plan offset, so overlapping union members land
/// on each other exactly as they did when the record was read. For a dynamic
/// record there are no precomputed offsets and fields simply follow one another.
///
/// A fixed record's buffer is already the right length, so a field whose offset
/// or length would run past the end is clipped rather than growing the record.
fn place(out: &mut Vec<u8>, fixed: bool, at: usize, encoded: &[u8]) {
    if !fixed {
        out.extend_from_slice(encoded);
        return;
    }
    if at >= out.len() {
        return;
    }
    let end = (at + encoded.len()).min(out.len());
    out[at..end].copy_from_slice(&encoded[..end - at]);
}

/// Pull the raw bit pattern out of a Python integer, whatever its sign.
///
/// Extracting as `i128` alone is wrong for `uint128`: half that type's range
/// sits above `i128::MAX`, and those are ordinary values, not overflow.
///
/// The signed form is tried first because it is the common case, and casting it
/// to `u128` keeps the bit pattern — which is all the callers write out.
///
/// The final fallback reads `.value`, so an `IntEnum` or `IntFlag` member, or
/// any object that carries its integer there, can be assigned to a field and
/// encoded.
pub(crate) fn integer_bits(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<u128> {
    if let Ok(number) = value.extract::<i128>() {
        return Ok(number as u128);
    }
    if let Ok(number) = value.extract::<u128>() {
        return Ok(number);
    }
    if value.is_instance_of::<pyo3::types::PyInt>() {
        // Plainly an integer, just not one that fits in the widest type
        // cstruct has. Say that, rather than reporting a missing attribute.
        return Err(pyo3::exceptions::PyOverflowError::new_err(
            "int too big to convert",
        ));
    }
    // An enum or flag member carries its integer on `.value`; `dissect.ntfs`
    // relies on that spelling, so it stays supported.
    let inner = value.getattr(pyo3::intern!(py, "value"))?;
    if let Ok(number) = inner.extract::<i128>() {
        return Ok(number as u128);
    }
    inner.extract::<u128>()
}

/// Pull out and range-check an integer for a declared storage width.
///
/// Current dissect.cstruct has two integer writer families.  Its ordinary
/// 8/16/32/64-bit types use :mod:`struct` and therefore raise ``struct.error``
/// with that module's exact range message; odd-width and 128-bit types use
/// ``int.to_bytes`` and raise ``OverflowError``.  The bytes are still produced
/// by the native checked encoder, but mirroring that split keeps invalid-value
/// behavior observable by callers exact too.
pub(crate) fn checked_integer_bits(
    py: Python<'_>,
    value: &Bound<'_, PyAny>,
    width: u8,
    signed: bool,
    packed: bool,
) -> PyResult<u128> {
    let extracted = extract_integer(py, value)?;
    let bits = u32::from(width) * 8;
    let valid = match (signed, extracted) {
        (true, IntegerValue::Signed(number)) => {
            let (minimum, maximum) = if bits == 128 {
                (i128::MIN, i128::MAX)
            } else {
                (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
            };
            (minimum..=maximum)
                .contains(&number)
                .then_some(number as u128)
        }
        (true, IntegerValue::Unsigned(number)) => {
            let maximum = if bits == 128 {
                i128::MAX as u128
            } else {
                (1u128 << (bits - 1)) - 1
            };
            (number <= maximum).then_some(number)
        }
        (false, IntegerValue::Signed(number)) => {
            if number < 0 {
                None
            } else {
                unsigned_in_range(number as u128, bits)
            }
        }
        (false, IntegerValue::Unsigned(number)) => unsigned_in_range(number, bits),
        (_, IntegerValue::TooLarge { .. }) => None,
    };
    valid.ok_or_else(|| integer_range_error(py, width, signed, packed, extracted))
}

/// A Python integer represented without losing the sign of negative values.
#[derive(Clone, Copy)]
enum IntegerValue {
    Signed(i128),
    Unsigned(u128),
    TooLarge { negative: bool },
}

/// Extract a plain integer or an enum/flag member's integer ``value``.
fn extract_integer(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<IntegerValue> {
    if let Ok(number) = value.extract::<i128>() {
        return Ok(IntegerValue::Signed(number));
    }
    if let Ok(number) = value.extract::<u128>() {
        return Ok(IntegerValue::Unsigned(number));
    }
    if value.is_instance_of::<pyo3::types::PyInt>() {
        return Ok(IntegerValue::TooLarge {
            negative: value.lt(0i8)?,
        });
    }
    let inner = value.getattr(pyo3::intern!(py, "value"))?;
    if let Ok(number) = inner.extract::<i128>() {
        return Ok(IntegerValue::Signed(number));
    }
    if let Ok(number) = inner.extract::<u128>() {
        return Ok(IntegerValue::Unsigned(number));
    }
    if inner.is_instance_of::<pyo3::types::PyInt>() {
        return Ok(IntegerValue::TooLarge {
            negative: inner.lt(0i8)?,
        });
    }
    inner.extract::<u128>().map(IntegerValue::Unsigned)
}

/// Return an unsigned value only when it fits in ``bits`` bits.
fn unsigned_in_range(number: u128, bits: u32) -> Option<u128> {
    let maximum = if bits == 128 {
        u128::MAX
    } else {
        (1u128 << bits) - 1
    };
    (number <= maximum).then_some(number)
}

/// Construct the same exception family and message as the source integer type.
fn integer_range_error(
    py: Python<'_>,
    width: u8,
    signed: bool,
    packed: bool,
    value: IntegerValue,
) -> PyErr {
    if !packed {
        let negative_unsigned = !signed
            && matches!(
                value,
                IntegerValue::Signed(number) if number < 0
            )
            || !signed && matches!(value, IntegerValue::TooLarge { negative: true });
        return pyo3::exceptions::PyOverflowError::new_err(if negative_unsigned {
            "can't convert negative int to unsigned"
        } else {
            "int too big to convert"
        });
    }

    let bits = u32::from(width) * 8;
    let (format, message) = if signed {
        let format = match width {
            1 => 'b',
            2 => 'h',
            4 => 'i',
            _ => 'q',
        };
        let minimum = -(1i128 << (bits - 1));
        let maximum = (1i128 << (bits - 1)) - 1;
        (
            format,
            format!("'{format}' format requires {minimum} <= number <= {maximum}"),
        )
    } else {
        let format = match width {
            1 => 'B',
            2 => 'H',
            4 => 'I',
            _ => 'Q',
        };
        let maximum = (1u128 << bits) - 1;
        (
            format,
            format!("'{format}' format requires 0 <= number <= {maximum}"),
        )
    };
    let _ = format;
    py.import("struct")
        .and_then(|module| module.getattr("error"))
        .and_then(|class| class.call1((message.clone(),)))
        .map_or_else(
            |_| pyo3::exceptions::PyOverflowError::new_err(message),
            PyErr::from_value,
        )
}

/// Write an integer's low `width` bytes in `endian` order.
fn put_uint_bytes(out: &mut Vec<u8>, raw: u128, width: usize, big: bool) {
    let bytes = raw.to_le_bytes();
    if big {
        out.extend(bytes[..width].iter().rev());
    } else {
        out.extend_from_slice(&bytes[..width]);
    }
}

/// Write an IEEE 754 float at `width` bytes.
///
/// One place rather than four. `float16` needs its own conversion, and when the
/// scalar arm had it and the array arm did not, an array of `float16` wrote
/// **eight** bytes per two-byte element and corrupted everything after it.
/// Sharing the code is what keeps the two arms from drifting again.
fn put_float_bytes(out: &mut Vec<u8>, raw: f64, width: usize, big: bool) -> PyResult<()> {
    match width {
        2 => {
            let bits = decode::float16_bits(raw).map_err(to_py_err)?;
            put_bytes(out, &bits.to_le_bytes(), big);
        }
        4 => put_bytes(out, &(raw as f32).to_bits().to_le_bytes(), big),
        _ => put_bytes(out, &raw.to_bits().to_le_bytes(), big),
    }
    Ok(())
}

/// Append little-endian bytes, reversed when the record is big-endian.
fn put_bytes(out: &mut Vec<u8>, little_endian: &[u8], big: bool) {
    if big {
        out.extend(little_endian.iter().rev());
    } else {
        out.extend_from_slice(little_endian);
    }
}

/// Encode a `str` as UTF-16 code units in `endian` order.
fn put_utf16(out: &mut Vec<u8>, text: &str, big: bool) {
    for unit in text.encode_utf16() {
        out.extend_from_slice(&if big {
            unit.to_be_bytes()
        } else {
            unit.to_le_bytes()
        });
    }
}

/// Pad with NULs, or truncate, so exactly `want` bytes were written since `from`.
///
/// A fixed-width field must occupy its declared width whatever was assigned to
/// it, or every field after it moves. Only reached for a *mutated* record: a
/// decoded value always already has the right length.
fn fit(out: &mut Vec<u8>, from: usize, want: usize) {
    let written = out.len() - from;
    if written > want {
        out.truncate(from + want);
    } else {
        out.extend(std::iter::repeat_n(0u8, want - written));
    }
}

/// Write a fixed-width field's raw bytes, padded or truncated to its width.
///
/// Used by the `char`/`wchar` arms, which both have three cases — terminated,
/// dynamic, and declared-width — differing only in how the width is decided.
fn put_sized(out: &mut Vec<u8>, raw: &[u8], plan: &CompiledPlan, index: usize, field: &FieldOp) {
    if plan.is_null_terminated(index) {
        // The terminator is part of what the record occupied, and
        // `len(dumps())` is exactly what a reader seeks past. Note this is
        // keyed on the *terminated* form, not on `field.dynamic`: a count-sized
        // `char d[n]` is dynamic too and has no terminator.
        out.extend_from_slice(raw);
        out.extend(std::iter::repeat_n(0u8, terminator_width(field)));
    } else if field.dynamic {
        // Count-sized or read-to-end: whatever was decoded is exactly what was
        // there, with no padding to a declared width.
        out.extend_from_slice(raw);
    } else {
        let start = out.len();
        out.extend_from_slice(raw);
        fit(out, start, field.size as usize);
    }
}

/// How many NUL bytes terminate this field's element type.
///
/// One for `char`, two for `wchar`, because the terminator is a zero *element*
/// rather than a zero byte.
fn terminator_width(field: &FieldOp) -> usize {
    match field.op {
        Op::Wchar | Op::WcharArray => 2,
        _ => 1,
    }
}

/// Encode an array field's value, walking its declared dimensions.
///
/// `dims` is the *remaining* shape. This has to mirror `materialize::nested_array`
/// exactly, because the value it is handed is whatever that produced:
///
/// * a multi-dimensional array is nested lists, one level per dimension;
/// * **except** at the innermost dimension of a `char` or `wchar` array, where
///   the row folds into a single `bytes`/`str` — `char h[3][4]` decodes to three
///   `bytes` of four, not to three lists of four one-byte values.
///
/// That last rule is the whole reason this function exists. Iterating the value
/// once and treating every item as a single element hands a *list* to the integer
/// encoder for a 2-D numeric array, which then fails on the enum-member fallback
/// with `AttributeError: 'list' object has no attribute 'value'` — one cause
/// wearing an unrelated message. Both upstreams round-trip these, so getting it
/// wrong is a divergence rather than a shared limitation.
///
/// `Op::Char` and `Op::CharArray` are deliberately *not* treated alike: a `Char`
/// element is one byte and folds into its row, while a `CharArray` element is
/// itself an array and stays one item per element. Same for `Wchar` against
/// `WcharArray`.
fn encode_array(
    py: Python<'_>,
    field: &FieldOp,
    dims: &[u32],
    value: &Bound<'_, PyAny>,
    big: bool,
    integer_signed: bool,
    out: &mut Vec<u8>,
) -> PyResult<()> {
    let width = field.elem_size.max(1) as usize;

    // More than one dimension left: this level is a sequence of sub-arrays.
    if dims.len() > 1 {
        for item in value.try_iter()? {
            encode_array(py, field, &dims[1..], &item?, big, integer_signed, out)?;
        }
        return Ok(());
    }

    let count = dims.first().copied().map(|count| count as usize);

    match field.elem {
        // The innermost dimension of a char/wchar array: one value, not a list.
        Op::Char => {
            let raw = crate::scalars::char_bytes(py, value)?;
            let start = out.len();
            out.extend_from_slice(&raw);
            if let Some(count) = count {
                fit(out, start, count * width);
            }
        }
        Op::Wchar => {
            let text: String = value.extract()?;
            let start = out.len();
            put_utf16(out, &text, big);
            if let Some(count) = count {
                fit(out, start, count * width);
            }
        }
        // Anything else: one item per element.
        _ => {
            for item in value.try_iter()? {
                let item = item?;
                let start = out.len();
                match field.elem {
                    Op::Float => put_float_bytes(out, item.extract()?, width, big)?,
                    Op::CharArray => {
                        let raw = crate::scalars::char_bytes(py, &item)?;
                        out.extend_from_slice(&raw);
                        fit(out, start, width);
                    }
                    Op::WcharArray => {
                        let text: String = item.extract()?;
                        put_utf16(out, &text, big);
                        fit(out, start, width);
                    }
                    _ => {
                        let raw = checked_integer_bits(
                            py,
                            &item,
                            field.elem_size.max(1),
                            integer_signed,
                            matches!(width, 1 | 2 | 4 | 8),
                        )?;
                        put_uint_bytes(out, raw, width, big);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Encode the uncommon matrix form whose dimensions are resolved per record.
///
/// Expression-sized levels simply walk the value already stored on the record;
/// fixed innermost rows retain their declared padding, and a terminated
/// character row appends one zero element. This mirrors the shape produced by
/// `runtime_matrix_value` without evaluating Python fields during writing.
fn encode_runtime_array(
    py: Python<'_>,
    field: &FieldOp,
    dimensions: &[ArrayDimension],
    value: &Bound<'_, PyAny>,
    big: bool,
    integer_signed: bool,
    out: &mut Vec<u8>,
) -> PyResult<()> {
    if dimensions.len() > 1 {
        for item in value.try_iter()? {
            encode_runtime_array(
                py,
                field,
                &dimensions[1..],
                &item?,
                big,
                integer_signed,
                out,
            )?;
        }
        return Ok(());
    }

    let dimension = dimensions.first();
    let fixed = match dimension {
        Some(ArrayDimension::Fixed(count)) => Some(*count as usize),
        _ => None,
    };
    let terminated = matches!(dimension, Some(ArrayDimension::NullTerminated));
    let width = field.elem_size.max(1) as usize;
    match field.elem {
        Op::Char => {
            let raw = crate::scalars::char_bytes(py, value)?;
            let start = out.len();
            out.extend_from_slice(&raw);
            if let Some(count) = fixed {
                fit(out, start, count * width);
            }
        }
        Op::Wchar => {
            let text: String = value.extract()?;
            let start = out.len();
            put_utf16(out, &text, big);
            if let Some(count) = fixed {
                fit(out, start, count * width);
            }
        }
        _ => {
            for item in value.try_iter()? {
                let item = item?;
                match field.elem {
                    Op::Float => put_float_bytes(out, item.extract()?, width, big)?,
                    _ => {
                        let raw = checked_integer_bits(
                            py,
                            &item,
                            field.elem_size.max(1),
                            integer_signed,
                            matches!(width, 1 | 2 | 4 | 8),
                        )?;
                        put_uint_bytes(out, raw, width, big);
                    }
                }
            }
        }
    }
    if terminated {
        out.extend(std::iter::repeat_n(0, width));
    }
    Ok(())
}

/// Encode one field's value back to bytes.
///
/// The bytes are appended to `out`, which the caller then places at the field's
/// offset — this function does not know where in the record the field sits.
fn encode_field(
    py: Python<'_>,
    plan: &CompiledPlan,
    index: usize,
    field: &FieldOp,
    value: &Bound<'_, PyAny>,
    endian: Endian,
    out: &mut Vec<u8>,
) -> PyResult<()> {
    let big = matches!(endian, Endian::Big);

    match field.op {
        Op::Uint | Op::Int | Op::Enum => {
            // An enum member is an int; `integer_bits` also unwraps IntEnum/IntFlag.
            // An enum carries its storage width in `elem_size`, a plain integer
            // in `size`.
            let width = if field.op == Op::Enum {
                field.elem_size as usize
            } else {
                field.size as usize
            };
            let signed = match field.op {
                Op::Int => true,
                Op::Enum => plan
                    .enums
                    .get(field.aux as usize)
                    .is_some_and(|enum_plan| enum_plan.signed),
                _ => false,
            };
            let raw = checked_integer_bits(
                py,
                value,
                width as u8,
                signed,
                matches!(width, 1 | 2 | 4 | 8),
            )?;
            put_uint_bytes(out, raw, width, big);
        }
        Op::Float => {
            // The field's own width, not a fixed eight. Writing an `f64` for a
            // two-byte `float16` overruns the field and corrupts whatever
            // follows it, with no error.
            put_float_bytes(out, value.extract()?, field.size as usize, big)?;
        }
        Op::Char | Op::CharArray => {
            let raw = crate::scalars::char_bytes(py, value)?;
            put_sized(out, &raw, plan, index, field);
        }
        Op::Wchar | Op::WcharArray => {
            let text: String = value.extract()?;
            let mut units: Vec<u8> = Vec::new();
            put_utf16(&mut units, &text, big);
            put_sized(out, &units, plan, index, field);
        }
        Op::Void | Op::VoidArray => {}
        Op::ScalarArray => {
            let width = field.elem_size.max(1) as usize;
            // The declared shape, so a multi-dimensional array is written one
            // dimension at a time. Empty or single for the ordinary 1-D case,
            // which makes `encode_array` fall straight through to its leaf loop.
            let dims = plan.array_dims.get(index).map(Box::as_ref).unwrap_or(&[]);
            let integer_signed = match field.elem {
                Op::Int => true,
                Op::Enum => plan
                    .enums
                    .get(field.aux as usize)
                    .is_some_and(|enum_plan| enum_plan.signed),
                _ => false,
            };
            if !field.dynamic && dims.is_empty() {
                let actual = value.len()?;
                if actual > field.count as usize {
                    return Err(typed_err(
                        "ArraySizeError",
                        format!(
                            "Expected static array size {}, got {actual} instead.",
                            field.count
                        ),
                    ));
                }
            }
            let runtime_dims = plan
                .runtime_array_dims
                .get(index)
                .map(Box::as_ref)
                .unwrap_or(&[]);
            if runtime_dims.is_empty() {
                encode_array(py, field, dims, value, big, integer_signed, out)?;
            } else {
                encode_runtime_array(py, field, runtime_dims, value, big, integer_signed, out)?;
            }
            // `uint16 d[]` stops at a zero element, and that element is part of
            // what the record occupied -- the same reasoning as `char d[]`.
            if plan.is_null_terminated(index) {
                out.extend(std::iter::repeat_n(0u8, width));
            }
        }
        Op::Nested => {
            let sub = value
                .cast::<Record>()
                .map_err(|_| internal("a nested field holds something other than a record"))?;
            out.extend_from_slice(&record_to_bytes_with_endian(sub, Some(endian))?);
        }
        Op::NestedArray => {
            for item in value.try_iter()? {
                let item = item?;
                let sub = item
                    .cast::<Record>()
                    .map_err(|_| internal("a nested array holds something other than a record"))?;
                out.extend_from_slice(&record_to_bytes_with_endian(sub, Some(endian))?);
            }
            // The terminator, as for `char d[]` and `uint16 d[]` above. Only a
            // fixed-size record has a terminator this side can write without
            // building a zero instance of the sub-plan; a variable-size one
            // would need its own default record, so it is left short rather
            // than guessed at.
            if plan.is_null_terminated(index) {
                if let Some(stride) = plan
                    .subplans
                    .get(field.aux as usize)
                    .and_then(|sub| sub.total_size)
                {
                    out.extend(std::iter::repeat_n(0u8, stride as usize));
                }
            }
        }
        Op::Leb128 => put_leb128(
            out,
            integer_bits(py, value)? as u64,
            matches!(field.elem, Op::Int),
        ),
        Op::Leb128Array => {
            for item in value.try_iter()? {
                put_leb128(
                    out,
                    integer_bits(py, &item?)? as u64,
                    matches!(field.elem, Op::Int),
                );
            }
            out.push(0);
        }
        Op::Bits => {
            // Bitfields share a storage unit, so `record_to_bytes` packs them in
            // one pass rather than reaching this function field by field.
            return Err(internal("bitfields are packed by the caller"));
        }
        Op::Custom | Op::Unsupported => {
            return Err(pyo3::exceptions::PyNotImplementedError::new_err(
                "cannot encode a consumer-supplied type, or a field whose \
                 shape could not be resolved when the type was compiled",
            ))
        }
    }
    Ok(())
}

/// Write LEB128, stopping when the remaining bits are only sign extension.
///
/// Signed and unsigned LEB128 use the same seven-bit chunks but not the same
/// stopping rule. In particular, shifting a negative value as an unsigned
/// integer would emit ten bytes instead of preserving upstream's compact
/// signed representation.
fn put_leb128(out: &mut Vec<u8>, mut raw: u64, signed: bool) {
    if signed {
        let mut value = raw as i64;
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
            out.push(if done { byte } else { byte | 0x80 });
            if done {
                return;
            }
        }
    }

    loop {
        let byte = (raw & 0x7f) as u8;
        raw >>= 7;
        if raw == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}
