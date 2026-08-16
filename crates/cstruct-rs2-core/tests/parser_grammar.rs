//! Every declaration form the definition language accepts, and every way it is
//! refused.
//!
//! The scope rule this grammar follows is worth restating, because it is the
//! The grammar is wide because it has to be. A definition is parsed when the
//! module holding it is imported, so every construct in it must parse even if
//! nothing ever decodes that type. Narrowing the grammar to "what gets decoded"
//! does not reduce risk — it turns unusual declarations into import errors.
//!
//! The tests are organised by declaration form for that reason, and each says
//! what the form is for.
//!
//! # The refusal tests matter as much as the acceptance tests
//!
//! A definition that fails to parse takes its whole module with it, and a module
//! that failed to import produces no output *and no error* — much harder to
//! notice than a wrong value. So each refusal below is checked to be a refusal
//! of something genuinely malformed, rather than of something merely unusual.

use cstruct_rs2_core::error::Error;
use cstruct_rs2_core::parser::{parse, parse_aligned_into, parse_into, parse_into_with_metadata};
use cstruct_rs2_core::schema::{
    ArrayLen, ConstValue, Endian, EnumDef, Scalar, Schema, StructDef, TypeDef, TypeRef,
};

fn schema_of(source: &str) -> Schema {
    parse(source, Endian::Little).unwrap_or_else(|e| panic!("{source}\n\nshould parse: {e}"))
}

fn struct_of(schema: &Schema, name: &str) -> StructDef {
    match schema.resolve(name) {
        Ok(TypeDef::Struct(def)) => def.clone(),
        other => panic!("{name} should be a struct, got {other:?}"),
    }
}

fn enum_of(schema: &Schema, name: &str) -> EnumDef {
    match schema.resolve(name) {
        Ok(TypeDef::Enum(def)) => def.clone(),
        other => panic!("{name} should be an enum, got {other:?}"),
    }
}

fn parse_failure(source: &str) -> Error {
    parse(source, Endian::Little).expect_err(&format!("{source}\n\nshould not parse"))
}

// ---------------------------------------------------------------------------
// Structures
// ---------------------------------------------------------------------------

/// A struct declares its fields in order, and the tag is the type's name.
#[test]
fn a_plain_struct_keeps_its_fields_in_order() {
    let def = struct_of(
        &schema_of("struct entry { uint16 a; uint32 b; uint8 c; };"),
        "entry",
    );
    assert_eq!(
        def.fields
            .iter()
            .map(|f| f.name.as_deref().unwrap())
            .collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
    assert!(!def.is_union);
    assert!(
        !def.align,
        "align is a property of the load, not of the text"
    );
}

/// One declaration may introduce several fields of the same type.
#[test]
fn a_declaration_may_name_several_fields() {
    let def = struct_of(&schema_of("struct s { uint32 a, b, c; };"), "s");
    assert_eq!(def.fields.len(), 3);
    assert!(def
        .fields
        .iter()
        .all(|f| f.ty == TypeRef::Named("uint32".to_owned())));
}

/// `typedef struct _FOO { ... } FOO;` is `dissect.ntfs`'s house style, and both
/// spellings have to reach the same type.
#[test]
fn a_typedef_struct_registers_its_tag_and_its_alias() {
    let schema = schema_of(
        "typedef struct _MFT_SEGMENT_REFERENCE {
             ULONG  SegmentNumberLowPart;
             USHORT SegmentNumberHighPart;
             USHORT SequenceNumber;
         } MFT_SEGMENT_REFERENCE;",
    );
    assert_eq!(
        struct_of(&schema, "_MFT_SEGMENT_REFERENCE"),
        struct_of(&schema, "MFT_SEGMENT_REFERENCE")
    );
}

/// `typedef struct X { ... } X;` -- the alias repeating the tag.
///
/// C allows it and `dissect.ntfs` writes it for its reparse buffers. Inserting
/// the alias anyway would replace the struct with an alias pointing at itself,
/// and the type would then not resolve at all — reported as "X is not defined
/// in this cstruct namespace" for a type that is plainly there.
#[test]
fn an_alias_that_repeats_its_own_tag_does_not_erase_the_type() {
    let schema = schema_of("typedef struct X { uint32 a; } X;");
    assert_eq!(struct_of(&schema, "X").fields.len(), 1);
}

/// `typedef struct _X { ... } X, *PX;` -- several names, one of them a pointer.
///
/// `dissect.hypervisor`'s VDI headers are written this way. The pointer alias
/// names a pointer-sized integer, not the structure.
#[test]
fn a_type_body_may_be_followed_by_several_names_including_a_pointer() {
    let schema = schema_of("typedef struct _VDI { uint32 a; } VDI, *PVDI;");
    assert_eq!(struct_of(&schema, "VDI").fields.len(), 1);
    assert_eq!(
        schema.resolve("PVDI").unwrap(),
        &TypeDef::Scalar(Scalar::Uint(8)),
        "a pointer alias is a pointer-sized integer, not the struct"
    );
}

/// Names after a non-typedef body are C variables, not new cstruct types.
/// Current upstream ignores them after parsing the body.
#[test]
fn plain_top_level_variable_declarators_are_not_registered_as_types() {
    let schema = schema_of(
        "struct named { uint8 value; } first, *second;\n\
         struct { uint8 value; } a, b, **c;",
    );
    for name in ["first", "second", "a", "b", "c"] {
        assert!(!schema.types.contains_key(name), "{name}");
    }
}

/// Released Dissect definitions also use one anonymous body and one plain name
/// as a type declaration. That unambiguous legacy form remains supported.
#[test]
fn one_anonymous_legacy_name_remains_a_type_alias() {
    let schema = schema_of("struct { uint32 seconds; } timeval;");
    assert_eq!(struct_of(&schema, "timeval").fields.len(), 1);
}

/// `typedef struct prange prange_t;` -- an alias for a struct declared
/// elsewhere, with no body of its own. `dissect.apfs` declares every one of its
/// types this way.
#[test]
fn a_typedef_may_reference_a_struct_declared_earlier() {
    let schema = schema_of(
        "struct prange { uint64 start; uint64 count; };
         typedef struct prange prange_t;",
    );
    assert_eq!(struct_of(&schema, "prange_t").name, "prange");
}

/// `typedef char uuid_t[16];` -- the subscript belongs to the *alias*.
///
/// `uuid_t` names "sixteen chars", so the schema records an alias for an array
/// rather than an alias for `char`. Reading it the other way round would make
/// `uuid_t id;` one byte.
#[test]
fn a_typedef_may_name_an_array() {
    let schema = schema_of("typedef char uuid_t[16];");
    let Some(TypeDef::Alias(TypeRef::Array(element, len))) = schema.types.get("uuid_t") else {
        panic!(
            "uuid_t should alias an array, got {:?}",
            schema.types.get("uuid_t")
        );
    };
    assert_eq!(**element, TypeRef::Named("char".to_owned()));
    assert_eq!(*len, ArrayLen::Fixed(16));
}

/// Dimensions on an alias fold outside-in, exactly as they do on a field.
#[test]
fn a_typedef_array_may_have_several_dimensions() {
    let schema = schema_of("typedef uint16 grid_t[2][3];");
    let Some(TypeDef::Alias(outer)) = schema.types.get("grid_t") else {
        panic!("grid_t should be an alias");
    };
    // Outermost first: `[2]` of (`[3]` of uint16).
    let TypeRef::Array(inner, outer_len) = outer else {
        panic!("expected an array");
    };
    assert_eq!(*outer_len, ArrayLen::Fixed(2));
    let TypeRef::Array(base, inner_len) = inner.as_ref() else {
        panic!("expected a nested array");
    };
    assert_eq!(*inner_len, ArrayLen::Fixed(3));
    assert_eq!(**base, TypeRef::Named("uint16".to_owned()));
}

/// `dissect.eventlog` closes a struct with `}` and no `;`, then starts the next
/// declaration. Upstream accepts it, and requiring the semicolon made the whole
/// `wevt_object` definition unparseable -- which made the `evtx` plugin
/// unavailable, and an unavailable plugin produces no records *and no error*.
#[test]
fn a_missing_trailing_semicolon_is_accepted() {
    let schema =
        schema_of("struct PRVA_HEADER { uint32 data_offset; }\n\nstruct PRVA { uint32 unknown; };");
    assert!(schema.resolve("PRVA_HEADER").is_ok());
    assert!(schema.resolve("PRVA").is_ok());
}

/// Stray semicolons between declarations are common and harmless.
#[test]
fn stray_semicolons_between_declarations_are_ignored() {
    let schema = schema_of(";;struct a { uint8 x; };;;struct b { uint8 y; };;");
    assert!(schema.resolve("a").is_ok());
    assert!(schema.resolve("b").is_ok());
}

// ---------------------------------------------------------------------------
// Unions and nesting
// ---------------------------------------------------------------------------

/// A union is a struct that overlaps, and the parser records only that -- the
/// overlapping itself is the plan compiler's job.
#[test]
fn a_union_is_marked_as_one() {
    let def = struct_of(&schema_of("union u { uint32 a; uint16 b; };"), "u");
    assert!(def.is_union);
    assert_eq!(def.fields.len(), 2);
}

/// An anonymous member has no name; the compiler folds its fields into the
/// parent later. `_INDEX_ENTRY` is 18.16% of all decodes in an NTFS `walkfs`
/// and this is its shape.
#[test]
fn anonymous_members_are_unnamed_fields_of_a_generated_type() {
    let schema = schema_of(
        "typedef struct _INDEX_ENTRY {
             union {
                 uint64 FileReference;
                 struct { uint16 DataOffset; uint16 DataLength; uint32 _; };
             };
             uint16 Length;
         } INDEX_ENTRY;",
    );
    let def = struct_of(&schema, "INDEX_ENTRY");
    assert_eq!(def.fields.len(), 2, "the union is one member, then Length");
    assert!(
        def.fields[0].name.is_none(),
        "the union member is anonymous"
    );
    // The generated type has to be registered under *some* name so the parent
    // can refer to it. Upstream's scheme is reproduced rather than invented,
    // because those generated names show up in reprs and error messages.
    let TypeRef::Named(generated) = &def.fields[0].ty else {
        panic!("the anonymous member should name a generated type");
    };
    assert!(generated.starts_with("__anonymous_"), "got {generated}");
    assert!(schema.resolve(generated).is_ok());
}

/// A nested type may be named, in which case it is an ordinary type that also
/// happens to be declared inside another.
#[test]
fn a_named_nested_struct_is_registered_and_referenced() {
    let schema = schema_of("struct outer { struct inner { uint8 a; } m; uint8 b; };");
    assert_eq!(struct_of(&schema, "inner").fields.len(), 1);
    let outer = struct_of(&schema, "outer");
    assert_eq!(outer.fields[0].name.as_deref(), Some("m"));
    assert_eq!(outer.fields[0].ty, TypeRef::Named("inner".to_owned()));
}

/// `struct exit_status ut_exit;` -- an elaborated type specifier, not a
/// definition. C allows naming an already-declared type with its tag keyword,
/// and `dissect.target`'s `utmp` and `lastlog` definitions both do.
#[test]
fn an_elaborated_specifier_is_a_field_not_a_definition() {
    let schema = schema_of(
        "struct exit_status { uint16 e_termination; uint16 e_exit; };
         union payload { uint32 raw; };
         struct entry {
             struct exit_status ut_exit;
             union payload body;
             struct exit_status history[4];
         };",
    );
    let def = struct_of(&schema, "entry");
    assert_eq!(def.fields[0].ty, TypeRef::Named("exit_status".to_owned()));
    assert_eq!(def.fields[1].ty, TypeRef::Named("payload".to_owned()));
    assert!(
        matches!(&def.fields[2].ty, TypeRef::Array(inner, ArrayLen::Fixed(4))
            if **inner == TypeRef::Named("exit_status".to_owned())),
        "an elaborated specifier may still be an array, got {:?}",
        def.fields[2].ty
    );
}

// ---------------------------------------------------------------------------
// Arrays
// ---------------------------------------------------------------------------

/// The four array length forms, which between them are every way a definition
/// can say how long something is.
#[test]
fn every_array_length_form_is_recognised() {
    let def = struct_of(
        &schema_of(
            "struct s {
                 char   fixed[8];
                 wchar  named[NameLength];
                 uint8  terminated[];
                 char   rest[EOF];
             };",
        ),
        "s",
    );
    assert!(matches!(
        &def.fields[0].ty,
        TypeRef::Array(_, ArrayLen::Fixed(8))
    ));
    assert!(
        matches!(&def.fields[1].ty, TypeRef::Array(_, ArrayLen::Expr(text)) if text == "NameLength")
    );
    assert!(matches!(
        &def.fields[2].ty,
        TypeRef::Array(_, ArrayLen::NullTerminated)
    ));
    // `[EOF]` is a distinct sentinel rather than a name a consumer defines.
    // Treating it as an unknown identifier made `dissect.database`'s NTDS key
    // material fail, which took the whole Active Directory surface with it.
    assert!(matches!(
        &def.fields[3].ty,
        TypeRef::Array(_, ArrayLen::ToEnd)
    ));
}

/// An expression length is kept as text and resolved later, when the values it
/// names exist.
#[test]
fn an_expression_length_is_carried_verbatim() {
    let def = struct_of(
        &schema_of("struct s { uint8 n; char d[(n + 1) * 2]; };"),
        "s",
    );
    let TypeRef::Array(_, ArrayLen::Expr(text)) = &def.fields[1].ty else {
        panic!("expected an expression length, got {:?}", def.fields[1].ty);
    };
    assert_eq!(text, "(n+1)*2", "whitespace is dropped, structure is not");
}

/// A length may use the two call forms the language has.
///
/// `offsetof` needs a comma, and a comma inside `[...]` can only ever be that
/// separator -- cstruct has no comma operator. Without an arm for it the form
/// was a parse error, so `sizeof` worked in a definition and its twin did not,
/// even though the evaluator resolved both. What the lengths evaluate to is
/// asserted in `plan_compilation.rs`.
#[test]
fn an_array_length_may_call_sizeof_or_offsetof() {
    let def = struct_of(
        &schema_of(
            "struct h { uint32 a; uint16 b; };
             struct s { char sized[sizeof(h)]; char at[offsetof(h, b)]; };",
        ),
        "s",
    );
    let lengths: Vec<&str> = def
        .fields
        .iter()
        .map(|field| match &field.ty {
            TypeRef::Array(_, ArrayLen::Expr(text)) => text.as_str(),
            other => panic!("expected an expression length, got {other:?}"),
        })
        .collect();
    assert_eq!(lengths, ["sizeof(h)", "offsetof(h,b)"]);
}

/// `CHAR Hash[29][16]` -- `dissect.database`'s NTDS definition. The dimensions
/// apply outside-in, so the *first* subscript is the outer array.
#[test]
fn multi_dimensional_arrays_nest_outermost_first() {
    let def = struct_of(&schema_of("struct s { char Hash[29][16]; };"), "s");
    let TypeRef::Array(inner, ArrayLen::Fixed(29)) = &def.fields[0].ty else {
        panic!(
            "the outer dimension should be 29, got {:?}",
            def.fields[0].ty
        );
    };
    assert!(
        matches!(inner.as_ref(), TypeRef::Array(_, ArrayLen::Fixed(16))),
        "the inner dimension should be 16, got {inner:?}"
    );
}

// ---------------------------------------------------------------------------
// Bitfields
// ---------------------------------------------------------------------------

#[test]
fn bitfields_carry_their_declared_widths() {
    let def = struct_of(
        &schema_of("struct s { uint32 a : 4; uint32 b : 12; uint32 c; };"),
        "s",
    );
    assert_eq!(def.fields[0].bits, Some(4));
    assert_eq!(def.fields[1].bits, Some(12));
    assert_eq!(
        def.fields[2].bits, None,
        "an ordinary field is not a bitfield"
    );
}

/// A width of zero is C's "align to the next unit" idiom, and a width past 64
/// has no storage unit to sit in. Upstream handles neither, and guessing would
/// silently shift every later field -- which is a wrong record rather than an
/// error.
#[test]
fn a_zero_or_oversized_bit_width_is_refused() {
    for source in [
        "struct s { uint32 a : 0; };",
        "struct s { uint32 a : 65; };",
    ] {
        assert!(
            matches!(parse_failure(source), Error::InvalidValue { .. }),
            "{source} should be refused as an out-of-range value"
        );
    }
}

/// A width that is not a literal at all is a *parse* error rather than a
/// value one, and the difference is deliberate.
///
/// `: -1` never reaches the range check: the lexer hands the parser a `-`
/// followed by a `1`, so the failure is "there is no width here" and not "the
/// width is wrong". Both refuse, and they refuse for the reasons they say.
#[test]
fn a_bitfield_without_a_literal_width_is_a_parse_error() {
    for source in ["struct s { uint32 a : ; };", "struct s { uint32 a : -1; };"] {
        match parse_failure(source) {
            Error::Parse { message, .. } => {
                assert!(message.contains("bit width"), "{source}: {message}")
            }
            other => panic!("{source} should be a parse error, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Pointers
// ---------------------------------------------------------------------------

/// A pointer member decodes as a pointer-sized unsigned integer, and the type
/// it *addresses* is recorded separately.
///
/// Both halves are part of upstream's pointer value. The integer supplies the
/// address; the target type determines what `Pointer.dereference()` reads at
/// that address.
#[test]
fn a_pointer_member_records_both_its_width_and_its_target() {
    let def = struct_of(&schema_of("struct s { uint32 *p; char **argv; };"), "s");
    assert_eq!(def.fields[0].ty, TypeRef::Named("uint64".to_owned()));
    assert_eq!(def.fields[0].points_to.as_deref(), Some("uint32"));
    // Indirection depth is kept rather than collapsed: `char **argv` addresses
    // a `char*`, and dereferencing twice is exactly what a caller does with it.
    assert_eq!(def.fields[1].points_to.as_deref(), Some("char*"));
}

/// A pointer typedef keeps its target for the public class while layout still
/// resolves it to the namespace's pointer-sized integer.
#[test]
fn pointer_typedefs_resolve_to_the_pointer_integer() {
    let schema = schema_of("typedef uint32 *PULONG;\ntypedef void *PVOID;");
    for name in ["PULONG", "PVOID"] {
        assert_eq!(
            schema.resolve(name).unwrap(),
            &TypeDef::Scalar(Scalar::Uint(8)),
            "{name}"
        );
    }
    assert_eq!(schema.pointer_aliases.get("PULONG").unwrap(), "uint32");
    assert_eq!(schema.pointer_aliases.get("PVOID").unwrap(), "void");
}

/// The pointer width belongs to the namespace, not to the definition.
///
/// Eight bytes is current upstream's default, but `cstruct(pointer="uint16")`
/// selects two-byte pointers. Treating the default as part of the grammar would
/// also move every field that follows a narrow pointer.
#[test]
fn the_pointer_width_comes_from_the_schema() {
    let mut schema = Schema::with_builtins(Endian::Little);
    schema.pointer_size = 2;
    parse_into("struct s { uint32 *p; };", &mut schema).unwrap();
    assert_eq!(
        struct_of(&schema, "s").fields[0].ty,
        TypeRef::Named("uint16".to_owned())
    );

    // Zero is not a legal width; it means "never set" and reads as eight.
    let mut unset = Schema::with_builtins(Endian::Little);
    unset.pointer_size = 0;
    parse_into("struct s { uint32 *p; };", &mut unset).unwrap();
    assert_eq!(
        struct_of(&unset, "s").fields[0].ty,
        TypeRef::Named("uint64".to_owned())
    );
}

/// The elaborated form of a pointer member records its target too.
#[test]
fn an_elaborated_pointer_member_records_its_target() {
    let schema = schema_of("struct inner { uint8 a; }; struct s { struct inner *p; };");
    let def = struct_of(&schema, "s");
    assert_eq!(def.fields[0].points_to.as_deref(), Some("inner"));
    assert_eq!(def.fields[0].ty, TypeRef::Named("uint64".to_owned()));
}

// ---------------------------------------------------------------------------
// Enums and flags
// ---------------------------------------------------------------------------

/// A flag carries its base type and its members, and is marked as a flag so it
/// becomes an `IntFlag` rather than an `IntEnum`.
#[test]
fn a_flag_keeps_its_base_type_and_members() {
    let def = enum_of(
        &schema_of("flag FILE_ATTRIBUTE : DWORD { READONLY = 0x1, HIDDEN = 0x2, };"),
        "FILE_ATTRIBUTE",
    );
    assert!(def.is_flag);
    assert_eq!(
        def.base, "DWORD",
        "the base is kept as written, not resolved"
    );
    assert_eq!(
        def.members,
        vec![("READONLY".to_owned(), 1), ("HIDDEN".to_owned(), 2)]
    );
}

/// C enums continue from the previous value when one is omitted.
#[test]
fn enum_members_auto_increment() {
    let def = enum_of(&schema_of("enum E : uint8 { A, B, C = 7, D };"), "E");
    assert_eq!(
        def.members,
        vec![
            ("A".to_owned(), 0),
            ("B".to_owned(), 1),
            ("C".to_owned(), 7),
            ("D".to_owned(), 8),
        ]
    );
}

/// Flags choose the next free bit rather than the next integer.
#[test]
fn flag_members_advance_to_the_next_power_of_two() {
    let def = enum_of(
        &schema_of("flag F : uint16 { A, B, C, D = 32, E, F };"),
        "F",
    );
    assert_eq!(
        def.members,
        vec![
            ("A".to_owned(), 1),
            ("B".to_owned(), 2),
            ("C".to_owned(), 4),
            ("D".to_owned(), 32),
            ("E".to_owned(), 64),
            ("F".to_owned(), 128),
        ]
    );
}

/// Anonymous members are integer expression names while retaining their
/// hidden owner so the Python binding can publish typed enum values.
#[test]
fn anonymous_enum_members_are_expression_constants_with_an_owner() {
    let schema =
        schema_of("enum : uint32 { nElements = 4 }; struct S { uint32 values[nElements]; };");
    assert_eq!(schema.const_int("nElements"), Some(4));
    let owner = schema
        .anonymous_enum_members
        .get("nElements")
        .expect("anonymous member owner");
    assert!(owner.starts_with("__anonymous_"));
    let TypeDef::Enum(definition) = schema.resolve(owner).expect("anonymous enum") else {
        panic!("anonymous owner should resolve to an enum");
    };
    assert!(definition.name.is_empty());
}

/// A base type is optional; without one an enum is 32 bits, as in C.
#[test]
fn an_enum_without_a_base_defaults_to_uint32() {
    assert_eq!(enum_of(&schema_of("enum E { A = 1 };"), "E").base, "uint32");
}

/// An anonymous enum gets a generated name so it can still be registered.
#[test]
fn an_anonymous_enum_is_given_a_generated_name() {
    let schema = schema_of("enum : uint8 { A = 1 };");
    assert!(
        schema
            .order
            .iter()
            .any(|name| name.starts_with("__anonymous_")),
        "an anonymous enum should be registered under a generated name"
    );
}

/// `LEAF_NODE = 1 << 1` in `dissect.volume`'s device-mapper definition.
///
/// Flag enums are normally written as shifts, so a parser that accepts only
/// literals here cannot read them — and since the definition is parsed at import
/// time, that costs the whole module rather than one type.
#[test]
fn an_enum_member_value_may_be_an_expression() {
    let def = enum_of(
        &schema_of(
            "#define BASE 4
             enum E : uint32 { A = 1 << 1, B = A + 1, C = (BASE * 2) };",
        ),
        "E",
    );
    assert_eq!(
        def.members,
        vec![
            ("A".to_owned(), 2),
            ("B".to_owned(), 3),
            ("C".to_owned(), 8),
        ],
        "earlier members and schema constants are both in scope"
    );
}

/// Members separated by newlines rather than commas, which upstream tolerates
/// and `dissect.target`'s trusteddocs relies on.
///
/// The rule is that an identifier followed by `=` starts a new member and
/// cannot be part of the previous one's value; identifiers *inside* an
/// expression are never followed by `=`.
#[test]
fn enum_members_may_be_separated_by_nothing_at_all() {
    let def = enum_of(
        &schema_of("enum E : uint32 { EDITING_ENABLED = 1\n MACROS_ENABLED = 2\n };"),
        "E",
    );
    assert_eq!(
        def.members,
        vec![
            ("EDITING_ENABLED".to_owned(), 1),
            ("MACROS_ENABLED".to_owned(), 2),
        ]
    );
}

/// An enum member may be spelled as a bare number.
///
/// C forbids it, cstruct does not, and `dissect.executable`'s PE virtual-key
/// enum declares members `0` through `9`. Refusing loses the whole PE
/// definition.
#[test]
fn an_enum_member_may_be_a_bare_number() {
    let def = enum_of(&schema_of("enum VK : uint8 { 0 = 48, 1 = 49 };"), "VK");
    assert_eq!(
        def.members,
        vec![("0".to_owned(), 48), ("1".to_owned(), 49)]
    );
}

/// An enum takes the same trailing-alias tail as a struct.
#[test]
fn an_enum_may_be_followed_by_aliases() {
    let schema = schema_of("enum E : uint8 { A = 1 } EAlias, EOther;");
    for name in ["E", "EAlias", "EOther"] {
        assert!(
            matches!(schema.resolve(name), Ok(TypeDef::Enum(_))),
            "{name} should resolve to the enum"
        );
    }
}

/// `typedef enum` and `typedef flag` reach the same handling as the bare forms.
#[test]
fn typedef_enum_and_flag_are_accepted() {
    let schema = schema_of("typedef enum E : uint8 { A = 1 }; typedef flag F : uint8 { B = 2 };");
    assert!(!enum_of(&schema, "E").is_flag);
    assert!(enum_of(&schema, "F").is_flag);
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// All four kinds of `#define` value.
///
/// Integers are the interesting case because they can size an array. Strings
/// and byte strings are carried because upstream exposes them as module
/// constants; they simply cannot size anything.
#[test]
fn defines_carry_every_value_kind() {
    let schema = schema_of(
        "#define MAX_PATH 260
         #define XATTR_HURD_PREFIX \"gnu.\"
         #define ARCHIVE_START b\"!<arch>\"
         #define EXT4_XATTR_PAD_BITS 2
         #define EXT4_XATTR_PAD (1<<EXT4_XATTR_PAD_BITS)
         struct s { uint8 p[MAX_PATH]; };",
    );
    assert_eq!(
        schema.constants.get("MAX_PATH"),
        Some(&ConstValue::Int(260))
    );
    assert_eq!(
        schema.constants.get("XATTR_HURD_PREFIX"),
        Some(&ConstValue::Str("gnu.".to_owned()))
    );
    assert_eq!(
        schema.constants.get("ARCHIVE_START"),
        Some(&ConstValue::Bytes(b"!<arch>".to_vec()))
    );
    assert!(matches!(
        schema.constants.get("EXT4_XATTR_PAD"),
        Some(ConstValue::Expr { .. })
    ));
    // An expression constant is resolved on demand, so it can name a constant
    // defined anywhere in the file.
    assert_eq!(schema.const_int("EXT4_XATTR_PAD"), Some(4));
}

#[test]
fn an_unresolved_define_keeps_canonical_expression_text() {
    let schema = schema_of("#define A B + 1");

    assert!(matches!(
        schema.constants.get("A"),
        Some(ConstValue::Expr { source, .. }) if source == "B + 1"
    ));
    assert_eq!(schema.cdef(), "#define A 'B + 1'");
}

/// `#define NAME = value` is accepted as well as `#define NAME value`.
///
/// Upstream takes both. Without this the `=` was swept into the value, so
/// `#define B = 6` produced the *string* `"=6"` instead of the integer 6 --
/// silently, with no error, and a constant declared that way could no longer
/// size an array.
#[test]
fn a_define_may_be_written_with_an_equals_sign() {
    let schema = schema_of("#define B = 6\nstruct s { uint8 d[B]; };");
    assert_eq!(schema.const_int("B"), Some(6));
}

/// The macro name is removed only after the ``define`` keyword. A one-letter
/// name such as ``d`` also occurs inside that keyword and must not split it.
#[test]
fn a_short_define_name_does_not_match_the_directive_word() {
    let schema = schema_of("#define d 1 << 1");
    assert_eq!(schema.const_int("d"), Some(2));
}

/// A value that is neither an integer, a quoted string nor an expression is
/// carried as raw text rather than refused.
///
/// `dissect.evidence` writes `#define ADSEGMENTEDFILE_MAGIC ADSEGMENTEDFILE\00`.
/// Current upstream preserves this opaque token sequence. A constant no decode
/// path reads must not make the surrounding definition unloadable.
#[test]
fn an_unparseable_define_value_never_breaks_the_load() {
    let schema =
        schema_of("#define ADSEGMENTEDFILE_MAGIC ADSEGMENTEDFILE\\00\nstruct s { uint8 a; };");
    assert!(schema.constants.contains_key("ADSEGMENTEDFILE_MAGIC"));
    assert!(
        schema.resolve("s").is_ok(),
        "the rest of the file must still load"
    );
}

/// Empty defines are useful as include guards and have an empty string value,
/// matching current upstream.
#[test]
fn a_define_with_no_value_marks_the_name_as_defined() {
    let schema = schema_of("#define GUARD\n");
    assert_eq!(
        schema.constants.get("GUARD"),
        Some(&ConstValue::Str(String::new()))
    );
}

/// Conditional directives select declarations before the ordinary parser sees
/// them. Nested false branches must remain false even when their own condition
/// happens to be true.
#[test]
fn nested_conditionals_keep_only_the_active_branch() {
    let schema = schema_of(
        "#define OUTER\n\
         #ifdef OUTER\n\
           #ifndef INNER\n\
             #define RESULT 1\n\
           #else\n\
             #define RESULT 2\n\
           #endif\n\
         #else\n\
           #ifdef OUTER\n\
             #define RESULT 3\n\
           #endif\n\
         #endif\n\
         struct selected { uint8 value[RESULT]; };",
    );
    assert_eq!(schema.const_int("RESULT"), Some(1));
    assert_eq!(
        struct_of(&schema, "selected").fields[0].ty,
        TypeRef::Array(
            Box::new(TypeRef::Named("uint8".to_owned())),
            ArrayLen::Expr("RESULT".to_owned())
        )
    );
}

/// Directives inside a structure body use the same constant namespace as
/// top-level directives. Removing a name must affect later conditionals.
#[test]
fn preprocessor_directives_inside_a_body_share_namespace_state() {
    let schema = schema_of(
        "struct packet {\n\
           #define VERSION 2\n\
           #ifdef VERSION\n\
             uint16 version;\n\
           #endif\n\
           #undef VERSION\n\
           #ifdef VERSION\n\
             uint32 absent;\n\
           #endif\n\
           uint8 tail;\n\
         };",
    );
    let packet = struct_of(&schema, "packet");
    assert_eq!(
        packet
            .fields
            .iter()
            .filter_map(|field| field.name.as_deref())
            .collect::<Vec<_>>(),
        ["version", "tail"]
    );
    assert!(!schema.constants.contains_key("VERSION"));
}

/// Includes are recorded for callers but are not read from the filesystem.
/// Parser flags are returned by the same native parse that mutates the schema.
#[test]
fn includes_and_parser_flags_are_returned_without_a_second_parse() {
    let mut schema = Schema::with_builtins(Endian::Little);
    let metadata = parse_into_with_metadata(
        "#include <stdint.h>\n#include \"local.h\"\n#[packed, nocompile]",
        &mut schema,
    )
    .unwrap();
    assert_eq!(schema.includes, ["<stdint.h>", "local.h"]);
    assert_eq!(metadata.flags, ["packed", "nocompile"]);
}

/// A continued macro is one expression, as in current upstream's lexer.
#[test]
fn multiline_defines_are_joined_before_tokenization() {
    let schema = schema_of(concat!(
        "#define TOTAL (1 + \\\n",
        "        2 + \\\n",
        "        3)\n",
    ));
    assert_eq!(schema.const_int("TOTAL"), Some(6));
}

/// Conditional syntax errors point at the directive that caused them and keep
/// the compact source context used by upstream's parser.
#[test]
fn conditional_errors_keep_the_directive_line_and_source_context() {
    match parse_failure("#define A 1\nstruct s { uint8 a; };\n#endif") {
        Error::Parse { line, message } => {
            assert_eq!(line, 3);
            assert!(message.starts_with("#endif without matching #ifdef/#ifndef"));
            assert!(message.contains("  2: struct s { uint8 a; };\n  3: #endif"));
        }
        other => panic!("expected a parse error, got {other:?}"),
    }

    match parse_failure("#ifndef GUARD\n#define GUARD\nstruct s { uint8 a; };") {
        Error::Parse { line, message } => {
            assert_eq!(line, 1);
            assert!(message.starts_with("unclosed conditional statement"));
            assert!(message.contains("  1: #ifndef GUARD"));
        }
        other => panic!("expected a parse error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Type specifiers
// ---------------------------------------------------------------------------

/// `typedef unsigned long long time_t;` -- every word is an identifier, and
/// only the type table says where the type ends and the name begins.
#[test]
fn multi_word_type_specifiers_resolve() {
    let schema = schema_of(
        "typedef unsigned long long u64_t;
         typedef long long s64_t;
         struct s { unsigned long a; long b; unsigned int c; };",
    );
    assert_eq!(
        schema.resolve("u64_t").unwrap(),
        &TypeDef::Scalar(Scalar::Uint(8))
    );
    assert_eq!(
        schema.resolve("s64_t").unwrap(),
        &TypeDef::Scalar(Scalar::Int(8))
    );
    let def = struct_of(&schema, "s");
    assert_eq!(def.fields[0].ty, TypeRef::Named("unsigned long".to_owned()));
    assert_eq!(def.fields[1].ty, TypeRef::Named("long".to_owned()));
    assert_eq!(def.fields[2].ty, TypeRef::Named("unsigned int".to_owned()));
}

/// `unsigned char` is `char`, not `uint8`: upstream produces `bytes` for it.
///
/// A tidier mapping would silently change a value's Python type, which is
/// precisely the kind of difference the compatibility contract exists to
/// prevent.
#[test]
fn unsigned_char_stays_bytes_like_upstream() {
    let schema = schema_of("struct s { unsigned char a; };");
    assert_eq!(
        schema.resolve("unsigned char").unwrap(),
        &TypeDef::Scalar(Scalar::Char)
    );
}

/// A plain `typedef` of an existing type.
#[test]
fn a_typedef_alias_resolves_to_its_target() {
    let schema = schema_of("typedef uint32 MYINT; typedef MYINT MYINT2;");
    assert_eq!(
        schema.resolve("MYINT2").unwrap(),
        &TypeDef::Scalar(Scalar::Uint(4))
    );
}

// ---------------------------------------------------------------------------
// Byte order, alignment, and accumulation
// ---------------------------------------------------------------------------

/// The byte order is a property of the schema the definition is parsed into.
#[test]
fn the_schema_records_its_byte_order() {
    assert_eq!(schema_of("struct s { uint32 a; };").endian, Endian::Little);
    assert_eq!(
        parse("struct s { uint32 a; };", Endian::Big)
            .unwrap()
            .endian,
        Endian::Big
    );
}

/// Alignment is a property of the *declaration*, not of the namespace.
///
/// Upstream stamps it on the types a given `load()` creates, and
/// `dissect.target`'s atop reader loads one definition unaligned and the next
/// aligned into the same namespace -- so the two must be able to coexist.
#[test]
fn alignment_is_recorded_per_declaration_not_per_schema() {
    let mut schema = Schema::with_builtins(Endian::Little);
    parse_into("struct loose { uint8 a; uint32 b; };", &mut schema).unwrap();
    parse_aligned_into("struct tight { uint8 a; uint32 b; };", &mut schema).unwrap();

    assert!(!struct_of(&schema, "loose").align);
    assert!(struct_of(&schema, "tight").align);
}

/// Consumers call `cs.load()` more than once and later definitions refer to
/// types from earlier ones, so accumulating into a schema is the normal case.
#[test]
fn definitions_accumulate_into_one_schema() {
    let mut schema = Schema::with_builtins(Endian::Little);
    parse_into("struct first { uint32 a; };", &mut schema).unwrap();
    parse_into("struct second { first nested; uint8 b; };", &mut schema).unwrap();
    assert_eq!(
        struct_of(&schema, "second").fields[0].ty,
        TypeRef::Named("first".to_owned())
    );
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// Every malformed shape, with the phrase that tells the reader what went
/// wrong. A parse error is only useful if it names the mistake.
#[test]
fn malformed_definitions_are_refused_with_a_reason() {
    let cases = [
        ("uint32 stray;", "unexpected token 'uint32'"),
        ("struct s { uint32 a;", "unterminated type body"),
        ("struct s uint32 a;", "expected"),
        ("enum E : uint8 { A", "unterminated enum body"),
        ("enum E : uint8 { A = };", "empty value"),
        ("struct s { uint32 a[3", "unterminated array length"),
        ("struct s { char a[\"eight\"]; };", "string literal"),
        ("struct s { uint32 a; } 5;", "after type body"),
        ("struct s { uint32 a }", "expected ',' or ';'"),
        ("enum E : uint8 { A = 1 } 5;", "after enum body"),
        // Two names after a type read as a *multi-word type* with no field
        // name -- which is how `unsigned long long x;` has to be read -- so
        // the complaint is the missing name, not the extra word.
        ("struct s { uint32 a b; };", "expected an identifier"),
    ];
    for (source, phrase) in cases {
        match parse_failure(source) {
            Error::Parse { message, .. } => assert!(
                message.contains(phrase),
                "{source:?} should mention {phrase:?}, got {message:?}"
            ),
            other => panic!("{source:?} should be a parse error, got {other:?}"),
        }
    }
}

/// A parse error names the line it was detected on, because a schema is written
/// by a human and the message is read by one.
#[test]
fn parse_errors_name_a_line() {
    match parse_failure("struct s {\n  uint32 a\n") {
        Error::Parse { line, .. } => assert!(line >= 2, "the error should point past line 1"),
        other => panic!("expected a parse error, got {other:?}"),
    }
}

/// A literal array length that does not fit a `u32` is refused at parse time
/// rather than becoming a wrapped-around count.
#[test]
fn an_out_of_range_literal_array_length_is_refused() {
    assert!(matches!(
        parse_failure("struct s { uint8 d[99999999999]; };"),
        Error::InvalidValue { .. }
    ));
}

/// A *negative* length is not a parse error, and that is upstream's semantics
/// rather than an oversight.
///
/// `-1` is not a literal to the lexer -- it is a `-` and a `1` -- so it takes
/// the expression path and is evaluated when a record is read. Upstream builds
/// arrays with `range(count)`, and `range(-1)` is empty, so a negative length
/// is an empty array. `dissect.extfs` depends on this:
/// `char i_extra[256 - 128 - i_extra_isize]` goes negative on every ext3 image.
/// The decode-side half of this rule is asserted in `decode_layout.rs`.
#[test]
fn a_negative_array_length_is_an_expression_not_a_parse_error() {
    let def = struct_of(&schema_of("struct s { uint8 d[-1]; };"), "s");
    assert!(
        matches!(&def.fields[0].ty, TypeRef::Array(_, ArrayLen::Expr(text)) if text == "-1"),
        "got {:?}",
        def.fields[0].ty
    );
}

/// Refusing a *definition* must never depend on whether anything decodes it.
///
/// None of the constructs below needs to *decode* for a definition to be
/// usable, and every one still has to *parse* — because a definition is parsed
/// when its module is imported, not when a record is read.
#[test]
fn constructs_that_nothing_decodes_still_have_to_parse() {
    let never_decoded = "
        #define UNUSED_CONSTANT (1 << 20)
        typedef unsigned long long unused_t;
        struct unused { uint32 flags : 3; uint32 rest : 29; void *opaque; };
        flag UnusedFlags : uint16 { A = 1, B = 2 };
    ";
    let schema = schema_of(never_decoded);
    assert!(schema.resolve("unused").is_ok());
    assert!(schema.resolve("unused_t").is_ok());
    assert!(schema.resolve("UnusedFlags").is_ok());
}
