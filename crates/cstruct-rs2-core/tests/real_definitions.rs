//! Parse complete, real definitions rather than fragments.
//!
//! The fixtures are the definition strings taken verbatim from `dissect.ntfs`
//! and `dissect.extfs`. Hand-written test definitions exercise one construct at
//! a time and stay accidentally simple; a real one is hundreds of lines of C
//! written by someone who was not thinking about this parser, and it combines
//! constructs in ways a fragment does not.
//!
//! If the parser cannot read these, it cannot read the definitions it exists to
//! read, whatever the narrower tests say.

use cstruct_rs2_core::parser::parse;
use cstruct_rs2_core::schema::{Endian, TypeDef};

const NTFS: &str = include_str!("fixtures/ntfs.txt");
const EXTFS: &str = include_str!("fixtures/extfs.txt");

#[test]
fn parses_the_real_ntfs_definition() {
    let schema = parse(NTFS, Endian::Little).expect("dissect.ntfs must parse");

    // Check by field count as well as by name, so a parse that "succeeds"
    // while quietly dropping members still fails.
    for (name, fields) in [
        ("_MFT_SEGMENT_REFERENCE", 3),
        ("_INDEX_ENTRY", 5),
        ("_FILE_RECORD_SEGMENT_HEADER", 10),
    ] {
        match schema.resolve(name) {
            Ok(TypeDef::Struct(def)) => assert_eq!(
                def.fields.len(),
                fields,
                "{name} should have {fields} top-level members"
            ),
            other => panic!("{name} did not resolve to a struct: {other:?}"),
        }
    }

    // The flag type NTFS uses on every file record.
    match schema.resolve("FILE_ATTRIBUTE") {
        Ok(TypeDef::Enum(def)) => assert!(def.is_flag),
        other => panic!("FILE_ATTRIBUTE should be a flag: {other:?}"),
    }
}

#[test]
fn parses_the_real_extfs_definition() {
    let schema = parse(EXTFS, Endian::Little).expect("dissect.extfs must parse");
    // The directory entry and the inode: the two types an ext reader uses most.
    assert!(matches!(
        schema.resolve("ext2_dir_entry_2"),
        Ok(TypeDef::Struct(_))
    ));
    assert!(matches!(
        schema.resolve("ext4_inode"),
        Ok(TypeDef::Struct(_))
    ));
}

/// Loading two definitions into one schema is the normal case: `dissect`
/// consumers call `cs.load()` repeatedly, and later definitions reference
/// earlier types.
#[test]
fn definitions_accumulate() {
    use cstruct_rs2_core::parser::parse_into;
    use cstruct_rs2_core::schema::Schema;

    let mut schema = Schema::with_builtins(Endian::Little);
    parse_into(NTFS, &mut schema).unwrap();
    parse_into(EXTFS, &mut schema).unwrap();
    assert!(schema.resolve("_INDEX_ENTRY").is_ok());
    assert!(schema.resolve("ext2_dir_entry_2").is_ok());
}
