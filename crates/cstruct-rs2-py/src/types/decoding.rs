//! Turning a Python source object into a [`Record`].
//!
//! This is the record-level half of decoding. Three modules share the job and
//! it is worth knowing which does what before changing any of them:
//!
//! * [`cstruct_rs2_core::decode`] — the arithmetic. Where each field starts and
//!   how long it is, with no interpreter involved.
//! * [`crate::materialize`] — one field's bytes into one Python value.
//! * this module — a source object into a whole record: choosing how to get the
//!   bytes, laying the record out if it needs it, and assembling the result.
//!
//! # The two shortcuts, and why they are one branch
//!
//! Two shapes are recognised early because they are both common and cheap to
//! recognise:
//!
//! 1. **A fixed-size structure read from a stream.** The size is on the plan, so
//!    this is one exact-size `read()`, one copy from the returned Python bytes
//!    into per-call native storage, and a bounds check, with no layout pass.
//! 2. **A single scalar read from a stream.** Readers that pull values one at a
//!    time — binary XML, varint-framed formats — do this constantly, and routing
//!    each one through the record machinery would dominate their cost.
//!
//! Both reduce to "the plan knows the size", so they are the same branch here
//! rather than two.
//!
//! Anything else goes through the general path, which records *why* it did not
//! qualify (see [`cstruct_rs2_core::telemetry`]) so the gap ranks itself rather
//! than being guessed at.

use std::cell::RefCell;
use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::PyDict;

use cstruct_rs2_core::decode::{self, CustomRead, FieldSpan};
use cstruct_rs2_core::error::Error;
use cstruct_rs2_core::plan::{CompiledPlan, Op, DYNAMIC_COUNT};
use cstruct_rs2_core::telemetry::{self, FallbackReason};

use crate::errors::{internal, to_py_err};
use crate::input::{self, InputKind};
use crate::materialize::{field_value, runtime_matrix_value};
#[cfg(feature = "perf-telemetry")]
use crate::perf_telemetry::{self, Stage};
use crate::types::field::declared_field_name;
use crate::types::record::Record;
use crate::types::runtime::PlanRuntime;

/// Decode one record from whatever Python supplied.
pub fn decode_one<'py>(
    py: Python<'py>,
    plan: &Arc<CompiledPlan>,
    runtime: &Arc<PlanRuntime>,
    source: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    decode_one_classified(py, plan, runtime, source, input::classify(source)?)
}

/// Decode after the caller has already distinguished a buffer from a stream.
///
/// Generated structure construction must classify a single positional value
/// to decide whether it is input bytes or the first field's value. Passing that
/// answer here avoids repeating the buffer-protocol and ``read`` checks on the
/// ordinary decode path.
pub fn decode_one_classified<'py>(
    py: Python<'py>,
    plan: &Arc<CompiledPlan>,
    runtime: &Arc<PlanRuntime>,
    source: &Bound<'py, PyAny>,
    input_kind: InputKind,
) -> PyResult<Bound<'py, PyAny>> {
    match input_kind {
        InputKind::Buffer => {
            // A `bytes` object's buffer is read in place. Other buffer kinds are
            // copied first, which is the simple and correct option for the rarer
            // shapes.
            if let Ok(bytes) = source.cast::<pyo3::types::PyBytes>() {
                return materialise(py, plan, runtime, bytes.as_bytes());
            }
            let mut scratch = Vec::new();
            #[cfg(not(feature = "perf-telemetry"))]
            input::copy_buffer(source, &mut scratch)?;
            #[cfg(feature = "perf-telemetry")]
            {
                let copy_started = perf_telemetry::start();
                let copy_result = input::copy_buffer(source, &mut scratch);
                perf_telemetry::record(
                    &plan.name,
                    Stage::BufferCopy,
                    0,
                    scratch.len(),
                    copy_started.elapsed(),
                    copy_result.is_err(),
                );
                copy_result?;
            }
            materialise(py, plan, runtime, &scratch)
        }
        InputKind::Stream => match plan.total_size {
            Some(size) => {
                #[cfg(feature = "perf-telemetry")]
                let allocation_started = perf_telemetry::start();
                let mut scratch = Vec::with_capacity(size as usize);
                #[cfg(feature = "perf-telemetry")]
                perf_telemetry::record(
                    &plan.name,
                    Stage::FixedScratchAllocation,
                    size as usize,
                    0,
                    allocation_started.elapsed(),
                    false,
                );

                #[cfg(not(feature = "perf-telemetry"))]
                input::read_exact(source, size as usize, &mut scratch)?;
                #[cfg(feature = "perf-telemetry")]
                {
                    let read_started = perf_telemetry::start();
                    let read_result = input::read_exact(source, size as usize, &mut scratch);
                    perf_telemetry::record(
                        &plan.name,
                        Stage::FixedStreamReadCopy,
                        size as usize,
                        scratch.len(),
                        read_started.elapsed(),
                        read_result.is_err(),
                    );
                    read_result?;
                }

                #[cfg(not(feature = "perf-telemetry"))]
                {
                    materialise_from_source(py, plan, runtime, &scratch, Some(source))
                }
                #[cfg(feature = "perf-telemetry")]
                {
                    let materialise_started = perf_telemetry::start();
                    let result = materialise_from_source(py, plan, runtime, &scratch, Some(source));
                    perf_telemetry::record(
                        &plan.name,
                        Stage::FixedMaterialise,
                        size as usize,
                        size as usize,
                        materialise_started.elapsed(),
                        result.is_err(),
                    );
                    result
                }
            }
            None => {
                // A dynamic type cannot be sized before it is read.
                telemetry::record_fallback(&plan.name, FallbackReason::DynamicSize);
                #[cfg(not(feature = "perf-telemetry"))]
                {
                    read_dynamic_from_stream(py, plan, runtime, source)
                }
                #[cfg(feature = "perf-telemetry")]
                {
                    let total_started = perf_telemetry::start();
                    let result = read_dynamic_from_stream(py, plan, runtime, source);
                    perf_telemetry::record(
                        &plan.name,
                        Stage::DynamicStreamTotal,
                        0,
                        0,
                        total_started.elapsed(),
                        result.is_err(),
                    );
                    result
                }
            }
        },
    }
}

/// Default ceiling for one dynamically sized stream record, in bytes.
///
/// A dynamic record has to be retained while its layout is resolved and its
/// Python value is built. The default permits substantial on-disk tables while
/// still making an attacker-controlled length finite. Deployments that know
/// their input envelope can lower or raise it with
/// `CSTRUCT_RS2_MAX_DYNAMIC_RECORD_BYTES`; the hard maximum prevents an
/// environment setting from turning the guard into an unbounded allocation.
const DEFAULT_MAX_PROBE: usize = 64 << 20;

/// Highest accepted value for `CSTRUCT_RS2_MAX_DYNAMIC_RECORD_BYTES`.
const MAX_CONFIGURED_PROBE: usize = 1 << 30;

/// First probe size, in bytes. Comfortably covers the ordinary dynamic record.
const FIRST_PROBE: usize = 512;

/// Decode a content-sized record from a stream.
///
/// The size cannot be known before reading, because the length fields are inside
/// the record. So this reads a chunk, tries to resolve the layout, and grows if
/// the record turns out to be longer than what it read — rewinding each time so
/// the caller's cursor is never left mid-record.
///
/// On success the cursor is left exactly where the record ended, because that is
/// where the caller reads the next one from. On failure it is put back to where
/// the record started.
#[cfg(not(feature = "perf-telemetry"))]
fn read_dynamic_from_stream<'py>(
    py: Python<'py>,
    plan: &Arc<CompiledPlan>,
    runtime: &Arc<PlanRuntime>,
    stream: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let start = stream
        .call_method0(pyo3::intern!(py, "tell"))
        .and_then(|pos| pos.extract::<u64>())
        .map_err(|_| {
            pyo3::exceptions::PyNotImplementedError::new_err(
                "a dynamically sized type needs a seekable stream in this implementation",
            )
        })?;

    let reader = PyCustomReader::new(py, plan, runtime);
    let limit = dynamic_probe_limit()?;
    let mut probe = FIRST_PROBE;
    loop {
        stream.call_method1(pyo3::intern!(py, "seek"), (start,))?;
        let chunk = stream.call_method1(pyo3::intern!(py, "read"), (probe,))?;
        let bytes = chunk
            .cast::<pyo3::types::PyBytes>()
            .map_err(|_| internal("stream.read() did not return bytes"))?;
        let data = bytes.as_bytes();
        let exhausted = data.len() < probe;

        match decode::resolve_layout_with(plan, data, Some(&reader)) {
            // A layout may be computable from count fields before the probe
            // contains the full record. Treat `layout.consumed > data.len()` as
            // a request to grow inside this retry loop; materialising the short
            // probe would turn available trailing stream bytes into a false EOF.
            Ok(layout) => {
                let consumed = layout.consumed as usize;
                if consumed > limit {
                    rewind(py, stream, start)?;
                    return Err(probe_limit_error(limit));
                }
                if consumed > data.len() {
                    // The loop's next read seeks to `start`; only a terminal
                    // growth error needs an explicit rewind here.
                    if let Err(error) = grow_or_fail(&mut probe, exhausted, Some(consumed), limit) {
                        rewind(py, stream, start)?;
                        return Err(error);
                    }
                    continue;
                }
                let record =
                    materialise_resolved_layout(py, plan, runtime, data, &layout, Some(stream))?;
                stream.call_method1(
                    pyo3::intern!(py, "seek"),
                    (start + u64::from(layout.consumed),),
                )?;
                return Ok(record);
            }
            Err(Error::EndOfStream) => {
                // A successful retry seeks to `start` at the top of the loop.
                // Rewind here only when growth fails, so terminal errors keep
                // the cursor contract without paying for the same seek twice.
                if let Err(error) = grow_or_fail(&mut probe, exhausted, None, limit) {
                    rewind(py, stream, start)?;
                    return Err(error);
                }
            }
            Err(other) => {
                rewind(py, stream, start)?;
                return Err(reader.restore_py_error(other));
            }
        }
    }
}

/// Decode a content-sized record while collecting opt-in stage timings.
///
/// This stays separate from the ordinary implementation so a feature-disabled
/// build has no timer state or reporting branches in its optimized control
/// flow.
#[cfg(feature = "perf-telemetry")]
fn read_dynamic_from_stream<'py>(
    py: Python<'py>,
    plan: &Arc<CompiledPlan>,
    runtime: &Arc<PlanRuntime>,
    stream: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let start = stream
        .call_method0(pyo3::intern!(py, "tell"))
        .and_then(|pos| pos.extract::<u64>())
        .map_err(|_| {
            pyo3::exceptions::PyNotImplementedError::new_err(
                "a dynamically sized type needs a seekable stream in this implementation",
            )
        })?;

    let reader = PyCustomReader::new(py, plan, runtime);
    let limit = dynamic_probe_limit()?;
    let mut probe = FIRST_PROBE;
    loop {
        stream.call_method1(pyo3::intern!(py, "seek"), (start,))?;
        let read_started = perf_telemetry::start();
        let chunk_result = stream.call_method1(pyo3::intern!(py, "read"), (probe,));
        let read_failed = chunk_result.is_err();
        let chunk = chunk_result?;
        let bytes = chunk
            .cast::<pyo3::types::PyBytes>()
            .map_err(|_| internal("stream.read() did not return bytes"))?;
        let data = bytes.as_bytes();
        perf_telemetry::record(
            &plan.name,
            Stage::DynamicProbeRead,
            probe,
            data.len(),
            read_started.elapsed(),
            read_failed,
        );
        let exhausted = data.len() < probe;

        let layout_started = perf_telemetry::start();
        let layout_result = decode::resolve_layout_with(plan, data, Some(&reader));
        perf_telemetry::record(
            &plan.name,
            Stage::DynamicStreamLayout,
            data.len(),
            data.len(),
            layout_started.elapsed(),
            layout_result.is_err(),
        );

        match layout_result {
            // A layout may be computable from count fields before the probe
            // contains the full record. Treat `layout.consumed > data.len()` as
            // a request to grow inside this retry loop; materialising the short
            // probe would turn available trailing stream bytes into a false EOF.
            Ok(layout) => {
                let consumed = layout.consumed as usize;
                if consumed > limit {
                    rewind(py, stream, start)?;
                    return Err(probe_limit_error(limit));
                }
                if consumed > data.len() {
                    // The loop's next read seeks to `start`; only a terminal
                    // growth error needs an explicit rewind here.
                    if let Err(error) = grow_or_fail(&mut probe, exhausted, Some(consumed), limit) {
                        rewind(py, stream, start)?;
                        return Err(error);
                    }
                    continue;
                }
                let materialise_started = perf_telemetry::start();
                let record_result =
                    materialise_resolved_layout(py, plan, runtime, data, &layout, Some(stream));
                perf_telemetry::record(
                    &plan.name,
                    Stage::DynamicStreamMaterialise,
                    data.len(),
                    layout.consumed as usize,
                    materialise_started.elapsed(),
                    record_result.is_err(),
                );
                let record = record_result?;
                stream.call_method1(
                    pyo3::intern!(py, "seek"),
                    (start + u64::from(layout.consumed),),
                )?;
                return Ok(record);
            }
            Err(Error::EndOfStream) => {
                // A successful retry seeks to `start` at the top of the loop.
                // Rewind here only when growth fails, so terminal errors keep
                // the cursor contract without paying for the same seek twice.
                if let Err(error) = grow_or_fail(&mut probe, exhausted, None, limit) {
                    rewind(py, stream, start)?;
                    return Err(error);
                }
            }
            Err(other) => {
                rewind(py, stream, start)?;
                return Err(reader.restore_py_error(other));
            }
        }
    }
}

/// Put the cursor back where the record started.
fn rewind(py: Python<'_>, stream: &Bound<'_, PyAny>, start: u64) -> PyResult<()> {
    stream
        .call_method1(pyo3::intern!(py, "seek"), (start,))
        .map(|_| ())
}

/// Decide whether to read more, and by how much, or give up.
///
/// `exhausted` means the last read returned less than it asked for, so the
/// stream really has ended and there is nothing to grow into.
///
/// `needed` is the size the layout reported when it could be computed. Jumping
/// straight to it costs one extra read for a large record instead of several,
/// which is why it is preferred over multiplying blindly. The candidate is
/// checked before assignment so no oversized value can become a read request.
fn grow_or_fail(
    probe: &mut usize,
    exhausted: bool,
    needed: Option<usize>,
    limit: usize,
) -> PyResult<()> {
    if exhausted {
        return Err(pyo3::exceptions::PyEOFError::new_err(
            "unexpected end of stream",
        ));
    }
    let candidate = next_probe(*probe, needed).ok_or_else(|| probe_limit_error(limit))?;
    if candidate > limit {
        return Err(probe_limit_error(limit));
    }
    *probe = candidate;
    Ok(())
}

/// Calculate the next read size without overflowing machine-sized arithmetic.
///
/// The result is deliberately separate from assignment: callers validate the
/// candidate against their configured ceiling before it can become a stream request.
/// A resolved layout rounds up to a power of two to retain the one-retry fast
/// path; an unresolved layout uses the established fourfold growth sequence.
fn next_probe(probe: usize, needed: Option<usize>) -> Option<usize> {
    let candidate = match needed {
        Some(needed) => probe.max(needed).checked_next_power_of_two()?,
        None => probe.checked_mul(4)?,
    };
    // Clamping would return `limit` once the calculation crosses the ceiling.
    // The caller would then read a value that the resource policy was meant to
    // reject, and an unresolved record already at the ceiling would never make
    // progress. Preserve the oversized candidate so `grow_or_fail` can refuse
    // it before it becomes a read request.
    Some(candidate)
}

/// Construct the stable exception used for every dynamic-record size refusal.
fn probe_limit_error(limit: usize) -> PyErr {
    pyo3::exceptions::PyValueError::new_err(format!(
        "record exceeds the maximum size this implementation will probe ({limit} bytes)"
    ))
}

/// Resolve the per-process dynamic-record ceiling before probing begins.
fn dynamic_probe_limit() -> PyResult<usize> {
    let Some(raw) = std::env::var_os("CSTRUCT_RS2_MAX_DYNAMIC_RECORD_BYTES") else {
        return Ok(DEFAULT_MAX_PROBE);
    };
    let raw = raw.into_string().map_err(|_| {
        pyo3::exceptions::PyValueError::new_err(
            "CSTRUCT_RS2_MAX_DYNAMIC_RECORD_BYTES must contain decimal digits",
        )
    })?;
    let limit = raw.parse::<usize>().map_err(|_| {
        pyo3::exceptions::PyValueError::new_err(format!(
            "CSTRUCT_RS2_MAX_DYNAMIC_RECORD_BYTES must be an integer from {FIRST_PROBE} to {MAX_CONFIGURED_PROBE}",
        ))
    })?;
    if !(FIRST_PROBE..=MAX_CONFIGURED_PROBE).contains(&limit) {
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
            "CSTRUCT_RS2_MAX_DYNAMIC_RECORD_BYTES must be an integer from {FIRST_PROBE} to {MAX_CONFIGURED_PROBE}",
        )));
    }
    Ok(limit)
}

/// Build a record from a buffer, laying it out first if the type is dynamic.
pub fn materialise<'py>(
    py: Python<'py>,
    plan: &Arc<CompiledPlan>,
    runtime: &Arc<PlanRuntime>,
    buf: &[u8],
) -> PyResult<Bound<'py, PyAny>> {
    // A direct buffer is its own zero-based pointer address space. Build one
    // shared stream only when this plan or a nested plan actually contains a
    // pointer; ordinary records keep the allocation-free path.
    let pointer_stream = if contains_pointer(plan) {
        Some(make_stream(py, buf)?)
    } else {
        None
    };
    materialise_from_source(py, plan, runtime, buf, pointer_stream.as_ref())
}

/// Build a record while preserving the root address space used by pointers.
///
/// A nested record receives the same source as its parent. Pointer values are
/// absolute in that source; rebasing them onto the nested record slice would
/// make the result depend on temporary native buffering boundaries.
fn materialise_from_source<'py>(
    py: Python<'py>,
    plan: &Arc<CompiledPlan>,
    runtime: &Arc<PlanRuntime>,
    buf: &[u8],
    pointer_stream: Option<&Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    match plan.total_size {
        // Fixed: every offset is already on the plan, so no layout pass runs.
        // This is the whole reason a fixed structure is the fast path.
        Some(size) => {
            decode::check_fixed_size(plan, buf).map_err(to_py_err)?;
            let spans: Vec<FieldSpan> = plan.fields.iter().map(span_from_plan).collect();
            materialise_with_layout(py, plan, runtime, buf, &spans, size, pointer_stream)
        }
        None => {
            let reader = PyCustomReader::new(py, plan, runtime);
            #[cfg(feature = "perf-telemetry")]
            let layout_started = perf_telemetry::start();
            let layout_result = decode::resolve_layout_with(plan, buf, Some(&reader));
            #[cfg(feature = "perf-telemetry")]
            perf_telemetry::record(
                &plan.name,
                Stage::DynamicBufferLayout,
                buf.len(),
                buf.len(),
                layout_started.elapsed(),
                layout_result.is_err(),
            );
            let layout = reader.restore_py_result(layout_result)?;
            materialise_resolved_layout(py, plan, runtime, buf, &layout, pointer_stream)
        }
    }
}

/// Whether a plan tree needs a Python source object for pointer dereference.
fn contains_pointer(plan: &CompiledPlan) -> bool {
    plan.pointer_targets.iter().any(Option::is_some)
        || plan
            .subplans
            .iter()
            .any(|subplan| contains_pointer(subplan))
}

/// One field's span, taken straight from a fixed plan with no arithmetic.
///
/// The count is **not** clamped to a minimum of one. `uint32 block_list[0]` is a
/// real, empty array — the C flexible-array-member idiom, which
/// `dissect.squashfs` uses on every regular-file inode. Coercing its count to
/// one reads four bytes past the end of the record and raises EOF on every
/// `.snap` file.
fn span_from_plan(field: &cstruct_rs2_core::plan::FieldOp) -> FieldSpan {
    FieldSpan {
        offset: field.offset,
        size: field.size,
        count: if field.count == DYNAMIC_COUNT {
            1
        } else {
            field.count
        },
    }
}

/// Calls a consumer's `_read` to find out how many bytes it consumed.
///
/// The class is given a `BytesIO` over the remainder of the record and asked to
/// read itself; where it leaves the cursor is the field's length. That is how
/// upstream measures these too — there is no declared size to trust, which is
/// the entire reason `add_custom_type` exists.
struct PyCustomReader<'a, 'py> {
    py: Python<'py>,
    plan: &'a CompiledPlan,
    runtime: &'a PlanRuntime,
    callback_error: RefCell<Option<PyErr>>,
}

impl<'a, 'py> PyCustomReader<'a, 'py> {
    /// Pair the Python runtime tree with the core plan tree it mirrors.
    fn new(py: Python<'py>, plan: &'a CompiledPlan, runtime: &'a PlanRuntime) -> Self {
        Self {
            py,
            plan,
            runtime,
            callback_error: RefCell::new(None),
        }
    }

    /// Restore a Python callback exception after the Python-free layout pass.
    fn restore_py_error(&self, fallback: Error) -> PyErr {
        self.callback_error
            .borrow_mut()
            .take()
            .unwrap_or_else(|| to_py_err(fallback))
    }

    /// Convert a core result while preferring the consumer's original error.
    fn restore_py_result<T>(&self, result: Result<T, Error>) -> PyResult<T> {
        result.map_err(|error| self.restore_py_error(error))
    }

    /// Carry a callback failure through the Python-independent core error type.
    fn callback_failed(&self, error: PyErr) -> Error {
        if error.is_instance_of::<pyo3::exceptions::PyEOFError>(self.py) {
            return Error::EndOfStream;
        }
        *self.callback_error.borrow_mut() = Some(error);
        Error::InvalidValue {
            message: "a consumer-supplied reader raised an exception".to_owned(),
        }
    }
}

impl decode::CustomReader for PyCustomReader<'_, '_> {
    fn read(&self, name: &str, buf: &[u8], offset: usize) -> Result<CustomRead, Error> {
        let fail = |message: String| Error::InvalidValue { message };
        let class = find_custom_type(self.plan, self.runtime, name)
            .ok_or_else(|| fail("missing custom type".to_owned()))?;
        let tail = buf.get(offset..).ok_or(Error::EndOfStream)?;
        let stream = make_stream(self.py, tail).map_err(|err| fail(err.to_string()))?;
        let value = call_custom_read(self.py, class, &stream, self.plan.endian)
            .map_err(|err| self.callback_failed(err))?;
        let consumed: u32 = stream
            .call_method0(pyo3::intern!(self.py, "tell"))
            .and_then(|value| value.extract())
            .map_err(|err| fail(err.to_string()))?;
        // `extract` accepts Python integers and their subclasses. A custom
        // value that is not integer-like can still be materialised normally;
        // it simply cannot supply a later cstruct array length.
        Ok(CustomRead {
            consumed,
            integer: value.extract::<i128>().ok(),
        })
    }
}

/// Find a registered class by name in the runtime tree parallel to `plan`.
///
/// Custom indexes are local to each compiled plan. The registered name is the
/// stable identity across recursion, and `add_custom_type` assigns one class to
/// that name for the whole namespace. This walk runs only when Python is about
/// to execute a custom callback, so its small cost is dominated by that call.
fn find_custom_type<'a>(
    plan: &CompiledPlan,
    runtime: &'a PlanRuntime,
    name: &str,
) -> Option<&'a Py<PyAny>> {
    for (candidate, class) in plan.custom_names.iter().zip(&runtime.custom_types) {
        if candidate.as_ref() == name {
            return Some(class);
        }
    }
    for (subplan, subruntime) in plan.subplans.iter().zip(&runtime.subplans) {
        if let Some(class) = find_custom_type(subplan, subruntime, name) {
            return Some(class);
        }
    }
    None
}

/// A `BytesIO` over a slice, for handing to a consumer's `_read`.
fn make_stream<'py>(py: Python<'py>, data: &[u8]) -> PyResult<Bound<'py, PyAny>> {
    py.import(pyo3::intern!(py, "io"))?
        .getattr(pyo3::intern!(py, "BytesIO"))?
        .call1((pyo3::types::PyBytes::new(py, data),))
}

/// Invoke a custom reader with upstream's current keyword-only protocol.
///
/// This helper is reached only by `Op::Custom`; built-in fields never allocate
/// the keyword dictionary or cross into Python.
fn call_custom_read<'py>(
    py: Python<'py>,
    class: &Py<PyAny>,
    stream: &Bound<'py, PyAny>,
    endian: cstruct_rs2_core::schema::Endian,
) -> PyResult<Bound<'py, PyAny>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item(pyo3::intern!(py, "context"), py.None())?;
    kwargs.set_item(
        pyo3::intern!(py, "endian"),
        if matches!(endian, cstruct_rs2_core::schema::Endian::Little) {
            "<"
        } else {
            ">"
        },
    )?;
    class
        .bind(py)
        .call_method(pyo3::intern!(py, "_read"), (stream,), Some(&kwargs))
}

/// Build the Python values for a laid-out record.
///
/// Every field kind that needs the interpreter is handled here rather than in
/// [`crate::materialize`], because those kinds need the [`PlanRuntime`] and that
/// is what this function owns: a nested structure recurses, an array of
/// structures loops, and a consumer-supplied type reads itself.
pub fn materialise_with_layout<'py>(
    py: Python<'py>,
    plan: &Arc<CompiledPlan>,
    runtime: &Arc<PlanRuntime>,
    buf: &[u8],
    spans: &[FieldSpan],
    consumed: u32,
    pointer_stream: Option<&Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    materialise_fields::<false>(py, plan, runtime, buf, spans, consumed, &[], pointer_stream)
}

/// Dispatch a resolved layout without putting matrix checks in fixed records.
fn materialise_resolved_layout<'py>(
    py: Python<'py>,
    plan: &Arc<CompiledPlan>,
    runtime: &Arc<PlanRuntime>,
    buf: &[u8],
    layout: &decode::Layout,
    pointer_stream: Option<&Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    if layout.runtime_array_dims.is_empty() {
        return materialise_with_layout(
            py,
            plan,
            runtime,
            buf,
            &layout.spans,
            layout.consumed,
            pointer_stream,
        );
    }
    materialise_fields::<true>(
        py,
        plan,
        runtime,
        buf,
        &layout.spans,
        layout.consumed,
        &layout.runtime_array_dims,
        pointer_stream,
    )
}

/// Build record fields, compiling matrix support out of the ordinary path.
#[allow(clippy::too_many_arguments)]
fn materialise_fields<'py, const MATRICES: bool>(
    py: Python<'py>,
    plan: &Arc<CompiledPlan>,
    runtime: &Arc<PlanRuntime>,
    buf: &[u8],
    spans: &[FieldSpan],
    consumed: u32,
    runtime_array_dims: &[(u32, Box<[u32]>)],
    pointer_stream: Option<&Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let mut values = Vec::with_capacity(plan.fields.len());
    // Fixed records dominate practical parsing. Preserve the upstream-visible
    // empty map lazily rather than allocating a Python dictionary per record.
    let mut dynamic_sizes: Option<Bound<'py, PyDict>> = None;

    for (index, field) in plan.fields.iter().enumerate() {
        let span = *spans
            .get(index)
            .ok_or_else(|| internal("layout is short"))?;
        let matrix_dimensions = if MATRICES {
            runtime_array_dims
                .iter()
                .find(|(field_index, _)| *field_index as usize == index)
                .map(|(_, dimensions)| dimensions.as_ref())
        } else {
            None
        };
        let value = if let Some(dimensions) = matrix_dimensions {
            runtime_matrix_value(py, plan, runtime, field, span, dimensions, buf)?
        } else {
            match field.op {
                Op::Nested => nested_record(py, plan, runtime, field, span, buf, pointer_stream)?,
                Op::NestedArray => {
                    #[cfg(feature = "perf-telemetry")]
                    let nested_started = perf_telemetry::start();
                    let result = nested_array(py, plan, runtime, field, span, buf, pointer_stream);
                    #[cfg(feature = "perf-telemetry")]
                    perf_telemetry::record(
                        &plan.name,
                        Stage::NestedArrayMaterialise,
                        span.size as usize,
                        span.size as usize,
                        nested_started.elapsed(),
                        result.is_err(),
                    );
                    result?
                }
                // A consumer-supplied type reads itself, so it is materialised here
                // where the interpreter is rather than in `field_value`.
                Op::Custom => {
                    let class = runtime
                        .custom_types
                        .get(field.aux as usize)
                        .ok_or_else(|| internal("missing custom type"))?;
                    let slice = buf
                        .get(span.offset as usize..)
                        .ok_or_else(|| to_py_err(Error::EndOfStream))?;
                    let stream = make_stream(py, slice)?;
                    call_custom_read(py, class, &stream, plan.endian)?
                }
                _ => field_value(py, plan, runtime, field, index, span, buf)?,
            }
        };

        values.push(wrap_pointer(py, runtime, index, field, value, pointer_stream)?.unbind());
        if field.dynamic {
            if let Some(name) = declared_field_name(plan, index) {
                dynamic_sizes
                    .get_or_insert_with(|| PyDict::new(py))
                    .set_item(
                        name,
                        observed_dynamic_size(py, plan, runtime, index, field, span, buf)?,
                    )?;
            }
        }
    }

    let record = Record::new(
        Arc::clone(plan),
        Arc::clone(runtime),
        values,
        consumed,
        dynamic_sizes.map(Bound::unbind),
        true,
    );
    Ok(Bound::new(py, record)?.into_any())
}

/// Bytes upstream records for one dynamic field in `__dynamic_sizes__`.
///
/// A span describes the Python value, which excludes a null terminator. Upstream
/// measures the stream position before and after the field read, so its size map
/// includes that terminator. Scalar terminators have the compiled element
/// width. An array of dynamic structures needs the terminator record laid out to
/// discover its width; its bytes start immediately after the value span.
fn observed_dynamic_size(
    py: Python<'_>,
    plan: &CompiledPlan,
    runtime: &PlanRuntime,
    index: usize,
    field: &cstruct_rs2_core::plan::FieldOp,
    span: FieldSpan,
    buf: &[u8],
) -> PyResult<u32> {
    if !plan.is_null_terminated(index) {
        return Ok(span.size);
    }

    let terminator_size = if field.op == Op::NestedArray {
        let subplan = plan
            .subplans
            .get(field.aux as usize)
            .ok_or_else(|| internal("nested array refers to a missing sub-plan"))?;
        match subplan.total_size {
            Some(size) => size,
            None => {
                let terminator_offset = span
                    .offset
                    .checked_add(span.size)
                    .ok_or_else(|| internal("dynamic field span overflowed"))?;
                let terminator = buf
                    .get(terminator_offset as usize..)
                    .ok_or_else(|| internal("dynamic terminator starts past the input"))?;
                let subruntime = runtime
                    .subplans
                    .get(field.aux as usize)
                    .ok_or_else(|| internal("nested array refers to a missing sub-runtime"))?;
                let reader = PyCustomReader::new(py, subplan, subruntime);
                reader
                    .restore_py_result(decode::resolve_layout_with(
                        subplan,
                        terminator,
                        Some(&reader),
                    ))?
                    .consumed
            }
        }
    } else {
        u32::from(field.elem_size.max(1))
    };

    span.size
        .checked_add(terminator_size)
        .ok_or_else(|| internal("dynamic field size overflowed"))
}

/// Materialise one nested structure field from its slice of the parent.
///
/// The slice is bounded to exactly the sub-record's extent, so a sub-record
/// cannot read into the field that follows it.
fn nested_record<'py>(
    py: Python<'py>,
    plan: &Arc<CompiledPlan>,
    runtime: &Arc<PlanRuntime>,
    field: &cstruct_rs2_core::plan::FieldOp,
    span: FieldSpan,
    buf: &[u8],
    pointer_stream: Option<&Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let (sub, sub_runtime) = subplan(plan, runtime, field)?;
    let start = span.offset as usize;
    let end = start
        .checked_add(span.size as usize)
        .ok_or_else(|| to_py_err(Error::EndOfStream))?;
    let slice = buf
        .get(start..end)
        .ok_or_else(|| to_py_err(Error::EndOfStream))?;
    materialise_from_source(py, sub, sub_runtime, slice, pointer_stream)
}

/// Materialise an array of structures into a Python list.
///
/// Arrays of structures go through the general path rather than a specialised
/// one, and the miss is counted so their share is visible if it ever stops being
/// small.
///
/// There are two strides, and the second one is easy to miss. When the element
/// type has a fixed size the stride is arithmetic. When it does not — an array of
/// dynamically sized structures — each element's width is only known once it has
/// been laid out, so the elements are walked in order and the cursor advances by
/// what each one actually consumed. Upstream decodes both forms, so assuming one
/// stride for every element is a divergence rather than a shared limitation.
fn nested_array<'py>(
    py: Python<'py>,
    plan: &Arc<CompiledPlan>,
    runtime: &Arc<PlanRuntime>,
    field: &cstruct_rs2_core::plan::FieldOp,
    span: FieldSpan,
    buf: &[u8],
    pointer_stream: Option<&Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    telemetry::record_fallback(&plan.name, FallbackReason::UnspecialisedFieldKind);
    let (sub, sub_runtime) = subplan(plan, runtime, field)?;
    let list = pyo3::types::PyList::empty(py);

    match sub.total_size {
        Some(stride) if stride > 0 => {
            let stride = stride as usize;
            for element in 0..span.count as usize {
                let start = span.offset as usize + element * stride;
                let slice = buf
                    .get(start..start + stride)
                    .ok_or_else(|| to_py_err(Error::EndOfStream))?;
                list.append(materialise_from_source(
                    py,
                    sub,
                    sub_runtime,
                    slice,
                    pointer_stream,
                )?)?;
            }
        }
        _ => {
            let mut at = span.offset as usize;
            for _ in 0..span.count as usize {
                let slice = buf.get(at..).ok_or_else(|| to_py_err(Error::EndOfStream))?;
                let record = materialise_from_source(py, sub, sub_runtime, slice, pointer_stream)?;
                // The record knows its own extent, so reading it back is cheaper
                // than laying the element out again.
                let consumed = record
                    .cast::<Record>()
                    .map_err(|_| internal("a nested array element is not a record"))?
                    .borrow()
                    .consumed_bytes() as usize;
                // A zero-width element would make this loop place every record
                // at the same offset, forever.
                if consumed == 0 {
                    return Err(pyo3::exceptions::PyNotImplementedError::new_err(
                        "an array of zero-width structures has no stride",
                    ));
                }
                list.append(record)?;
                at += consumed;
            }
        }
    }
    Ok(list.into_any())
}

/// The sub-plan and sub-runtime a nested field refers to.
fn subplan<'a>(
    plan: &'a Arc<CompiledPlan>,
    runtime: &'a Arc<PlanRuntime>,
    field: &cstruct_rs2_core::plan::FieldOp,
) -> PyResult<(&'a Arc<CompiledPlan>, &'a Arc<PlanRuntime>)> {
    let sub = plan
        .subplans
        .get(field.aux as usize)
        .ok_or_else(|| internal("missing sub-plan"))?;
    let sub_runtime = runtime
        .subplans
        .get(field.aux as usize)
        .ok_or_else(|| internal("missing sub-runtime"))?;
    Ok((sub, sub_runtime))
}

/// Wrap a decoded pointer field in the `Pointer` subclass built for it.
///
/// A pointer field decodes to the same integer it always did; the wrapper adds
/// the target type and source address space, which are the two things
/// `dereference()` needs and an integer cannot carry. `Pointer` subclasses
/// `int`, so every consumer that treated the field as a number still works.
///
/// Returns `value` untouched for every field that is not a pointer, which is
/// nearly all of them — the overwhelming majority of types have no pointer field
/// and never build the stream at all.
///
/// `char *args[4]` is an *array of pointers*, not a pointer to an array: each
/// element is an address, so the elements are wrapped individually. Wrapping the
/// list instead hands `Pointer.__new__` a list to read an address out of.
fn wrap_pointer<'py>(
    py: Python<'py>,
    runtime: &Arc<PlanRuntime>,
    index: usize,
    field: &cstruct_rs2_core::plan::FieldOp,
    value: Bound<'py, PyAny>,
    pointer_stream: Option<&Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let Some(class) = runtime.pointer_classes.get(index).and_then(Option::as_ref) else {
        return Ok(value);
    };
    let class = class.bind(py);
    let stream = pointer_stream
        .ok_or_else(|| internal("a pointer field has no source address space"))?
        .clone();

    if matches!(field.op, Op::ScalarArray | Op::NestedArray) {
        let items = pyo3::types::PyList::empty(py);
        for item in value.try_iter()? {
            items.append(class.call1((item?, stream.clone()))?)?;
        }
        return Ok(items.into_any());
    }
    class.call1((value, stream))
}

#[cfg(test)]
mod tests {
    use super::{next_probe, DEFAULT_MAX_PROBE};

    #[test]
    fn next_probe_includes_the_exact_limit() {
        assert_eq!(
            next_probe(512, Some(DEFAULT_MAX_PROBE)),
            Some(DEFAULT_MAX_PROBE)
        );
    }

    #[test]
    fn next_probe_reports_resolved_size_overflow() {
        assert_eq!(next_probe(512, Some(usize::MAX)), None);
    }

    #[test]
    fn next_probe_reports_geometric_overflow() {
        assert_eq!(next_probe(usize::MAX / 2 + 1, None), None);
    }

    #[test]
    fn next_probe_exposes_a_candidate_past_the_configured_limit() {
        let limit = 1 << 20;

        assert_eq!(next_probe(limit / 2, None), Some(2 * limit));
        assert_eq!(next_probe(limit, None), Some(4 * limit));
    }
}
