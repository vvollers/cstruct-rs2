//! One place that turns a core error into a Python exception.
//!
//! Which class a failure raises is part of the interface: callers catch
//! specific ones, and `EOFError` in particular is ordinary control flow at the
//! end of a record run. Keeping the mapping in one function stops it drifting
//! between call sites.

use pyo3::exceptions::{PyEOFError, PyNotImplementedError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;

use cstruct_rs2_core::error::Error;

/// Translate a core error into the exception upstream would have raised.
///
/// `EndOfStream` is matched first and kept cheap: it is how a record loop ends,
/// so it happens often rather than exceptionally.
pub fn to_py_err(error: Error) -> PyErr {
    match error {
        Error::EndOfStream | Error::Leb128EndOfStream => PyEOFError::new_err(error.to_string()),
        Error::Unsupported { .. } => PyNotImplementedError::new_err(error.to_string()),
        Error::InvalidValue { .. } | Error::LimitExceeded { .. } => {
            PyValueError::new_err(error.to_string())
        }
        // Parse and resolve failures have no stdlib equivalent, so they are
        // raised as cstruct's own classes -- which is what upstream raises and
        // what its suite asserts with `pytest.raises(ParserError)`.
        //
        // Both derive from ValueError in `dissect.cstruct.exception`, so every
        // consumer that already catches ValueError keeps working. That
        // inheritance is what makes this a safe change rather than a breaking
        // one, and it is why the classes were given the mixin.
        Error::Parse { .. } => typed_err("ParserError", error.to_string()),
        Error::Lex { .. } => typed_err("LexerError", error.to_string()),
        Error::UnknownType { .. } => typed_err("ResolveError", error.to_string()),
    }
}

/// Raise one of cstruct's own exception classes by name.
///
/// The classes live in Python (`dissect.cstruct.exception`) because consumers
/// import and subclass them; looking them up here keeps a single definition
/// rather than a Rust copy that would be a *different* class with the same
/// name -- the failure this project has hit before, where an `isinstance` check
/// against the wrong one fails silently.
///
/// Falls back to `ValueError` if the lookup fails for any reason. An error
/// being reported is more important than which class reports it, and raising
/// from inside error translation would lose the original entirely.
pub fn typed_err(class_name: &str, message: String) -> PyErr {
    Python::attach(|py| {
        py.import("dissect.cstruct.exception")
            .and_then(|module| module.getattr(class_name))
            .and_then(|class| class.call1((message.clone(),)))
            .map_or_else(|_| PyValueError::new_err(message), PyErr::from_value)
    })
}

/// Report an internal invariant failure.
///
/// Used only where a code path should be unreachable given a well-formed plan.
/// It exists so such a case surfaces as an exception at the boundary rather
/// than as a panic — a panic must still unwind (never `panic = "abort"`), but
/// an exception is what a caller can diagnose.
pub fn internal(message: &str) -> PyErr {
    PyRuntimeError::new_err(format!("cstruct-rs2 internal error: {message}"))
}
