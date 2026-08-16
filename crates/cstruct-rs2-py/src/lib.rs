//! The Python extension module: everything that talks to CPython.
//!
//! `cstruct-rs2-core` does the parsing, planning and decoding without knowing
//! Python exists. This crate is the layer that turns the result into Python
//! objects and exposes the API upstream `dissect.cstruct` exposes, so that a
//! consumer can import this package instead of upstream and see no difference.
//!
//! The pieces, in the order a caller meets them:
//!
//! * [`registry`] — the `cstruct` namespace object, `load()`, and attribute
//!   access for the types and constants a definition declares.
//! * [`types`] — `StructType` and `Record`: a compiled structure type and one
//!   decoded instance of it.
//! * [`scalars`] — `ScalarType` and `ArrayType`, the primitives a namespace
//!   hands out as `cs.uint32` and `cs.uint32[4]`.
//! * [`materialize`] — turning a field's bytes into the Python value for it.
//! * [`input`] — accepting `bytes`, a buffer, or a stream.
//! * [`errors`] — mapping a core failure onto the exception upstream raises.
//!
//! # Two rules that shape the code here
//!
//! **Make input ownership explicit.** A direct `bytes` source is read in place.
//! Other contiguous buffer exporters are copied into native storage, and a
//! non-contiguous exporter is refused. A stream `read()` creates a Python
//! `bytes`; fixed-record decoding copies those bytes into per-call native
//! storage before materialization. These boundaries must remain visible when a
//! change claims to remove an allocation or copy.
//!
//! **Build the plan once.** Upstream reads a record by walking a Python class's
//! field list and calling each field type's `_read`. Here that walk happened
//! already, when the type was compiled, so decoding crosses into Rust once per
//! record rather than once per field.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

pub mod errors;
pub mod expression;
pub mod input;
pub mod materialize;
#[cfg(feature = "perf-telemetry")]
pub mod perf_telemetry;
pub mod registry;
pub mod scalars;
pub mod types;

use cstruct_rs2_core::telemetry;

/// Tokenise definition text for the small public Python lexer façade.
///
/// The returned tuples are deliberately plain values. Python turns them into
/// immutable `Token` objects; the native parser keeps using the same Rust token
/// representation without depending on Python classes.
#[pyfunction(name = "_tokenize")]
fn tokenize_definition(source: &str) -> PyResult<Vec<(String, String, u32, u32)>> {
    let mut result = cstruct_rs2_core::lexer::tokenize_public(source)
        .map_err(errors::to_py_err)?
        .into_iter()
        .map(|token| (token.kind, token.value, token.line, token.column))
        .collect::<Vec<_>>();

    let (line, column) = match source.rsplit_once('\n') {
        Some((prefix, tail)) => (
            prefix.matches('\n').count() as u32 + 2,
            tail.chars().count() as u32 + 1,
        ),
        None => (1, source.chars().count() as u32 + 1),
    };
    result.push(("EOF".to_owned(), String::new(), line, column));
    Ok(result)
}

/// Returns a dictionary describing exactly what this extension was built as.
///
/// Which CPU baseline, source tree, and CPython the extension was built against
/// decide what code the wheel contains and where it can run. Reporting them is
/// more reliable than inferring either property from a generic wheel filename.
#[pyfunction]
fn build_info(py: Python<'_>) -> PyResult<Py<PyDict>> {
    let info = PyDict::new(py);
    info.set_item("core_version", cstruct_rs2_core::VERSION)?;
    info.set_item("source_tree_sha256", env!("CSTRUCT_RS2_SOURCE_TREE_SHA256"))?;
    info.set_item(
        "target_cpu_baseline",
        cstruct_rs2_core::target_cpu_baseline(),
    )?;
    info.set_item("avx512_enabled", cstruct_rs2_simd::avx512_enabled())?;
    // Our own feature rather than PyO3's internal `Py_LIMITED_API` cfg, which
    // cargo does not know about and therefore warns on.
    info.set_item("abi3", cfg!(feature = "abi3"))?;
    info.set_item("debug_assertions", cfg!(debug_assertions))?;
    Ok(info.unbind())
}

/// Returns the fallback report as a list of dicts, most frequent first.
///
/// Read this after decoding to see which types are missing the fast paths and
/// why. The order is the useful part: whatever is at the top is responsible for
/// the most misses.
#[pyfunction]
fn fallback_report(py: Python<'_>) -> PyResult<Py<PyList>> {
    let rows = telemetry::report();
    let out = PyList::empty(py);
    for row in rows {
        let entry = PyDict::new(py);
        entry.set_item("type_name", row.type_name)?;
        entry.set_item("reason", row.reason.as_str())?;
        entry.set_item("count", row.count)?;
        out.append(entry)?;
    }
    Ok(out.unbind())
}

/// Total fallbacks recorded in this process.
#[pyfunction]
fn fallback_total() -> u64 {
    telemetry::total()
}

/// Clears the fallback counters.
///
/// Counters are process-wide, so reset between runs when a report should
/// describe one piece of work rather than everything since import.
#[pyfunction]
fn fallback_reset() {
    telemetry::reset();
}

/// Return native decode-stage timings from an opt-in measurement build.
///
/// Ordinary wheels do not export this function because their build omits the
/// `perf-telemetry` feature and every associated clock read and counter update.
#[cfg(feature = "perf-telemetry")]
#[pyfunction]
fn perf_telemetry_report(py: Python<'_>) -> PyResult<Py<PyList>> {
    let out = PyList::empty(py);
    for row in perf_telemetry::report() {
        let entry = PyDict::new(py);
        entry.set_item("type_name", row.type_name)?;
        entry.set_item("stage", row.stage.as_str())?;
        entry.set_item("requested_bytes", row.requested_bytes)?;
        entry.set_item("calls", row.calls)?;
        entry.set_item("observed_bytes", row.observed_bytes)?;
        entry.set_item("elapsed_ns", row.elapsed_ns)?;
        entry.set_item("errors", row.errors)?;
        out.append(entry)?;
    }
    Ok(out.unbind())
}

/// Clear opt-in native decode-stage timings before the next observed command.
#[cfg(feature = "perf-telemetry")]
#[pyfunction]
fn perf_telemetry_reset() {
    perf_telemetry::reset();
}

#[pymodule]
fn _cstruct_rs2(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(build_info, m)?)?;
    m.add_function(wrap_pyfunction!(fallback_report, m)?)?;
    m.add_function(wrap_pyfunction!(fallback_total, m)?)?;
    m.add_function(wrap_pyfunction!(fallback_reset, m)?)?;
    m.add_function(wrap_pyfunction!(tokenize_definition, m)?)?;
    #[cfg(feature = "perf-telemetry")]
    {
        m.add_function(wrap_pyfunction!(perf_telemetry_report, m)?)?;
        m.add_function(wrap_pyfunction!(perf_telemetry_reset, m)?)?;
    }
    m.add_function(wrap_pyfunction!(types::record_as_dict, m)?)?;
    m.add_function(wrap_pyfunction!(types::record_dump_fields, m)?)?;
    m.add_function(wrap_pyfunction!(types::input_is_bytesio, m)?)?;
    m.add_function(wrap_pyfunction!(types::record_type, m)?)?;
    types::register(m)?;
    registry::register(m)?;
    scalars::register(m)?;
    // `Expression` is exposed because upstream exposes it publicly and code
    // evaluates length expressions with it directly, not only through a
    // namespace.
    m.add_class::<expression::Expression>()?;
    Ok(())
}
