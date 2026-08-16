//! The fallback counter, tested because a telemetry counter that lies is worse
//! than none at all.
//!
//! The failure mode is specific: the predicate that decides whether a shape
//! qualifies and the code that counts the misses drift apart, so the number
//! stays plausible while describing nothing — and nobody investigates a
//! plausible number.
//!
//! So the counter is checked *against the predicate*: a non-qualifying shape
//! must increment it, a qualifying one must not, and both must consult the same
//! decision.
//!
//! # These tests share process-wide state
//!
//! The counters are global by design: a report covers a whole `target-query`
//! run, not one call. Rust runs tests in parallel threads within a binary, so
//! each test's `reset()` would clobber the others. Rather than weaken the
//! production design to suit the tests, the tests take a lock -- and every case
//! uses a distinct type name so that even without one they could not collide.

use std::sync::{Mutex, MutexGuard};

use cstruct_rs2_core::telemetry::{
    check_and_record, fast_path_eligible, record_fallback, report, reset, total, FallbackReason,
    FallbackRow, ShapeSummary, MAX_SPECIALISED_NESTING,
};

/// Serialises the tests in this file against the process-wide counters.
fn counter_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    match LOCK.lock() {
        Ok(guard) => guard,
        // A poisoned lock here means another test panicked; carrying on gives
        // that test's failure rather than burying it under a second one.
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// A shape that should take the fast path: fixed size, flat, no dependencies.
fn qualifying() -> ShapeSummary {
    ShapeSummary {
        fixed_size: true,
        dependent_lengths: false,
        contains_union: false,
        nesting_depth: 0,
    }
}

/// Every reason has a stable name, because the report is aggregated and ranked
/// and a reason spelled two ways splits its own count.
#[test]
fn every_reason_has_a_stable_name() {
    let names = [
        (FallbackReason::DynamicSize, "dynamic_size"),
        (FallbackReason::DependentLength, "dependent_length"),
        (FallbackReason::ContainsUnion, "contains_union"),
        (FallbackReason::UnrecognisedInput, "unrecognised_input"),
        (FallbackReason::NestingTooDeep, "nesting_too_deep"),
        (
            FallbackReason::UnspecialisedFieldKind,
            "unspecialised_field_kind",
        ),
    ];
    for (reason, name) in names {
        assert_eq!(reason.as_str(), name, "{reason:?}");
    }

    // Distinct, so two reasons cannot merge into one row.
    let mut seen: Vec<&str> = names.iter().map(|(_, name)| *name).collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), names.len());
}

/// The predicate discriminates, and it hands back the reason the counter will
/// store -- which is what keeps "decided it was ineligible" and "counted it as
/// ineligible" from becoming two different judgements.
#[test]
fn the_predicate_names_the_reason_it_refuses() {
    assert_eq!(fast_path_eligible(&qualifying()), Ok(()));

    assert_eq!(
        fast_path_eligible(&ShapeSummary {
            fixed_size: false,
            ..qualifying()
        }),
        Err(FallbackReason::DynamicSize)
    );
    assert_eq!(
        fast_path_eligible(&ShapeSummary {
            dependent_lengths: true,
            ..qualifying()
        }),
        Err(FallbackReason::DependentLength)
    );
    assert_eq!(
        fast_path_eligible(&ShapeSummary {
            nesting_depth: MAX_SPECIALISED_NESTING + 1,
            ..qualifying()
        }),
        Err(FallbackReason::NestingTooDeep)
    );

    // The limit is inclusive: exactly at it is still eligible.
    assert_eq!(
        fast_path_eligible(&ShapeSummary {
            nesting_depth: MAX_SPECIALISED_NESTING,
            ..qualifying()
        }),
        Ok(())
    );
}

/// A fixed-size union stays eligible, and that is a correction rather than an
/// oversight.
///
/// An earlier version disqualified any type containing a union, on the grounds
/// that unions need write-synchronisation semantics. That is true for
/// *writing*, and this project does not write. Reading a fixed-size union is
/// exactly as cheap as reading a fixed-size struct -- and keeping the exclusion
/// disqualified `_INDEX_ENTRY`, which is 18.16% of all decodes in an NTFS
/// `walkfs` and the single shape the fast path exists to serve.
#[test]
fn a_fixed_size_union_is_still_eligible() {
    assert_eq!(
        fast_path_eligible(&ShapeSummary {
            contains_union: true,
            ..qualifying()
        }),
        Ok(())
    );
}

/// A non-qualifying shape must increment the counter, and a qualifying one must
/// not. This is what fails if the predicate and the counter drift apart.
#[test]
fn the_counter_records_exactly_what_the_predicate_refuses() {
    let _guard = counter_lock();
    reset();

    assert!(check_and_record("CounterQualifying", &qualifying()));
    assert!(
        report()
            .iter()
            .all(|row| row.type_name != "CounterQualifying"),
        "a qualifying shape must not increment the fallback counter"
    );

    let cases = [
        (
            "CounterDynamic",
            ShapeSummary {
                fixed_size: false,
                ..qualifying()
            },
            FallbackReason::DynamicSize,
        ),
        (
            "CounterDependent",
            ShapeSummary {
                dependent_lengths: true,
                ..qualifying()
            },
            FallbackReason::DependentLength,
        ),
        (
            "CounterDeep",
            ShapeSummary {
                nesting_depth: MAX_SPECIALISED_NESTING + 1,
                ..qualifying()
            },
            FallbackReason::NestingTooDeep,
        ),
    ];

    for (name, shape, expected) in cases {
        assert!(
            !check_and_record(name, &shape),
            "{name} should not be eligible"
        );
        let row = report()
            .into_iter()
            .find(|row| row.type_name == name)
            .unwrap_or_else(|| panic!("{name} did not increment the counter"));
        assert_eq!(row.reason, expected, "{name} recorded the wrong reason");
        assert_eq!(row.count, 1, "{name} should be counted exactly once");
    }
}

/// Repeated misses accumulate on one row rather than producing several.
#[test]
fn repeated_misses_accumulate_on_one_row() {
    let _guard = counter_lock();
    reset();

    for _ in 0..5 {
        record_fallback("AccumulateMe", FallbackReason::DynamicSize);
    }
    // A second reason for the same type is its own row: the pair is the key,
    // because "this type is slow" and "this type is slow *for this reason*" are
    // different questions and only the second is a worklist item.
    record_fallback("AccumulateMe", FallbackReason::DependentLength);

    let rows: Vec<FallbackRow> = report()
        .into_iter()
        .filter(|row| row.type_name == "AccumulateMe")
        .collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter()
            .find(|r| r.reason == FallbackReason::DynamicSize)
            .unwrap()
            .count,
        5
    );
    assert_eq!(total(), 6);
}

/// The report is ranked by count, because it *is* the next iteration's
/// worklist and an unranked worklist is not one.
#[test]
fn the_report_is_ranked_most_frequent_first() {
    let _guard = counter_lock();
    reset();

    let dynamic = ShapeSummary {
        fixed_size: false,
        ..qualifying()
    };
    for _ in 0..3 {
        check_and_record("RankRare", &dynamic);
    }
    for _ in 0..10 {
        check_and_record("RankCommon", &dynamic);
    }
    for _ in 0..7 {
        check_and_record("RankMiddling", &dynamic);
    }

    let order: Vec<String> = report().into_iter().map(|row| row.type_name).collect();
    assert_eq!(order, ["RankCommon", "RankMiddling", "RankRare"]);
}

/// Ties break by name and then by reason, so two identical runs produce
/// identical reports and can be compared directly.
#[test]
fn ties_break_deterministically() {
    let _guard = counter_lock();
    reset();

    record_fallback("TieB", FallbackReason::DynamicSize);
    record_fallback("TieA", FallbackReason::DynamicSize);
    record_fallback("TieA", FallbackReason::ContainsUnion);

    let rows = report();
    let names: Vec<&str> = rows.iter().map(|row| row.type_name.as_str()).collect();
    assert_eq!(names, ["TieA", "TieA", "TieB"], "equal counts sort by name");
    // Within one name, by reason -- using the enum's own declaration order, so
    // it is stable rather than dependent on hashing.
    let first_two: Vec<FallbackReason> = rows[..2].iter().map(|row| row.reason).collect();
    let mut sorted = first_two.clone();
    sorted.sort();
    assert_eq!(first_two, sorted);
}

/// Reset clears everything, so a report can describe one piece of work rather
/// than everything since import.
#[test]
fn reset_clears_the_counters() {
    let _guard = counter_lock();
    reset();
    assert_eq!(total(), 0);
    assert!(report().is_empty());

    record_fallback("ResetMe", FallbackReason::UnrecognisedInput);
    assert_eq!(total(), 1);

    reset();
    assert_eq!(total(), 0);
    assert!(report().is_empty());
}

/// Counting is off the hot path by construction: it happens only when a fast
/// path was *missed*.
///
/// Deliberately not a timing assertion — those are flaky and teach people to
/// ignore failures. This asserts the structural property that makes timing
/// irrelevant: an eligible shape touches no counter at all, however many times
/// it is checked.
#[test]
fn an_eligible_shape_never_touches_the_counter() {
    let _guard = counter_lock();
    reset();
    for _ in 0..1_000 {
        assert!(check_and_record("NeverCounted", &qualifying()));
    }
    assert_eq!(total(), 0);
}
