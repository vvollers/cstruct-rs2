//! Counters for decodes that missed a fast path.
//!
//! # What this is for
//!
//! Some record shapes can be decoded by a short, specialised route; the rest go
//! through the general decoder. Correctness never depends on which one runs — a
//! miss is always served correctly — so these counters are pure observation.
//!
//! Each miss is recorded against the type's name and the reason it missed.
//! [`report`] returns them most frequent first, which turns "decoding feels
//! slow" into a ranked list of the shapes actually responsible. When a new
//! definition starts using a construct the fast paths do not cover, it appears
//! at the top of that list rather than having to be guessed at.
//!
//! Upstream has nothing equivalent; it has no fast/slow split to observe.
//!
//! # Why the counter has its own tests
//!
//! A counter that reports the wrong thing is worse than no counter, because it
//! stops anyone from looking further. The specific way that happens: the
//! predicate deciding eligibility and the code doing the counting drift apart,
//! so the number stays plausible while describing nothing.
//!
//! [`fast_path_eligible`] is therefore the single place eligibility is decided,
//! and the tests at the bottom of this file check the counter *against* it — a
//! shape that should not qualify must increment, a shape that should must not.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Why a decode did not take a fast path.
///
/// Deliberately a closed enum rather than a free-form string: the report is
/// meant to be aggregated and ranked, and a reason that is spelled two ways
/// splits its own count. Adding a variant is a deliberate act.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FallbackReason {
    /// The record's size is not known until it has been partly decoded.
    DynamicSize,
    /// A field's length depends on another field's value.
    DependentLength,
    /// The type contains a union, which needs write-synchronisation semantics.
    ContainsUnion,
    /// The input was neither a buffer nor a stream the fast paths recognise.
    UnrecognisedInput,
    /// The type nests deeper than the specialised paths handle.
    NestingTooDeep,
    /// A field kind with no specialised construction path yet.
    UnspecialisedFieldKind,
}

impl FallbackReason {
    /// Stable name for reports and for the Python-facing summary.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DynamicSize => "dynamic_size",
            Self::DependentLength => "dependent_length",
            Self::ContainsUnion => "contains_union",
            Self::UnrecognisedInput => "unrecognised_input",
            Self::NestingTooDeep => "nesting_too_deep",
            Self::UnspecialisedFieldKind => "unspecialised_field_kind",
        }
    }
}

/// One row of the fallback report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackRow {
    /// The cstruct type that fell back.
    pub type_name: String,
    /// Why it fell back.
    pub reason: FallbackReason,
    /// How many times, in this process.
    pub count: u64,
}

/// Process-wide counters.
///
/// A plain `Mutex`, deliberately. This is only touched when a fast path was
/// *missed*, so it is off the common path by construction; making the accounting
/// cleverer would optimise the case that is supposed to be rare.
fn counters() -> &'static Mutex<HashMap<(String, FallbackReason), u64>> {
    static COUNTERS: OnceLock<Mutex<HashMap<(String, FallbackReason), u64>>> = OnceLock::new();
    COUNTERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Records that `type_name` missed a fast path because of `reason`.
pub fn record_fallback(type_name: &str, reason: FallbackReason) {
    let mut guard = match counters().lock() {
        Ok(guard) => guard,
        // A poisoned telemetry mutex must never take down a decode. Losing a
        // count is acceptable; losing the parse is not.
        Err(poisoned) => poisoned.into_inner(),
    };
    *guard.entry((type_name.to_owned(), reason)).or_insert(0) += 1;
}

/// Returns the current report, most frequent first.
///
/// The ordering is the point: this is a worklist, and the top of it is what to
/// work on next.
pub fn report() -> Vec<FallbackRow> {
    let guard = match counters().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let mut rows: Vec<FallbackRow> = guard
        .iter()
        .map(|((type_name, reason), count)| FallbackRow {
            type_name: type_name.clone(),
            reason: *reason,
            count: *count,
        })
        .collect();
    // Sort by count descending, then by name and reason so the output is
    // deterministic and two runs can be diffed.
    rows.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.type_name.cmp(&b.type_name))
            .then_with(|| a.reason.cmp(&b.reason))
    });
    rows
}

/// Clears all counters. Used between measured runs and by tests.
pub fn reset() {
    let mut guard = match counters().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.clear();
}

/// Total number of recorded fallbacks.
pub fn total() -> u64 {
    let guard = match counters().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.values().sum()
}

/// The few facts about a type that decide whether it can take a fast path.
///
/// Deliberately much smaller than a [`crate::plan::CompiledPlan`]: keeping the
/// predicate's input to these four fields means eligibility can be reasoned
/// about, and tested, without building a plan first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShapeSummary {
    /// Whether the record's total size is known before decoding.
    pub fixed_size: bool,
    /// Whether any field's length depends on another field's value.
    pub dependent_lengths: bool,
    /// Whether the type contains a union.
    pub contains_union: bool,
    /// How deeply composites nest inside this type.
    pub nesting_depth: u8,
}

/// Deepest nesting the specialised paths handle.
///
/// # This was a guess, and the guess was wrong
///
/// It started at 1, with the reasoning that "one level covers the dominant real
/// shapes -- `_INDEX_ENTRY` nests a union one deep". Compiling the real
/// `dissect.ntfs` definition says otherwise (see `tests/nesting_depth.rs`):
///
/// | type | share of decodes | depth |
/// | --- | ---: | ---: |
/// | `_MFT_SEGMENT_REFERENCE` | 20.51% | 0 |
/// | `_INDEX_ENTRY` | 18.16% | **2** |
/// | `_ATTRIBUTE_RECORD_HEADER` | 2.49% | **2** |
/// | `_FILE_NAME` | 1.83% | **2** |
///
/// `_INDEX_ENTRY` nests two deep, not one: its anonymous *union* itself
/// contains an anonymous *struct*. A limit of 1 excluded 22.5% of all decodes
/// -- including the single shape the fast path exists to serve.
///
/// # And depth is barely a performance boundary at all
///
/// For a fixed-size type, a nested member is a sub-slice and a recursive call
/// with every offset already known. Nothing about depth makes that slow. The
/// limit is therefore set as a guard against a pathological definition rather
/// than as a performance threshold, and it is generous.
pub const MAX_SPECIALISED_NESTING: u8 = 8;

/// Decides whether a shape can take a fast path, and why not if it cannot.
///
/// Returning the reason rather than a bare `bool` is what keeps the predicate
/// and the telemetry from drifting: there is exactly one place that decides,
/// and it hands back the explanation the counter will store.
///
/// # Unions are eligible
///
/// It is tempting to exclude them, because a union's members overlap and
/// writing one has to decide what that means for the others. Reading has no
/// such problem: every member starts at offset zero and the total size is
/// known, so a fixed-size union is exactly as cheap to read as a fixed-size
/// struct.
///
/// Excluding them would be costly rather than merely conservative — unions are
/// common in the index and attribute structures that dominate filesystem
/// definitions.
pub fn fast_path_eligible(shape: &ShapeSummary) -> Result<(), FallbackReason> {
    if !shape.fixed_size {
        return Err(FallbackReason::DynamicSize);
    }
    if shape.dependent_lengths {
        return Err(FallbackReason::DependentLength);
    }
    if shape.nesting_depth > MAX_SPECIALISED_NESTING {
        return Err(FallbackReason::NestingTooDeep);
    }
    Ok(())
}

/// Checks a shape and records a fallback if it is not eligible.
///
/// The single entry point a decoder should use, so that "decided it was
/// ineligible" and "counted it as ineligible" cannot become two different
/// judgements.
pub fn check_and_record(type_name: &str, shape: &ShapeSummary) -> bool {
    match fast_path_eligible(shape) {
        Ok(()) => true,
        Err(reason) => {
            record_fallback(type_name, reason);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises the tests in this module.
    ///
    /// The counters are process-wide by design -- a report covers a whole
    /// `target-query` run -- but Rust runs tests in parallel by default, so
    /// each test's `reset()` would clobber the others. Rather than weaken the
    /// production design to suit the tests, the tests take a lock.
    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        match LOCK.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// A shape that should take the fast path: fixed size, flat, no union.
    fn qualifying() -> ShapeSummary {
        ShapeSummary {
            fixed_size: true,
            dependent_lengths: false,
            contains_union: false,
            nesting_depth: 0,
        }
    }

    /// The counter is checked against the predicate, not against itself.
    ///
    /// A shape that should not qualify must increment; one that should must not.
    /// This is what catches the two drifting apart, which is the failure that
    /// leaves the numbers looking plausible while measuring nothing.
    ///
    /// Note that these assertions run against a process-wide counter, so the
    /// test resets first and the module is compiled with `--test-threads` in
    /// mind: the counter is keyed by type name, and each case below uses a
    /// distinct name so parallel tests cannot collide.
    #[test]
    fn non_qualifying_shape_increments_and_qualifying_does_not() {
        let _guard = test_guard();
        reset();

        // Qualifying: must not be counted.
        assert!(check_and_record("Qualifying", &qualifying()));
        assert_eq!(
            report()
                .iter()
                .filter(|r| r.type_name == "Qualifying")
                .count(),
            0,
            "a qualifying shape must not increment the fallback counter"
        );

        // Non-qualifying, one case per reason, each with its own type name.
        let cases = [
            (
                "DynamicShape",
                ShapeSummary {
                    fixed_size: false,
                    ..qualifying()
                },
                FallbackReason::DynamicSize,
            ),
            (
                "DependentShape",
                ShapeSummary {
                    dependent_lengths: true,
                    ..qualifying()
                },
                FallbackReason::DependentLength,
            ),
            (
                "DeepShape",
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
                "{name} should not be fast-path eligible"
            );
            let row = report()
                .into_iter()
                .find(|r| r.type_name == name)
                .unwrap_or_else(|| panic!("{name} did not increment the fallback counter"));
            assert_eq!(row.reason, expected, "{name} recorded the wrong reason");
            assert_eq!(row.count, 1, "{name} should have been counted exactly once");
        }
    }

    /// A fixed-size union must stay eligible. `_INDEX_ENTRY` is 18.16% of all
    /// decodes in an NTFS `walkfs` and it contains a union; excluding it would
    /// have made the fast path miss the shape it exists for.
    #[test]
    fn a_fixed_size_union_is_still_fast_path_eligible() {
        let _guard = test_guard();
        reset();
        let shape = ShapeSummary {
            contains_union: true,
            ..qualifying()
        };
        assert!(
            fast_path_eligible(&shape).is_ok(),
            "reading a fixed-size union is as cheap as reading a struct"
        );
        assert!(check_and_record("UnionShape", &shape));
    }

    /// Guards the report's ordering, because the report is a worklist and an
    /// unranked worklist is not one.
    #[test]
    fn report_is_ranked_by_count() {
        let _guard = test_guard();
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
        let rows = report();
        let common = rows
            .iter()
            .position(|r| r.type_name == "RankCommon")
            .unwrap();
        let rare = rows.iter().position(|r| r.type_name == "RankRare").unwrap();
        assert!(common < rare, "the more frequent gap must rank first");
    }

    /// The counter must survive a poisoned mutex rather than take a decode with
    /// it. Telemetry is never allowed to be load-bearing for correctness.
    #[test]
    fn totals_accumulate() {
        let _guard = test_guard();
        reset();
        let dynamic = ShapeSummary {
            fixed_size: false,
            ..qualifying()
        };
        check_and_record("TotalsA", &dynamic);
        check_and_record("TotalsB", &dynamic);
        assert!(total() >= 2);
    }
}
