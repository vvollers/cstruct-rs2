//! Getting bytes out of whatever the caller passed.
//!
//! A decode can be handed three kinds of thing, and upstream accepts all three:
//!
//! * a `bytes` object, whose buffer can be read in place;
//! * any other object supporting the buffer protocol — `bytearray`,
//!   `memoryview`, `mmap`, and slices of them;
//! * a file-like object with a `read` method.
//!
//! [`classify`] decides which, and the rest of the module reads it.
//!
//! # Streams are the common case
//!
//! Most decoding works through a stream rather than a `bytes` object, because
//! records are read one after another out of a file or a mapped region. The
//! stream path is therefore the one worth keeping short, and [`classify`] tests
//! for it first.
//!
//! # Reads are exact, and the cursor is observable
//!
//! [`read_exact`] asks for exactly the number of bytes the plan says the record
//! needs. Reading more would be fewer system calls and is tempting, but the
//! caller can see the difference: after decoding a record, the stream position
//! must sit at the start of the next one, because that is how the caller reads
//! the next one. The same applies when a read fails — an over-read that then
//! raises leaves the cursor somewhere the caller did not expect.

use pyo3::buffer::PyBuffer;
use pyo3::exceptions::PyEOFError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyType};

/// What kind of thing a decode was handed.
///
/// A discriminant rather than a borrowed enum on purpose: carrying the borrowed
/// slice in the enum ties the caller's lifetimes together for no benefit, and
/// the caller already has the object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    /// A `bytes`-like object. Exact `bytes` can be borrowed; other contiguous
    /// exporters are copied. There is no cursor to move.
    Buffer,
    /// A file-like object that must be asked for its bytes.
    Stream,
}

/// Recognise what the caller passed.
///
/// Streams are tested for first because they are the common case; checking for
/// `bytes` first would make every stream decode pay for that check.
pub fn classify(obj: &Bound<'_, PyAny>) -> PyResult<InputKind> {
    if obj.hasattr(pyo3::intern!(obj.py(), "read"))? {
        return Ok(InputKind::Stream);
    }
    // Any buffer, not just `bytes`: current `dissect.ntfs` passes
    // `memoryview(self.buf)[-8:]` to a scalar type while reading an index.
    if obj.cast::<PyBytes>().is_ok() || PyBuffer::<u8>::get(obj).is_ok() {
        return Ok(InputKind::Buffer);
    }
    Err(pyo3::exceptions::PyTypeError::new_err(
        "expected a bytes-like object or a stream with a read() method",
    ))
}

/// Fill `scratch` with a buffer object's bytes.
///
/// `bytes` is copied in one shot; anything else goes through the buffer
/// protocol, which also handles the non-contiguous case by refusing it rather
/// than reading the wrong bytes.
pub fn copy_buffer(obj: &Bound<'_, PyAny>, scratch: &mut Vec<u8>) -> PyResult<()> {
    scratch.clear();
    if let Ok(bytes) = obj.cast::<PyBytes>() {
        scratch.extend_from_slice(bytes.as_bytes());
        return Ok(());
    }
    let buffer = PyBuffer::<u8>::get(obj)?;
    let slice = buffer
        .as_slice(obj.py())
        .ok_or_else(|| pyo3::exceptions::PyBufferError::new_err("buffer is not contiguous"))?;
    scratch.reserve(slice.len());
    scratch.extend(slice.iter().map(|cell| cell.get()));
    Ok(())
}

/// Copy at most `limit` bytes from a buffer object.
///
/// Null-terminated arrays need to inspect a bounded prefix before they know
/// their length. Copying the entire exporter first would make the limit
/// ineffective for a large `bytes`, `memoryview`, or `mmap`. The return value
/// says whether bytes remain beyond the copied prefix.
pub fn copy_buffer_prefix(
    obj: &Bound<'_, PyAny>,
    limit: usize,
    scratch: &mut Vec<u8>,
) -> PyResult<bool> {
    scratch.clear();
    if let Ok(bytes) = obj.cast::<PyBytes>() {
        let data = bytes.as_bytes();
        let copied = data.len().min(limit);
        scratch.extend_from_slice(&data[..copied]);
        return Ok(data.len() > copied);
    }
    let buffer = PyBuffer::<u8>::get(obj)?;
    let slice = buffer
        .as_slice(obj.py())
        .ok_or_else(|| pyo3::exceptions::PyBufferError::new_err("buffer is not contiguous"))?;
    let copied = slice.len().min(limit);
    scratch.reserve(copied);
    scratch.extend(slice.iter().take(copied).map(|cell| cell.get()));
    Ok(slice.len() > copied)
}

/// Read exactly `size` bytes from a stream into `scratch`.
///
/// One Python `read()` produces a new `bytes` object, whose contents are copied
/// into the caller's native storage. Fixed-record decoding creates that storage
/// for each top-level call; keeping the copy explicit avoids borrowing Python
/// memory across interpreter re-entry.
///
/// The read is for the **exact** size the plan computed — see the module docs on
/// why the cursor position matters.
pub fn read_exact(stream: &Bound<'_, PyAny>, size: usize, scratch: &mut Vec<u8>) -> PyResult<()> {
    let chunk = stream.call_method1(pyo3::intern!(stream.py(), "read"), (size,))?;
    let bytes = chunk.cast::<PyBytes>().map_err(|_| {
        pyo3::exceptions::PyTypeError::new_err("stream.read() did not return bytes")
    })?;
    let data = bytes.as_bytes();
    if data.len() < size {
        // Short read means the stream ended. `dissect` uses this as control
        // flow, so it must be `EOFError` and it must be cheap.
        return Err(PyEOFError::new_err("unexpected end of stream"));
    }
    scratch.clear();
    scratch.extend_from_slice(data);
    Ok(())
}

/// Whether `obj` is exactly `io.BytesIO`.
///
/// Used only for reporting, never to change what a decode produces: a `BytesIO`
/// and any other file-like object must decode identically. Exposed so a test can
/// assert that the specialised path was *taken*, which behaviour alone cannot
/// show.
pub fn is_bytesio(obj: &Bound<'_, PyAny>) -> bool {
    let py = obj.py();
    static NAME: &str = "BytesIO";
    match py.import("io").and_then(|io| io.getattr(NAME)) {
        Ok(cls) => match cls.cast::<PyType>() {
            Ok(cls) => obj.is_instance(cls).unwrap_or(false),
            Err(_) => false,
        },
        Err(_) => false,
    }
}
