//! How deeply do real definitions actually nest?
//!
//! The nesting limit exists to stop a self-referential definition recursing
//! until the stack runs out. It is only useful if it sits well above what real
//! definitions need, so this checks the depth of a real one rather than trusting
//! an estimate.

use cstruct_rs2_core::parser::parse;
use cstruct_rs2_core::plan::{Compiler, MAX_NESTING};
use cstruct_rs2_core::schema::Endian;

const NTFS: &str = include_str!("fixtures/ntfs.txt");

/// Types from the NTFS definition that nest, including the deepest.
///
/// `_INDEX_ENTRY` is the interesting one: it contains an anonymous union which
/// itself contains a struct, so it is two levels deep where most types are flat.
const DOMINANT: &[(&str, &str)] = &[
    ("_MFT_SEGMENT_REFERENCE", "flat"),
    ("_INDEX_ENTRY", "union inside a struct"),
    ("_ATTRIBUTE_RECORD_HEADER", "union of two member layouts"),
    ("_FILE_NAME", "nested reference"),
    ("_INDEX_HEADER", "flat"),
    ("_MULTI_SECTOR_HEADER", "flat"),
];

#[test]
fn the_dominant_shapes_are_within_the_specialised_nesting_limit() {
    let schema = parse(NTFS, Endian::Little).unwrap();
    let mut compiler = Compiler::new(&schema);

    let mut report = Vec::new();
    for (name, share) in DOMINANT {
        let plan = compiler
            .compile(name)
            .unwrap_or_else(|e| panic!("{name} must compile: {e}"));
        report.push((
            *name,
            *share,
            plan.shape.nesting_depth,
            plan.fast_path_eligible(),
        ));
    }

    for (name, share, depth, eligible) in &report {
        println!("{name:28} {share:>7}  depth={depth}  fast_path={eligible}");
    }

    // `_INDEX_ENTRY` is the shape the fast path exists to serve. If it is not
    // eligible, the fast path is decoration.
    let index_entry = report
        .iter()
        .find(|(name, ..)| *name == "_INDEX_ENTRY")
        .unwrap();
    assert!(
        index_entry.3,
        "_INDEX_ENTRY is 18.16% of decodes and must take the fast path; \
         it nests {} deep",
        index_entry.2
    );

    // Constant on purpose: this is a build guard wearing a test's clothes.
    #[allow(clippy::assertions_on_constants)]
    {
        assert!(
            MAX_NESTING > 2,
            "the compiler's hard limit is unrelated and must stay generous"
        );
    }
}
