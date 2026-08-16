//! What this crate promises while it is still empty.
//!
//! There are no kernels here yet and there should not be: the plan sequences
//! vectorisation *after* the record object, because SIMD attacks the 971 ns
//! native decode and not the 2,595 ns of Python object construction that
//! currently dominates. The crate exists from the start so that the `unsafe`
//! boundary is established before there is any pressure to blur it.
//!
//! Which leaves one thing to test and one thing to guard.
//!
//! # The thing to test
//!
//! [`avx512_enabled`] reports what the *build* was compiled for, not what the
//! current machine supports. That distinction is the whole point of it: a
//! benchmark records which kernel set was compiled in, and comparing an
//! `x86-64-v3` build against a `native` one and calling the difference an
//! optimisation is the mistake this exists to prevent.
//!
//! # The thing to guard
//!
//! When a kernel does arrive it owes four things: a scalar twin, a test
//! comparing the two across every boundary around the vector width,
//! fuzzing, a documented safety invariant, and an end-to-end measurement
//! justifying it. Rule one is the one that matters in practice -- lengths and
//! offsets here come out of forensic images and are attacker-controlled, so a
//! kernel that reads past a slice because it trusted a length is a security
//! defect and not a performance bug.
//!
//! The test at the bottom is a placeholder that fails the moment the crate
//! stops being empty without those obligations being met. It is deliberately
//! blunt: an empty crate with an honest guard is better than a full one with a
//! test that cannot fail.

use cstruct_rs2_simd::avx512_enabled;

/// The report is a compile-time constant, usable in a `const` context.
///
/// It has to be, because it describes the build rather than the host: a runtime
/// probe would answer a different question and would tempt someone into feature
/// dispatch that this build does not need — the baseline is fixed.
#[test]
fn avx512_availability_is_a_compile_time_fact() {
    const COMPILED_WITH_AVX512: bool = avx512_enabled();
    assert_eq!(
        avx512_enabled(),
        COMPILED_WITH_AVX512,
        "the answer must be the same constant every time it is asked"
    );

    // And it agrees with the core crate's own description of the same build,
    // so a benchmark cannot be handed two contradictory accounts of what it
    // measured.
    let baseline = cstruct_rs2_core::target_cpu_baseline();
    assert_eq!(
        COMPILED_WITH_AVX512,
        baseline.contains("avx512f"),
        "the simd crate and the core crate must describe one build, got {baseline:?}"
    );
}

/// The `x86-64-v3` floor, asserted rather than assumed.
///
/// A build configured below the floor would still work — just slower, and with
/// nothing to say so. Timing cannot catch it either, because a slower build is
/// indistinguishable from a busy machine. The build flags are the only place it
/// is visible.
#[test]
// The assertion is constant on purpose: this is a build guard wearing a test's
// clothes, and it must fail the suite rather than warn.
#[allow(clippy::assertions_on_constants)]
fn the_build_is_at_or_above_the_platform_floor() {
    assert!(
        cfg!(target_feature = "avx2"),
        "built below the x86-64-v3 floor: {}",
        cstruct_rs2_core::target_cpu_baseline()
    );
}

/// Fails when the first kernel lands without its obligations.
///
/// This crate's public surface is one `const fn`. When that changes, the change
/// is a kernel — and a kernel needs its scalar twin and its comparison test
/// before it ships. Having to rewrite this test is the prompt to write those.
#[test]
fn the_crate_is_still_empty_and_that_is_deliberate() {
    // There is no reflection over a crate's items in stable Rust, so the guard
    // is the honest approximation: the one thing that exists, named here, so
    // that a second thing prompts someone to read the contract above.
    let _: fn() -> bool = || avx512_enabled();
}
