//! Reading bytes according to a compiled plan.
//!
//! # What lives here, and what does not
//!
//! This module does everything that does not need CPython: bounds checking,
//! byte-order handling, dynamic length resolution, and resource limits. It
//! never creates a Python object, which is what keeps it fuzzable and testable
//! on its own.
//!
//! The binding crate walks a plan's fields, calls the readers here, and turns
//! the results into Python objects. Creating those objects is the expensive part
//! of a decode and can only happen where the interpreter is. Everything else —
//! the arithmetic, the bounds checks, the length resolution — does not need one,
//! so it lives here.
//!
//! # The two halves
//!
//! * [`read`] turns a slice, an offset and a width into one number.
//! * [`layout`] decides what those offsets and widths are for a record whose size
//!   depends on its content.
//!
//! The dependency runs one way: layout calls readers, readers know nothing about
//! plans. Both are re-exported here, so `decode::read_uint` and
//! `decode::resolve_layout` name the same items they always did.
//!
//! # Fixed records cost nothing to lay out
//!
//! For a record whose size is known — the overwhelming majority, and the fast
//! path — every offset is already in the plan and [`resolve_layout`] is never
//! called. Only a record with a content-dependent length needs its layout
//! computed, and only then does an expression get evaluated.
//!
//! # Input is untrusted
//!
//! Lengths, counts and offsets are read out of the data being decoded, so they
//! may be damaged or deliberately hostile. Every one of them must produce an
//! [`Error`](crate::error::Error) rather than a panic, an out-of-bounds read, or
//! an allocation sized by whatever the input asked for.

pub mod layout;
pub mod read;

pub use layout::{check_fixed_size, resolve_layout, resolve_layout_with, FieldSpan, Layout};
pub use read::{
    float16_bits, read_bytes, read_float, read_int, read_int128, read_leb128, read_uint,
    read_uint128,
};

use crate::error::Result;

/// Largest number of elements one array may declare.
///
/// A length field is just bytes from the input: a 16-bit one can legitimately
/// ask for 65,535 elements, and a corrupted 32-bit one can ask for four billion.
/// Without a ceiling that second case becomes an allocation the process cannot
/// satisfy. 2^24 leaves room for the large tables real formats contain while
/// still failing fast on nonsense.
///
/// Upstream has no equivalent limit — it builds the list and lets Python run out
/// of memory.
pub const MAX_ARRAY_ELEMENTS: u32 = 1 << 24;

/// What the binding layer learns by reading a consumer-supplied type.
///
/// A custom reader always determines its own byte width. Some readers, such as
/// the protobuf varint used by `dissect.target`, also return an integer which a
/// following array length names. Keeping that value beside the width lets the
/// layout pass evaluate the same `char body[length]` definitions that upstream
/// evaluates after calling the custom reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CustomRead {
    /// Bytes consumed by the custom field.
    pub consumed: u32,
    /// Integer value, when the custom reader returned one.
    pub integer: Option<i128>,
}

/// How the binding layer reads a consumer-supplied type.
///
/// `add_custom_type` registers a Python class with its own `_read`, which this
/// crate cannot call: it holds no interpreter and deliberately does not link
/// PyO3. The layout pass needs to know how many bytes such a field consumed,
/// so it asks through this trait and the binding crate does the calling.
pub trait CustomReader {
    /// Read the named custom type at `offset` and report its observable result.
    ///
    /// The name is stable across nested plans. A plan-local integer index is
    /// not: every [`crate::plan::CompiledPlan`] has its own `custom_names`
    /// table, so forwarding a root reader into a sub-plan and reusing its index
    /// can call the wrong consumer type.
    fn read(&self, name: &str, buf: &[u8], offset: usize) -> Result<CustomRead>;
}
