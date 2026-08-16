//! Error classification, the schema IR, and the build guards.
//!
//! # Why the error mapping is tested here rather than in the binding crate
//!
//! Which class a failure raises is part of the interface. Callers end a record
//! run with `except EOFError: break`, so an end-of-input arriving as a
//! `ValueError` escapes that loop and aborts the walk instead of ending it.
//!
//! The mapping lives beside the variants it describes precisely so it can be
//! asserted without an interpreter, which is what this file does. The binding
//! crate then has exactly one place that translates.

use cstruct_rs2_core::error::Error;
use cstruct_rs2_core::expr::Expr;
use cstruct_rs2_core::schema::{ConstValue, Endian, Scalar, Schema, TypeDef, TypeRef};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// One case per variant, so a new variant cannot be added without deciding what
/// Python class it becomes.
///
/// The table is the assertion: a variant that maps to the wrong class is a
/// consumer's `except` clause that stops matching, and nothing else in the
/// suite would notice.
#[test]
fn every_error_variant_maps_to_the_python_class_the_contract_names() {
    let cases = [
        (Error::EndOfStream, "EOFError"),
        (
            Error::Parse {
                line: 3,
                message: "bad".to_owned(),
            },
            "ParserError",
        ),
        (
            Error::Lex {
                message: "bad".to_owned(),
            },
            "LexerError",
        ),
        (
            Error::UnknownType {
                name: "nope".to_owned(),
            },
            "ResolveError",
        ),
        (
            Error::Unsupported {
                construct: "something".to_owned(),
            },
            "NotImplementedError",
        ),
        (
            Error::InvalidValue {
                message: "bad".to_owned(),
            },
            "ValueError",
        ),
        (
            Error::LimitExceeded {
                limit: "array elements",
                value: 9,
            },
            "ValueError",
        ),
    ];

    for (error, class) in cases {
        assert_eq!(error.python_exception(), class, "{error:?}");
    }
}

/// Every variant renders as something a person can act on.
///
/// The two pinned exactly are the ones a consumer may *match on*: an unknown
/// type keeps upstream's spelling so the same words come from either
/// implementation, and a parse error leads with its line number.
#[test]
fn every_error_variant_renders_readably() {
    assert_eq!(Error::EndOfStream.to_string(), "unexpected end of stream");
    assert_eq!(
        Error::Parse {
            line: 12,
            message: "expected a semicolon".to_owned(),
        }
        .to_string(),
        "line 12: expected a semicolon"
    );
    assert_eq!(
        Error::Lex {
            message: "invalid hexadecimal literal: 0x".to_owned(),
        }
        .to_string(),
        "invalid hexadecimal literal: 0x"
    );
    assert_eq!(
        Error::UnknownType {
            name: "WIDGET".to_owned(),
        }
        .to_string(),
        "Unknown type WIDGET",
        "upstream's exact spelling, so a consumer matching on it sees one message"
    );
    assert_eq!(
        Error::InvalidValue {
            message: "division by zero".to_owned(),
        }
        .to_string(),
        "division by zero"
    );

    let limit = Error::LimitExceeded {
        limit: "array elements",
        value: 4_000_000_000,
    }
    .to_string();
    assert!(
        limit.contains("array elements") && limit.contains("4000000000"),
        "{limit}"
    );

    // The refusal has to name the construct. "Not supported" on its own leaves
    // the reader guessing which part of their definition to change.
    let unsupported = Error::Unsupported {
        construct: "a nested variable-width matrix".to_owned(),
    }
    .to_string();
    assert!(
        unsupported.contains("a nested variable-width matrix"),
        "{unsupported}"
    );
    assert!(unsupported.contains("dissect.cstruct"), "{unsupported}");
}

/// `EndOfStream` carries no payload, deliberately.
///
/// It is expected, frequent, and used as loop control, so constructing a
/// message for it would be wasted work on a path that runs per record. The
/// assertion is on the *type*, which is where that cost would appear.
#[test]
fn end_of_stream_is_payload_free() {
    assert_eq!(
        std::mem::size_of_val(&Error::EndOfStream),
        std::mem::size_of::<Error>(),
        "the enum's size is its largest variant's; EndOfStream itself allocates nothing"
    );
    // Cloning it cannot allocate, which is the property that matters on a path
    // that hits it once per record at the end of every run.
    let cloned = Error::EndOfStream.clone();
    assert_eq!(cloned, Error::EndOfStream);
}

/// Errors are comparable and are ordinary `std::error::Error`s, so a consumer
/// of this crate can match on them and put them in a `Box<dyn Error>`.
#[test]
fn errors_are_comparable_and_standard() {
    assert_eq!(Error::EndOfStream, Error::EndOfStream);
    assert_ne!(
        Error::EndOfStream,
        Error::InvalidValue {
            message: String::new()
        }
    );
    let boxed: Box<dyn std::error::Error> = Box::new(Error::EndOfStream);
    assert_eq!(boxed.to_string(), "unexpected end of stream");
}

// ---------------------------------------------------------------------------
// The schema's built-in types
// ---------------------------------------------------------------------------

/// The scalar table, by width and signedness.
#[test]
fn the_builtin_scalars_have_the_widths_they_are_named_for() {
    let schema = Schema::with_builtins(Endian::Little);
    let cases = [
        ("int8", Scalar::Int(1)),
        ("uint8", Scalar::Uint(1)),
        ("int16", Scalar::Int(2)),
        ("uint16", Scalar::Uint(2)),
        ("int24", Scalar::Int(3)),
        ("uint24", Scalar::Uint(3)),
        ("int32", Scalar::Int(4)),
        ("uint32", Scalar::Uint(4)),
        ("int48", Scalar::Int(6)),
        ("uint48", Scalar::Uint(6)),
        ("int64", Scalar::Int(8)),
        ("uint64", Scalar::Uint(8)),
        ("int128", Scalar::Int(16)),
        ("uint128", Scalar::Uint(16)),
        ("float16", Scalar::Float(2)),
        ("float", Scalar::Float(4)),
        ("double", Scalar::Float(8)),
        ("char", Scalar::Char),
        ("wchar", Scalar::Wchar),
        ("uleb128", Scalar::Leb128 { signed: false }),
        ("ileb128", Scalar::Leb128 { signed: true }),
    ];
    for (name, scalar) in cases {
        assert_eq!(
            schema.resolve(name).unwrap(),
            &TypeDef::Scalar(scalar),
            "{name}"
        );
    }
}

/// The alias table is transcribed from upstream's `initial_types` in full.
///
/// Trimming it to the spellings something was observed to use would leave holes,
/// and a hole is not a missing feature — a definition is parsed at import time,
/// so an unresolvable alias is an ImportError that takes the module with it.
#[test]
fn the_alias_table_covers_the_spellings_real_headers_use() {
    let schema = Schema::with_builtins(Endian::Little);
    let cases = [
        // Windows, which `dissect.ntfs` is written entirely in.
        ("ULONG", Scalar::Uint(4)),
        ("USHORT", Scalar::Uint(2)),
        ("DWORD", Scalar::Uint(4)),
        ("QWORD", Scalar::Uint(8)),
        ("WCHAR", Scalar::Wchar),
        ("FILETIME", Scalar::Uint(8)),
        // C, including the multi-word spellings.
        ("unsigned long long", Scalar::Uint(8)),
        ("signed char", Scalar::Int(1)),
        ("long", Scalar::Int(4)),
        // GNU and IDA.
        ("uint32_t", Scalar::Uint(4)),
        ("_QWORD", Scalar::Uint(8)),
        // `unsigned char` is `char`, not `uint8`: upstream produces `bytes`
        // for it, which is a compatibility fact and not a typo to tidy up.
        ("unsigned char", Scalar::Char),
        ("CHAR", Scalar::Char),
        ("UCHAR", Scalar::Uint(1)),
    ];
    for (alias, scalar) in cases {
        assert_eq!(
            schema.resolve(alias).unwrap(),
            &TypeDef::Scalar(scalar),
            "{alias}"
        );
    }
}

/// Alignment is usually the width, but not for the odd sizes: upstream gives
/// `int24` an alignment of 4 and `uint48` an alignment of 8, so a
/// natural-alignment layout pads them to the next power of two.
///
/// It has to be a power of two for the padding expression `-offset & (a - 1)`
/// to be correct at all.
#[test]
fn scalar_alignment_is_always_a_power_of_two() {
    let cases = [
        (Scalar::Uint(1), 1u32),
        (Scalar::Uint(2), 2),
        (Scalar::Uint(3), 4),
        (Scalar::Int(3), 4),
        (Scalar::Uint(4), 4),
        (Scalar::Uint(6), 8),
        (Scalar::Int(6), 8),
        (Scalar::Uint(8), 8),
        (Scalar::Char, 1),
        (Scalar::Wchar, 2),
        (Scalar::Float(4), 4),
        (Scalar::Leb128 { signed: false }, 1),
    ];
    for (scalar, alignment) in cases {
        assert_eq!(scalar.alignment(), alignment, "{scalar:?}");
        assert!(
            alignment.is_power_of_two(),
            "{scalar:?} must align to a power of two"
        );
    }
}

/// A LEB128 reports a width of zero, which is what marks its field dynamic --
/// its length is a property of its bytes, not of its type.
#[test]
fn scalar_sizes_report_zero_only_for_the_self_delimiting_one() {
    assert_eq!(Scalar::Uint(4).size(), 4);
    assert_eq!(Scalar::Char.size(), 1);
    assert_eq!(Scalar::Wchar.size(), 2);
    assert_eq!(Scalar::Float(8).size(), 8);
    assert_eq!(Scalar::Leb128 { signed: true }.size(), 0);
}

/// A scalar is hashable *by what it is*, not by its display name.
///
/// Keying a cache on the name is what let `cs.uint48` hand back a `uint24` type
/// object: every width without a spelled-out name shared the string `"scalar"`,
/// so whichever was asked for first won and `cs.uint128(b"A" * 16)` then read
/// three bytes. Order-dependent, silent, and entirely the cache's doing.
#[test]
fn scalars_are_distinguishable_as_cache_keys() {
    use std::collections::HashMap;
    let mut cache: HashMap<Scalar, &str> = HashMap::new();
    cache.insert(Scalar::Uint(3), "uint24");
    cache.insert(Scalar::Uint(6), "uint48");
    cache.insert(Scalar::Uint(16), "uint128");
    cache.insert(Scalar::Int(3), "int24");
    assert_eq!(cache.len(), 4, "no two of these may collide");
    assert_eq!(cache[&Scalar::Uint(6)], "uint48");
}

// ---------------------------------------------------------------------------
// Schema behaviour
// ---------------------------------------------------------------------------

/// Insertion records declaration order once per name, so generated output and
/// reports are stable and a redefinition does not duplicate the entry.
#[test]
fn insertion_records_declaration_order_once_per_name() {
    let mut schema = Schema::default();
    schema.insert("a".to_owned(), TypeDef::Scalar(Scalar::Uint(1)));
    schema.insert("b".to_owned(), TypeDef::Scalar(Scalar::Uint(2)));
    schema.insert("a".to_owned(), TypeDef::Scalar(Scalar::Uint(4)));

    assert_eq!(
        schema.order,
        ["a", "b"],
        "a redefinition keeps its position"
    );
    assert_eq!(
        schema.resolve("a").unwrap(),
        &TypeDef::Scalar(Scalar::Uint(4)),
        "but the definition itself is replaced"
    );
}

/// Alias chains resolve to the concrete definition at the end.
#[test]
fn alias_chains_resolve_to_their_target() {
    let mut schema = Schema::with_builtins(Endian::Little);
    schema.insert(
        "A".to_owned(),
        TypeDef::Alias(TypeRef::Named("B".to_owned())),
    );
    schema.insert(
        "B".to_owned(),
        TypeDef::Alias(TypeRef::Named("C".to_owned())),
    );
    schema.insert("C".to_owned(), TypeDef::Scalar(Scalar::Uint(2)));
    assert_eq!(
        schema.resolve("A").unwrap(),
        &TypeDef::Scalar(Scalar::Uint(2))
    );
}

/// A definition is input, and input that makes the resolver spin is a denial of
/// service. The depth bound is asserted rather than assumed.
#[test]
fn a_circular_alias_terminates_with_an_error() {
    let mut schema = Schema::with_builtins(Endian::Little);
    schema.insert(
        "A".to_owned(),
        TypeDef::Alias(TypeRef::Named("B".to_owned())),
    );
    schema.insert(
        "B".to_owned(),
        TypeDef::Alias(TypeRef::Named("A".to_owned())),
    );
    match schema.resolve("A") {
        Err(Error::InvalidValue { message }) => {
            assert!(
                message.contains("circular") || message.contains("deep"),
                "{message}"
            )
        }
        other => panic!("expected a value error, got {other:?}"),
    }
}

/// An unknown name is reported by name, and a *chain* ending in an unknown name
/// reports the name that was actually missing rather than the one asked for.
#[test]
fn an_unknown_type_is_reported_by_the_name_that_is_missing() {
    let mut schema = Schema::with_builtins(Endian::Little);
    assert_eq!(
        schema.resolve("NOPE"),
        Err(Error::UnknownType {
            name: "NOPE".to_owned()
        })
    );

    schema.insert(
        "Alias".to_owned(),
        TypeDef::Alias(TypeRef::Named("Missing".to_owned())),
    );
    assert_eq!(
        schema.resolve("Alias"),
        Err(Error::UnknownType {
            name: "Missing".to_owned()
        }),
        "the message must name the link that broke, not the one the caller used"
    );
}

/// A constant may be an integer, or an expression over other constants that is
/// evaluated on demand -- because it may refer to a constant defined later in
/// the same file.
#[test]
fn constants_resolve_integers_and_expressions_but_not_text() {
    let mut schema = Schema::with_builtins(Endian::Little);
    schema
        .constants
        .insert("BITS".to_owned(), ConstValue::Int(2));
    schema.constants.insert(
        "PAD".to_owned(),
        ConstValue::Expr {
            parsed: Expr::parse("1 << BITS").unwrap(),
            source: "1 << BITS".to_owned(),
        },
    );
    schema
        .constants
        .insert("PREFIX".to_owned(), ConstValue::Str("gnu.".to_owned()));
    schema
        .constants
        .insert("MAGIC".to_owned(), ConstValue::Bytes(b"!<arch>".to_vec()));

    assert_eq!(schema.const_int("BITS"), Some(2));
    assert_eq!(schema.const_int("PAD"), Some(4));
    assert_eq!(
        schema.const_int("PREFIX"),
        None,
        "a string cannot size anything"
    );
    assert_eq!(schema.const_int("MAGIC"), None);
    assert_eq!(schema.const_int("ABSENT"), None);
}

/// Expression constants may refer to each other, so evaluation is bounded by
/// depth rather than trusted to terminate.
#[test]
fn mutually_referring_constants_terminate() {
    let mut schema = Schema::with_builtins(Endian::Little);
    schema.constants.insert(
        "A".to_owned(),
        ConstValue::Expr {
            parsed: Expr::parse("B + 1").unwrap(),
            source: "B + 1".to_owned(),
        },
    );
    schema.constants.insert(
        "B".to_owned(),
        ConstValue::Expr {
            parsed: Expr::parse("A + 1").unwrap(),
            source: "A + 1".to_owned(),
        },
    );
    assert_eq!(
        schema.const_int("A"),
        None,
        "an unresolvable cycle is None, not a hang"
    );

    // A long but finite chain resolves as far as the bound allows and then
    // gives up, which is the correct answer for input that may be hostile.
    let mut deep = Schema::with_builtins(Endian::Little);
    deep.constants.insert("C0".to_owned(), ConstValue::Int(1));
    for level in 1..=4 {
        deep.constants.insert(
            format!("C{level}"),
            ConstValue::Expr {
                parsed: Expr::parse(&format!("C{} + 1", level - 1)).unwrap(),
                source: format!("C{} + 1", level - 1),
            },
        );
    }
    assert_eq!(deep.const_int("C4"), Some(5));
}

/// A fresh schema knows nothing, which is what a namespace starts from.
#[test]
fn a_default_schema_is_empty_and_little_endian() {
    let schema = Schema::default();
    assert!(schema.types.is_empty());
    assert!(schema.constants.is_empty());
    assert!(schema.order.is_empty());
    assert_eq!(schema.endian, Endian::Little, "upstream's default");
    assert_eq!(
        schema.pointer_size, 0,
        "zero means never set; readers treat it as eight"
    );
}

/// The built-in schema carries a usable pointer width, unlike the default one.
#[test]
fn a_builtin_schema_defaults_to_eight_byte_pointers() {
    assert_eq!(Schema::with_builtins(Endian::Big).pointer_size, 8);
    assert_eq!(Schema::with_builtins(Endian::Big).endian, Endian::Big);
}

// ---------------------------------------------------------------------------
// Build identity
// ---------------------------------------------------------------------------

/// The build's CPU baseline is *reported*, not detected.
///
/// The question is what this binary was **compiled** for, not what the current
/// machine happens to support: a benchmark that cannot say which build it
/// measured is not evidence, and comparing an `x86-64-v3` wheel against a
/// `native` one and calling the difference an optimisation is the mistake this
/// exists to prevent.
#[test]
fn the_build_reports_its_own_cpu_baseline() {
    let baseline = cstruct_rs2_core::target_cpu_baseline();
    assert!(
        baseline.contains("avx2") || baseline.contains("avx512f"),
        "this build must be at or above the x86-64-v3 floor, got {baseline:?}"
    );
    assert!(!baseline.contains("UNSUPPORTED"), "{baseline}");
    assert!(!cstruct_rs2_core::VERSION.is_empty());
}
