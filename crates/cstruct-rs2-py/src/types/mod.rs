//! Compiled structure types and the records they decode.
//!
//! This is the per-record half of the extension. `cs.MY_STRUCT` is a
//! [`StructType`] wrapped in a generated Python class; calling it produces a
//! [`Record`]. Everything else here supports one of those two.
//!
//! # How a decode is dispatched
//!
//! Upstream reaches a field's bytes through several Python calls per record: the
//! class's `__call__`, then a read helper, then each field type's `_read`. Here
//! the equivalent walk happened once already, when the type was compiled, so the
//! work per record is one call into Rust plus one Python object per field.
//!
//! # The modules, in the order a decode meets them
//!
//! ```text
//! struct_type   cs.X(source)      the entry point, and everything a type
//!                                 reports about itself
//! decoding      source -> Record  choose how to get the bytes, lay the record
//!                                 out if it is dynamic, assemble the values
//! record        record.field      the decoded record and its Python protocol
//! field         cs.X.fields       what a type says about its own shape
//! encode        record.dumps()    the write direction
//! runtime       (internal)        the Python classes a plan needs but,
//!                                 being Python-free, cannot hold
//! ```
//!
//! Two further pieces sit outside this module and are easy to look for in the
//! wrong place: the per-field bytes-to-value step is [`crate::materialize`], and
//! the layout arithmetic that decides where each field starts is
//! [`cstruct_rs2_core::decode`].
//!
//! Everything is re-exported here, so `crate::types::Record` and
//! `crate::types::StructType` name the same items they always did regardless of
//! which file they now live in.

use pyo3::prelude::*;

pub mod decoding;
pub mod encode;
pub mod field;
pub mod record;
pub mod runtime;
pub mod struct_type;

pub use field::Field;
pub use record::{record_as_dict, record_dump_fields, record_type, Record};
pub use runtime::PlanRuntime;
pub use struct_type::{struct_new, StructType};

/// Whether `obj` is a `BytesIO`, exposed for the fast-path coverage test.
///
/// Behaviour cannot show which input path was taken — every path produces the
/// same record — so the test asks directly.
#[pyfunction]
pub fn input_is_bytesio(obj: &Bound<'_, PyAny>) -> bool {
    crate::input::is_bytesio(obj)
}

/// Expose the record and type classes on the module.
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<Record>()?;
    module.add_class::<Field>()?;
    module.add_class::<StructType>()?;
    module.add_function(wrap_pyfunction!(struct_new, module)?)?;
    Ok(())
}
