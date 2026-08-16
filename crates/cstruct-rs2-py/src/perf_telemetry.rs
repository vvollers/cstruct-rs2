//! Opt-in timings for native decode stages.
//!
//! A normal wheel does not compile this module. An instrumented build enables the
//! `perf-telemetry` feature and can then answer which native stage consumed time,
//! grouped by cstruct type and requested byte count. Values and input contents
//! are never retained.
//!
//! The clock stops before the counter lock is taken. Counter-management overhead
//! therefore cannot be mistaken for decoder cost, although the instrumented
//! process itself is intentionally unsuitable for headline wall-clock claims.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// A native operation whose cost is useful when analyzing decoder work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Stage {
    /// Reserve the per-call vector used by a fixed stream decode.
    FixedScratchAllocation,
    /// Ask a fixed stream for bytes and copy them into the native vector.
    FixedStreamReadCopy,
    /// Materialise a fixed stream record after its bytes are available.
    FixedMaterialise,
    /// Copy a non-`bytes` buffer exporter into native storage.
    BufferCopy,
    /// Read one geometrically sized probe for a dynamic stream record.
    DynamicProbeRead,
    /// Resolve a dynamic stream record's field spans from a probe.
    DynamicStreamLayout,
    /// Materialise a dynamic stream record from resolved spans.
    DynamicStreamMaterialise,
    /// Complete dynamic-stream decode, including seeks and retry control flow.
    DynamicStreamTotal,
    /// Resolve a dynamic buffer or nested record's field spans.
    DynamicBufferLayout,
    /// Materialise an array of nested records through the general path.
    NestedArrayMaterialise,
}

impl Stage {
    /// Stable spelling used by machine-readable reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FixedScratchAllocation => "fixed_scratch_allocation",
            Self::FixedStreamReadCopy => "fixed_stream_read_copy",
            Self::FixedMaterialise => "fixed_materialise",
            Self::BufferCopy => "buffer_copy",
            Self::DynamicProbeRead => "dynamic_probe_read",
            Self::DynamicStreamLayout => "dynamic_stream_layout",
            Self::DynamicStreamMaterialise => "dynamic_stream_materialise",
            Self::DynamicStreamTotal => "dynamic_stream_total",
            Self::DynamicBufferLayout => "dynamic_buffer_layout",
            Self::NestedArrayMaterialise => "nested_array_materialise",
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct Aggregate {
    calls: u64,
    observed_bytes: u64,
    elapsed_ns: u64,
    errors: u64,
}

/// One deterministic row returned to the Python census runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Compiled cstruct type owning the operation.
    pub type_name: String,
    /// Native stage that ran.
    pub stage: Stage,
    /// Requested size or zero when the stage has no byte request.
    pub requested_bytes: u64,
    /// Number of stage executions.
    pub calls: u64,
    /// Bytes actually returned or processed.
    pub observed_bytes: u64,
    /// Sum of stage durations, excluding counter updates.
    pub elapsed_ns: u64,
    /// Stage executions whose operation returned an error.
    pub errors: u64,
}

type Key = (String, Stage, u64);

fn counters() -> &'static Mutex<HashMap<Key, Aggregate>> {
    static COUNTERS: OnceLock<Mutex<HashMap<Key, Aggregate>>> = OnceLock::new();
    COUNTERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Start a stage timer.
#[inline]
pub fn start() -> Instant {
    Instant::now()
}

/// Record one completed stage after its timer has stopped.
pub fn record(
    type_name: &str,
    stage: Stage,
    requested_bytes: usize,
    observed_bytes: usize,
    elapsed: Duration,
    failed: bool,
) {
    let nanos = elapsed.as_nanos().min(u128::from(u64::MAX)) as u64;
    let mut guard = match counters().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let aggregate = guard
        .entry((type_name.to_owned(), stage, requested_bytes as u64))
        .or_default();
    aggregate.calls = aggregate.calls.saturating_add(1);
    aggregate.observed_bytes = aggregate
        .observed_bytes
        .saturating_add(observed_bytes as u64);
    aggregate.elapsed_ns = aggregate.elapsed_ns.saturating_add(nanos);
    aggregate.errors = aggregate.errors.saturating_add(u64::from(failed));
}

/// Return all rows in stable stage/type/size order.
pub fn report() -> Vec<Row> {
    let guard = match counters().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let mut rows: Vec<Row> = guard
        .iter()
        .map(|((type_name, stage, requested_bytes), aggregate)| Row {
            type_name: type_name.clone(),
            stage: *stage,
            requested_bytes: *requested_bytes,
            calls: aggregate.calls,
            observed_bytes: aggregate.observed_bytes,
            elapsed_ns: aggregate.elapsed_ns,
            errors: aggregate.errors,
        })
        .collect();
    rows.sort_by(|left, right| {
        left.stage
            .cmp(&right.stage)
            .then_with(|| left.type_name.cmp(&right.type_name))
            .then_with(|| left.requested_bytes.cmp(&right.requested_bytes))
    });
    rows
}

/// Clear every row before observing a new command.
pub fn reset() {
    let mut guard = match counters().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_accumulate_without_retaining_values() {
        reset();
        record(
            "Header",
            Stage::FixedStreamReadCopy,
            16,
            16,
            Duration::from_nanos(20),
            false,
        );
        record(
            "Header",
            Stage::FixedStreamReadCopy,
            16,
            8,
            Duration::from_nanos(30),
            true,
        );
        assert_eq!(
            report(),
            vec![Row {
                type_name: "Header".to_owned(),
                stage: Stage::FixedStreamReadCopy,
                requested_bytes: 16,
                calls: 2,
                observed_bytes: 24,
                elapsed_ns: 50,
                errors: 1,
            }]
        );
        reset();
        assert!(report().is_empty());
    }
}
