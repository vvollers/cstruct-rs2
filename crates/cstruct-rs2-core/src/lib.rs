//! Definition parser, schema, compiled plan and decoder for `dissect.cstruct`.
//!
//! This crate knows nothing about Python. Everything that talks to CPython lives
//! in `cstruct-rs2-py`, and everything that needs `unsafe` lives there or in
//! `cstruct-rs2-simd`. That boundary is what lets the parser and decoder be
//! tested and fuzzed on their own, with no interpreter in the loop.
//!
//! # The four stages
//!
//! A cstruct definition is a small C-like language:
//!
//! ```text
//! struct FILETIME {
//!     uint32 lo;
//!     uint32 hi;
//! };
//! ```
//!
//! Turning that text into decoded values happens in four stages, and each one
//! runs at a different frequency:
//!
//! ```text
//! definition text  ->  schema  ->  compiled plan  ->  decoded record
//!  once per load       once        once per type       once per record
//! ```
//!
//! 1. [`lexer`] splits the text into tokens.
//! 2. [`parser`] builds a [`schema`], which is a readable description of the
//!    declared types: names, nesting, array lengths as written.
//! 3. [`plan`] compiles one schema type into a [`plan::CompiledPlan`] — a flat
//!    list of field operations, with every offset and size that can be resolved
//!    ahead of time already resolved.
//! 4. [`decode`] walks a plan over a buffer.
//!
//! The split is what makes decoding cheap. By the time bytes arrive there are no
//! names to look up, no aliases to follow and, for a fixed-size type, no
//! arithmetic to do — the plan already holds each field's offset.
//!
//! # How this differs from upstream
//!
//! Upstream `dissect.cstruct` builds a Python class per declared type and reads
//! a record by walking that class's `__fields__` list, calling each field type's
//! `_read` in turn. It also ships an optional compiler that generates Python
//! source for a specialised `_read` and compiles it, which removes much of that
//! per-field overhead for simple structures.
//!
//! Here the equivalent of that per-field walk is [`plan::CompiledPlan`], built
//! once when a type is first used and then reused. The walk itself is Rust, so
//! there is no code generation and no per-field Python call either way.
//!
//! # Input is untrusted
//!
//! Every length, count, offset and nesting depth comes from the data being
//! decoded. Damaged or hostile input must produce an [`error::Error`] — never a
//! panic, an out-of-bounds read, or an unbounded allocation. The limits that
//! enforce this live next to the code they bound: [`decode::MAX_ARRAY_ELEMENTS`]
//! and [`plan::MAX_NESTING`].

#![doc(html_no_source)]

pub mod decode;
pub mod error;
pub mod expr;
pub mod lexer;
pub mod parser;
pub mod plan;
pub mod schema;
pub mod telemetry;

/// This crate's version, so a caller can report exactly which build answered.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Which instruction set this binary was compiled for.
///
/// This build targets x86-64-v3 or better and will not run on anything older, so
/// which baseline was used is worth being able to ask about — a wheel built for
/// a newer baseline than the machine it lands on fails with an illegal
/// instruction rather than a clear error.
pub fn target_cpu_baseline() -> &'static str {
    // Reported from compile-time configuration, not detected at run time: the
    // question is what this binary was *built* for, not what the current CPU
    // happens to support.
    if cfg!(target_feature = "avx512f") {
        "avx512f (native or better)"
    } else if cfg!(target_feature = "avx2") {
        "x86-64-v3 (avx2)"
    } else {
        "below x86-64-v3 - unsupported"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fails the build if it was configured below the supported baseline.
    ///
    /// Such a build would still work — just slower, and with nothing to say so.
    /// Catching it here means the mistake surfaces when the crate is built
    /// rather than as an unexplained slowdown later.
    #[test]
    // Constant on purpose: this is a build check wearing a test's clothes, and
    // it should fail the suite rather than warn.
    #[allow(clippy::assertions_on_constants)]
    fn built_at_or_above_the_platform_floor() {
        assert!(
            cfg!(target_feature = "avx2"),
            "built below the x86-64-v3 floor: {}",
            target_cpu_baseline()
        );
    }
}
