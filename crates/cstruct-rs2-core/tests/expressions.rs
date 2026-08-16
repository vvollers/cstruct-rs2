//! The integer expression grammar: what it computes, and how it fails.
//!
//! Expressions decide **array lengths**, which makes them a correctness surface
//! rather than a convenience. A length that evaluates to the wrong number does
//! not raise; it produces a record whose every later field is read from the
//! wrong offset. So the tests here are as interested in the error paths as in
//! the arithmetic: an expression that cannot be resolved must *fail*, never
//! quietly become zero.
//!
//! Two consumers share this one evaluator, deliberately: array lengths
//! (`wchar FileName[FileNameLength]`) and `#define` constants
//! (`#define EXT4_XATTR_PAD (1<<EXT4_XATTR_PAD_BITS)`). Two implementations of
//! one grammar would be two places for it to drift, and the drift would show up
//! as a length.
//!
//! # On the error *messages*
//!
//! The messages are asserted loosely -- by the distinguishing phrase rather
//! than verbatim -- on purpose. What a consumer depends on is the exception
//! *class* and the message *meaning*; pinning whole sentences turns every
//! wording improvement into a test failure. The one thing pinned exactly is
//! that no message leaks a Rust `Debug` rendering, which is a defect
//! independent of any parity question.

use std::collections::HashMap;

use cstruct_rs2_core::error::Error;
use cstruct_rs2_core::expr::{Expr, NoScope, Scope};

/// Evaluate against nothing but literals.
fn eval(text: &str) -> i128 {
    Expr::parse(text)
        .unwrap_or_else(|e| panic!("{text:?} should parse: {e}"))
        .eval(&NoScope)
        .unwrap_or_else(|e| panic!("{text:?} should evaluate: {e}"))
}

/// Evaluate against a bag of named integers, which is what a decoded record's
/// earlier fields amount to.
fn eval_with(text: &str, vars: &[(&str, i128)]) -> i128 {
    let scope: HashMap<String, i128> = vars.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect();
    Expr::parse(text).unwrap().eval(&scope).unwrap()
}

/// The error from parsing, for the failure tests.
fn parse_error(text: &str) -> Error {
    Expr::parse(text).expect_err(&format!("{text:?} should not parse"))
}

/// The error from evaluating something that parses but cannot resolve.
fn eval_error(text: &str) -> Error {
    Expr::parse(text)
        .unwrap_or_else(|e| panic!("{text:?} should parse: {e}"))
        .eval(&NoScope)
        .expect_err(&format!("{text:?} should not evaluate"))
}

fn message(error: &Error) -> String {
    error.to_string()
}

// ---------------------------------------------------------------------------
// Arithmetic
// ---------------------------------------------------------------------------

/// Every binary operator in the grammar, one case each.
///
/// A table rather than prose because the interesting property is *complete
/// coverage of the operator set*: an operator the evaluator forgets is an
/// operator a definition can write and get a wrong answer from.
#[test]
fn every_binary_operator_computes_what_c_computes() {
    for (source, want) in [
        ("7 + 3", 10i128),
        ("7 - 3", 4),
        ("7 * 3", 21),
        ("7 / 3", 2),
        ("7 % 3", 1),
        ("1 << 4", 16),
        ("256 >> 4", 16),
        ("12 & 10", 8),
        ("12 | 10", 14),
        ("12 ^ 10", 6),
    ] {
        assert_eq!(eval(source), want, "{source}");
    }
}

/// Both unary operators, including stacked ones.
#[test]
fn unary_operators_stack() {
    assert_eq!(eval("-5"), -5);
    assert_eq!(eval("~0"), -1);
    assert_eq!(eval("--5"), 5, "negation applied twice");
    assert_eq!(eval("~~7"), 7);
    assert_eq!(eval("-~0"), 1, "~0 is -1, negated is 1");
}

/// Precedence is C's, which is the only reason a definition can be written the
/// way its author expects.
#[test]
fn precedence_and_grouping_match_c() {
    assert_eq!(eval("2 + 3 * 4"), 14);
    assert_eq!(eval("(2 + 3) * 4"), 20);
    assert_eq!(eval("1 | 2 & 3"), 3, "& binds tighter than |");
    assert_eq!(eval("1 ^ 3 & 1"), 0, "& binds tighter than ^");
    assert_eq!(eval("1 << 2 + 1"), 8, "+ binds tighter than <<");
    assert_eq!(eval("8 - 3 - 2"), 3, "subtraction is left-associative");
    assert_eq!(eval("100 / 5 / 2"), 10, "division is left-associative");
}

/// The ternary, including the nesting that makes it worth having.
#[test]
fn the_ternary_selects_a_branch_and_nests() {
    assert_eq!(eval_with("n ? 4 : 8", &[("n", 1)]), 4);
    assert_eq!(eval_with("n ? 4 : 8", &[("n", 0)]), 8);
    // Any non-zero condition is true, as in C.
    assert_eq!(eval_with("n ? 4 : 8", &[("n", -1)]), 4);
    // Nested in the false branch, which is how a three-way choice is written.
    assert_eq!(eval_with("a ? 1 : b ? 2 : 3", &[("a", 0), ("b", 1)]), 2);
    assert_eq!(eval_with("a ? 1 : b ? 2 : 3", &[("a", 0), ("b", 0)]), 3);
    // The untaken branch is not evaluated, so a division by zero in it is fine.
    assert_eq!(eval("1 ? 5 : 1/0"), 5);
}

/// Literal bases inside an expression, including C octal.
///
/// `010` is eight. Parsing it as ten is silently wrong for any definition that
/// writes permissions or flags the way C does, and there is no error to notice.
#[test]
fn literal_bases_include_c_octal() {
    assert_eq!(eval("42"), 42);
    assert_eq!(eval("0x2a"), 42);
    assert_eq!(eval("0b101010"), 42);
    assert_eq!(eval("010"), 8, "a leading zero means base 8");
    assert_eq!(eval("0"), 0, "a bare zero is still zero");
    assert_eq!(eval("0x10UL"), 16, "type suffixes are tolerated");
}

/// A bare name is the overwhelmingly common case: `char name[n]`.
#[test]
fn a_bare_name_resolves_from_the_scope() {
    assert_eq!(eval_with("FileNameLength", &[("FileNameLength", 11)]), 11);
    assert_eq!(
        eval_with("_leading_underscore", &[("_leading_underscore", 3)]),
        3
    );
    assert_eq!(eval_with("n2", &[("n2", 5)]), 5, "digits inside a name");
}

/// `color.RED` is one name, not two with an operator between.
///
/// cstruct has no member-access operator; the *resolver* is what knows how to
/// walk the dots, so the lexer has to hand it a single identifier.
#[test]
fn a_dotted_identifier_is_one_name() {
    assert_eq!(eval_with("color.RED", &[("color.RED", 4)]), 4);
    assert_eq!(eval_with("a.b.c + 1", &[("a.b.c", 6)]), 7);
}

/// The real `dissect.extfs` constants, which are why this module exists.
#[test]
fn the_extfs_constants_evaluate() {
    assert_eq!(
        eval_with("(1<<EXT4_XATTR_PAD_BITS)", &[("EXT4_XATTR_PAD_BITS", 2)]),
        4
    );
    assert_eq!(eval_with("(EXT4_XATTR_PAD-1)", &[("EXT4_XATTR_PAD", 4)]), 3);
}

/// A literal length is detectable as constant, which is what lets the compiler
/// fold it so the decoder never evaluates an expression for it.
#[test]
fn constant_folding_is_detectable() {
    assert_eq!(Expr::parse("16").unwrap().as_const(), Some(16));
    assert_eq!(
        Expr::parse("Length").unwrap().as_const(),
        None,
        "a name is not foldable at compile time"
    );
    assert_eq!(
        Expr::parse("2 + 2").unwrap().as_const(),
        None,
        "as_const asks whether the node *is* a literal, not whether it could be reduced to one"
    );
}

// ---------------------------------------------------------------------------
// sizeof and offsetof
// ---------------------------------------------------------------------------

/// A scope that knows about one made-up type, so the two call forms can be
/// exercised without dragging a whole schema in.
struct TypeScope;

impl Scope for TypeScope {
    fn lookup(&self, _name: &str) -> Option<i128> {
        None
    }

    fn size_of(&self, type_name: &str) -> Option<i128> {
        match type_name {
            "header" => Some(12),
            _ => None,
        }
    }

    fn offset_of(&self, type_name: &str, member: &str) -> Option<i128> {
        match (type_name, member) {
            ("header", "length") => Some(4),
            _ => None,
        }
    }
}

/// `sizeof(TYPE)` and `offsetof(TYPE, member)` are the only two call forms in
/// the language, and they resolve through the scope rather than being folded at
/// parse time -- the parser sees text, and only a schema knows how big a type
/// is.
#[test]
fn sizeof_and_offsetof_resolve_through_the_scope() {
    assert_eq!(
        Expr::parse("sizeof(header)")
            .unwrap()
            .eval(&TypeScope)
            .unwrap(),
        12
    );
    assert_eq!(
        Expr::parse("offsetof(header, length)")
            .unwrap()
            .eval(&TypeScope)
            .unwrap(),
        4
    );
    // They compose with ordinary arithmetic, which is the point of having them
    // in an *expression* rather than as a special length form.
    assert_eq!(
        Expr::parse("sizeof(header) - offsetof(header, length)")
            .unwrap()
            .eval(&TypeScope)
            .unwrap(),
        8
    );
}

/// A scope with no types says so, and the expression fails rather than
/// producing a plausible number.
#[test]
fn sizeof_against_a_scope_without_types_is_an_error() {
    let error = eval_error("sizeof(uint32)");
    assert!(
        message(&error).contains("Unknown type in sizeof"),
        "{error}"
    );

    let error = Expr::parse("offsetof(header, missing)")
        .unwrap()
        .eval(&TypeScope)
        .expect_err("an unknown member must not resolve");
    assert!(
        message(&error).contains("Unknown member in offsetof"),
        "{error}"
    );
}

/// The argument of `sizeof` is a *type name*, so something that is not one is a
/// resolution failure rather than a syntax error.
///
/// The distinction is the useful part: `sizeof(0 +)` and `1 +` are different
/// problems for whoever wrote them, and blanketing both as one class made a
/// malformed literal indistinguishable from a grammar error.
#[test]
fn a_non_type_argument_to_sizeof_is_a_resolution_failure() {
    match parse_error("sizeof(0 +)") {
        Error::UnknownType { name } => assert!(name.contains('0'), "got {name:?}"),
        other => panic!("expected an unknown-type error, got {other:?}"),
    }
}

/// `offsetof` needs both of its arguments.
#[test]
fn offsetof_requires_a_type_and_a_member() {
    for source in ["offsetof(header)", "offsetof(header, 4)"] {
        let error = parse_error(source);
        assert!(
            message(&error).contains("offsetof takes a type and a member"),
            "{source}: {error}"
        );
    }
}

/// A call form with its bracket left open reports the bracket, not the operand.
///
/// From the reader's point of view the open bracket *is* the problem; blaming
/// the missing operand sends them looking in the wrong place.
#[test]
fn an_unclosed_call_reports_the_parenthesis() {
    for source in ["sizeof(", "sizeof(uint32", "sizeof 4"] {
        let error = parse_error(source);
        assert!(message(&error).contains("parenthes"), "{source}: {error}");
    }
}

// ---------------------------------------------------------------------------
// Failure paths
// ---------------------------------------------------------------------------

/// A malformed *literal* is a lexer error, and a malformed *expression* is a
/// parser error. They map to different Python classes because they are
/// different mistakes.
#[test]
fn a_malformed_literal_is_distinguishable_from_a_malformed_expression() {
    for source in ["0x", "0b"] {
        match parse_error(source) {
            Error::Lex { message } => assert!(
                message.contains("literal"),
                "{source} should name the literal, got {message:?}"
            ),
            other => panic!("{source} should be a lex error, got {other:?}"),
        }
    }
    assert!(matches!(parse_error("1 +"), Error::InvalidValue { .. }));
}

/// A character the grammar has no meaning for is a lexer error naming it.
#[test]
fn an_unexpected_character_is_named() {
    match parse_error("1 $ 2") {
        Error::Lex { message } => assert!(message.contains('$'), "got {message:?}"),
        other => panic!("expected a lex error, got {other:?}"),
    }
    // A trailing dot is not swallowed into an identifier: the look-ahead
    // requires a name character after it, so this stays an error rather than
    // becoming the name `color.`.
    assert!(matches!(parse_error("color."), Error::Lex { .. }));
}

/// Every shape of "this is not an expression", with the phrase that tells the
/// reader which one it is.
#[test]
fn structural_failures_say_which_structure_failed() {
    let cases = [
        ("1 2", "too many operands"),
        ("1 +", "not enough operands"),
        ("+", "not enough operands"),
        ("", "not enough operands"),
        (")", "Mismatched parentheses"),
        ("()", "empty parenthesis"),
        ("(1", "Mismatched parentheses"),
        ("1 ? 2", "ternary"),
    ];
    for (source, phrase) in cases {
        let error = parse_error(source);
        assert!(
            message(&error).contains(phrase),
            "{source:?} should mention {phrase:?}, got {error}"
        );
    }
}

/// No error message may carry a Rust `Debug` dump of this crate's internals.
///
/// This is not a wording preference. The parser once formatted an unexpected
/// token with `{:?}`, so a Python traceback read
/// `unexpected token in expression: Some(Op(")"))` -- the names of Rust data
/// structures surfacing in a consumer's exception. That is a defect on any
/// reading, independent of any parity question, and it is the one thing here
/// asserted structurally rather than by phrase.
#[test]
fn no_error_message_leaks_a_rust_debug_rendering() {
    for source in ["1 2", ")", "1 ) 2", "(1", "1 +", "sizeof(0 +)", "0x", "@"] {
        let text = message(&parse_error(source));
        for leak in ["Some(", "Op(", "Num(", "Name(", "Tok::"] {
            assert!(
                !text.contains(leak),
                "{source:?} leaked {leak:?} into a user-facing message: {text}"
            );
        }
    }
}

/// A name that is not in scope is an error, never a silent zero.
///
/// A silent zero decodes a zero-length array and produces a wrong record with
/// no exception, which is the failure mode this whole project is built to
/// avoid.
#[test]
fn an_unresolvable_name_is_an_error_not_a_zero() {
    let error = eval_error("Missing");
    assert!(
        message(&error).contains("not defined in this scope"),
        "{error}"
    );
    // A dotted name gets its own wording, because "not defined in this scope"
    // describes only one of its two ways to fail.
    let error = eval_error("color.RED");
    assert!(message(&error).contains("Unknown identifier"), "{error}");
}

/// A scope that knows *why* a name failed gets to say so.
///
/// The default is the generic message, which is right for a bag of numbers. A
/// scope backed by a whole namespace can distinguish "no such member" from
/// "that member is not a number", and those are different problems.
#[test]
fn a_scope_may_describe_its_own_failure() {
    struct FussyScope;
    impl Scope for FussyScope {
        fn lookup(&self, _name: &str) -> Option<i128> {
            None
        }
        fn describe_failure(&self, name: &str) -> Option<String> {
            Some(format!("'{name}' names a string, which cannot be a length"))
        }
    }

    let error = Expr::parse("prefix")
        .unwrap()
        .eval(&FussyScope)
        .unwrap_err();
    assert!(
        message(&error).contains("cannot be a length"),
        "the scope's own explanation must reach the message, got {error}"
    );
}

// ---------------------------------------------------------------------------
// Hostile input
// ---------------------------------------------------------------------------

/// Lengths come out of forensic images, so every arithmetic edge is an error
/// rather than a panic that escapes as an interpreter crash.
#[test]
fn hostile_arithmetic_errors_rather_than_panics() {
    for source in ["1/0", "1%0", "1<<128", "1>>128", "1<<-1", "1>>-1"] {
        let error = eval_error(source);
        assert!(
            matches!(error, Error::InvalidValue { .. }),
            "{source} should be a value error, got {error:?}"
        );
    }
    // The boundary is inclusive at the bottom and exclusive at the top: a
    // 127-bit shift is defined for an i128 and must still work.
    assert_eq!(eval("1 << 127"), i128::MIN);
    assert_eq!(eval("1 << 0"), 1);
}

/// Overflow wraps rather than panicking.
///
/// Wrapping is a deliberate choice over saturating or erroring: the values are
/// C-like integers and a definition doing arithmetic near the edge is far more
/// likely to be relying on wraparound than to want a hard failure. What matters
/// for safety is that the *consumer* of the result -- the array length check --
/// refuses an absurd count, which `decode` does.
#[test]
fn arithmetic_at_the_edge_wraps_instead_of_panicking() {
    let max = i128::MAX;
    assert_eq!(eval_with("n + 1", &[("n", max)]), i128::MIN);
    assert_eq!(eval_with("n * 2", &[("n", max)]), -2);
    assert_eq!(eval_with("0 - n", &[("n", i128::MIN)]), i128::MIN);
    assert_eq!(eval_with("-n", &[("n", i128::MIN)]), i128::MIN);
}

/// Deep nesting is bounded by the input's own length rather than by a guard,
/// which is worth an explicit check: a definition is input, and an expression
/// that recurses per character would be a denial of service through a `.h`
/// file.
///
/// A hundred levels is far past anything a real definition nests, and is here to
/// show the parser walks the input rather than recursing per character.
#[test]
fn deeply_nested_parentheses_are_handled() {
    let depth = 100;
    let source = format!("{}1{}", "(".repeat(depth), ")".repeat(depth));
    assert_eq!(eval(&source), 1);
}

/// The `NoScope` marker resolves nothing, including the call forms.
///
/// It exists for expressions that must be resolvable from literals alone, and
/// the assertion is that it really does refuse rather than defaulting to zero.
#[test]
fn the_empty_scope_resolves_nothing() {
    assert!(NoScope.lookup("anything").is_none());
    assert!(NoScope.size_of("uint32").is_none());
    assert!(NoScope.offset_of("t", "m").is_none());
    assert!(NoScope.describe_failure("anything").is_none());
}

/// A `HashMap<String, i128>` is a scope, which is what makes the constant table
/// usable directly.
#[test]
fn a_map_of_names_is_a_scope() {
    let mut scope: HashMap<String, i128> = HashMap::new();
    scope.insert("N".to_owned(), 7);
    assert_eq!(scope.lookup("N"), Some(7));
    assert_eq!(scope.lookup("M"), None);
    assert_eq!(Expr::parse("N * 3").unwrap().eval(&scope).unwrap(), 21);
}

/// Parsing is separate from evaluating, and a parsed expression is reusable.
///
/// This is the property the hot path depends on: a dynamic field's length is
/// parsed **once** at compile time and only evaluated per record. Re-parsing
/// text per record would put string handling on the decode path.
#[test]
fn one_parse_serves_many_evaluations() {
    let expr = Expr::parse("n * 2 + 1").unwrap();
    for n in 0..5i128 {
        let scope: HashMap<String, i128> = [("n".to_owned(), n)].into_iter().collect();
        assert_eq!(expr.eval(&scope).unwrap(), n * 2 + 1);
    }
}
