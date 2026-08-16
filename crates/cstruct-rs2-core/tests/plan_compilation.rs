//! Compiling a schema into the flat plan the decoder walks.
//!
//! Everything that can be known before any bytes arrive is resolved here, once
//! per type per process. That makes this the module where a mistake is most
//! expensive and least visible: a wrong offset in a plan is not an error, it is
//! a record whose fields are read from the wrong bytes, for every record of
//! that type, forever.
//!
//! Three invariants carry most of the weight, and each has a defect behind it:
//!
//! * **The side tables stay parallel to `fields`.** They are indexed by field
//!   position, so one extra push makes every later entry describe the wrong
//!   field.
//! * **`aux` means exactly one thing.** It once doubled as an element count,
//!   which is impossible for an array of structures -- and that decoded the
//!   MBR's four-entry partition table as one entry, making every partitioned
//!   image undetectable while the unit tests stayed green.
//! * **Plan identity is type identity.** `isinstance` compares the compiled
//!   plan's address, so two plans for one type is two types.

use std::collections::HashMap;
use std::sync::Arc;

use cstruct_rs2_core::error::Error;
use cstruct_rs2_core::parser::{parse, parse_aligned_into, parse_into};
use cstruct_rs2_core::plan::{
    BitWindow, CompiledPlan, Compiler, FieldOp, Op, DYNAMIC_COUNT, DYNAMIC_OFFSET, MAX_NESTING,
};
use cstruct_rs2_core::schema::{Endian, Schema, TypeDef};

fn compile_with(source: &str, type_name: &str, endian: Endian) -> Arc<CompiledPlan> {
    let schema = parse(source, endian).unwrap_or_else(|e| panic!("{source}\n\nshould parse: {e}"));
    Compiler::new(&schema)
        .compile(type_name)
        .unwrap_or_else(|e| panic!("{type_name} should compile: {e}"))
}

fn compile(source: &str, type_name: &str) -> Arc<CompiledPlan> {
    compile_with(source, type_name, Endian::Little)
}

fn compile_failure(source: &str, type_name: &str) -> Error {
    let schema = parse(source, Endian::Little).expect("the definition should parse");
    Compiler::new(&schema)
        .compile(type_name)
        .expect_err(&format!("{type_name} should not compile"))
}

// ---------------------------------------------------------------------------
// Structural invariants
// ---------------------------------------------------------------------------

/// Every side table has exactly one entry per field.
///
/// `CompiledPlan` keeps names, lengths, bit windows, field alignments, `[EOF]`
/// flags, pointer targets and array dimensions *beside* the hot `fields` array rather than in
/// it, because `FieldOp` is 20 bytes and its width decides how many cache lines
/// a record occupies. The price of that layout is this invariant: the tables
/// are indexed by field position, so a table one entry too long makes every
/// entry after the extra one describe the wrong field.
///
/// It has been broken exactly once, by a duplicated push in the bitfield
/// branch, which is why the shapes below deliberately mix bitfields with
/// pointers, arrays, `[EOF]` and nesting rather than testing a plain struct.
#[test]
fn every_side_table_stays_parallel_to_the_field_array() {
    let shapes = [
        ("struct s { uint32 a; uint16 b; };", "s"),
        ("struct s { uint32 a : 4; uint32 b : 12; uint32 c; };", "s"),
        (
            "struct s { uint8 flags : 3; uint32 *p; char name[4]; };",
            "s",
        ),
        (
            "struct inner { uint8 a; };
             struct s { uint8 n : 2; uint8 m : 6; inner nested; uint8 rest[EOF]; };",
            "s",
        ),
        (
            "struct s { uint16 len; uint8 a : 1; char d[len]; uint8 tail[]; };",
            "s",
        ),
        ("union u { uint8 a : 3; uint32 b; };", "u"),
    ];

    for (source, name) in shapes {
        let plan = compile(source, name);
        let width = plan.fields.len();
        assert_eq!(plan.names.len(), width, "names, in {name}");
        assert_eq!(plan.lengths.len(), width, "lengths, in {name}");
        assert_eq!(
            plan.array_length_sources.len(),
            width,
            "array_length_sources, in {name}"
        );
        assert_eq!(plan.bit_windows.len(), width, "bit_windows, in {name}");
        assert_eq!(
            plan.field_alignments.len(),
            width,
            "field_alignments, in {name}"
        );
        assert_eq!(plan.to_end.len(), width, "to_end, in {name}");
        assert_eq!(
            plan.pointer_targets.len(),
            width,
            "pointer_targets, in {name}"
        );
        assert_eq!(plan.array_dims.len(), width, "array_dims, in {name}");
        assert_eq!(
            plan.runtime_array_dims.len(),
            width,
            "runtime_array_dims, in {name}"
        );
    }
}

/// Content-sized matrices remain compiled operations rather than the generic
/// unsupported placeholder.
#[test]
fn runtime_matrix_dimensions_are_retained() {
    let plan = compile("struct args { uint32 argc; char argv[argc][]; };", "args");
    assert_eq!(plan.fields[1].op, Op::ScalarArray);
    assert!(plan.fields[1].dynamic);
    assert_eq!(plan.runtime_array_dims[1].len(), 2);

    let cube = compile(
        "struct cube { uint8 x; uint8 y; uint8 z; uint8 data[x][y][z]; };",
        "cube",
    );
    assert_eq!(cube.runtime_array_dims[3].len(), 3);
}

/// A pointer's target must be reported against the pointer's own field, which
/// is the observable half of the invariant above.
#[test]
fn a_pointer_target_lands_on_the_pointer_field_even_after_a_bitfield() {
    let plan = compile(
        "struct s { uint8 flags : 3; uint8 more : 5; uint32 *p; uint16 tail; };",
        "s",
    );
    let index = plan.field_index("p").expect("p should be a declared field");
    assert_eq!(
        plan.pointer_targets[index].as_deref(),
        Some("uint32"),
        "the target must sit against `p`, not against whatever follows it"
    );
    assert!(
        plan.pointer_targets
            .iter()
            .enumerate()
            .filter(|(position, _)| *position != index)
            .all(|(_, target)| target.is_none()),
        "no other field points at anything"
    );
}

/// `aux` is the sub-plan or enum index and nothing else.
///
/// When it doubled as an element count, an array of structures decoded as a
/// single element: the MBR's four-entry partition table became one entry, and
/// every partitioned image became undetectable.
#[test]
fn an_array_of_structs_keeps_both_its_subplan_and_its_count() {
    let plan = compile(
        "struct part { uint8 flag; uint8 chs[3]; uint8 type; uint8 chs2[3];
                       uint32 lba; uint32 sectors; };
         struct mbr { char boot[446]; part partitions[4]; uint16 signature; };",
        "mbr",
    );
    let partitions = &plan.fields[1];
    assert_eq!(partitions.op, Op::NestedArray);
    assert_eq!(partitions.count, 4, "four partition entries, not one");
    assert_eq!(partitions.size, 64, "4 x 16 bytes");
    assert_eq!(
        plan.subplans[partitions.aux as usize].total_size,
        Some(16),
        "aux must still index the element's plan"
    );
    assert_eq!(plan.fields[2].offset, 510);
    assert_eq!(plan.total_size, Some(512));
}

/// An alias is a second spelling, not a second type.
///
/// `isinstance` compares the compiled plan's address, so compiling
/// `FILE_REFERENCE` and `_REF` to different plans would make them different
/// types — and a nested member, which always compiles through the tag, would
/// then fail `isinstance` against the alias its own declaration used.
#[test]
fn an_alias_and_its_tag_compile_to_the_same_plan() {
    let schema = parse(
        "typedef struct _REF { uint32 a; uint16 b; uint16 c; } FILE_REFERENCE;
         struct outer { FILE_REFERENCE r; uint16 t; };",
        Endian::Little,
    )
    .unwrap();
    let mut compiler = Compiler::new(&schema);

    // Deliberately alias first: the tag must join the *existing* plan rather
    // than the other way round, since a consumer may touch either spelling
    // first and the answer cannot depend on which.
    let via_alias = compiler.compile("FILE_REFERENCE").unwrap();
    let via_tag = compiler.compile("_REF").unwrap();
    assert!(
        Arc::ptr_eq(&via_alias, &via_tag),
        "both spellings must reach one plan"
    );

    let outer = compiler.compile("outer").unwrap();
    assert!(
        Arc::ptr_eq(&outer.subplans[0], &via_alias),
        "a nested member's plan must be the same one the alias hands out"
    );
}

/// The plan cache has to outlive a single `compile` call.
///
/// Compiling a type on its own and then again as a nested field must not
/// produce two plans for it. If it does, a record decoded through the nested
/// route fails `isinstance` against the very class that produced it — with no
/// error, just a check that quietly answers `False`.
#[test]
fn a_shared_cache_gives_one_plan_per_type_across_compilers() {
    let schema = parse(
        "struct inner { uint32 a; };
         struct outer { inner nested; uint16 tail; };",
        Endian::Little,
    )
    .unwrap();

    let mut first = Compiler::new(&schema);
    let standalone = first.compile("inner").unwrap();
    let cache = first.into_cache();

    let mut second = Compiler::with_cache(&schema, cache);
    let outer = second.compile("outer").unwrap();

    assert!(
        Arc::ptr_eq(&standalone, &outer.subplans[0]),
        "the nested plan must be the same object as the standalone one"
    );
}

/// Names resolve to a *path*, because anonymous members fold their fields into
/// the parent's namespace and reaching one means stepping through the member
/// that declares it.
///
/// A single index would hand back the anonymous union rather than the field the
/// consumer asked for -- caught by decoding a real `_INDEX_ENTRY` rather than a
/// synthetic one.
#[test]
fn folded_names_resolve_through_the_member_that_declares_them() {
    let plan = compile(
        "typedef struct _REF { uint32 Low; uint16 High; uint16 Seq; } FILE_REFERENCE;
         typedef struct _INDEX_ENTRY {
             union {
                 FILE_REFERENCE FileReference;
                 struct { uint16 DataOffset; uint16 DataLength; uint32 _; };
             };
             uint16 Length;
         } INDEX_ENTRY;",
        "INDEX_ENTRY",
    );

    assert_eq!(plan.field_path("Length").map(<[u32]>::len), Some(1));
    assert_eq!(plan.field_index("Length"), Some(1));

    let folded = plan.field_path("FileReference").expect("must resolve");
    assert!(
        folded.len() > 1,
        "a folded name steps inward, got {folded:?}"
    );
    assert_eq!(
        plan.field_index("FileReference"),
        None,
        "field_index answers only for directly declared fields"
    );

    // Two levels of folding: the anonymous struct inside the anonymous union.
    assert!(plan.field_path("DataOffset").is_some());
    assert_eq!(plan.field_path("nothing_of_the_sort"), None);
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

#[test]
fn a_fixed_struct_gets_every_offset_and_a_total_size() {
    let plan = compile("struct s { uint16 a; uint32 b; uint8 c; };", "s");
    assert_eq!(plan.total_size, Some(7));
    assert_eq!(
        plan.fields.iter().map(|f| f.offset).collect::<Vec<_>>(),
        [0, 2, 6]
    );
    assert!(
        plan.fast_path_eligible(),
        "a fixed, flat type is the fast path"
    );
}

/// A union's members all start at zero and its size is the largest member's.
#[test]
fn union_members_overlap_at_offset_zero() {
    let plan = compile("union u { uint32 a; uint16 b; char c[7]; };", "u");
    assert!(plan.is_union);
    assert!(plan.fields.iter().all(|f| f.offset == 0));
    assert_eq!(plan.total_size, Some(7), "as big as its largest member");
}

/// A dynamic field has to make the *rest* of the record dynamic too. Otherwise
/// a trailing field keeps the offset it was assigned at compile time, which is
/// only correct when the dynamic part happens to be empty — so the record reads
/// correctly in exactly the case that hides the bug.
#[test]
fn everything_after_a_dynamic_field_is_dynamic() {
    let plan = compile(
        "struct s { uint16 len; wchar name[len]; uint32 trailing; };",
        "s",
    );
    assert_eq!(plan.total_size, None);
    assert_eq!(plan.fields[0].offset, 0);
    assert!(plan.fields[1].dynamic);
    assert_eq!(plan.fields[2].offset, DYNAMIC_OFFSET);
    assert!(!plan.fast_path_eligible());
    assert!(plan.shape.dependent_lengths);
}

/// `char[n]` and `wchar[n]` are a single `bytes` or `str`, not arrays of
/// one-element values. Getting this wrong returns a list of one-character
/// strings where a consumer expects a name.
#[test]
fn char_and_wchar_arrays_are_their_own_ops() {
    let plan = compile(
        "struct s { char name[8]; wchar wide[4]; uint16 nums[4]; };",
        "s",
    );
    assert_eq!(plan.fields[0].op, Op::CharArray);
    assert_eq!(plan.fields[0].size, 8);
    assert_eq!(plan.fields[1].op, Op::WcharArray);
    assert_eq!(plan.fields[1].size, 8, "4 code units of 2 bytes");
    assert_eq!(plan.fields[2].op, Op::ScalarArray);
    assert_eq!(plan.fields[2].elem, Op::Uint);
    assert_eq!(plan.fields[2].elem_size, 2);
}

/// A length naming only `#define`s is still fixed, and folding it here means
/// the decoder never evaluates an expression for it.
#[test]
fn constant_lengths_fold_at_compile_time() {
    let plan = compile("#define N 4\nstruct s { uint32 v[N]; };", "s");
    assert!(!plan.fields[0].dynamic);
    assert_eq!(plan.fields[0].size, 16);
    assert_eq!(plan.fields[0].count, 4);
    assert!(
        plan.lengths[0].is_none(),
        "a folded length needs no expression"
    );
    assert_eq!(plan.total_size, Some(16));
}

/// `char a[sizeof(uint32)]` and `char d[offsetof(h, b)]` resolve from the
/// schema while compiling, so they are fixed lengths like any other.
#[test]
fn sizeof_and_offsetof_lengths_fold_against_the_schema() {
    let plan = compile(
        "struct h { uint32 a; uint16 b; };
         struct s { char sized[sizeof(uint32)]; char nested[sizeof(h)]; char at[offsetof(h, b)]; };",
        "s",
    );
    assert_eq!(plan.fields[0].size, 4);
    assert_eq!(plan.fields[1].size, 6);
    assert_eq!(plan.fields[2].size, 4);
    assert_eq!(plan.total_size, Some(14));
}

/// An enum member is deliberately *not* resolvable as an array length.
///
/// Upstream raises for `uint16 vals[SD_COUNT]` where `SD_COUNT` is an enum
/// member, so resolving it here would decode a record where the oracle produces
/// none. Being wrong in a generous direction is still being wrong.
#[test]
fn an_enum_member_does_not_fold_as_an_array_length() {
    let plan = compile(
        "enum E : uint8 { SD_COUNT = 4 };
         struct s { uint16 vals[SD_COUNT]; };",
        "s",
    );
    assert!(
        plan.fields[0].dynamic,
        "the length must stay unresolved rather than silently folding to 4"
    );
    assert_eq!(plan.total_size, None);
}

/// A string constant cannot size anything, and the failure is at decode rather
/// than at compile -- upstream defers length evaluation to read time, and a
/// type that will not build is a module that will not import.
#[test]
fn a_string_constant_length_leaves_the_field_dynamic() {
    let plan = compile(
        "#define PREFIX \"gnu.\"\nstruct s { char d[PREFIX]; };",
        "s",
    );
    assert!(plan.fields[0].dynamic);
    assert!(
        plan.lengths[0].is_some(),
        "the expression is kept for read time"
    );
}

// ---------------------------------------------------------------------------
// Alignment
// ---------------------------------------------------------------------------

/// Natural alignment pads each field up to its own alignment, and rounds the
/// whole structure up so an array of them puts every element on a boundary.
#[test]
fn an_aligned_struct_pads_its_fields_and_its_size() {
    let mut schema = Schema::with_builtins(Endian::Little);
    parse_aligned_into("struct s { uint8 a; uint32 b; uint8 c; };", &mut schema).unwrap();
    let plan = Compiler::new(&schema).compile("s").unwrap();

    assert!(plan.aligned);
    assert_eq!(plan.fields[0].offset, 0);
    assert_eq!(plan.fields[1].offset, 4, "padded up to a 4-byte boundary");
    assert_eq!(plan.fields[2].offset, 8);
    assert_eq!(
        plan.total_size,
        Some(12),
        "rounded up to the type's alignment"
    );
    assert_eq!(plan.alignment, 4, "the largest member's alignment");
}

/// The same declaration, unaligned, packs tight. That is the default: on-disk
/// structures are laid out by their format, not by a compiler's padding rules.
#[test]
fn an_unaligned_struct_packs_tight() {
    let plan = compile("struct s { uint8 a; uint32 b; uint8 c; };", "s");
    assert!(!plan.aligned);
    assert_eq!(
        plan.fields.iter().map(|f| f.offset).collect::<Vec<_>>(),
        [0, 1, 5]
    );
    assert_eq!(plan.total_size, Some(6));
    assert_eq!(
        plan.alignment, 4,
        "alignment is computed either way, so an aligned parent can use it"
    );
}

/// `int24` aligns to 4 and `uint48` to 8, which is upstream's rule and not
/// their own width.
///
/// It matters because the padding expression `-offset & (a - 1)` is only
/// correct for a power-of-two alignment; a three-byte alignment would compute
/// nonsense.
#[test]
fn odd_width_scalars_align_to_the_next_power_of_two() {
    let mut schema = Schema::with_builtins(Endian::Little);
    parse_aligned_into(
        "struct s { uint8 a; uint24 b; uint8 c; uint48 d; };",
        &mut schema,
    )
    .unwrap();
    let plan = Compiler::new(&schema).compile("s").unwrap();
    assert_eq!(plan.fields[1].offset, 4, "uint24 aligns to 4");
    assert_eq!(plan.fields[2].offset, 7);
    assert_eq!(plan.fields[3].offset, 8, "uint48 aligns to 8");
    assert_eq!(plan.alignment, 8);
}

/// An aligned parent aligns a nested member to the nested type's own
/// alignment, which is why alignment is computed for unaligned types too.
#[test]
fn a_nested_member_contributes_its_alignment_to_an_aligned_parent() {
    let mut schema = Schema::with_builtins(Endian::Little);
    parse_aligned_into(
        "struct inner { uint32 a; };
         struct outer { uint8 lead; inner nested; };",
        &mut schema,
    )
    .unwrap();
    let plan = Compiler::new(&schema).compile("outer").unwrap();
    assert_eq!(plan.fields[1].offset, 4);
    assert_eq!(plan.total_size, Some(8));
}

/// An array aligns like one of its elements, not like the whole array.
#[test]
fn an_array_aligns_like_its_element() {
    let mut schema = Schema::with_builtins(Endian::Little);
    parse_aligned_into("struct s { uint8 a; uint32 v[2]; };", &mut schema).unwrap();
    let plan = Compiler::new(&schema).compile("s").unwrap();
    assert_eq!(plan.fields[1].offset, 4);
    assert_eq!(plan.total_size, Some(12));
}

// ---------------------------------------------------------------------------
// Bitfields
// ---------------------------------------------------------------------------

/// Consecutive bitfields of one type share a storage unit, and **only the
/// field that opens the unit consumes bytes**.
///
/// Without that distinction the layout walk advances the cursor once per
/// bitfield instead of once per unit, and every field after a bitfield run sits
/// at the wrong offset -- which is a silently wrong record, not a crash.
#[test]
fn consecutive_bitfields_share_one_storage_unit() {
    let plan = compile(
        "struct s { uint16 a : 4; uint16 b : 4; uint16 c : 8; uint32 after; };",
        "s",
    );
    let windows: Vec<BitWindow> = plan.bit_windows[..3].to_vec();
    assert!(windows[0].opens_unit, "the first field opens the unit");
    assert!(!windows[1].opens_unit);
    assert!(!windows[2].opens_unit);
    assert!(
        plan.fields[..3].iter().all(|f| f.offset == 0),
        "one unit, one offset"
    );
    assert_eq!(
        plan.fields[3].offset, 2,
        "the unit costs two bytes in total, not two per field"
    );
    assert_eq!(plan.total_size, Some(6));
}

/// A change of underlying type starts a new unit, as does exhausting the
/// current one.
#[test]
fn a_new_storage_unit_starts_when_the_type_or_the_space_changes() {
    let plan = compile(
        "struct s { uint8 a : 4; uint16 b : 4; uint16 c : 12; uint16 d : 4; };",
        "s",
    );
    assert!(plan.bit_windows[0].opens_unit, "a: opens a uint8 unit");
    assert!(
        plan.bit_windows[1].opens_unit,
        "b: a different type, new unit"
    );
    assert!(!plan.bit_windows[2].opens_unit, "c: 12 bits still fit");
    assert!(plan.bit_windows[3].opens_unit, "d: the uint16 unit is full");
    assert_eq!(plan.total_size, Some(1 + 2 + 2));
}

/// Little-endian fills a unit from the low bits up; big-endian from the high
/// bits down. Both are upstream's `BitBuffer`, computed once here so the
/// decoder is a stateless shift-and-mask.
#[test]
fn bit_windows_shift_according_to_byte_order() {
    let source = "struct s { uint8 a : 3; uint8 b : 5; };";

    let little = compile_with(source, "s", Endian::Little);
    assert_eq!(
        (little.bit_windows[0].shift, little.bit_windows[0].width),
        (0, 3)
    );
    assert_eq!(
        (little.bit_windows[1].shift, little.bit_windows[1].width),
        (3, 5)
    );

    let big = compile_with(source, "s", Endian::Big);
    assert_eq!((big.bit_windows[0].shift, big.bit_windows[0].width), (5, 3));
    assert_eq!((big.bit_windows[1].shift, big.bit_windows[1].width), (0, 5));
}

/// The mask is derived from the declared width, including at the full width of
/// the storage unit where a naive `1 << width` would overflow.
#[test]
fn a_bit_window_masks_to_its_declared_width() {
    assert_eq!(BitWindow::default().mask(), 0);
    assert!(!BitWindow::default().is_bitfield());

    let plan = compile("struct s { uint64 whole : 64; };", "s");
    let window = plan.bit_windows[0];
    assert!(window.is_bitfield());
    assert_eq!(window.width, 64);
    assert_eq!(
        window.mask(),
        u64::MAX,
        "a full-width mask must not overflow"
    );

    let plan = compile("struct s { uint32 nibble : 4; };", "s");
    assert_eq!(plan.bit_windows[0].mask(), 0xf);
}

/// Upstream refuses to split a bitfield across two units rather than choosing a
/// split, and so does this -- a silently different split is a wrong record.
#[test]
fn straddling_bitfields_are_refused() {
    match parse("struct s { uint8 a : 5; uint8 b : 5; };", Endian::Little)
        .expect_err("the definition should be rejected while loading")
    {
        Error::InvalidValue { message } => {
            assert_eq!(message, "Straddled bit fields are unsupported")
        }
        other => panic!("expected a value error, got {other:?}"),
    }
}

/// A bitfield's storage unit must have a fixed width. Upstream treats `char` as
/// an unsigned byte here even though ordinary `char` values materialize as
/// bytes.
#[test]
fn a_bitfield_over_a_non_integer_is_refused() {
    for source in ["struct s { float a : 4; };", "struct s { uleb128 a : 4; };"] {
        assert!(
            matches!(compile_failure(source, "s"), Error::InvalidValue { .. }),
            "{source} should be refused"
        );
    }

    let plan = compile("struct s { char a : 4; };", "s");
    assert_eq!(plan.fields[0].op, Op::Bits);
    assert_eq!(plan.fields[0].elem, Op::Char);
}

/// An enum *is* an integer, so a bitfield over one is allowed.
#[test]
fn a_bitfield_over_an_enum_is_allowed() {
    let plan = compile(
        "enum E : uint8 { A = 1, B = 2 };
         struct s { E kind : 4; E other : 4; };",
        "s",
    );
    assert_eq!(plan.fields[0].op, Op::Bits);
    assert_eq!(
        plan.fields[0].elem,
        Op::Enum,
        "the underlying op is kept in `elem`"
    );
    assert_eq!(plan.total_size, Some(1));
}

// ---------------------------------------------------------------------------
// Element kinds
// ---------------------------------------------------------------------------

/// An enum carries everything needed to read it *and* to name the member,
/// including the byte order -- an enum type is callable on a stream, so it has
/// to know how to read, not merely how to name.
#[test]
fn an_enum_plan_carries_its_width_signedness_and_members() {
    let plan = compile_with(
        "flag F : int16 { A = 1, B = 2 };
         struct s { F flags; };",
        "s",
        Endian::Big,
    );
    assert_eq!(plan.fields[0].op, Op::Enum);
    assert_eq!(plan.fields[0].size, 2);
    let enum_plan = &plan.enums[plan.fields[0].aux as usize];
    assert!(enum_plan.is_flag);
    assert!(enum_plan.signed, "an int16 base is signed");
    assert_eq!(enum_plan.endian, Endian::Big);
    assert_eq!(enum_plan.members.len(), 2);
    assert_eq!(&*enum_plan.name, "F");
}

/// An enum whose base is not an integer at all cannot be given a width, and
/// guessing one would misplace every field after it.
#[test]
fn an_enum_over_a_non_integer_base_is_refused() {
    for source in [
        "enum E : char { A = 1 }; struct s { E e; };",
        "enum E : float { A = 1 }; struct s { E e; };",
        "struct b { uint8 x; }; enum E : b { A = 1 }; struct s { E e; };",
    ] {
        assert!(
            matches!(compile_failure(source, "s"), Error::InvalidValue { .. }),
            "{source} should be refused"
        );
    }
}

/// A LEB128 is the one scalar with no width until it is read, so it makes its
/// structure dynamic.
#[test]
fn a_leb128_field_is_dynamic_by_construction() {
    let plan = compile("struct s { uleb128 n; ileb128 signed_n; };", "s");
    assert_eq!(plan.fields[0].op, Op::Leb128);
    assert_eq!(
        plan.fields[0].elem,
        Op::Uint,
        "signedness is carried in `elem`"
    );
    assert_eq!(plan.fields[1].elem, Op::Int);
    assert!(plan.fields[0].dynamic);
    assert_eq!(plan.total_size, None);
}

/// A consumer-supplied type is recorded by name and read by calling back into
/// Python; the core crate only has to know it exists and that its length is
/// whatever the consumer's reader consumes.
#[test]
fn a_custom_type_becomes_a_named_callback_field() {
    let mut schema = Schema::with_builtins(Endian::Little);
    schema.insert("varint".to_owned(), TypeDef::Custom);
    parse_into("struct s { varint v; uint8 tail; };", &mut schema).unwrap();
    let plan = Compiler::new(&schema).compile("s").unwrap();

    assert_eq!(plan.fields[0].op, Op::Custom);
    assert!(plan.fields[0].dynamic);
    assert_eq!(&*plan.custom_names[plan.fields[0].aux as usize], "varint");
    assert_eq!(
        plan.total_size, None,
        "an unknown length makes the record dynamic"
    );
}

/// `[EOF]` is flagged on the plan rather than inferred at read time.
///
/// Both `[]` and `[EOF]` are dynamic arrays with no length expression, and
/// guessing between "stop at a zero" and "read to the end" is how an evtx
/// corruption happened.
#[test]
fn read_to_end_arrays_are_flagged_rather_than_inferred() {
    let plan = compile("struct s { uint8 a; char rest[EOF]; char term[]; };", "s");
    assert!(!plan.to_end[0]);
    assert!(plan.to_end[1], "[EOF] is flagged");
    assert!(!plan.to_end[2], "[] is not");
    assert_eq!(plan.fields[1].count, DYNAMIC_COUNT);
}

/// `is_null_terminated` exists as **one** function because the decoder and the
/// encoder both need the answer and disagreeing is silent corruption.
///
/// The encoder used to ask `FieldOp::dynamic`, which is true for `char d[n]`
/// too, so every count-sized char array grew a null terminator on `dumps()` --
/// and `dissect.shellitem`'s LNK reader seeks by exactly `len(dumps())`.
#[test]
fn only_a_terminated_array_counts_as_null_terminated() {
    let plan = compile(
        "struct s { uint8 n; char terminated[]; char counted[n]; char fixed[4]; char one; char to_end[EOF]; };",
        "s",
    );
    let expected = [
        ("n", false),
        ("terminated", true),
        ("counted", false),
        ("fixed", false),
        ("one", false),
        ("to_end", false),
    ];
    for (name, want) in expected {
        let index = plan.field_index(name).unwrap();
        assert_eq!(
            plan.is_null_terminated(index),
            want,
            "{name} should{} be null-terminated",
            if want { "" } else { " not" }
        );
    }
    assert!(
        !plan.is_null_terminated(999),
        "an out-of-range index answers no rather than panicking"
    );
}

// ---------------------------------------------------------------------------
// Multi-dimensional arrays
// ---------------------------------------------------------------------------

/// `CHAR Hash[29][16]` flattens to one field with a stride and a dimension
/// list, rather than an array of arrays that a flat `FieldOp` cannot describe.
#[test]
fn a_matrix_flattens_to_a_count_and_a_dimension_list() {
    let plan = compile("struct s { char Hash[29][16]; };", "s");
    assert_eq!(plan.fields[0].op, Op::ScalarArray);
    assert_eq!(plan.fields[0].count, 29 * 16);
    assert_eq!(plan.fields[0].size, 29 * 16);
    assert_eq!(&*plan.array_dims[0], &[29, 16], "outermost first");
    assert_eq!(plan.total_size, Some(464));
}

/// A dimension may be a constant rather than a literal --
/// `zones[SD_COUNT][ALLOC_COUNT]` in `dissect.apfs` -- so each is folded
/// against the schema exactly as a one-dimensional length is.
#[test]
fn matrix_dimensions_fold_against_schema_constants() {
    let plan = compile(
        "#define ROWS 3\n#define COLS 4\nstruct s { uint16 grid[ROWS][COLS]; };",
        "s",
    );
    assert_eq!(plan.fields[0].count, 12);
    assert_eq!(plan.fields[0].size, 24);
    assert_eq!(&*plan.array_dims[0], &[3, 4]);
}

/// A matrix of structures stays a matrix of structures.
///
/// Calling it a scalar array would read the sub-records as integers.
#[test]
fn a_matrix_of_structs_keeps_its_element_kind() {
    let plan = compile(
        "struct cell { uint16 a; uint16 b; };
         struct s { cell grid[2][3]; };",
        "s",
    );
    assert_eq!(plan.fields[0].op, Op::NestedArray);
    assert_eq!(plan.fields[0].count, 6);
    assert_eq!(plan.fields[0].size, 24);
}

/// A dimension that cannot be resolved remains a runtime expression rather
/// than failing the compile.
///
/// Upstream stores a length expression and evaluates it when a record is read,
/// so a name it cannot resolve does not stop the *type* from being created --
/// `dissect.apfs` imports on upstream for exactly this reason. The field is
/// compiled and fails only if it is decoded without that name in scope.
#[test]
fn an_unresolvable_matrix_dimension_stays_a_runtime_expression() {
    let plan = compile("struct s { uint16 grid[UNKNOWN_COUNT][4]; };", "s");
    assert_eq!(plan.fields[0].op, Op::ScalarArray);
    assert!(plan.fields[0].dynamic);
    assert_eq!(plan.runtime_array_dims[0].len(), 2);
    assert_eq!(plan.total_size, None);
}

/// A multi-dimensional array of a variable-width type has no stride, and
/// guessing at which dimension a length field sizes would be silently wrong.
#[test]
fn a_matrix_of_a_variable_width_type_is_refused() {
    match compile_failure(
        "struct inner { uint8 n; char d[n]; };
         struct s { inner grid[2][2]; };",
        "s",
    ) {
        Error::Unsupported { construct } => {
            assert!(construct.contains("multi-dimensional"), "{construct}")
        }
        other => panic!("expected an unsupported-construct error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------

/// A definition is input. Self-reference and runaway nesting must be errors,
/// not a stack overflow.
#[test]
fn nesting_deeper_than_the_limit_is_refused() {
    let mut source = String::from("struct L0 { uint8 a; };\n");
    let depth = usize::from(MAX_NESTING) + 8;
    for level in 1..=depth {
        source.push_str(&format!("struct L{level} {{ L{} m; }};\n", level - 1));
    }
    match compile_failure(&source, &format!("L{depth}")) {
        Error::LimitExceeded { limit, .. } => assert_eq!(limit, "structure nesting"),
        other => panic!("expected a limit error, got {other:?}"),
    }

    // Just inside the limit still compiles, so the guard is a guard and not a
    // ceiling that real definitions bump into.
    let plan = compile(&source, "L8");
    assert_eq!(plan.shape.nesting_depth, 8);
}

/// An array whose declared bytes exceed what an offset can address is refused
/// while compiling rather than becoming a wrapped-around size.
#[test]
fn an_oversized_fixed_array_is_refused() {
    match compile_failure("struct s { uint64 v[1000000000]; };", "s") {
        Error::LimitExceeded { limit, .. } => assert_eq!(limit, "array byte size"),
        other => panic!("expected a limit error, got {other:?}"),
    }
    match compile_failure("struct s { uint8 v[65536][65536]; };", "s") {
        Error::LimitExceeded { limit, .. } => assert_eq!(limit, "array byte size"),
        other => panic!("expected a limit error, got {other:?}"),
    }
}

/// Only structures have plans; asking for anything else is refused rather than
/// mis-planned.
#[test]
fn compiling_a_non_structure_is_refused() {
    let schema = parse("enum E : uint8 { A = 1 };", Endian::Little).unwrap();
    let mut compiler = Compiler::new(&schema);
    for name in ["uint32", "E"] {
        assert!(
            matches!(compiler.compile(name), Err(Error::InvalidValue { .. })),
            "{name} is not a structure"
        );
    }
    assert!(matches!(
        compiler.compile("NoSuchType"),
        Err(Error::UnknownType { .. })
    ));
}

// ---------------------------------------------------------------------------
// The layout claim in the module docs
// ---------------------------------------------------------------------------

/// `FieldOp`'s size is load-bearing, not incidental: it decides how many cache
/// lines an eight-field record occupies, which is the reason the side tables
/// exist at all. Pinned so a new member cannot quietly change the trade.
#[test]
fn a_field_op_stays_twenty_bytes() {
    assert_eq!(std::mem::size_of::<FieldOp>(), 20);
    assert_eq!(
        std::mem::size_of::<FieldOp>() * 8,
        160,
        "an eight-field record must stay within three cache lines"
    );
}

/// The real `_INDEX_ENTRY`, which is 18.16% of all decodes in an NTFS `walkfs`
/// and the single shape the fast path exists to serve.
#[test]
fn the_real_index_entry_is_a_fixed_sixteen_bytes_on_the_fast_path() {
    let ntfs = include_str!("fixtures/ntfs.txt");
    let schema = parse(ntfs, Endian::Little).unwrap();
    let plan = Compiler::new(&schema).compile("_INDEX_ENTRY").unwrap();
    assert_eq!(
        plan.total_size,
        Some(16),
        "an 8-byte union plus four shorts"
    );
    assert!(plan.fast_path_eligible());
    assert_eq!(plan.shape.nesting_depth, 2, "a union containing a struct");
}

/// A plan built from a schema with no types at all is an unknown type, not a
/// panic -- the empty schema is what a fresh namespace starts from.
#[test]
fn an_empty_schema_has_no_plans() {
    let schema = Schema::default();
    let mut compiler = Compiler::new(&schema);
    assert!(matches!(
        compiler.compile("anything"),
        Err(Error::UnknownType { .. })
    ));
    assert!(compiler.into_cache().is_empty());
}

/// The cache a compiler hands back is keyed by the names that were asked for,
/// which is what lets a namespace carry it from one `load()` to the next.
#[test]
fn the_returned_cache_holds_what_was_compiled() {
    let schema = parse(
        "struct inner { uint8 a; }; struct outer { inner m; };",
        Endian::Little,
    )
    .unwrap();
    let mut compiler = Compiler::new(&schema);
    compiler.compile("outer").unwrap();
    let cache: HashMap<String, Arc<CompiledPlan>> = compiler.into_cache();
    assert!(cache.contains_key("outer"));
    assert!(
        cache.contains_key("inner"),
        "a nested type is compiled and cached too"
    );
}
