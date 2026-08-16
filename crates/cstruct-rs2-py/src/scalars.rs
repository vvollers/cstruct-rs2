//! Scalar and array type objects.
//!
//! A cstruct namespace exposes more than structures. `dissect.extfs` writes
//! `c_ext.uint32[15](self.inode.i_block)` to reinterpret an inode's block list,
//! and that one expression needs three things this module provides:
//!
//! 1. a scalar reachable as a namespace attribute (`c_ext.uint32`);
//! 2. subscripting it into an array type (`[15]`);
//! 3. calling the result on bytes or a stream.
//!
//! # Why these are separate from [`crate::types::StructType`]
//!
//! A structure carries a compiled plan; a scalar needs only an op and a width.
//! Routing scalars through a one-field plan would allocate a plan per scalar
//! type and send every scalar read round a field loop built for records — real
//! cost for code that reads values one at a time, which is exactly what a
//! binary-XML or varint reader does.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyList, PyType};

use cstruct_rs2_core::decode::{self, MAX_ARRAY_ELEMENTS};
use cstruct_rs2_core::plan::Op;
use cstruct_rs2_core::schema::{Endian, Scalar};

use crate::errors::{to_py_err, typed_err};
use crate::input::{self, InputKind};

/// Largest read request made while looking for a zero array element.
///
/// Four KiB keeps the common string to one Python call without speculatively
/// retaining a large suffix. The size is rounded down to a whole number of
/// elements for widths that do not divide it.
const TERMINATED_SCAN_CHUNK: usize = 4 * 1024;

/// Maximum number of source bytes a standalone terminated array may inspect.
///
/// This shares the core array limit's 2^24 scale but also caps bytes, so a
/// sixteen-byte scalar cannot turn that element allowance into a 256-MiB scan.
/// Upstream has no resource ceiling and reads one element until zero or EOF;
/// this implementation deliberately refuses a longer unterminated value.
const MAX_TERMINATED_SCAN_BYTES: usize = MAX_ARRAY_ELEMENTS as usize;

/// The one place that maps a byte-order spelling onto an [`Endian`].
///
/// Constructors and the writable namespace property must share this mapping.
/// In particular, `"="` and `"@"` mean host-native byte order rather than an
/// unconditional little-endian default.
pub(crate) fn parse_endian(spelling: &str) -> PyResult<Endian> {
    Ok(match spelling {
        "<" | "little" | "le" => Endian::Little,
        ">" | "!" | "big" | "be" | "network" => Endian::Big,
        "=" | "@" => {
            if cfg!(target_endian = "little") {
                Endian::Little
            } else {
                Endian::Big
            }
        }
        other => {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "unknown endianness: {other}"
            )))
        }
    })
}

/// Map a schema scalar onto a decode op and a width.
fn scalar_parts(scalar: Scalar) -> (Op, u8) {
    match scalar {
        Scalar::Uint(n) => (Op::Uint, n),
        Scalar::Int(n) => (Op::Int, n),
        Scalar::Float(n) => (Op::Float, n),
        Scalar::Char => (Op::Char, 1),
        Scalar::Wchar => (Op::Wchar, 2),
        Scalar::Void => (Op::Void, 0),
        // Width zero marks "read to find out"; see `read_one`.
        Scalar::Leb128 { signed } => (if signed { Op::Int } else { Op::Uint }, 0),
    }
}

/// Read one scalar value out of a buffer.
fn read_one<'py>(
    py: Python<'py>,
    op: Op,
    width: u8,
    endian: Endian,
    buf: &[u8],
    offset: usize,
) -> PyResult<Bound<'py, PyAny>> {
    // Widths past eight bytes (`int128`, `uint128`) need the u128 pair;
    // `read_int`/`read_uint` cap at 64 bits and would report an unsupported
    // width. Checked before the op match so both signednesses route together.
    if width > 8 && matches!(op, Op::Int | Op::Uint) {
        return Ok(if matches!(op, Op::Int) {
            decode::read_int128(buf, offset, width, endian)
                .map_err(to_py_err)?
                .into_pyobject(py)?
                .into_any()
        } else {
            decode::read_uint128(buf, offset, width, endian)
                .map_err(to_py_err)?
                .into_pyobject(py)?
                .into_any()
        });
    }

    Ok(match op {
        Op::Int => decode::read_int(buf, offset, width, endian)
            .map_err(to_py_err)?
            .into_pyobject(py)?
            .into_any(),
        Op::Float => decode::read_float(buf, offset, width, endian)
            .map_err(to_py_err)?
            .into_pyobject(py)?
            .into_any(),
        Op::Char => {
            let bytes = decode::read_bytes(buf, offset, 1).map_err(to_py_err)?;
            PyBytes::new(py, bytes).into_any()
        }
        Op::Wchar => {
            let bytes = decode::read_bytes(buf, offset, 2).map_err(to_py_err)?;
            let unit = match endian {
                Endian::Little => u16::from_le_bytes([bytes[0], bytes[1]]),
                Endian::Big => u16::from_be_bytes([bytes[0], bytes[1]]),
            };
            String::from_utf16_lossy(&[unit])
                .into_pyobject(py)?
                .into_any()
        }
        _ => decode::read_uint(buf, offset, width, endian)
            .map_err(to_py_err)?
            .into_pyobject(py)?
            .into_any(),
    })
}

/// Read one LEB128 value, whose length is discovered by reading it.
///
/// A stream is read one byte at a time rather than in a speculative block: the
/// value ends at the first byte with a clear continuation bit, and over-reading
/// would leave the cursor past it. Ten bytes is where a 64-bit LEB128 ends, so
/// the loop is bounded by the encoding rather than by the input.
fn read_leb128_from<'py>(
    py: Python<'py>,
    source: &Bound<'py, PyAny>,
    signed: bool,
) -> PyResult<Bound<'py, PyAny>> {
    let mut scratch = Vec::new();
    match input::classify(source)? {
        InputKind::Buffer => input::copy_buffer(source, &mut scratch)?,
        InputKind::Stream => {
            let mut byte = Vec::with_capacity(1);
            for _ in 0..10 {
                byte.clear();
                input::read_exact(source, 1, &mut byte)?;
                scratch.push(byte[0]);
                if byte[0] & 0x80 == 0 {
                    break;
                }
            }
        }
    }
    let (value, _) = decode::read_leb128(&scratch, 0, signed).map_err(to_py_err)?;
    Ok(value.into_pyobject(py)?.into_any())
}

/// Borrow the bytes for `size` from a buffer or a stream.
///
/// A stream is read for exactly `size` bytes, so the cursor ends where the next
/// read expects it.
fn take_bytes(source: &Bound<'_, PyAny>, size: usize, scratch: &mut Vec<u8>) -> PyResult<()> {
    match input::classify(source)? {
        InputKind::Buffer => {
            input::copy_buffer(source, scratch)?;
            if scratch.len() < size {
                return Err(pyo3::exceptions::PyEOFError::new_err(
                    "unexpected end of stream",
                ));
            }
            scratch.truncate(size);
            Ok(())
        }
        InputKind::Stream => input::read_exact(source, size, scratch),
    }
}

/// Wrap a value in this type's Python value class.
///
/// Built in Python (`dissect.cstruct._make_value`) rather than in Rust, because
/// the class it produces is an ordinary Python subclass of `int`/`bytes`/`str`
/// and nothing about creating one belongs on a decode path -- this is reached
/// only when a type object is *called with a value*, once per call.
fn make_value<'py>(
    type_object: &Bound<'py, PyAny>,
    value: Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    type_object
        .py()
        .import(pyo3::intern!(type_object.py(), "dissect.cstruct"))?
        .getattr(pyo3::intern!(type_object.py(), "_make_value"))?
        .call1((type_object, value))
}

/// Whether a call is *constructing* a value rather than decoding one.
///
/// Anything that is not a buffer and not a stream is a value: `cs.uint32(5)`.
/// `char` needs one extra case, and it is upstream's: `bytes` of exactly the
/// type's own width is the value, not a buffer to read it from -- otherwise
/// `cs.char(b"a")` would be indistinguishable from a one-byte read, and
/// upstream resolves it the same way.
fn is_construction(source: &Bound<'_, PyAny>, op: Op, width: u8) -> bool {
    if input::classify(source).is_err() {
        return true;
    }
    if matches!(op, Op::Char | Op::CharArray) {
        if let Ok(raw) = source.extract::<Vec<u8>>() {
            return raw.len() == width as usize;
        }
    }
    false
}

/// A primitive type: `c_ext.uint32`, `cs.wchar`, and so on.
#[pyclass(name = "ScalarType", module = "dissect.cstruct", skip_from_py_object)]
#[derive(Clone)]
pub struct ScalarType {
    op: Op,
    width: u8,
    /// Definition-time alignment; factory-created types may override width.
    alignment: u32,
    /// Select the exception contract for integer writes.
    ///
    /// Upstream creates its built-in 8/16/32/64-bit integers through
    /// ``struct.Struct`` but creates types returned by ``_make_int_type``
    /// through ``int.to_bytes``, even when the requested width is the same.
    /// Both paths produce identical valid bytes; only their range errors differ.
    packed_integer_writer: bool,
    /// Shared by every scalar and array type from one namespace.
    ///
    /// Upstream keeps type objects stable while `cs.endian` changes. Sharing
    /// the cold namespace setting lets an array obtained before the assignment
    /// observe it too, without consulting Python or adding work to record-field
    /// decoding.
    endian_is_little: Arc<AtomicBool>,
    name: &'static str,
}

impl ScalarType {
    /// Wrap a schema scalar.
    pub fn new(scalar: Scalar, endian: Endian, name: &'static str) -> Self {
        Self::with_endian_state(
            scalar,
            Arc::new(AtomicBool::new(matches!(endian, Endian::Little))),
            name,
        )
    }

    /// Wrap a scalar that follows a namespace's live byte order.
    pub fn with_endian_state(
        scalar: Scalar,
        endian_is_little: Arc<AtomicBool>,
        name: &'static str,
    ) -> Self {
        let (op, width) = scalar_parts(scalar);
        Self {
            op,
            width,
            alignment: scalar.alignment(),
            packed_integer_writer: matches!(op, Op::Int | Op::Uint)
                && matches!(width, 1 | 2 | 4 | 8),
            endian_is_little,
            name,
        }
    }

    /// Current byte order, loaded once per scalar or array operation.
    fn byte_order(&self) -> Endian {
        if self.endian_is_little.load(Ordering::Relaxed) {
            Endian::Little
        } else {
            Endian::Big
        }
    }
}

/// Encode one standalone scalar without involving the record writer.
///
/// The same helper backs ``dumps`` and ``write`` so the public methods cannot
/// disagree about accepted values, byte order, or the bytes handed to a short
/// writer.  This is intentionally off the decode path: source repositories use
/// type writes while constructing output files, never while parsing records.
fn encode_scalar(
    py: Python<'_>,
    scalar: &ScalarType,
    value: &Bound<'_, PyAny>,
    byte_order: Endian,
) -> PyResult<Vec<u8>> {
    let width = scalar.width as usize;
    match scalar.op {
        Op::Float => {
            let number: f64 = value.extract()?;
            Ok(match width {
                2 => {
                    // CPython's ``struct.pack('e', value)`` reports values
                    // beyond binary16's finite range as ``OverflowError``.
                    // Upstream's packed float delegates to that function, so
                    // the standalone writer must translate the native range
                    // failure to the same exception class.
                    let raw = decode::float16_bits(number).map_err(|error| {
                        pyo3::exceptions::PyOverflowError::new_err(error.to_string())
                    })?;
                    match byte_order {
                        Endian::Little => raw.to_le_bytes().to_vec(),
                        Endian::Big => raw.to_be_bytes().to_vec(),
                    }
                }
                4 => {
                    let raw = (number as f32).to_bits();
                    match byte_order {
                        Endian::Little => raw.to_le_bytes().to_vec(),
                        Endian::Big => raw.to_be_bytes().to_vec(),
                    }
                }
                _ => match byte_order {
                    Endian::Little => number.to_bits().to_le_bytes().to_vec(),
                    Endian::Big => number.to_bits().to_be_bytes().to_vec(),
                },
            })
        }
        // ``char`` accepts bytes, integers, and Latin-1 text in current
        // dissect.cstruct.  The latter two are most visible through
        // ``char[None].write`` in dissect.hypervisor.
        Op::Char | Op::CharArray => char_bytes(py, value),
        Op::Wchar | Op::WcharArray => {
            let text: String = value.extract()?;
            let mut raw = Vec::with_capacity(text.len() * 2);
            for unit in text.encode_utf16() {
                match byte_order {
                    Endian::Little => raw.extend_from_slice(&unit.to_le_bytes()),
                    Endian::Big => raw.extend_from_slice(&unit.to_be_bytes()),
                }
            }
            Ok(raw)
        }
        Op::Int | Op::Uint if width == 0 => {
            let signed = matches!(scalar.op, Op::Int);
            let mut number: i128 = value.extract()?;
            if !signed && number < 0 {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "Attempt to encode a negative integer using unsigned LEB128 encoding",
                ));
            }
            let mut raw = Vec::new();
            loop {
                let byte = (number & 0x7f) as u8;
                number >>= 7;
                let done = if signed {
                    (number == 0 && byte & 0x40 == 0) || (number == -1 && byte & 0x40 != 0)
                } else {
                    number == 0
                };
                raw.push(if done { byte } else { byte | 0x80 });
                if done {
                    return Ok(raw);
                }
            }
        }
        _ => {
            let number = crate::types::encode::checked_integer_bits(
                py,
                value,
                scalar.width,
                matches!(scalar.op, Op::Int),
                scalar.packed_integer_writer,
            )?;
            let full = match byte_order {
                Endian::Little => number.to_le_bytes(),
                Endian::Big => number.to_be_bytes(),
            };
            Ok(match byte_order {
                Endian::Little => full[..width].to_vec(),
                Endian::Big => full[full.len() - width..].to_vec(),
            })
        }
    }
}

/// Convert the three character-writer inputs upstream accepts to bytes.
///
/// A scalar accepts one integer as well as bytes or Latin-1 text; arrays also
/// accept integer lists, which PyO3's ``Vec<u8>`` extraction handles here.
pub(crate) fn char_bytes(py: Python<'_>, value: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    if let Ok(raw) = value.extract::<Vec<u8>>() {
        return Ok(raw);
    }
    if let Ok(number) = value.extract::<u8>() {
        return Ok(vec![number]);
    }
    let text: String = value.extract()?;
    py.import("codecs")?
        .call_method1("latin_1_encode", (text,))?
        .get_item(0)?
        .extract()
}

/// Hand one encoded value to a file-like object's ``write`` method.
///
/// Returning the object verbatim preserves the source contract for unusual
/// writers which report a short count or ``None`` rather than the conventional
/// full byte count.
fn write_encoded<'py>(
    py: Python<'py>,
    stream: &Bound<'py, PyAny>,
    raw: &[u8],
) -> PyResult<Bound<'py, PyAny>> {
    stream.call_method1(pyo3::intern!(py, "write"), (PyBytes::new(py, raw),))
}

/// Emit one of upstream's compatibility deprecation warnings.
///
/// These aliases are introspection-only. Importing ``warnings`` on demand
/// keeps both the module import and ordinary record decoding free of this
/// compatibility machinery.
fn warn_deprecated(py: Python<'_>, message: &'static str) -> PyResult<()> {
    py.import(pyo3::intern!(py, "warnings"))?.call_method1(
        pyo3::intern!(py, "warn"),
        (
            message,
            py.get_type::<pyo3::exceptions::PyDeprecationWarning>(),
            2,
        ),
    )?;
    Ok(())
}

#[pymethods]
impl ScalarType {
    /// Decode one value from bytes or a stream.
    /// `endian` overrides the namespace's byte order for this read only.
    ///
    /// Needed because a `Pointer` can be dereferenced with an explicit byte
    /// order (`p.dereference(endian="<")`) without the namespace changing under
    /// everything else that shares it.
    #[pyo3(signature = (source, /, *, endian = None))]
    fn __call__<'py>(
        slf: &Bound<'py, Self>,
        source: &Bound<'py, PyAny>,
        endian: Option<&str>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.borrow().clone();
        // Called with a value rather than something to read from: build the
        // value object. Decoding is untouched -- it still returns the native
        // `int`/`bytes`/`str`, which is what keeps the record path free.
        if is_construction(source, this.op, this.width) {
            return make_value(slf.as_any(), source.clone());
        }
        let endian = match endian {
            None => this.byte_order(),
            Some(spelling) => parse_endian(spelling)?,
        };
        // A LEB128 scalar has no declared width -- that is what the zero means
        // -- so it cannot go through `take_bytes`, which would take no bytes at
        // all and then report "unsupported integer width: 0". The *field* path
        // has always handled this through `Op::Leb128`; the standalone type
        // object (`cs.uleb128(buf)`) did not, and `dissect.regf`-style code
        // that reaches for the scalar directly got an error for a construct rs2
        // decodes correctly one layer down.
        if this.width == 0 {
            return read_leb128_from(py, source, matches!(this.op, Op::Int));
        }

        let mut scratch = Vec::with_capacity(this.width as usize);
        take_bytes(source, this.width as usize, &mut scratch)?;
        read_one(py, this.op, this.width, endian, &scratch, 0)
    }

    /// Low-level reader spelling used by consumer-defined wrapper types.
    #[pyo3(signature = (source, /, *, context = None, endian))]
    fn _read<'py>(
        slf: &Bound<'py, Self>,
        source: &Bound<'py, PyAny>,
        context: Option<&Bound<'py, PyAny>>,
        endian: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let _ = context;
        let value = Self::__call__(slf, source, Some(endian))?;
        // `_read` is the Python type protocol and therefore returns the
        // namespace-bound value subclass with `.dumps()`. Ordinary compiled
        // record fields deliberately keep their plain native values.
        make_value(slf.as_any(), value)
    }

    /// Encode one integer to bytes.
    ///
    /// Definitions commonly use this at import time to turn a declared magic
    /// number into the bytes to compare a header against, so it is part of the
    /// type's surface rather than an extra.
    #[pyo3(signature = (value, /, *, endian = None))]
    fn dumps<'py>(
        slf: &Bound<'py, Self>,
        value: &Bound<'py, PyAny>,
        endian: Option<&str>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let py = slf.py();
        let this = slf.borrow().clone();
        let byte_order = match endian {
            Some(spelling) => parse_endian(spelling)?,
            None => this.byte_order(),
        };
        let bytes = encode_scalar(py, &this, value, byte_order)?;
        Ok(PyBytes::new(py, &bytes))
    }

    /// Encode one value, write it once, and return the sink's reported count.
    #[pyo3(signature = (stream, value, /, *, endian = None))]
    fn write<'py>(
        slf: &Bound<'py, Self>,
        stream: &Bound<'py, PyAny>,
        value: &Bound<'py, PyAny>,
        endian: Option<&str>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let this = slf.borrow().clone();
        let byte_order = match endian {
            Some(spelling) => parse_endian(spelling)?,
            None => this.byte_order(),
        };
        let raw = encode_scalar(slf.py(), &this, value, byte_order)?;
        write_encoded(slf.py(), stream, &raw)
    }

    /// Low-level writer spelling used by upstream type implementations.
    ///
    /// This is an alias over the same checked encoder as :meth:`write`; it is
    /// kept because custom types and source tests call the internal protocol
    /// directly.
    #[pyo3(signature = (stream, value, /, *, endian))]
    fn _write<'py>(
        slf: &Bound<'py, Self>,
        stream: &Bound<'py, PyAny>,
        value: &Bound<'py, PyAny>,
        endian: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        Self::write(slf, stream, value, Some(endian))
    }

    /// Give a factory-created integer the ``int.to_bytes`` error contract.
    ///
    /// ``cstruct._make_int_type`` calls this immediately after registration.
    /// It is deliberately not public package API; the marker lives on the
    /// cached type object so valid writes and record decoding remain unchanged.
    fn _use_arbitrary_integer_writer(&mut self) {
        self.packed_integer_writer = false;
    }

    /// `uint32[15]` -- build an array type.
    ///
    /// `type[None]` is a **null-terminated** array: it stops at the first zero
    /// element and does not consume it. It is *not* "read to the end of the
    /// input", which is what this originally did -- and that difference
    /// corrupted 1,317 `evtx` records, because a binary-XML string value
    /// swallowed the bytes after its terminator and every value after it was
    /// then read at the wrong offset.
    fn __getitem__(slf: &Bound<'_, Self>, count: Option<usize>) -> ArrayType {
        let this = slf.borrow().clone();
        ArrayType {
            element: this,
            count,
            expression: None,
        }
    }

    /// Decode from a stream or a buffer.
    ///
    /// Callers bind this as a value: `handlers[STRING] = cs.wchar[None].read`.
    /// It therefore has to exist as an attribute, not merely work when called.
    #[pyo3(signature = (obj, /))]
    fn read<'py>(slf: &Bound<'py, Self>, obj: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        Self::__call__(slf, obj, None)
    }

    /// Decode from a `bytes`-like object.
    #[pyo3(signature = (data, /))]
    fn reads<'py>(slf: &Bound<'py, Self>, data: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        Self::__call__(slf, data, None)
    }

    /// The value this type reads from all-zero bytes.
    ///
    /// Upstream's default-value protocol: `cs.uint32.__default__()` is 0,
    /// `cs.char.__default__()` is `b"\x00"`, `cs.wchar.__default__()` is
    /// `"\x00"`. Not a stored default -- it is what decoding a zero-filled
    /// field of this type produces, which is why it is computed from the
    /// scalar rather than tabulated.
    fn __default__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        Ok(match self.op {
            Op::Char => PyBytes::new(py, b"\x00").into_any(),
            Op::Wchar => "\u{0}".into_pyobject(py)?.into_any(),
            Op::Float => 0.0f64.into_pyobject(py)?.into_any(),
            _ => 0i64.into_pyobject(py)?.into_any(),
        })
    }

    /// Width in bytes.
    #[getter]
    fn size(&self, py: Python<'_>) -> PyResult<u32> {
        warn_deprecated(
            py,
            "The 'size' property is deprecated, use '__size__', len(cls) or sizeof(cls) instead.",
        )?;
        Ok(u32::from(self.width))
    }

    /// Width in bytes, using current upstream's canonical metadata spelling.
    #[getter]
    fn __size__(&self) -> u32 {
        u32::from(self.width)
    }

    fn __len__(&self) -> usize {
        self.width as usize
    }

    /// A scalar's size never depends on content.
    #[getter]
    fn dynamic(&self, py: Python<'_>) -> PyResult<bool> {
        warn_deprecated(
            py,
            "The 'dynamic' property is deprecated, use '__dynamic__' instead.",
        )?;
        Ok(false)
    }

    /// Scalars always have a definition-time size.
    #[getter]
    fn __dynamic__(&self) -> bool {
        false
    }

    /// Natural alignment in bytes, including an explicit factory override.
    #[getter]
    fn __alignment__(&self) -> u32 {
        self.alignment
    }

    /// Apply `_make_int_type(..., alignment=N)` without changing read plans.
    fn _set_alignment(&mut self, alignment: u32) -> PyResult<()> {
        if alignment == 0 || !alignment.is_power_of_two() {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "alignment must be a non-zero power of two",
            ));
        }
        self.alignment = alignment;
        Ok(())
    }

    /// Concrete array class used when this scalar is subscripted.
    #[getter("__ArrayType__")]
    fn array_type_class<'py>(&self, py: Python<'py>) -> Bound<'py, PyType> {
        py.get_type::<ArrayType>()
    }

    /// Deprecated spelling for :meth:`__ArrayType__`.
    #[getter("ArrayType")]
    fn deprecated_array_type_class<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyType>> {
        warn_deprecated(
            py,
            "The 'ArrayType' property is deprecated, use '__ArrayType__' instead.",
        )?;
        Ok(py.get_type::<ArrayType>())
    }

    /// ``struct`` module format character for packed primitive types.
    #[getter]
    fn __fmt__(&self) -> PyResult<&'static str> {
        match (self.op, self.width) {
            (Op::Uint, 1) => Ok("B"),
            (Op::Uint, 2) => Ok("H"),
            (Op::Uint, 4) => Ok("I"),
            (Op::Uint, 8) => Ok("Q"),
            (Op::Int, 1) => Ok("b"),
            (Op::Int, 2) => Ok("h"),
            (Op::Int, 4) => Ok("i"),
            (Op::Int, 8) => Ok("q"),
            (Op::Float, 2) => Ok("e"),
            (Op::Float, 4) => Ok("f"),
            (Op::Float, 8) => Ok("d"),
            (Op::Char, 1) => Ok("c"),
            (Op::Wchar, 2) => Ok("H"),
            _ => Err(pyo3::exceptions::PyAttributeError::new_err(format!(
                "{} has no struct packing format",
                self.name
            ))),
        }
    }

    /// Deprecated spelling for :attr:`__fmt__`.
    #[getter]
    fn packchar(&self, py: Python<'_>) -> PyResult<&'static str> {
        warn_deprecated(
            py,
            "The 'packchar' property is deprecated, use '__fmt__' instead.",
        )?;
        self.__fmt__()
    }

    #[getter]
    fn __name__(&self) -> &'static str {
        self.name
    }

    /// Whether this integer type uses two's-complement signed decoding.
    ///
    /// The enum factory reads this instead of inferring from the display name:
    /// an arbitrary-width type has the generic display name `scalar`, which
    /// carries no signedness information.
    #[getter]
    fn signed(&self) -> bool {
        matches!(self.op, Op::Int)
    }

    /// Whether this scalar can be the storage type of an enum or flag.
    ///
    /// Upstream accepts only subclasses of its integer base. The Python
    /// factory cannot infer that from ``signed`` alone because floats and
    /// character types also expose scalar metadata, so it consults this cold
    /// marker before constructing an enum class.
    #[getter]
    fn _is_integer(&self) -> bool {
        matches!(self.op, Op::Int | Op::Uint) && self.width != 0
    }

    /// The byte order this type object was built with, spelled as `<` or `>`.
    ///
    /// Value objects consult their type's stored order before applying a
    /// `dumps(endian=)` override. Without that comparison, an override matching
    /// the existing order would reverse bytes that are already correct.
    #[getter]
    fn endian(&self) -> &'static str {
        match self.byte_order() {
            Endian::Little => "<",
            Endian::Big => ">",
        }
    }

    fn __repr__(&self) -> String {
        format!("<ScalarType {}>", self.name)
    }
}

/// An array of primitives: the result of `uint32[15]`.
#[pyclass(name = "ArrayType", module = "dissect.cstruct", skip_from_py_object)]
#[derive(Clone)]
pub struct ArrayType {
    element: ScalarType,
    /// `None` means null-terminated: stop at the first zero element.
    count: Option<usize>,
    /// Canonical count expression for a structure field's cold metadata.
    ///
    /// Standalone scalar subscripting never sets this: it accepts a number or
    /// `None`. A compiled structure may expose `uint8[a + 1]` through
    /// `Field.type`, and retaining that spelling must not turn it into a
    /// terminated `uint8[]`.
    expression: Option<String>,
}

/// Build the metadata-only array object for an expression-sized field.
pub(crate) fn expression_array_type(
    py: Python<'_>,
    element: &Bound<'_, PyAny>,
    expression: &str,
) -> PyResult<Py<PyAny>> {
    let scalar = element
        .cast::<ScalarType>()
        .map_err(|_| {
            pyo3::exceptions::PyTypeError::new_err(
                "an expression array currently requires a scalar element type",
            )
        })?
        .borrow()
        .clone();
    Ok(Py::new(
        py,
        ArrayType {
            element: scalar,
            count: None,
            expression: Some(expression.to_owned()),
        },
    )?
    .into_any())
}

/// Build the Python value for a decoded array.
///
/// Current upstream specialises character arrays: `char[n]` is one `bytes`
/// value and `wchar[n]` is one `str`, while other element types produce lists.
/// The materialised Python shape must follow that distinction as well as the
/// array's byte length.
fn array_value<'py>(
    py: Python<'py>,
    element: ScalarType,
    data: &[u8],
    count: usize,
) -> PyResult<Bound<'py, PyAny>> {
    let width = element.width.max(1) as usize;
    match element.op {
        Op::Char => Ok(PyBytes::new(py, &data[..count.min(data.len())]).into_any()),
        Op::Wchar => {
            let units: Vec<u16> = data[..(count * width).min(data.len())]
                .chunks_exact(2)
                .map(|pair| match element.byte_order() {
                    Endian::Little => u16::from_le_bytes([pair[0], pair[1]]),
                    Endian::Big => u16::from_be_bytes([pair[0], pair[1]]),
                })
                .collect();
            // Lossy: forensic data contains unpaired surrogates, and losing a
            // record over one is worse than a replacement character.
            Ok(String::from_utf16_lossy(&units)
                .into_pyobject(py)?
                .into_any())
        }
        _ => {
            let list = PyList::empty(py);
            for index in 0..count {
                list.append(read_one(
                    py,
                    element.op,
                    element.width,
                    element.byte_order(),
                    data,
                    index * width,
                )?)?;
            }
            Ok(list.into_any())
        }
    }
}

/// Count elements up to a zero terminator.
fn scan_terminated(data: &[u8], width: usize) -> Option<usize> {
    if width == 1 {
        return data.iter().position(|&byte| byte == 0);
    }
    let mut count = 0;
    let mut pos = 0;
    while pos + width <= data.len() {
        if data[pos..pos + width].iter().all(|&b| b == 0) {
            return Some(count);
        }
        count += 1;
        pos += width;
    }
    None
}

/// Byte ceiling rounded down to a complete element.
fn terminated_scan_limit(width: usize) -> usize {
    let byte_limited_elements = MAX_TERMINATED_SCAN_BYTES / width;
    let element_limit = (MAX_ARRAY_ELEMENTS as usize).min(byte_limited_elements);
    element_limit * width
}

/// Stable resource-limit exception for a missing terminator beyond the cap.
fn terminated_limit_error() -> PyErr {
    pyo3::exceptions::PyValueError::new_err(format!(
        "maximum null-terminated array scan is {MAX_TERMINATED_SCAN_BYTES} bytes"
    ))
}

/// The exception upstream's element-at-a-time `_read_0` raises at physical EOF.
fn terminated_eof_error() -> PyErr {
    pyo3::exceptions::PyEOFError::new_err("unexpected end of stream")
}

/// Read a terminated array from a buffer without copying beyond its scan cap.
fn read_terminated_buffer(
    source: &Bound<'_, PyAny>,
    width: usize,
    scratch: &mut Vec<u8>,
) -> PyResult<usize> {
    let scan_limit = terminated_scan_limit(width);
    let has_more = input::copy_buffer_prefix(source, scan_limit, scratch)?;
    if let Some(count) = scan_terminated(scratch, width) {
        scratch.truncate(count * width);
        return Ok(count);
    }
    if has_more || scratch.len() == scan_limit {
        return Err(terminated_limit_error());
    }
    Err(terminated_eof_error())
}

/// Return the starting offset when a stream exposes a usable cursor.
fn seekable_start(stream: &Bound<'_, PyAny>) -> Option<u64> {
    stream
        .call_method0(pyo3::intern!(stream.py(), "tell"))
        .and_then(|value| value.extract::<u64>())
        .ok()
}

/// Read one complete element at a time when bytes cannot be put back.
fn read_terminated_non_seekable(
    stream: &Bound<'_, PyAny>,
    width: usize,
    scratch: &mut Vec<u8>,
) -> PyResult<usize> {
    scratch.clear();
    let scan_limit = terminated_scan_limit(width);
    while scratch.len() < scan_limit {
        let chunk = stream.call_method1(pyo3::intern!(stream.py(), "read"), (width,))?;
        let bytes = chunk.cast::<PyBytes>().map_err(|_| {
            pyo3::exceptions::PyTypeError::new_err("stream.read() did not return bytes")
        })?;
        let data = bytes.as_bytes();
        if data.len() < width {
            return Err(terminated_eof_error());
        }
        if data.len() > width {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "stream.read(size) returned more bytes than requested",
            ));
        }
        if data.iter().all(|&byte| byte == 0) {
            return Ok(scratch.len() / width);
        }
        scratch.extend_from_slice(data);
    }
    Err(terminated_limit_error())
}

/// Scan a seekable stream in chunks while retaining only an incomplete element.
fn read_terminated_seekable(
    stream: &Bound<'_, PyAny>,
    start: u64,
    width: usize,
    scratch: &mut Vec<u8>,
) -> PyResult<usize> {
    scratch.clear();
    let scan_limit = terminated_scan_limit(width);
    let request_size = (TERMINATED_SCAN_CHUNK / width).max(1) * width;
    let mut carry = Vec::new();
    let mut total_read = 0usize;

    while total_read < scan_limit {
        let request = request_size.min(scan_limit - total_read);
        let chunk = stream.call_method1(pyo3::intern!(stream.py(), "read"), (request,))?;
        let bytes = chunk.cast::<PyBytes>().map_err(|_| {
            pyo3::exceptions::PyTypeError::new_err("stream.read() did not return bytes")
        })?;
        let returned = bytes.as_bytes();
        if returned.is_empty() {
            return Err(terminated_eof_error());
        }

        // A broken file-like object can ignore its size argument. Inspect only
        // the requested prefix and restore the logical position before the next
        // iteration, so it cannot bypass the byte budget or skip input.
        let accepted_len = returned.len().min(request);
        let accepted = &returned[..accepted_len];
        if carry.is_empty() {
            // Ordinary files honour the whole-element request. Scan their
            // bytes in place: allocating a joined carry/window on every short
            // string would make the bounded path needlessly slower than
            // upstream's element loop.
            let complete_bytes = (accepted.len() / width) * width;
            if let Some(count_in_chunk) = scan_terminated(&accepted[..complete_bytes], width) {
                let value_bytes = count_in_chunk * width;
                scratch.extend_from_slice(&accepted[..value_bytes]);
                return finish_terminated(
                    stream,
                    start,
                    total_read,
                    value_bytes,
                    width,
                    scratch.len() / width,
                );
            }
            scratch.extend_from_slice(&accepted[..complete_bytes]);
            carry.extend_from_slice(&accepted[complete_bytes..]);
        } else {
            // A short read split an element. Only this cold path joins the
            // `width - 1` carry with the next bytes.
            let processed_before = total_read - carry.len();
            let mut window = Vec::with_capacity(carry.len() + accepted.len());
            window.extend_from_slice(&carry);
            window.extend_from_slice(accepted);
            let complete_bytes = (window.len() / width) * width;
            if let Some(count_in_window) = scan_terminated(&window[..complete_bytes], width) {
                let value_bytes = count_in_window * width;
                scratch.extend_from_slice(&window[..value_bytes]);
                return finish_terminated(
                    stream,
                    start,
                    processed_before,
                    value_bytes,
                    width,
                    scratch.len() / width,
                );
            }
            scratch.extend_from_slice(&window[..complete_bytes]);
            carry.clear();
            carry.extend_from_slice(&window[complete_bytes..]);
        }
        total_read += accepted_len;

        if returned.len() > request {
            let logical = start
                .checked_add(total_read as u64)
                .ok_or_else(terminated_limit_error)?;
            stream.call_method1(pyo3::intern!(stream.py(), "seek"), (logical,))?;
        }
    }
    Err(terminated_limit_error())
}

/// Seek past a found terminator and return the number of value elements.
fn finish_terminated(
    stream: &Bound<'_, PyAny>,
    start: u64,
    processed_before: usize,
    value_bytes: usize,
    width: usize,
    count: usize,
) -> PyResult<usize> {
    let consumed = processed_before
        .checked_add(value_bytes)
        .and_then(|value| value.checked_add(width))
        .ok_or_else(terminated_limit_error)?;
    let end = start
        .checked_add(consumed as u64)
        .ok_or_else(terminated_limit_error)?;
    stream.call_method1(pyo3::intern!(stream.py(), "seek"), (end,))?;
    Ok(count)
}

/// Read a terminated array with exact cursor behavior for each stream class.
fn read_terminated_stream(
    stream: &Bound<'_, PyAny>,
    width: usize,
    scratch: &mut Vec<u8>,
) -> PyResult<usize> {
    match seekable_start(stream) {
        Some(start) => read_terminated_seekable(stream, start, width, scratch),
        None => read_terminated_non_seekable(stream, width, scratch),
    }
}

#[pymethods]
impl ArrayType {
    /// Encode a sequence of values at this array's element width.
    #[pyo3(signature = (value, /, *, endian = None))]
    fn dumps<'py>(
        slf: &Bound<'py, Self>,
        value: &Bound<'py, PyAny>,
        endian: Option<&str>,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let py = slf.py();
        let this = slf.borrow().clone();
        let element = &this.element;
        let byte_order = match endian {
            Some(spelling) => parse_endian(spelling)?,
            None => element.byte_order(),
        };
        // `char[n]` and `wchar[n]` are one value, not a sequence of them, so
        // they go to the element encoder whole -- the same distinction
        // `array_value` makes when decoding.
        if matches!(
            element.op,
            Op::Char | Op::CharArray | Op::Wchar | Op::WcharArray
        ) {
            let mut out = encode_scalar(py, element, value, byte_order)?;
            let unit = element.width.max(1) as usize;
            if this.count.is_none() && this.expression.is_none() {
                out.extend(std::iter::repeat_n(0, unit));
            } else if let Some(count) = this.count {
                let wanted = count * unit;
                if out.len() > wanted {
                    return Err(typed_err(
                        "ArraySizeError",
                        format!(
                            "Expected static array size {count}, got {} instead.",
                            out.len() / unit
                        ),
                    ));
                }
                out.resize(wanted, 0);
            }
            return Ok(PyBytes::new(py, &out));
        }
        let mut out = Vec::new();
        let items: Vec<_> = value.try_iter()?.collect::<PyResult<_>>()?;
        if let Some(count) = this.count {
            if items.len() > count {
                return Err(typed_err(
                    "ArraySizeError",
                    format!(
                        "Expected static array size {count}, got {} instead.",
                        items.len()
                    ),
                ));
            }
        }
        for item in &items {
            out.extend_from_slice(&encode_scalar(py, element, item, byte_order)?);
        }
        if let Some(count) = this.count {
            let missing = count - items.len();
            let default = element.__default__(py)?;
            let encoded_default = encode_scalar(py, element, &default, byte_order)?;
            for _ in 0..missing {
                out.extend_from_slice(&encoded_default);
            }
        } else if this.expression.is_none() {
            let default = element.__default__(py)?;
            out.extend_from_slice(&encode_scalar(py, element, &default, byte_order)?);
        }
        Ok(PyBytes::new(py, &out))
    }

    /// Encode an array, write it once, and return the sink's reported count.
    #[pyo3(signature = (stream, value, /, *, endian = None))]
    fn write<'py>(
        slf: &Bound<'py, Self>,
        stream: &Bound<'py, PyAny>,
        value: &Bound<'py, PyAny>,
        endian: Option<&str>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let raw = Self::dumps(slf, value, endian)?;
        write_encoded(slf.py(), stream, raw.as_bytes())
    }

    /// Low-level array writer spelling retained by current upstream.
    #[pyo3(signature = (stream, value, /, *, endian))]
    fn _write<'py>(
        slf: &Bound<'py, Self>,
        stream: &Bound<'py, PyAny>,
        value: &Bound<'py, PyAny>,
        endian: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        Self::write(slf, stream, value, Some(endian))
    }

    /// Decode the array from bytes or a stream.
    #[pyo3(signature = (source, /))]
    fn __call__<'py>(
        slf: &Bound<'py, Self>,
        source: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let this = slf.borrow().clone();
        // A list or tuple is the array's *value*; `char[n]` given exactly its
        // own byte count is too. Same rule as the scalar above.
        let element_width = this.element.width.max(1) as usize;
        let declared = this.count.unwrap_or(0) * element_width;
        if source.is_instance_of::<PyList>()
            || source.is_instance_of::<pyo3::types::PyTuple>()
            || (matches!(this.element.op, Op::Wchar | Op::WcharArray)
                && source.extract::<String>().is_ok())
            || (matches!(this.element.op, Op::Char | Op::CharArray)
                && source
                    .extract::<Vec<u8>>()
                    .is_ok_and(|raw| declared != 0 && raw.len() == declared))
        {
            return make_value(slf.as_any(), source.clone());
        }
        let width = this.element.width.max(1) as usize;

        let mut scratch = Vec::new();
        let count = match this.count {
            Some(count) => {
                take_bytes(source, count * width, &mut scratch)?;
                count
            }
            None if this.expression.is_none() => match input::classify(source)? {
                InputKind::Buffer => read_terminated_buffer(source, width, &mut scratch)?,
                InputKind::Stream => read_terminated_stream(source, width, &mut scratch)?,
            },
            None => {
                return Err(pyo3::exceptions::PyValueError::new_err(format!(
                    "array length expression {} needs a structure context",
                    this.expression.as_deref().unwrap_or("?")
                )))
            }
        };

        let value = array_value(py, this.element, &scratch, count)?;
        // A standalone array value carries its type just as a constructed
        // scalar does, so both ``type.write(stream, value)`` and
        // ``value.write(stream)`` are available.  Record fields still use the
        // native materializer and remain plain bytes/list values on the hot
        // decode path.
        make_value(slf.as_any(), value)
    }

    /// Decode from a stream or a buffer.
    ///
    /// Callers bind this as a value: `handlers[STRING] = cs.wchar[None].read`.
    /// It therefore has to exist as an attribute, not merely work when called.
    #[pyo3(signature = (obj, /))]
    fn read<'py>(slf: &Bound<'py, Self>, obj: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        Self::__call__(slf, obj)
    }

    /// Decode from a `bytes`-like object.
    #[pyo3(signature = (data, /))]
    fn reads<'py>(slf: &Bound<'py, Self>, data: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        Self::__call__(slf, data)
    }

    /// The value this array type reads from all-zero bytes.
    ///
    /// `cs.char[4].__default__()` is four NUL bytes; `cs.char[None]` has no
    /// declared length, so its default is empty rather than guessed.
    fn __default__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let count = self.count.unwrap_or(0);
        let op = self.element.op;
        Ok(match op {
            Op::Char => PyBytes::new(py, &vec![0u8; count]).into_any(),
            Op::Wchar => "\u{0}".repeat(count).into_pyobject(py)?.into_any(),
            Op::Float => {
                let list = PyList::empty(py);
                for _ in 0..count {
                    list.append(0.0f64)?;
                }
                list.into_any()
            }
            _ => {
                let list = PyList::empty(py);
                for _ in 0..count {
                    list.append(0i64)?;
                }
                list.into_any()
            }
        })
    }

    /// Total size in bytes, or `None` when the count is unbounded.
    #[getter]
    fn size(&self) -> Option<u32> {
        self.count
            .map(|count| count as u32 * u32::from(self.element.width))
    }

    /// Total declared byte size, or `None` for a terminated array.
    #[getter]
    fn __size__(&self) -> Option<u32> {
        self.size()
    }

    fn __len__(&self) -> PyResult<usize> {
        match self.count {
            Some(count) => Ok(count * self.element.width as usize),
            None => Err(pyo3::exceptions::PyTypeError::new_err(
                "an unbounded array has no fixed size",
            )),
        }
    }

    #[getter]
    fn dynamic(&self) -> bool {
        self.count.is_none()
    }

    /// Whether the array's extent is found while reading it.
    #[getter]
    fn __dynamic__(&self) -> bool {
        self.count.is_none()
    }

    /// Element type metadata.
    ///
    /// The decoder stores the compact scalar description directly. Returning
    /// a type object here is a cold introspection operation and does not add a
    /// Python handle to every array read.
    #[getter]
    fn __type__(&self) -> ScalarType {
        self.element.clone()
    }

    /// Deprecated spelling for :attr:`__type__`.
    #[getter(r#type)]
    fn deprecated_type(&self, py: Python<'_>) -> PyResult<ScalarType> {
        warn_deprecated(
            py,
            "The 'type' property is deprecated, use '__type__' instead.",
        )?;
        Ok(self.element.clone())
    }

    /// Declared element count, or `None` for a terminated array.
    #[getter]
    fn __num_entries__(&self) -> Option<usize> {
        self.count
    }

    /// Deprecated spelling for :attr:`__num_entries__`.
    #[getter]
    fn num_entries(&self, py: Python<'_>) -> PyResult<Option<usize>> {
        warn_deprecated(
            py,
            "The 'num_entries' property is deprecated, use '__num_entries__' instead.",
        )?;
        Ok(self.count)
    }

    /// Whether a zero element terminates this array.
    #[getter]
    fn __null_terminated__(&self) -> bool {
        self.count.is_none() && self.expression.is_none()
    }

    /// Deprecated spelling for :attr:`__null_terminated__`.
    #[getter]
    fn null_terminated(&self, py: Python<'_>) -> PyResult<bool> {
        warn_deprecated(
            py,
            "The 'null_terminated' property is deprecated, use '__null_terminated__' instead.",
        )?;
        Ok(self.count.is_none() && self.expression.is_none())
    }

    /// Type objects are true even when their size is dynamic.
    ///
    /// Without this, Python falls back to `__len__`, which correctly raises
    /// for a terminated array but makes a harmless truth test fail.
    fn __bool__(&self) -> bool {
        true
    }

    /// The live namespace byte order used when no per-call override is given.
    #[getter]
    fn endian(&self) -> &'static str {
        match self.element.byte_order() {
            Endian::Little => "<",
            Endian::Big => ">",
        }
    }

    /// Source-compatible display name including the declared element count.
    #[getter]
    fn __name__(&self) -> String {
        if let Some(expression) = &self.expression {
            return format!("{}[{expression}]", self.element.name);
        }
        match self.count {
            Some(count) => format!("{}[{count}]", self.element.name),
            None => format!("{}[]", self.element.name),
        }
    }

    fn __repr__(&self) -> String {
        if let Some(expression) = &self.expression {
            return format!("<ArrayType {}[{expression}]>", self.element.name);
        }
        match self.count {
            Some(count) => format!("<ArrayType {}[{}]>", self.element.name, count),
            None => format!("<ArrayType {}[]>", self.element.name),
        }
    }
}

/// Register both classes on the module.
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<ScalarType>()?;
    module.add_class::<ArrayType>()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{scan_terminated, terminated_scan_limit, MAX_TERMINATED_SCAN_BYTES};

    #[test]
    fn terminated_scan_uses_the_first_aligned_zero_element() {
        assert_eq!(scan_terminated(b"ab\0\0cd\0\0", 2), Some(1));
    }

    #[test]
    fn terminated_scan_does_not_accept_a_partial_element() {
        assert_eq!(scan_terminated(b"ab\0", 2), None);
    }

    #[test]
    fn terminated_byte_limit_is_a_whole_number_of_elements() {
        for width in 1..=16 {
            let limit = terminated_scan_limit(width);
            assert!(limit <= MAX_TERMINATED_SCAN_BYTES);
            assert_eq!(limit % width, 0);
        }
    }
}
