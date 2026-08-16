//! Working out where each field of a record is.
//!
//! # Most records never come here
//!
//! For a record whose size is known — the overwhelming majority, and the fast
//! path — the compiled plan already holds every field's offset, and the only
//! thing needed is [`check_fixed_size`]: one comparison. [`resolve_layout`] is
//! for the other kind, where a length is *inside* the record:
//!
//! ```text
//! struct Name { uint8 len; char text[len]; uint16 tail; };
//! ```
//!
//! `text` cannot be placed until `len` has been read, and `tail` cannot be placed
//! until `text` has been measured. So this walks the fields in order, reading
//! each one that might be a length into a small scope as it passes, and
//! evaluating each length expression against what has been read so far.
//!
//! # Value size and consumed size are not the same number
//!
//! Every field reports both, and exactly one construct makes them differ: a
//! null-terminated array's *value* excludes the terminator, and what it *consumes*
//! includes it. Conflating them leaves every field after such an array one byte
//! early — which does not fail immediately. It desyncs, and surfaces much later
//! as invalid text in an unrelated field.
//!
//! # Unions and bitfields both break the "cursor advances" rule
//!
//! A union's members all start at offset zero and the record's size is the widest
//! of them, so the cursor takes a maximum rather than a sum. A run of bitfields
//! shares one storage unit: they all report the unit's offset, and only the field
//! that *opens* the unit advances the cursor. Advancing per bitfield instead
//! misplaces every later field, silently.
//!
//! # Input is untrusted
//!
//! Lengths, counts and offsets come out of the data. Every one of them must
//! produce an [`Error`] rather than a panic, an out-of-bounds read, or an
//! allocation sized by whatever the input asked for. That is what
//! [`super::MAX_ARRAY_ELEMENTS`] and the checked arithmetic here are for.

use crate::error::{Error, Result};
use crate::expr::Scope;
use crate::plan::{ArrayDimension, CompiledPlan, FieldOp, Op, DYNAMIC_COUNT};
use crate::schema::Endian;

use super::read::{read_int, read_int128, read_leb128, read_uint, read_uint128};
use super::{CustomRead, CustomReader, MAX_ARRAY_ELEMENTS};

/// Where one field's bytes are, once the record has been laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldSpan {
    /// Byte offset from the start of the record.
    pub offset: u32,
    /// Byte length.
    pub size: u32,
    /// Element count, for arrays. One for scalars.
    pub count: u32,
}

/// A record's resolved layout, for types whose size depends on content.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    /// One span per field, parallel to [`CompiledPlan::fields`].
    pub spans: Vec<FieldSpan>,
    /// Total bytes the record consumed.
    ///
    /// Callers rely on this to advance a stream to the next record, so it has
    /// to count the bytes the record actually occupied — including, for example,
    /// a null-terminated array's terminator, which is consumed but is not part
    /// of the value.
    pub consumed: u32,
    /// Resolved shapes for content-sized matrices, keyed by field index.
    ///
    /// Empty for ordinary dynamic records. Keeping this sparse avoids a
    /// per-field allocation for the common one-dimensional length expression.
    pub runtime_array_dims: Vec<(u32, Box<[u32]>)>,
}

/// Resolve the layout of a record whose size depends on its content.
///
/// Only called for a plan with [`CompiledPlan::total_size`] of `None`. For a
/// fixed record the plan already holds every offset, and calling this would be
/// pure waste — the caller checks first.
pub fn resolve_layout(plan: &CompiledPlan, buf: &[u8]) -> Result<Layout> {
    LayoutContext::without_custom_reader().resolve(plan, buf)
}

/// Resolve a layout, with a reader for consumer-supplied types.
pub fn resolve_layout_with(
    plan: &CompiledPlan,
    buf: &[u8],
    custom: Option<&dyn CustomReader>,
) -> Result<Layout> {
    LayoutContext::new(custom).resolve(plan, buf)
}

/// Services that must follow every recursive dynamic-layout call.
///
/// Nested structures use the same custom-type registry as their parent. A
/// single context makes that ownership explicit and prevents a new recursive
/// branch from accidentally calling the reader-less entry point.
#[derive(Clone, Copy)]
struct LayoutContext<'a> {
    custom: Option<&'a dyn CustomReader>,
}

/// Inputs that describe where the field currently being laid out begins.
///
/// Keeping these together makes the hand-off from the record walk to
/// [`field_extent`] explicit: a custom read is valid only for the field and
/// offset at which it was obtained.
#[derive(Clone, Copy)]
struct FieldPlacement {
    index: usize,
    offset: u32,
    custom_read: Option<CustomRead>,
}

impl<'a> LayoutContext<'a> {
    /// Start a layout pass with the optional binding-owned custom reader.
    const fn new(custom: Option<&'a dyn CustomReader>) -> Self {
        Self { custom }
    }

    /// Start the Python-independent layout path used by ordinary core callers.
    const fn without_custom_reader() -> Self {
        Self { custom: None }
    }

    /// Resolve a root or nested plan without dropping contextual services.
    fn resolve(self, plan: &CompiledPlan, buf: &[u8]) -> Result<Layout> {
        if plan.has_runtime_matrices {
            resolve_layout_in::<true>(plan, buf, self)
        } else {
            resolve_layout_in::<false>(plan, buf, self)
        }
    }

    /// Ask the binding-owned reader to read a consumer-supplied type.
    fn read(self, name: &str, buf: &[u8], offset: usize) -> Result<CustomRead> {
        match self.custom {
            Some(reader) => reader.read(name, buf, offset),
            None => Err(Error::InvalidValue {
                message: "a custom type was reached without a reader for it".to_owned(),
            }),
        }
    }
}

/// Resolve one plan using the context supplied by the public entry point.
fn resolve_layout_in<const MATRICES: bool>(
    plan: &CompiledPlan,
    buf: &[u8],
    context: LayoutContext<'_>,
) -> Result<Layout> {
    let mut spans = Vec::with_capacity(plan.fields.len());
    let mut cursor: u32 = 0;
    // Values decoded so far, for expressions that name earlier fields. Only
    // integers can be lengths, so only integers are recorded.
    let mut scope = FieldScope::default();
    let mut runtime_array_dims = Vec::new();

    // Offset of the storage unit the current run of bitfields shares.
    let mut bit_unit_offset: u32 = 0;

    for (index, field) in plan.fields.iter().enumerate() {
        let window = plan.bit_windows.get(index).copied().unwrap_or_default();

        // A bitfield that continues an open unit reads the same bytes as the
        // field that opened it and consumes none of its own. Advancing the
        // cursor per bitfield instead of per unit would misplace every later
        // field, silently.
        if window.is_bitfield() && !window.opens_unit {
            spans.push(FieldSpan {
                offset: bit_unit_offset,
                size: 0,
                count: 1,
            });
            // Still goes into the scope. A continuing bitfield can name a later
            // array length just as the field that opened the unit can. Read the
            // unit's bytes at the opener's offset before this branch continues.
            if let Some(name) = plan.names.get(index) {
                if !name.is_empty() {
                    if let Some(value) = integer_value(
                        plan,
                        index,
                        field,
                        buf,
                        bit_unit_offset as usize,
                        plan.endian,
                    ) {
                        scope.insert(name.as_ref(), value);
                    }
                }
            }
            continue;
        }

        // Fixed aligned records already carry their final offsets. A dynamic
        // record cannot: a preceding array decides this field's starting
        // point. Round the live cursor using the boundary compiled alongside
        // the field, before the field reads any length or value bytes.
        if plan.aligned && !plan.is_union {
            let step = plan
                .field_alignments
                .get(index)
                .copied()
                .unwrap_or(1)
                .max(1);
            debug_assert!(step.is_power_of_two());
            cursor = cursor
                .checked_add(cursor.wrapping_neg() & (step - 1))
                .ok_or(Error::LimitExceeded {
                    limit: "record size",
                    value: u64::from(cursor),
                })?;
        }

        let offset = if plan.is_union { 0 } else { cursor };
        if window.is_bitfield() {
            bit_unit_offset = offset;
        }

        // A fixed field whose bytes are not all present means the record is
        // truncated, and that must be reported as end-of-stream.
        //
        // The check has to happen here rather than being left to the field
        // reader. Without it the *next* field's length expression looks up a
        // name that was never decoded and fails with "not defined in this
        // scope" — a `ValueError`. Callers end a record run by catching
        // `EOFError`, so a `ValueError` escapes that loop and aborts the whole
        // walk instead of ending it.
        if !field.dynamic {
            let end = (offset as usize).saturating_add(field.size as usize);
            if end > buf.len() {
                return Err(Error::EndOfStream);
            }
        }

        // Custom fields are read during layout because their byte width is not
        // declared in the schema. If that read produced an integer, it belongs
        // in the expression scope before the next field is placed. Upstream's
        // structure reader does the same after invoking a registered
        // `BaseType`; `ProtobufVarint` followed by `char body[length]` relies
        // on it in `dissect.target`.
        let custom_read = if field.dynamic && field.op == Op::Custom {
            let name =
                plan.custom_names
                    .get(field.aux as usize)
                    .ok_or_else(|| Error::InvalidValue {
                        message: "custom field refers to a missing type name".to_owned(),
                    })?;
            Some(context.read(name, buf, offset as usize)?)
        } else {
            None
        };

        let (size, count, consumed) = if !MATRICES {
            field_extent(
                plan,
                field,
                FieldPlacement {
                    index,
                    offset,
                    custom_read,
                },
                buf,
                &scope,
                context,
            )?
        } else {
            let dimensions = plan
                .runtime_array_dims
                .get(index)
                .map(Box::as_ref)
                .unwrap_or(&[]);
            if dimensions.is_empty() {
                field_extent(
                    plan,
                    field,
                    FieldPlacement {
                        index,
                        offset,
                        custom_read,
                    },
                    buf,
                    &scope,
                    context,
                )?
            } else {
                let (size, count, consumed, resolved) =
                    matrix_extent(dimensions, field, offset, buf, &scope)?;
                runtime_array_dims.push((index as u32, resolved));
                (size, count, consumed)
            }
        };

        spans.push(FieldSpan {
            offset,
            size,
            count,
        });

        // Record integer values so later fields can name this one as a length.
        if let Some(name) = plan.names.get(index) {
            if !name.is_empty() {
                if let Some(value) = custom_read.and_then(|read| read.integer).or_else(|| {
                    integer_value(plan, index, field, buf, offset as usize, plan.endian)
                }) {
                    scope.insert(name.as_ref(), value);
                }
            }
        }

        // The cursor advances by what the field *consumed*, which is not always
        // what its value spans -- a null-terminated array also eats its
        // terminator.
        if plan.is_union {
            cursor = cursor.max(consumed);
        } else {
            cursor = cursor.checked_add(consumed).ok_or(Error::LimitExceeded {
                limit: "record size",
                value: u64::from(cursor) + u64::from(consumed),
            })?;
        }
    }

    if plan.aligned {
        let step = plan.alignment.max(1);
        debug_assert!(step.is_power_of_two());
        cursor = cursor
            .checked_add(cursor.wrapping_neg() & (step - 1))
            .ok_or(Error::LimitExceeded {
                limit: "record size",
                value: u64::from(cursor),
            })?;
    }

    Ok(Layout {
        spans,
        consumed: cursor,
        runtime_array_dims,
    })
}

/// Resolve an uncommon multi-dimensional array whose shape depends on fields.
///
/// Upstream permits declarations such as `char argv[argc][]` and
/// `uint8 cube[x][y][z]`. The dimensions are parsed once and evaluated here,
/// next to the ordinary layout scope. A final terminated character dimension
/// is walked row by row because each string may have a different byte length.
#[inline(never)]
fn matrix_extent(
    dimensions: &[ArrayDimension],
    field: &FieldOp,
    offset: u32,
    buf: &[u8],
    scope: &FieldScope,
) -> Result<(u32, u32, u32, Box<[u32]>)> {
    let mut resolved = Vec::with_capacity(dimensions.len());
    for dimension in dimensions {
        let count = match dimension {
            ArrayDimension::Fixed(count) => *count,
            ArrayDimension::Expr(expr) => {
                let value = expr.eval(scope)?;
                if value < 0 {
                    0
                } else {
                    u32::try_from(value).map_err(|_| Error::LimitExceeded {
                        limit: "array elements",
                        value: value as u64,
                    })?
                }
            }
            ArrayDimension::NullTerminated | ArrayDimension::ToEnd => DYNAMIC_COUNT,
        };
        if count != DYNAMIC_COUNT && count > MAX_ARRAY_ELEMENTS {
            return Err(Error::LimitExceeded {
                limit: "array elements",
                value: u64::from(count),
            });
        }
        resolved.push(count);
    }

    let width = u32::from(field.elem_size.max(1));
    let terminal = dimensions.last();
    let outer_count = resolved[..resolved.len().saturating_sub(1)]
        .iter()
        .try_fold(1u32, |product, count| {
            product.checked_mul(*count).ok_or(Error::LimitExceeded {
                limit: "array elements",
                value: u64::from(product) * u64::from(*count),
            })
        })?;
    if outer_count > MAX_ARRAY_ELEMENTS {
        return Err(Error::LimitExceeded {
            limit: "array elements",
            value: u64::from(outer_count),
        });
    }

    if matches!(terminal, Some(ArrayDimension::NullTerminated)) {
        let mut consumed = 0u32;
        let mut values = 0u32;
        for _ in 0..outer_count {
            let row_offset = offset.checked_add(consumed).ok_or(Error::LimitExceeded {
                limit: "array byte size",
                value: u64::from(offset) + u64::from(consumed),
            })?;
            let count = scan_terminated(buf, row_offset as usize, field.elem_size.max(1))?;
            values = values.checked_add(count).ok_or(Error::LimitExceeded {
                limit: "array elements",
                value: u64::from(values) + u64::from(count),
            })?;
            let row = count
                .checked_add(1)
                .and_then(|elements| elements.checked_mul(width))
                .ok_or(Error::LimitExceeded {
                    limit: "array byte size",
                    value: (u64::from(count) + 1) * u64::from(width),
                })?;
            consumed = consumed.checked_add(row).ok_or(Error::LimitExceeded {
                limit: "array byte size",
                value: u64::from(consumed) + u64::from(row),
            })?;
        }
        return Ok((consumed, values, consumed, resolved.into_boxed_slice()));
    }

    if matches!(terminal, Some(ArrayDimension::ToEnd)) {
        if outer_count != 1 {
            return Err(Error::InvalidValue {
                message: "a read-to-end matrix cannot divide the input into several rows"
                    .to_owned(),
            });
        }
        let remaining = buf.len().saturating_sub(offset as usize) as u32;
        let count = remaining / width;
        if let Some(last) = resolved.last_mut() {
            *last = count;
        }
    }

    let count = resolved.iter().try_fold(1u32, |product, count| {
        product.checked_mul(*count).ok_or(Error::LimitExceeded {
            limit: "array elements",
            value: u64::from(product) * u64::from(*count),
        })
    })?;
    if count > MAX_ARRAY_ELEMENTS {
        return Err(Error::LimitExceeded {
            limit: "array elements",
            value: u64::from(count),
        });
    }
    let size = count.checked_mul(width).ok_or(Error::LimitExceeded {
        limit: "array byte size",
        value: u64::from(count) * u64::from(width),
    })?;
    Ok((size, count, size, resolved.into_boxed_slice()))
}

/// How many bytes and elements one field occupies at `offset`.
///
/// Returns `(value_size, count, consumed)`. The two sizes differ for exactly
/// one construct: a null-terminated array's *value* excludes the terminator and
/// the bytes it *consumes* include it. Conflating them left every field after
/// such an array one byte early — `dissect.target`'s mlocate reader desynced
/// after 28 of 228,071 records and then failed on invalid UTF-8, which is what
/// a one-byte offset error looks like from the outside.
fn field_extent(
    plan: &CompiledPlan,
    field: &FieldOp,
    placement: FieldPlacement,
    buf: &[u8],
    scope: &FieldScope,
    context: LayoutContext<'_>,
) -> Result<(u32, u32, u32)> {
    if !field.dynamic {
        // A count of zero is meaningful: `uint32 block_list[0]` declares an
        // empty array. Scalars already carry a count of one, so clamping here
        // only ever corrupted the zero case.
        let count = if field.count == DYNAMIC_COUNT {
            1
        } else {
            field.count
        };
        return Ok((field.size, count, field.size));
    }

    match field.op {
        // A consumer-supplied type reads itself; only it knows its length.
        Op::Custom => {
            let read = placement.custom_read.ok_or_else(|| Error::InvalidValue {
                message: "a custom type was reached without a reader for it".to_owned(),
            })?;
            Ok((read.consumed, 1, read.consumed))
        }
        // Self-delimiting: its length is discovered by reading it.
        Op::Leb128 => {
            let (_, length) = read_leb128(
                buf,
                placement.offset as usize,
                matches!(field.elem, Op::Int),
            )?;
            Ok((length, 1, length))
        }
        Op::Leb128Array => {
            let mut consumed = 0u32;
            let mut count = 0u32;
            loop {
                let at = placement
                    .offset
                    .checked_add(consumed)
                    .ok_or(Error::LimitExceeded {
                        limit: "array byte size",
                        value: u64::from(placement.offset) + u64::from(consumed),
                    })?;
                let (value, length) = read_leb128(buf, at as usize, matches!(field.elem, Op::Int))?;
                if value == 0 {
                    let total = consumed.checked_add(length).ok_or(Error::LimitExceeded {
                        limit: "array byte size",
                        value: u64::from(consumed) + u64::from(length),
                    })?;
                    return Ok((consumed, count, total));
                }
                consumed = consumed.checked_add(length).ok_or(Error::LimitExceeded {
                    limit: "array byte size",
                    value: u64::from(consumed) + u64::from(length),
                })?;
                count = count.checked_add(1).ok_or(Error::LimitExceeded {
                    limit: "array elements",
                    value: u64::from(count) + 1,
                })?;
                if count > MAX_ARRAY_ELEMENTS {
                    return Err(Error::LimitExceeded {
                        limit: "array elements",
                        value: u64::from(count),
                    });
                }
            }
        }
        Op::NestedArray => {
            // An array of structures whose *count* comes from an earlier field.
            //
            // This is not the same as a single dynamically sized structure, and
            // treating it as one silently decoded the first element and dropped
            // the rest. `dissect.regf` sizes its subkey lists this way, so a
            // registry hive reported one subkey where it had 82.
            let sub = plan
                .subplans
                .get(field.aux as usize)
                .ok_or_else(|| Error::InvalidValue {
                    message: "nested field refers to a missing sub-plan".to_owned(),
                })?;
            let count = if field.count != DYNAMIC_COUNT {
                // A fixed count can still have a dynamic *element* width. The
                // field therefore reaches the layout pass even though its
                // count needs no expression. Upstream reads every declared
                // element; treating the missing expression as a sentinel form
                // silently drops all but the first one.
                field.count
            } else {
                match plan.lengths.get(placement.index).and_then(Option::as_ref) {
                    Some(expr) => {
                        let value = expr.eval(scope)?;
                        if value < 0 {
                            return Ok((0, 0, 0));
                        }
                        let count = u32::try_from(value).map_err(|_| Error::LimitExceeded {
                            limit: "array elements",
                            value: value as u64,
                        })?;
                        if count > MAX_ARRAY_ELEMENTS {
                            return Err(Error::LimitExceeded {
                                limit: "array elements",
                                value: u64::from(count),
                            });
                        }
                        count
                    }
                    // `S d[]` -- read elements until one is entirely zero. That
                    // element is the terminator: the cursor steps over it, the
                    // value stops before it.
                    //
                    // Getting this wrong is silent: over `01 02 | 03 04 | 00 00 |
                    // ff`, stopping after one element leaves the following field to
                    // be read out of the second element's bytes. Wrong values, no
                    // exception.
                    //
                    // Upstream stops at an element equal to its type's zero value.
                    // For a structure of integers and characters that is the same
                    // test as "every byte is zero", which is what `scan_terminated`
                    // already applies to scalar and char arrays.
                    None if plan.is_null_terminated(placement.index) => {
                        let mut consumed = 0u32;
                        let mut count = 0u32;
                        loop {
                            let rest = buf
                                .get((placement.offset + consumed) as usize..)
                                .ok_or(Error::EndOfStream)?;
                            // A variable-size element has to be laid out to learn
                            // its width; a fixed one already knows.
                            let stride = match sub.total_size {
                                Some(stride) => stride,
                                None => context.resolve(sub, rest)?.consumed,
                            };
                            // A zero-width element would never reach a terminator.
                            if stride == 0 {
                                return Err(Error::InvalidValue {
                                    message: "a null-terminated array of zero-width records \
                                          has no end"
                                        .to_owned(),
                                });
                            }
                            let element = rest.get(..stride as usize).ok_or(Error::EndOfStream)?;
                            let end = consumed.checked_add(stride).ok_or(Error::LimitExceeded {
                                limit: "array byte size",
                                value: u64::from(consumed) + u64::from(stride),
                            })?;
                            if element.iter().all(|&byte| byte == 0) {
                                return Ok((consumed, count, end));
                            }
                            consumed = end;
                            count += 1;
                            if count > MAX_ARRAY_ELEMENTS {
                                return Err(Error::LimitExceeded {
                                    limit: "array elements",
                                    value: u64::from(count),
                                });
                            }
                        }
                    }
                    // `S d[EOF]` -- read records until the input runs out.
                    //
                    // Current upstream reads every complete nested record that
                    // fits in `S d[EOF]`. Keep that uncommon loop in the
                    // non-inlined helper: `field_extent` is part of the common
                    // dynamic-record walk, and embedding the whole loop there
                    // would enlarge its optimized instruction body for records
                    // that never contain an input-terminated nested array.
                    None if plan.to_end.get(placement.index).copied().unwrap_or(false) => {
                        return nested_array_to_end(sub, buf, placement.offset, context);
                    }
                    // Defensive: a dynamic nested array is always one of the three
                    // cases above (a length expression, `[]`, or `[EOF]`), so no
                    // compiled shape reaches this. One element rather than an error
                    // because a plan this side cannot classify is an internal
                    // inconsistency, not something an image should be refused over.
                    None => 1,
                }
            };

            match sub.total_size {
                // Fixed-size elements: the extent is arithmetic.
                Some(stride) => {
                    let total = u64::from(count) * u64::from(stride);
                    let total = u32::try_from(total).map_err(|_| Error::LimitExceeded {
                        limit: "array byte size",
                        value: total,
                    })?;
                    Ok((total, count, total))
                }
                // Variable-size elements: each has to be laid out in turn.
                None => {
                    let mut consumed = 0u32;
                    for _ in 0..count {
                        let rest = buf
                            .get((placement.offset + consumed) as usize..)
                            .ok_or(Error::EndOfStream)?;
                        let sub_layout = context.resolve(sub, rest)?;
                        consumed = consumed.checked_add(sub_layout.consumed).ok_or(
                            Error::LimitExceeded {
                                limit: "array byte size",
                                value: u64::from(consumed),
                            },
                        )?;
                    }
                    Ok((consumed, count, consumed))
                }
            }
        }
        Op::Nested => {
            // A single nested structure whose size depends on its content has
            // to be laid out to know how long it is.
            let sub = plan
                .subplans
                .get(field.aux as usize)
                .ok_or_else(|| Error::InvalidValue {
                    message: "nested field refers to a missing sub-plan".to_owned(),
                })?;
            let rest = buf
                .get(placement.offset as usize..)
                .ok_or(Error::EndOfStream)?;
            let sub_layout = context.resolve(sub, rest)?;
            Ok((sub_layout.consumed, 1, sub_layout.consumed))
        }
        _ => {
            // `[EOF]` takes everything that is left. Distinguished from `[]` by
            // the plan rather than inferred: both are dynamic arrays with no
            // length expression, and guessing between "stop at a zero" and
            // "read to the end" is how the evtx corruption happened.
            if plan.to_end.get(placement.index).copied().unwrap_or(false) {
                let remaining = buf.len().saturating_sub(placement.offset as usize) as u32;
                let elem = u32::from(field.elem_size.max(1));
                let count = remaining / elem;
                let size = count * elem;
                return Ok((size, count, size));
            }

            // No length expression on a dynamic array means null-terminated.
            let null_terminated = plan.is_null_terminated(placement.index);
            let count = match plan.lengths.get(placement.index).and_then(Option::as_ref) {
                Some(expr) => {
                    let value = expr.eval(scope)?;
                    // A negative length is an *empty* array, not an error.
                    //
                    // Upstream builds arrays with `range(count)`, and
                    // `range(-40)` is empty. `dissect.extfs` depends on this:
                    // `char i_extra[256 - 128 - i_extra_isize]` goes negative on
                    // every ext3 image, where `i_extra_isize` is not meaningful.
                    // Treating it as an error instead would refuse records
                    // upstream decodes without complaint, so this follows
                    // upstream's semantics rather than making a judgement about
                    // what a negative length ought to mean.
                    if value < 0 {
                        return Ok((0, 0, 0));
                    }
                    let count = u32::try_from(value).map_err(|_| Error::LimitExceeded {
                        limit: "array elements",
                        value: value as u64,
                    })?;
                    if count > MAX_ARRAY_ELEMENTS {
                        return Err(Error::LimitExceeded {
                            limit: "array elements",
                            value: u64::from(count),
                        });
                    }
                    count
                }
                // No expression and dynamic means null-terminated: scan for the
                // terminator, bounded by the input.
                None => scan_terminated(buf, placement.offset as usize, field.elem_size.max(1))?,
            };
            let elem_size = u32::from(field.elem_size.max(1));
            let size = u64::from(count) * u64::from(elem_size);
            let size = u32::try_from(size).map_err(|_| Error::LimitExceeded {
                limit: "array byte size",
                value: size,
            })?;
            // A null-terminated array also consumes its terminator. The value
            // does not include it; the cursor must step over it.
            let consumed = if null_terminated {
                size.checked_add(elem_size).ok_or(Error::LimitExceeded {
                    limit: "array byte size",
                    value: u64::from(size) + u64::from(elem_size),
                })?
            } else {
                size
            };
            Ok((size, count, consumed))
        }
    }
}

/// Lay out `S d[EOF]` — an array of structures running to the end of the input.
///
/// Returns `(size, count, consumed)`, like the other arms of [`field_extent`].
///
/// # Why this is a separate, never-inlined function
///
/// It is only reached for one rare declaration, but the cost of writing it
/// inline is paid by every dynamic record.
///
/// `field_extent` is small enough that the compiler inlines it into
/// `resolve_layout_with`, which holds the loop that runs over every field of
/// every dynamic record. Putting this loop directly in the match arm grows that
/// merged function by roughly a third — a kilobyte and a half of machine code
/// that, for an ordinary record, never executes but still has to be paged in,
/// predicted around, and allocated registers between.
///
/// Keep it out of line. The rule this is an instance of: a cold path may not be
/// allowed to change the shape of a hot one.
#[inline(never)]
fn nested_array_to_end(
    sub: &CompiledPlan,
    buf: &[u8],
    offset: u32,
    context: LayoutContext<'_>,
) -> Result<(u32, u32, u32)> {
    let mut consumed = 0u32;
    let mut count = 0u32;
    loop {
        // The input running out *is* the terminator here, so an exhausted
        // buffer ends the array rather than erroring.
        let rest = match buf.get((offset + consumed) as usize..) {
            Some(rest) if !rest.is_empty() => rest,
            _ => return Ok((consumed, count, consumed)),
        };
        let stride = match sub.total_size {
            Some(stride) => stride,
            // A variable-size element that will not lay out is a partial
            // trailing record, not a failure: `[EOF]` takes what is whole.
            None => match context.resolve(sub, rest) {
                Ok(layout) => layout.consumed,
                Err(Error::EndOfStream) => return Ok((consumed, count, consumed)),
                Err(error) => return Err(error),
            },
        };
        // A zero-width record would loop forever; a record wider than what is
        // left is not an element.
        if stride == 0 || stride as usize > rest.len() {
            return Ok((consumed, count, consumed));
        }
        consumed = consumed.checked_add(stride).ok_or(Error::LimitExceeded {
            limit: "array byte size",
            value: u64::from(consumed) + u64::from(stride),
        })?;
        count += 1;
        if count > MAX_ARRAY_ELEMENTS {
            return Err(Error::LimitExceeded {
                limit: "array elements",
                value: u64::from(count),
            });
        }
    }
}

/// Count elements up to a zero terminator.
///
/// Returns the element count *excluding* the terminator, and fails if the input
/// ends first — a run-on array in a corrupt record must be an error, not a read
/// past the buffer.
fn scan_terminated(buf: &[u8], offset: usize, elem_size: u8) -> Result<u32> {
    let elem = elem_size as usize;
    let mut pos = offset;
    let mut count = 0u32;
    loop {
        let chunk = buf.get(pos..pos + elem).ok_or(Error::EndOfStream)?;
        if chunk.iter().all(|&b| b == 0) {
            return Ok(count);
        }
        count += 1;
        if count > MAX_ARRAY_ELEMENTS {
            return Err(Error::LimitExceeded {
                limit: "array elements",
                value: u64::from(count),
            });
        }
        pos += elem;
    }
}

/// Read a field's value if it is an integer that could size a later field.
fn integer_value(
    plan: &CompiledPlan,
    index: usize,
    field: &FieldOp,
    buf: &[u8],
    offset: usize,
    endian: Endian,
) -> Option<i128> {
    match field.op {
        // A bitfield can name a length -- `uint16 size : 4; char d[size];` --
        // and upstream supports it.
        //
        // The compiler stores a bitfield's complete storage-unit width in
        // `field.size`; `field.elem_size` describes the logical scalar and is
        // not the number of bytes available at this offset. This is the same
        // computation the materializer performs, so the value placed in scope
        // for a later length expression cannot disagree with the field value.
        // Unsigned and unwidened, for the same reason -- upstream's
        // `BitBuffer.read` masks and does not sign-extend.
        Op::Bits => {
            let window = plan.bit_windows.get(index).copied().unwrap_or_default();
            read_uint(buf, offset, field.size as u8, endian)
                .ok()
                .map(|unit| i128::from((unit >> window.shift) & window.mask()))
        }
        // Widths past eight bytes need the u128/i128 readers; the narrow ones
        // report an unsupported width rather than a value, which would silently
        // drop the name from the scope.
        Op::Uint | Op::Enum if field.elem_size > 8 => {
            read_uint128(buf, offset, field.elem_size, endian)
                .ok()
                .and_then(|value| i128::try_from(value).ok())
        }
        Op::Int if field.elem_size > 8 => read_int128(buf, offset, field.elem_size, endian).ok(),
        Op::Uint | Op::Enum => read_uint(buf, offset, field.elem_size, endian)
            .ok()
            .map(i128::from),
        Op::Int => read_int(buf, offset, field.elem_size, endian)
            .ok()
            .map(i128::from),
        // A LEB128 can name a later length just like a fixed-width integer.
        // Current upstream exercises `uleb128 name_len; char name[name_len];`
        // in `tests/test_types_leb128.py`. Reading the value here places it in
        // the expression scope while preserving its variable byte width.
        Op::Leb128 => read_leb128(buf, offset, matches!(field.elem, Op::Int))
            .ok()
            .map(|(value, _)| i128::from(value)),
        _ => None,
    }
}

/// Field values decoded so far, for length expressions.
///
/// A `Vec` of pairs rather than a map: a structure has a handful of fields, and
/// a linear scan over a contiguous slice beats hashing for those sizes.
#[derive(Debug, Default)]
struct FieldScope {
    values: Vec<(String, i128)>,
}

impl FieldScope {
    fn insert(&mut self, name: &str, value: i128) {
        self.values.push((name.to_owned(), value));
    }
}

impl Scope for FieldScope {
    fn lookup(&self, name: &str) -> Option<i128> {
        // Last wins, so a later field of the same name shadows an earlier one,
        // matching C scoping.
        self.values
            .iter()
            .rev()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| *value)
    }
}

/// Check that `buf` holds a complete record for `plan`.
///
/// The whole of a fixed record's validation: one comparison. This is why a
/// fixed structure is the fast path.
#[inline]
pub fn check_fixed_size(plan: &CompiledPlan, buf: &[u8]) -> Result<u32> {
    match plan.total_size {
        Some(size) => {
            if buf.len() < size as usize {
                Err(Error::EndOfStream)
            } else {
                Ok(size)
            }
        }
        None => Err(Error::InvalidValue {
            message: format!("{} does not have a fixed size", plan.name),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{parse, parse_aligned_into};
    use crate::plan::Compiler;
    use crate::schema::Schema;
    use std::sync::Arc;

    fn plan_for(source: &str, name: &str) -> Arc<CompiledPlan> {
        let schema = parse(source, Endian::Little).unwrap();
        Compiler::new(&schema).compile(name).unwrap()
    }

    fn aligned_plan_for(source: &str, name: &str) -> Arc<CompiledPlan> {
        let mut schema = Schema::with_builtins(Endian::Little);
        parse_aligned_into(source, &mut schema).unwrap();
        Compiler::new(&schema).compile(name).unwrap()
    }

    /// A bitfield can size a later array, and *any* bitfield of a unit can --
    /// not only the one that opens it. Values checked against upstream.
    #[test]
    fn bitfields_can_size_a_later_array() {
        let plan = plan_for(
            "struct T { uint16 size : 4; char d[size]; uint8 tail; };",
            "T",
        );
        let buf = [0x03, 0x00, b'A', b'B', b'C', 0xff];
        let layout = resolve_layout(&plan, &buf).unwrap();
        assert_eq!(layout.spans[1].size, 3, "d should span three bytes");
        assert_eq!(layout.spans[2].offset, 5, "tail should follow d");

        // `b` is the second field of the unit, so it never opened one.
        let plan = plan_for(
            "struct T { uint8 a : 3; uint8 b : 5; char d[b]; uint8 tail; };",
            "T",
        );
        let buf = [0x11, b'A', b'B', b'C'];
        let layout = resolve_layout(&plan, &buf).unwrap();
        assert_eq!(layout.spans[2].size, 2, "0x11 >> 3 is 2");
        assert_eq!(layout.spans[3].offset, 3);
    }

    #[test]
    fn fixed_records_need_no_layout_pass() {
        let plan = plan_for("struct s { uint16 a; uint32 b; };", "s");
        assert_eq!(check_fixed_size(&plan, &[0u8; 6]).unwrap(), 6);
        assert_eq!(check_fixed_size(&plan, &[0u8; 5]), Err(Error::EndOfStream));
    }

    #[test]
    fn dynamic_arrays_resolve_against_an_earlier_field() {
        let plan = plan_for(
            "struct s { uint16 len; uint8 data[len]; uint16 tail; };",
            "s",
        );
        // len = 3, three data bytes, then the tail.
        let buf = [0x03, 0x00, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
        let layout = resolve_layout(&plan, &buf).unwrap();
        assert_eq!(
            layout.spans[0],
            FieldSpan {
                offset: 0,
                size: 2,
                count: 1
            }
        );
        assert_eq!(
            layout.spans[1],
            FieldSpan {
                offset: 2,
                size: 3,
                count: 3
            }
        );
        assert_eq!(
            layout.spans[2],
            FieldSpan {
                offset: 5,
                size: 2,
                count: 1
            },
            "the trailing field must follow the dynamic array, not a fixed offset"
        );
        assert_eq!(layout.consumed, 7);
    }

    /// Padding after a dynamic value cannot be baked into `FieldOp::offset`;
    /// the live cursor must apply each compiled field boundary.
    #[test]
    fn aligned_dynamic_fields_round_the_runtime_cursor() {
        let plan = aligned_plan_for("struct s { uint8 n; uint16 values[n]; uint32 tail; };", "s");
        let buf = [1, 0, 2, 0, 9, 0, 0, 0];
        let layout = resolve_layout(&plan, &buf).unwrap();
        assert_eq!(layout.spans[1].offset, 2);
        assert_eq!(layout.spans[2].offset, 4);
        assert_eq!(layout.consumed, 8);
    }

    #[test]
    fn runtime_matrices_resolve_every_dimension() {
        let plan = plan_for("struct args { uint32 argc; char argv[argc][]; };", "args");
        let buf = b"\x02\0\0\0hello\0world\0";
        let layout = resolve_layout(&plan, buf).unwrap();
        assert_eq!(layout.spans[1].offset, 4);
        assert_eq!(layout.spans[1].size, 12);
        assert_eq!(layout.consumed, 16);
        assert_eq!(layout.runtime_array_dims[0].1.as_ref(), &[2, u32::MAX]);

        let plan = plan_for(
            "struct cube { uint8 x; uint8 y; uint8 z; uint8 data[x][y][z]; };",
            "cube",
        );
        let layout = resolve_layout(&plan, &[2, 1, 3, 1, 2, 3, 4, 5, 6]).unwrap();
        assert_eq!(layout.spans[3].size, 6);
        assert_eq!(layout.runtime_array_dims[0].1.as_ref(), &[2, 1, 3]);
    }

    #[test]
    fn a_field_named_eof_wins_over_the_read_to_end_sentinel() {
        let plan = plan_for(
            "struct s { uint8 EOF; char data[EOF]; uint8 remainder; };",
            "s",
        );
        let layout = resolve_layout(&plan, &[1, b'a', 2]).unwrap();
        assert_eq!(layout.spans[1].size, 1);
        assert_eq!(layout.spans[2].offset, 2);
        assert_eq!(layout.consumed, 3);
    }

    /// A `wchar[len]` costs two bytes per element, not one. Getting the stride
    /// wrong would silently truncate every filename.
    #[test]
    fn wide_arrays_use_the_element_stride() {
        let plan = plan_for("struct s { uint16 len; wchar name[len]; };", "s");
        let mut buf = vec![0x02, 0x00];
        buf.extend_from_slice(&[b'h', 0, b'i', 0]);
        let layout = resolve_layout(&plan, &buf).unwrap();
        assert_eq!(layout.spans[1].size, 4, "two code units of two bytes");
        assert_eq!(layout.spans[1].count, 2);
        assert_eq!(layout.consumed, 6);
    }

    /// An array of structures sized by an earlier field. `dissect.regf` sizes
    /// its subkey lists this way, and treating it as one nested record made a
    /// hive report one subkey where it had 82.
    #[test]
    fn dynamic_arrays_of_structs_decode_every_element() {
        let plan = plan_for(
            "struct entry { uint32 offset; uint32 hash; };
             struct list { uint16 count; entry entries[count]; };",
            "list",
        );
        let mut buf = vec![0x03, 0x00];
        buf.extend_from_slice(&[0u8; 24]); // three 8-byte entries
        let layout = resolve_layout(&plan, &buf).unwrap();
        assert_eq!(layout.spans[1].count, 3, "all three entries, not one");
        assert_eq!(layout.spans[1].size, 24);
        assert_eq!(layout.consumed, 26);
    }

    #[test]
    fn null_terminated_arrays_stop_at_the_terminator() {
        let plan = plan_for("struct s { uint8 data[]; };", "s");
        let buf = [0xaa, 0xbb, 0x00, 0xcc];
        let layout = resolve_layout(&plan, &buf).unwrap();
        assert_eq!(layout.spans[0].count, 2);
    }

    /// A run-on array in a corrupt record must be an error, not a read past the
    /// end of the buffer.
    #[test]
    fn an_unterminated_array_is_end_of_stream() {
        let plan = plan_for("struct s { uint8 data[]; };", "s");
        let buf = [0xaa, 0xbb, 0xcc];
        assert!(matches!(
            resolve_layout(&plan, &buf),
            Err(Error::EndOfStream)
        ));
    }

    /// A corrupt length field must fail fast rather than try to allocate.
    #[test]
    fn absurd_lengths_are_refused() {
        let plan = plan_for("struct s { uint32 len; uint8 data[len]; };", "s");
        let mut buf = vec![0xff, 0xff, 0xff, 0xff];
        buf.extend_from_slice(&[0u8; 16]);
        match resolve_layout(&plan, &buf) {
            Err(Error::LimitExceeded { limit, .. }) => assert_eq!(limit, "array elements"),
            other => panic!("a four-billion-element array should be refused, got {other:?}"),
        }
    }

    /// A negative length is an empty array, matching upstream's `range(count)`.
    ///
    /// Not a stylistic choice: `dissect.extfs` declares
    /// `char i_extra[256 - 128 - i_extra_isize]`, which goes negative on every
    /// ext3 image. Refusing it made `walkfs` fail outright.
    #[test]
    fn negative_lengths_yield_an_empty_array() {
        let plan = plan_for(
            "struct s { uint16 n; uint8 data[10 - n]; uint16 after; };",
            "s",
        );
        // n = 50, so the array length is 10 - 50 = -40.
        let buf = [50, 0, 0xaa, 0xbb];
        let layout = resolve_layout(&plan, &buf).unwrap();
        assert_eq!(layout.spans[1].count, 0, "a negative length is empty");
        assert_eq!(layout.spans[1].size, 0);
        assert_eq!(
            layout.spans[2].offset, 2,
            "the following field must read straight after the empty array"
        );
    }

    #[test]
    fn unions_are_as_long_as_their_largest_member() {
        let plan = plan_for("union u { uint32 a; uint16 b; };", "u");
        assert_eq!(plan.total_size, Some(4));
    }
}
