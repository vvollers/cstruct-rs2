//! Vectorised bulk kernels.
//!
//! # Empty on purpose
//!
//! There are no kernels here yet. The crate exists from the start so the
//! `unsafe` boundary has a home before there is any pressure to blur it: this
//! is the only crate in the project where `unsafe` is permitted, and the core
//! crate forbids it outright.
//!
//! # What every kernel here has to come with
//!
//! 1. **A scalar twin.** The obvious, readable implementation, kept
//!    permanently, not deleted once the fast path works.
//! 2. **A test comparing the two** across lengths, offsets, alignments and
//!    awkward content — in particular every boundary either side of the vector
//!    width, where a kernel's tail handling lives.
//! 3. **A written-down safety invariant** on each `unsafe` block: what must be
//!    true of the pointers and lengths for it to be sound.
//! 4. **A measurement showing it was worth it**, taken end to end rather than
//!    on the kernel alone. A kernel is always faster than a scalar loop in
//!    isolation; that says nothing about whether the program got faster.
//!
//! The first two matter most. Lengths and offsets in this project come from the
//! data being decoded, so a kernel that reads past the end of a slice because it
//! trusted a length is a memory-safety bug, not a performance one. A kernel that
//! cannot be given a convincing twin and test does not belong here however fast
//! it is.
//!
//! # Try the compiler first
//!
//! Write the operation in safe Rust over `chunks_exact` and look at what LLVM
//! emits before reaching for intrinsics. Hand-written SIMD that only reproduces
//! what the compiler already generated is a liability: the same speed, plus an
//! `unsafe` block and a test to maintain.

#![doc(html_no_source)]

/// Reports whether this build may use AVX-512 kernels.
///
/// Compile-time, not runtime. This build targets a fixed microarchitecture and
/// does no feature dispatch, so this reports what was compiled in rather than
/// offering something to branch on.
pub const fn avx512_enabled() -> bool {
    cfg!(target_feature = "avx512f")
}
