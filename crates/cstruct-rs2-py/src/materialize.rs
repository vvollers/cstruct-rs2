//! Turning a field's bytes into the Python object for it.
//!
//! By the time this runs, the layout is settled: the caller knows where each
//! field starts and how long it is. What is left is creating one Python value
//! per field, which is the most expensive part of decoding a record — the
//! arithmetic is nearly free by comparison, and object creation is not.
//!
//! Values are built as the record is decoded, not on first access. Building
//! them lazily is possible and skips the fields nobody reads, but it means
//! holding the record's bytes alive for as long as the record lives.
//!
//! # The shape of a value follows upstream, not the field's width
//!
//! Two rules here are easy to get wrong because the "obvious" answer is also
//! plausible:
//!
//! * `char[n]` is a single `bytes`, and `wchar[n]` a single `str` — not a list
//!   of one-character values. A consumer reading a filename expects `b"name"`.
//! * An enum field yields a real `IntEnum`/`IntFlag` member, not the integer
//!   behind it, because callers read `.value` and `.name` off it.
//!
//! Both match upstream. Both compare equal to the wrong answer often enough
//! that a test asserting only equality would not notice.

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyList};

use cstruct_rs2_core::decode::{self, FieldSpan};
use cstruct_rs2_core::plan::{CompiledPlan, FieldOp, Op};
use cstruct_rs2_core::schema::Endian;

use crate::errors::to_py_err;
use crate::types::PlanRuntime;

/// Build the Python value for one field.
///
/// `span` locates the field within `buf`. For a fixed record the caller takes
/// the span straight from the plan and no layout pass has run at all.
pub fn field_value<'py>(
    py: Python<'py>,
    plan: &CompiledPlan,
    runtime: &PlanRuntime,
    field: &FieldOp,
    index: usize,
    span: FieldSpan,
    buf: &[u8],
) -> PyResult<Bound<'py, PyAny>> {
    let offset = span.offset as usize;
    let endian = plan.endian;

    match field.op {
        Op::Uint => int_value(py, false, buf, offset, field.elem_size, endian),
        Op::Int => int_value(py, true, buf, offset, field.elem_size, endian),
        Op::Float => {
            let value =
                decode::read_float(buf, offset, field.elem_size, endian).map_err(to_py_err)?;
            Ok(value.into_pyobject(py)?.into_any())
        }
        // A `char` is one byte of `bytes`, not an int. Flattening the two would
        // change the type a consumer sees, which the contract forbids.
        Op::Char => {
            let bytes = decode::read_bytes(buf, offset, 1).map_err(to_py_err)?;
            Ok(PyBytes::new(py, bytes).into_any())
        }
        Op::CharArray => {
            let bytes = decode::read_bytes(buf, offset, span.size as usize).map_err(to_py_err)?;
            Ok(PyBytes::new(py, bytes).into_any())
        }
        Op::Wchar => wide_string(py, buf, offset, 2, endian),
        Op::WcharArray => wide_string(py, buf, offset, span.size as usize, endian),
        Op::Void => py
            .import(pyo3::intern!(py, "dissect.cstruct"))?
            .getattr(pyo3::intern!(py, "Void"))?
            .call0(),
        Op::VoidArray => Ok(PyList::empty(py).into_any()),
        Op::ScalarArray => {
            let dims = plan.array_dims.get(index).map(Box::as_ref).unwrap_or(&[]);
            let enum_class = if field.elem == Op::Enum {
                Some(
                    runtime
                        .enum_classes
                        .get(field.aux as usize)
                        .ok_or_else(|| {
                            pyo3::exceptions::PyRuntimeError::new_err(
                                "missing enum class for array field",
                            )
                        })?,
                )
            } else {
                None
            };
            let enum_signed = plan
                .enums
                .get(field.aux as usize)
                .is_some_and(|enum_plan| enum_plan.signed);
            if dims.len() > 1 {
                nested_array(
                    py,
                    field,
                    span.offset as usize,
                    dims,
                    buf,
                    endian,
                    (enum_class, enum_signed),
                )
            } else {
                scalar_array(py, field, span, buf, endian, enum_class, enum_signed)
            }
        }
        Op::Enum => {
            let enum_plan = plan.enums.get(field.aux as usize).ok_or_else(|| {
                pyo3::exceptions::PyRuntimeError::new_err("missing enum plan for field")
            })?;
            let value = int_value(py, enum_plan.signed, buf, offset, field.elem_size, endian)?;
            let class = runtime
                .enum_classes
                .get(field.aux as usize)
                .ok_or_else(|| {
                    pyo3::exceptions::PyRuntimeError::new_err("missing enum class for field")
                })?;
            enum_value(class, value)
        }
        Op::Leb128 => {
            let (value, _) = decode::read_leb128(buf, offset, matches!(field.elem, Op::Int))
                .map_err(to_py_err)?;
            if matches!(field.elem, Op::Int) {
                Ok(value.into_pyobject(py)?.into_any())
            } else {
                Ok((value as u64).into_pyobject(py)?.into_any())
            }
        }
        Op::Leb128Array => {
            let list = PyList::empty(py);
            let mut at = offset;
            for _ in 0..span.count {
                let (value, length) = decode::read_leb128(buf, at, matches!(field.elem, Op::Int))
                    .map_err(to_py_err)?;
                if matches!(field.elem, Op::Int) {
                    list.append(value)?;
                } else {
                    list.append(value as u64)?;
                }
                at += length as usize;
            }
            Ok(list.into_any())
        }
        // A bitfield reads its whole storage unit and takes a window out of it.
        // The shift and mask were resolved while compiling, so there is no
        // running bit-buffer state to carry through the decode.
        Op::Bits => {
            let window = plan.bit_windows.get(index).copied().unwrap_or_default();
            let unit =
                decode::read_uint(buf, offset, field.size as u8, endian).map_err(to_py_err)?;
            let raw = (unit >> window.shift) & window.mask();

            match field.elem {
                // Masked, never sign-extended, even for a signed storage type.
                // Upstream's `BitBuffer.read` returns the masked bits and does
                // not widen them, so `int16 b : 12` over `0xa7f` is 2687 rather
                // than -1409. C would sign-extend here; upstream does not.
                Op::Enum => {
                    let class = runtime
                        .enum_classes
                        .get(field.aux as usize)
                        .ok_or_else(|| {
                            pyo3::exceptions::PyRuntimeError::new_err(
                                "missing enum class for bitfield",
                            )
                        })?;
                    enum_value(class, raw.into_pyobject(py)?.into_any())
                }
                _ => Ok(raw.into_pyobject(py)?.into_any()),
            }
        }
        // Compiled but not decodable: the field's shape could not be resolved
        // and upstream would have failed here too, just with its own error.
        Op::Unsupported => Err(pyo3::exceptions::PyNotImplementedError::new_err(
            "this field's shape could not be resolved when the type was compiled",
        )),
        Op::Nested | Op::NestedArray | Op::Custom => {
            Err(pyo3::exceptions::PyRuntimeError::new_err(
                "nested and custom fields are materialised by the caller, which owns the runtime",
            ))
        }
    }
}

/// Materialize a matrix whose dimensions were resolved while laying out this
/// record.
///
/// Fixed matrices use [`nested_array`] directly from [`field_value`]. This
/// entry point is reserved for shapes such as `uint8 cube[x][y][z]` and
/// `char argv[argc][]`. The latter is walked row by row because its terminators
/// are separators in the byte stream, not part of the returned strings.
pub(crate) fn runtime_matrix_value<'py>(
    py: Python<'py>,
    plan: &CompiledPlan,
    runtime: &PlanRuntime,
    field: &FieldOp,
    span: FieldSpan,
    dimensions: &[u32],
    buf: &[u8],
) -> PyResult<Bound<'py, PyAny>> {
    let enum_class = if field.elem == Op::Enum {
        Some(
            runtime
                .enum_classes
                .get(field.aux as usize)
                .ok_or_else(|| {
                    pyo3::exceptions::PyRuntimeError::new_err("missing enum class for matrix field")
                })?,
        )
    } else {
        None
    };
    let enum_signed = plan
        .enums
        .get(field.aux as usize)
        .is_some_and(|enum_plan| enum_plan.signed);

    if dimensions.last().copied() != Some(u32::MAX) {
        return nested_array(
            py,
            field,
            span.offset as usize,
            dimensions,
            buf,
            plan.endian,
            (enum_class, enum_signed),
        );
    }

    if dimensions.len() != 2 || !matches!(field.elem, Op::Char | Op::Wchar) {
        return Err(pyo3::exceptions::PyNotImplementedError::new_err(
            "only character rows may use a terminated runtime matrix",
        ));
    }
    let width = field.elem_size.max(1) as usize;
    let end = (span.offset as usize)
        .checked_add(span.size as usize)
        .ok_or_else(|| to_py_err(cstruct_rs2_core::error::Error::EndOfStream))?;
    let mut at = span.offset as usize;
    let rows = PyList::empty(py);
    for _ in 0..dimensions[0] {
        let mut terminator = at;
        loop {
            let unit_end = terminator
                .checked_add(width)
                .ok_or_else(|| to_py_err(cstruct_rs2_core::error::Error::EndOfStream))?;
            let unit = buf
                .get(terminator..unit_end)
                .filter(|_| unit_end <= end)
                .ok_or_else(|| to_py_err(cstruct_rs2_core::error::Error::EndOfStream))?;
            if unit.iter().all(|byte| *byte == 0) {
                break;
            }
            terminator = unit_end;
        }
        let value = match field.elem {
            Op::Char => PyBytes::new(py, &buf[at..terminator]).into_any(),
            Op::Wchar => wide_string(py, buf, at, terminator - at, plan.endian)?,
            _ => unreachable!("the terminated matrix kind was checked above"),
        };
        rows.append(value)?;
        at = terminator + width;
    }
    Ok(rows.into_any())
}

/// Decode UTF-16 into a Python `str`.
///
/// Decoded one code unit at a time rather than through a bulk conversion, which
/// keeps the lossy handling below simple. Wide strings are a small enough share
/// of most records for that to be the right trade.
fn wide_string<'py>(
    py: Python<'py>,
    buf: &[u8],
    offset: usize,
    len: usize,
    endian: Endian,
) -> PyResult<Bound<'py, PyAny>> {
    let bytes = decode::read_bytes(buf, offset, len).map_err(to_py_err)?;
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| match endian {
            Endian::Little => u16::from_le_bytes([pair[0], pair[1]]),
            Endian::Big => u16::from_be_bytes([pair[0], pair[1]]),
        })
        .collect();
    // Lossy on purpose: a forensic image contains unpaired surrogates, and
    // refusing to decode a filename because of one would lose the record.
    //
    // This is a deliberate difference from upstream, which decodes with
    // `utf-16-le` at its default strictness and raises `UnicodeDecodeError` on
    // an unpaired surrogate — losing the whole record over one bad character in
    // one field.
    let text = String::from_utf16_lossy(&units);
    Ok(text.into_pyobject(py)?.into_any())
}

/// Build nested lists for a multi-dimensional array.
///
/// `CHAR h[3][4]` is three `bytes` of four, not twelve one-byte values, and
/// `uint16 m[2][3]` is two lists of three. The recursion walks the dimensions
/// outermost-first; the innermost one is the row that actually reads bytes.
fn nested_array<'py>(
    py: Python<'py>,
    field: &FieldOp,
    offset: usize,
    dims: &[u32],
    buf: &[u8],
    endian: Endian,
    enum_context: (Option<&Py<PyAny>>, bool),
) -> PyResult<Bound<'py, PyAny>> {
    let stride = field.elem_size.max(1) as usize;
    let (outer, rest) = dims.split_first().expect("dims is non-empty");
    let inner_elements: usize = rest.iter().map(|count| *count as usize).product();
    let inner_bytes = inner_elements * stride;

    let list = PyList::empty(py);
    for index in 0..*outer as usize {
        let at = offset + index * inner_bytes;
        let value = if rest.len() > 1 {
            nested_array(py, field, at, rest, buf, endian, enum_context)?
        } else {
            // The innermost dimension. A row of `char` is `bytes`, exactly as a
            // one-dimensional `char[n]` would be.
            match field.elem {
                Op::Char => {
                    let bytes = decode::read_bytes(buf, at, inner_bytes).map_err(to_py_err)?;
                    PyBytes::new(py, bytes).into_any()
                }
                Op::Wchar => wide_string(py, buf, at, inner_bytes, endian)?,
                _ => {
                    let row = PyList::empty(py);
                    for element in 0..inner_elements {
                        row.append(read_element(
                            py,
                            field,
                            at + element * stride,
                            buf,
                            endian,
                            enum_context.0,
                            enum_context.1,
                        )?)?;
                    }
                    row.into_any()
                }
            }
        };
        list.append(value)?;
    }
    Ok(list.into_any())
}

/// Read one element of a scalar array.
fn read_element<'py>(
    py: Python<'py>,
    field: &FieldOp,
    at: usize,
    buf: &[u8],
    endian: Endian,
    enum_class: Option<&Py<PyAny>>,
    enum_signed: bool,
) -> PyResult<Bound<'py, PyAny>> {
    Ok(match field.elem {
        Op::Enum => enum_value(
            enum_class.ok_or_else(|| {
                pyo3::exceptions::PyRuntimeError::new_err("missing enum class for array element")
            })?,
            int_value(py, enum_signed, buf, at, field.elem_size, endian)?,
        )?,
        Op::Int => int_value(py, true, buf, at, field.elem_size, endian)?,
        Op::Float => decode::read_float(buf, at, field.elem_size, endian)
            .map_err(to_py_err)?
            .into_pyobject(py)?
            .into_any(),
        _ => int_value(py, false, buf, at, field.elem_size, endian)?,
    })
}

/// Read an integer of any supported width into a Python int.
///
/// Widths up to eight bytes go through the narrow decoders, whose power-of-two
/// arms compile to a single load; wider ones go through the u128/i128 pair.
/// Python integers are arbitrary precision, so the split is invisible on the
/// Python side -- it exists only so that `uint128` does not make every `uint8`
/// pay for 128-bit arithmetic.
#[inline]
fn int_value<'py>(
    py: Python<'py>,
    signed: bool,
    buf: &[u8],
    at: usize,
    width: u8,
    endian: Endian,
) -> PyResult<Bound<'py, PyAny>> {
    if width > 8 {
        return Ok(if signed {
            decode::read_int128(buf, at, width, endian)
                .map_err(to_py_err)?
                .into_pyobject(py)?
                .into_any()
        } else {
            decode::read_uint128(buf, at, width, endian)
                .map_err(to_py_err)?
                .into_pyobject(py)?
                .into_any()
        });
    }
    Ok(if signed {
        decode::read_int(buf, at, width, endian)
            .map_err(to_py_err)?
            .into_pyobject(py)?
            .into_any()
    } else {
        decode::read_uint(buf, at, width, endian)
            .map_err(to_py_err)?
            .into_pyobject(py)?
            .into_any()
    })
}

/// Build a Python list for an array of fixed-width scalars.
fn scalar_array<'py>(
    py: Python<'py>,
    field: &FieldOp,
    span: FieldSpan,
    buf: &[u8],
    endian: Endian,
    enum_class: Option<&Py<PyAny>>,
    enum_signed: bool,
) -> PyResult<Bound<'py, PyAny>> {
    let count = span.count as usize;
    let stride = field.elem_size.max(1) as usize;
    let list = PyList::empty(py);
    for index in 0..count {
        let at = span.offset as usize + index * stride;
        let value = match field.elem {
            Op::Enum => enum_value(
                enum_class.ok_or_else(|| {
                    pyo3::exceptions::PyRuntimeError::new_err(
                        "missing enum class for array element",
                    )
                })?,
                int_value(py, enum_signed, buf, at, field.elem_size, endian)?,
            )?,
            // An array of arrays: `CHAR Hash[29][16]` is 29 elements of 16
            // bytes each, not 464 integers.
            Op::CharArray => {
                let bytes = decode::read_bytes(buf, at, stride).map_err(to_py_err)?;
                PyBytes::new(py, bytes).into_any()
            }
            Op::WcharArray => wide_string(py, buf, at, stride, endian)?,
            Op::Int => int_value(py, true, buf, at, field.elem_size, endian)?,
            Op::Float => decode::read_float(buf, at, field.elem_size, endian)
                .map_err(to_py_err)?
                .into_pyobject(py)?
                .into_any(),
            _ => int_value(py, false, buf, at, field.elem_size, endian)?,
        };
        list.append(value)?;
    }
    Ok(list.into_any())
}

/// Build the Python member for an enum or flag value.
///
/// Returns a real `IntEnum` or `IntFlag` member, not a plain integer. That is a
/// **correctness** requirement rather than a nicety: `dissect.ntfs` reads
/// `attribute.header.type.value` on an `ATTRIBUTE_TYPE_CODE` field, and a plain
/// `int` has no `.value`.
///
/// A value with no matching member falls back to the raw integer. An `IntEnum`
/// raises `ValueError` for an unknown value, and a forensic image routinely
/// contains one; losing the record over it would be worse than reporting the
/// number.
///
/// Constructing a member is not cheap, and a field usually holds one of a
/// handful of values, so this is a reasonable place to add a small cache if it
/// is ever needed.
fn enum_value<'py>(class: &Py<PyAny>, value: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    // Current upstream creates a typed pseudo-member for an unnamed value.
    // `_CEnum._missing_` and `IntFlag(boundary=KEEP)` implement that policy;
    // construction errors are therefore genuine errors rather than a reason
    // to discard the enum type and return a plain integer.
    class.bind(value.py()).call1((value,))
}
