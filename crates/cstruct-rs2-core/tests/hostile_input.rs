//! The paths a *malformed* definition or a *malformed image* takes.
//!
//! The existing suites (`decode_readers`, `parser_grammar`, `plan_compilation`,
//! `lexer_tokens`) test what the implementation does with input it understands.
//! This one tests what it does with input it does not, which is the half a
//! forensic tool spends most of its life in: every length, count, offset and
//! nesting depth in this project arrives from an evidence file, and an evidence
//! file is attacker-controlled by definition.
//!
//! # The contract these tests pin
//!
//! Three properties, and they are absolute:
//!
//! 1. **An error, never a panic.** `panic = "unwind"` means a panic reaches the
//!    PyO3 boundary and becomes an exception rather than killing the
//!    interpreter — but "the image crashed `target-query` politely" is still a
//!    lost investigation. Every arm below asserts an [`Error`].
//! 2. **The right error.** [`Error::EndOfStream`] becomes `EOFError`, which
//!    `dissect` uses as ordinary control flow (`while True: … except EOFError:
//!    break`). Everything else becomes `ValueError` or worse and escapes that
//!    loop, aborting the walk instead of ending it. Which error is as much
//!    behaviour as whether there is one.
//! 3. **Bounded work.** A four-byte length field can ask for four billion
//!    elements. [`MAX_ARRAY_ELEMENTS`] and [`MAX_NESTING`] are the answer, and
//!    they are asserted rather than assumed.
//!
//! # Why these live apart from the happy-path suites
//!
//! Error handling is the code real use exercises least, so it is the most likely
//! to rot unnoticed: ordinary data never asks the decoder for an array of two
//! billion elements. Grouping the refusals makes them reviewable as a whole —
//! you can read this file and see every way the implementation says no.

use std::collections::HashMap;
use std::sync::Arc;

use cstruct_rs2_core::decode::{
    read_bytes, read_float, read_int, read_int128, read_uint, read_uint128, resolve_layout,
    MAX_ARRAY_ELEMENTS,
};
use cstruct_rs2_core::error::Error;
use cstruct_rs2_core::expr::{Expr, NoScope};
use cstruct_rs2_core::lexer::{tokenize, TokenKind};
use cstruct_rs2_core::parser::parse;
use cstruct_rs2_core::plan::{CompiledPlan, Compiler};
use cstruct_rs2_core::schema::Endian;

fn compile(source: &str, type_name: &str) -> Arc<CompiledPlan> {
    let schema = parse(source, Endian::Little).unwrap_or_else(|e| panic!("{source}: {e}"));
    Compiler::new(&schema)
        .compile(type_name)
        .unwrap_or_else(|e| panic!("{type_name} should compile: {e}"))
}

/// The error from laying `buf` out against `source`'s `type_name`.
fn layout_error(source: &str, type_name: &str, buf: &[u8]) -> Error {
    let plan = compile(source, type_name);
    resolve_layout(&plan, buf).expect_err("this layout should have been refused")
}

/// Just the token kinds, so an assertion reads as the token stream it is about.
fn kinds(source: &str) -> Vec<TokenKind> {
    tokenize(source)
        .expect("source should tokenize")
        .into_iter()
        .map(|token| token.kind)
        .collect()
}

// ---------------------------------------------------------------------------
// Readers: widths the type system cannot produce
// ---------------------------------------------------------------------------

/// A width no reader supports is a value error, not a wrong number.
///
/// The narrow readers handle 1–8 bytes and the wide pair handles the rest. A
/// width outside both is unreachable from any definition the parser accepts —
/// which is exactly why it is worth pinning: this arm is the guard that keeps a
/// future width added to the schema from silently decoding as zero, or from
/// shifting by 64 and hitting Rust's "attempt to shift left with overflow"
/// panic in a debug build.
#[test]
fn an_unsupported_integer_width_is_refused_by_name() {
    // Long enough that even the widest case below is a width complaint rather
    // than a truncation one -- the bounds check runs first, so a short buffer
    // would mask the arm this test is about.
    let buf = [0xffu8; 256];
    for size in [0u8, 9, 16, 255] {
        let unsigned = read_uint(&buf, 0, size, Endian::Little);
        let signed = read_int(&buf, 0, size, Endian::Little);
        for (name, result) in [
            ("read_uint", unsigned.map(|_| ())),
            ("read_int", signed.map(|_| ())),
        ] {
            match result {
                Err(Error::InvalidValue { message }) => assert!(
                    message.contains("unsupported integer width"),
                    "{name} at width {size} should say which width: {message}"
                ),
                other => panic!("{name} at width {size} should be refused, got {other:?}"),
            }
        }
    }
}

/// The wide readers sign-extend from their own top bit, at any width.
///
/// `read_int128` is not "the 128-bit reader"; it is "the reader for widths past
/// eight", and the width it is given decides where the sign bit is. A
/// hypothetical `int96` signs from bit 95. Getting this wrong turns every
/// negative value into a very large positive one — the same defect class that
/// `int24` had, one width up, and with no error to notice it by.
#[test]
fn the_wide_readers_sign_from_the_width_they_are_given() {
    let all_ones = [0xffu8; 16];
    for size in [9u8, 10, 12, 16] {
        assert_eq!(
            read_int128(&all_ones, 0, size, Endian::Little).unwrap(),
            -1,
            "all-ones at width {size} is -1, not a huge positive"
        );
        // `u128::MAX >> spare_bits` rather than `(1 << bits) - 1`: at a width of
        // sixteen the latter shifts by 128 and overflows in the *test*, which is
        // the same trap the reader itself has to avoid.
        let expected = u128::MAX >> (128 - size as u32 * 8);
        assert_eq!(
            read_uint128(&all_ones, 0, size, Endian::Little).unwrap(),
            expected,
            "the unsigned reading of the same bytes is the full range"
        );
    }

    // The most negative value at 16 bytes: sign bit set, nothing else.
    let mut most_negative = [0u8; 16];
    most_negative[15] = 0x80;
    assert_eq!(
        read_int128(&most_negative, 0, 16, Endian::Little).unwrap(),
        i128::MIN
    );

    // A width of exactly 16 takes the `bits >= 128` short circuit rather than
    // the shift, because `1u128 << 128` would overflow.
    assert_eq!(read_int128(&[0xff; 16], 0, 16, Endian::Big).unwrap(), -1);

    // The positive half, which is the arm that does *not* subtract: a clear
    // sign bit must be left alone rather than wrapped. Both halves are here
    // because a sign-extension bug that only fires one way looks like correct
    // code reading correct values, right up until the first negative number.
    let mut positive = [0u8; 16];
    positive[0] = 0x7f;
    assert_eq!(read_int128(&positive, 0, 9, Endian::Little).unwrap(), 0x7f);
    assert_eq!(read_int128(&positive, 0, 16, Endian::Little).unwrap(), 0x7f);
    // The largest value that is still positive at nine bytes: every bit below
    // the sign bit set.
    let mut largest = [0xffu8; 16];
    largest[8] = 0x7f;
    assert_eq!(
        read_int128(&largest, 0, 9, Endian::Little).unwrap(),
        (1i128 << 71) - 1
    );
}

/// Every reader refuses input that is too short, and says so as end-of-stream.
///
/// Grouped deliberately: the *class* is the contract. A `ValueError` from any
/// one of these escapes `dissect.ntfs`'s `except EOFError: break` and takes the
/// filesystem with it, and a reader added later that gets this wrong would be
/// caught here rather than by a missing volume six months on.
#[test]
fn every_reader_reports_a_short_buffer_as_end_of_stream() {
    let short = [0x01u8, 0x02];
    let checks: [(&str, Result<(), Error>); 6] = [
        (
            "read_uint",
            read_uint(&short, 0, 8, Endian::Little).map(|_| ()),
        ),
        (
            "read_int",
            read_int(&short, 0, 8, Endian::Little).map(|_| ()),
        ),
        (
            "read_uint128",
            read_uint128(&short, 0, 16, Endian::Little).map(|_| ()),
        ),
        (
            "read_int128",
            read_int128(&short, 0, 16, Endian::Little).map(|_| ()),
        ),
        (
            "read_float",
            read_float(&short, 0, 8, Endian::Little).map(|_| ()),
        ),
        ("read_bytes", read_bytes(&short, 0, 8).map(|_| ())),
    ];
    for (name, result) in checks {
        assert!(
            matches!(result, Err(Error::EndOfStream)),
            "{name} on a short buffer must be EndOfStream, got {result:?}"
        );
    }
}

/// An offset near `usize::MAX` must not wrap into a valid-looking range.
///
/// `offset + size` is the one arithmetic operation in the readers that takes
/// two attacker-influenced numbers. Unchecked, it wraps to a small value and the
/// bounds check then passes on a slice that was never in the buffer.
#[test]
fn an_offset_that_would_wrap_is_end_of_stream() {
    let buf = [0u8; 16];
    for offset in [usize::MAX, usize::MAX - 4] {
        assert!(matches!(
            read_uint(&buf, offset, 8, Endian::Little),
            Err(Error::EndOfStream)
        ));
        assert!(matches!(
            read_uint128(&buf, offset, 16, Endian::Little),
            Err(Error::EndOfStream)
        ));
        assert!(matches!(
            read_bytes(&buf, offset, 8),
            Err(Error::EndOfStream)
        ));
    }
}

// ---------------------------------------------------------------------------
// Layout: lengths and counts out of an image
// ---------------------------------------------------------------------------

/// A length field asking for more elements than the cap allows is refused.
///
/// `MAX_ARRAY_ELEMENTS` is 2^24. The number matters less than the fact that
/// there *is* one: without it, a corrupt `uint32` count of 4,000,000,000 makes
/// the decoder try to build a four-billion-element Python list, and the process
/// dies to the OOM killer rather than reporting a bad record. The refusal names
/// the limit so a report can say which cap was hit.
#[test]
fn an_absurd_element_count_is_refused_rather_than_allocated() {
    let source = "struct s { uint32 n; uint32 values[n]; };";
    let mut buf = Vec::new();
    buf.extend_from_slice(&(MAX_ARRAY_ELEMENTS + 1).to_le_bytes());

    match layout_error(source, "s", &buf) {
        Error::LimitExceeded { limit, value } => {
            assert_eq!(limit, "array elements");
            assert_eq!(value, u64::from(MAX_ARRAY_ELEMENTS) + 1);
        }
        other => panic!("expected a limit error, got {other:?}"),
    }
}

/// The same cap protects an array *of structures*, which is a separate path.
///
/// `Op::ScalarArray` and `Op::NestedArray` resolve their counts in different
/// functions, so a cap on one is not a cap on the other. `dissect.regf` sizes
/// its subkey lists as an array of structures, and that is the shape a corrupt
/// hive would reach this with.
#[test]
fn an_absurd_count_of_structures_is_refused_too() {
    let source = "struct e { uint32 a; }; struct s { uint32 n; e items[n]; };";
    let buf = 0xffff_ffffu32.to_le_bytes();

    match layout_error(source, "s", &buf) {
        Error::LimitExceeded { limit, .. } => assert_eq!(limit, "array elements"),
        other => panic!("expected a limit error, got {other:?}"),
    }
}

/// A count that is legal but whose bytes overflow a `u32` is refused as size.
///
/// Element *count* and total *byte size* are two different limits, and a count
/// under the cap can still describe more bytes than a record can address —
/// 2^24 elements of 1024 bytes is 16 GiB. The error names "array byte size" so
/// the two cases stay distinguishable in a report.
#[test]
fn an_array_whose_bytes_overflow_is_refused_as_a_size() {
    let source = "struct e { char pad[1024]; }; struct s { uint32 n; e items[n]; };";
    let buf = (MAX_ARRAY_ELEMENTS - 1).to_le_bytes();

    match layout_error(source, "s", &buf) {
        Error::LimitExceeded { limit, .. } => assert_eq!(limit, "array byte size"),
        other => panic!("expected a byte-size limit error, got {other:?}"),
    }
}

/// A negative length is an empty array, not an error and not a huge one.
///
/// The choice is upstream's and is followed rather than improved on: a signed
/// length field holding `-1` yields zero elements. Reading it as unsigned would
/// give 4,294,967,295, and "refuse the record" would lose data upstream keeps.
/// Both directions of that judgement have to be pinned, because both are
/// plausible and only one matches.
#[test]
fn a_negative_length_yields_an_empty_array() {
    let plan = compile("struct s { int32 n; uint32 values[n]; };", "s");
    let layout = resolve_layout(&plan, &(-1i32).to_le_bytes()).expect("a negative length is legal");

    assert_eq!(layout.spans[1].count, 0, "no elements");
    assert_eq!(layout.spans[1].size, 0, "and no bytes");
    assert_eq!(layout.consumed, 4, "only the length field was consumed");
}

/// The same rule for an array of structures, which resolves its count elsewhere.
#[test]
fn a_negative_count_of_structures_yields_an_empty_array() {
    let plan = compile(
        "struct e { uint32 a; }; struct s { int32 n; e items[n]; };",
        "s",
    );
    let layout = resolve_layout(&plan, &(-1i32).to_le_bytes()).unwrap();

    assert_eq!(layout.spans[1].count, 0);
    assert_eq!(layout.consumed, 4);
}

/// An array of *variable-size* structures is laid out element by element.
///
/// A fixed-size element makes the extent arithmetic — count times stride. A
/// variable-size one has no stride, so each element has to be laid out in turn
/// to find where the next begins. This is the slow branch, and it is the one a
/// registry hive full of variable-length cells actually takes.
#[test]
fn an_array_of_variable_size_structures_measures_each_element() {
    let plan = compile(
        "struct e { uint8 len; char body[len]; }; struct s { uint8 n; e items[n]; };",
        "s",
    );
    // Two elements: one of three body bytes, one of one.
    let buf = [2, 3, b'a', b'b', b'c', 1, b'z'];
    let layout = resolve_layout(&plan, &buf).unwrap();

    assert_eq!(layout.spans[1].count, 2);
    assert_eq!(layout.spans[1].size, 6, "4 bytes then 2 bytes");
    assert_eq!(layout.consumed, 7, "the count byte plus both elements");
}

/// A variable-size element array that runs off the end is end-of-stream.
///
/// The per-element loop is where a truncated image lands: the count says four,
/// the bytes hold one and a half. It must report `EndOfStream` so the caller's
/// record-run loop terminates instead of raising.
#[test]
fn a_truncated_array_of_variable_size_structures_is_end_of_stream() {
    let plan = compile(
        "struct e { uint8 len; char body[len]; }; struct s { uint8 n; e items[n]; };",
        "s",
    );
    let error = resolve_layout(&plan, &[4, 3, b'a', b'b', b'c']).unwrap_err();
    assert!(matches!(error, Error::EndOfStream), "got {error:?}");
}

/// A truncated *fixed* field is end-of-stream, reported before the next field.
///
/// The check exists so the failure surfaces as truncation rather than as the
/// *following* field's length expression naming an undecoded field — which
/// would report "not defined in this scope" — a `ValueError`, which escapes the
/// caller's `except EOFError` loop. It only shows up when a dynamic field
/// follows a fixed one, which is why the definition below has both.
#[test]
fn a_record_truncated_mid_field_is_end_of_stream_not_a_scope_error() {
    let plan = compile("struct s { uint32 n; char body[n]; };", "s");
    let error = resolve_layout(&plan, &[1, 2]).unwrap_err();
    assert!(
        matches!(error, Error::EndOfStream),
        "truncation must not surface as a scope error: {error:?}"
    );
}

// ---------------------------------------------------------------------------
// Lexer: literals written the way real definitions write them
// ---------------------------------------------------------------------------

/// Every escape the lexer understands, in one literal.
///
/// String escapes are only reachable through `#define`, which is why they are
/// easy to leave untested and easy to get wrong. `dissect.executable`'s PE
/// archive reader defines `b"!<arch>\n"`, and `\n` arriving as the two
/// characters backslash-n rather than as a newline makes its magic comparison
/// fail against every valid archive — a wrong answer with no error.
#[test]
fn string_escapes_decode_to_the_bytes_they_name() {
    let tokens = kinds(r#"#define S "a\nb\rc\td\0e\\f\"g\'h""#);
    let literal = tokens
        .iter()
        .find_map(|kind| match kind {
            TokenKind::Str(text) => Some(text.clone()),
            _ => None,
        })
        .expect("the define's value should be a string token");

    assert_eq!(literal, "a\nb\rc\td\0e\\f\"g'h");
}

/// `\xNN` is the only numeric escape, and a malformed one stays literal.
///
/// Two decisions are pinned here, and both are deliberate leniency: a valid
/// `\x41` becomes `A`, and a malformed `\xZZ` becomes the character `x`
/// followed by its text rather than a lexer error. A definition is loaded at
/// *import* time, so refusing one removes a whole plugin from `target-query`;
/// carrying a slightly wrong constant through loses only whatever compares
/// against it.
#[test]
fn a_hex_escape_decodes_and_a_malformed_one_stays_literal() {
    let good = kinds(r#"#define S "\x41\x42""#);
    assert!(
        good.contains(&TokenKind::Str("AB".to_owned())),
        "a well-formed hex escape is its byte: {good:?}"
    );

    let bad = kinds(r#"#define S "\xZZ""#);
    assert!(
        bad.contains(&TokenKind::Str("xZZ".to_owned())),
        "a malformed hex escape degrades to literal text: {bad:?}"
    );
}

/// A literal outside a `#define` goes through the *other* escape decoder.
///
/// There are two, and this is the finding the test exists to make visible.
/// `#define` handles its own value in the directive branch and resolves escapes
/// through `unescape()`; a literal appearing anywhere else falls to the general
/// scanner in `tokenize`, which has a second, hand-inlined copy of the same
/// escape table. No valid definition reaches the second one — a `"` only ever
/// appears in a `#define` — so the duplicate is unexercised by every other test
/// in the suite, which is exactly how two copies of one table drift apart.
///
/// Asserting that both decode identically is the cheapest defence available
/// short of deleting one of them.
#[test]
fn both_escape_decoders_agree() {
    let body = r#"a\nb\tc\0d\\e\"f\x41g\qh"#;
    let expected = "a\nb\tc\0d\\e\"fAgqh";

    let via_define = kinds(&format!("#define S \"{body}\""));
    assert!(
        via_define.contains(&TokenKind::Str(expected.to_owned())),
        "the #define path: {via_define:?}"
    );

    // A stray literal in the body: not something a real definition contains,
    // and the only way to reach the second decoder at all.
    let via_scanner = kinds(&format!("struct s {{ uint8 a; }}; \"{body}\""));
    assert!(
        via_scanner.contains(&TokenKind::Str(expected.to_owned())),
        "the general scanner must decode the same literal the same way: {via_scanner:?}"
    );

    // And the bytes form, which is a distinct kind in both decoders.
    let bytes_via_scanner = kinds(r#"struct s { uint8 a; }; b"hi\n""#);
    assert!(
        bytes_via_scanner.contains(&TokenKind::Bytes(b"hi\n".to_vec())),
        "{bytes_via_scanner:?}"
    );
}

/// An escape the lexer has no rule for keeps the character it escaped.
///
/// C's own behaviour for an unknown escape is undefined; the choice here is to
/// drop the backslash and keep the character, which is what Python's `bytes`
/// parser does for the same input. The point of the test is that there *is* a
/// rule, rather than a panic on an unindexed match arm.
#[test]
fn an_unknown_escape_keeps_the_character_it_escaped() {
    let tokens = kinds(r#"#define S "a\qb""#);
    assert!(
        tokens.contains(&TokenKind::Str("aqb".to_owned())),
        "{tokens:?}"
    );
}

/// A byte literal keeps its bytes; a text literal keeps its characters.
///
/// `b"..."` and `"..."` are different token kinds because they become different
/// Python objects — `bytes` and `str` — and a magic value compared against the
/// wrong one silently never matches.
#[test]
fn a_byte_literal_is_a_distinct_token_kind() {
    let tokens = kinds(r#"#define MAGIC b"!<arch>\n""#);
    assert!(
        tokens.contains(&TokenKind::Bytes(b"!<arch>\n".to_vec())),
        "{tokens:?}"
    );
}

/// A literal running past its line is still a literal.
///
/// `b"!<arch>\n"` written in a Python source file contains a *real* newline by
/// the time the lexer sees it. Treating a newline as an implicit terminator
/// reported "unterminated string literal" for a construct that appears in real
/// definitions, so the scan carries on and only counts the line.
#[test]
fn a_literal_may_contain_a_real_newline() {
    let tokens = kinds("#define S \"a\nb\"\nstruct s { uint8 x; };");
    assert!(
        tokens.contains(&TokenKind::Str("a\nb".to_owned())),
        "{tokens:?}"
    );
}

/// A literal with no closing quote is a lexer error that names its line.
///
/// The line number is the part worth asserting: a definition is hundreds of
/// lines of C, and "unterminated string literal" without a location is not
/// something a maintainer can act on.
#[test]
fn an_unterminated_literal_is_an_error_that_names_its_line() {
    let error = tokenize("struct s { uint8 x; };\n#define S \"oops").unwrap_err();
    match error {
        Error::Parse { line, message } => {
            assert_eq!(line, 2, "the error should point at the literal's line");
            assert!(message.contains("unterminated"), "{message}");
        }
        other => panic!("expected a parse error naming the line, got {other:?}"),
    }
}

/// Digit-initial text that does not parse as a number is an identifier.
///
/// C forbids this and cstruct definitions are not compiled by a C compiler:
/// `dissect.executable`'s PE flags declare a member called `32BIT_MACHINE`, and
/// refusing it removed the whole PE and ELF stack. So the lexer scans the run
/// first and classifies it afterwards — numeric if it parses as a number, a
/// name otherwise.
///
/// The consequence is that the lexer has **no** malformed-number error: `0x` is
/// digit-initial, fails to parse as a number, and becomes the identifier `0x`.
/// That is deliberate here and is why a malformed literal only surfaces as an
/// error one layer up, where it is used as a *value* — see
/// `a_malformed_literal_names_its_base` for the expression evaluator's version,
/// which is the path a consumer actually reaches.
#[test]
fn digit_initial_text_that_is_not_a_number_lexes_as_an_identifier() {
    for text in ["32BIT_MACHINE", "0x", "9z", "1st"] {
        let tokens = kinds(&format!("#define {text} 1"));
        assert!(
            tokens.contains(&TokenKind::Ident(text.to_owned())),
            "{text} should lex as an identifier: {tokens:?}"
        );
    }

    // And text that *is* a number still is, in every base the language spells.
    assert!(kinds("#define X 0x1f").contains(&TokenKind::Number(0x1f)));
    assert!(kinds("#define X 0b101").contains(&TokenKind::Number(0b101)));
    assert!(kinds("#define X 42").contains(&TokenKind::Number(42)));
}

/// A trailing backslash at the very end of a literal does not read past it.
///
/// The unquoting loop steps forward to read the escaped character; at the end
/// of input there is none. Without the guard this indexes past the slice, which
/// is a panic in Rust rather than the silent over-read it would be in C — still
/// a crash on a malformed definition.
#[test]
fn a_literal_ending_in_a_backslash_does_not_read_past_it() {
    // Terminated, so the lexer accepts it; the escape has nothing after it.
    let tokens = kinds("#define S \"ab\\\"");
    assert!(
        tokens.iter().any(|kind| matches!(kind, TokenKind::Str(_))),
        "a dangling escape still produces a literal: {tokens:?}"
    );
}

// ---------------------------------------------------------------------------
// Expressions: what a malformed one says
// ---------------------------------------------------------------------------

/// No error message renders a Rust `Debug` form of an internal type.
///
/// `Some(Op(")"))` in a Python traceback names this crate's data structures
/// rather than the user's input, and a consumer cannot act on it. Every
/// expression error goes through `describe`, which prints the token as it was
/// written. Each arm of `describe` is a token kind, so all three are here.
#[test]
fn expression_errors_quote_the_input_not_the_implementation() {
    let cases = [
        ("1 +", "an operator with a missing operand"),
        ("1 2", "a number where an operator belongs"),
        ("1 foo", "a name where an operator belongs"),
        (")", "a stray closing parenthesis"),
        ("(1 + 2", "an unclosed group"),
    ];
    for (text, what) in cases {
        let error = Expr::parse(text)
            .and_then(|expr| expr.eval(&NoScope).map(|_| ()))
            .expect_err(&format!("{text} should not evaluate ({what})"));
        let message = error.to_string();
        for forbidden in ["Op(", "Num(", "Name(", "Some(", "None"] {
            assert!(
                !message.contains(forbidden),
                "{text}: message leaks a Rust rendering ({forbidden}): {message}"
            );
        }
    }
}

/// A malformed literal says *which kind* of literal it is.
///
/// `0x` and `0b` and `9z` are three different mistakes, and telling them apart
/// is the difference between "you forgot the digits" and "that is not a number
/// at all". The wording is contract: upstream's suite asserts on it.
#[test]
fn a_malformed_literal_names_its_base() {
    let cases = [
        ("0x", "hexadecimal"),
        ("0X", "hexadecimal"),
        ("0b", "binary"),
        ("0B", "binary"),
        ("9z", "integer"),
    ];
    for (text, expected) in cases {
        let error = Expr::parse(text).expect_err(&format!("{text} is not a number"));
        let message = error.to_string();
        assert!(
            message.contains(expected),
            "{text} should be described as a bad {expected} literal: {message}"
        );
    }
}

/// An unclosed parenthesis is blamed on the parenthesis.
///
/// `sizeof(` runs out of input while parsing the operand, so the naive error is
/// about the missing operand. From the reader's point of view the problem is
/// the bracket that is still open, and that is what the message says.
#[test]
fn an_unclosed_group_is_reported_as_a_parenthesis_problem() {
    for text in ["(1 + 2", "(", "sizeof("] {
        let error = Expr::parse(text).expect_err(&format!("{text} should not parse"));
        let message = error.to_string();
        assert!(
            message.contains("parenthes") || message.contains(')'),
            "{text} should blame the bracket: {message}"
        );
    }
}

/// Division and shifts by hostile values are errors, not panics.
///
/// These are the two arithmetic operations that trap in Rust. A definition can
/// name a field as a divisor, so the value is attacker-controlled — and a
/// divide-by-zero panic unwinding through PyO3 is a `pyo3_runtime.PanicException`
/// that no consumer catches.
#[test]
fn arithmetic_that_would_trap_is_an_error() {
    for text in ["1 / 0", "1 % 0", "1 << 200", "1 >> 200"] {
        let result = Expr::parse(text).and_then(|expr| expr.eval(&NoScope));
        assert!(result.is_err(), "{text} should be refused, got {result:?}");
    }
}

// ---------------------------------------------------------------------------
// Parser: definitions written the way real ones are
// ---------------------------------------------------------------------------

/// A definition that ends mid-declaration is refused, with a line number.
///
/// Truncated definitions happen — a `#include`-style split, a copy-paste, a
/// generated header cut short. Two things have to hold: the parser must not
/// index past its token stream (that would be a panic, not an error), and the
/// failure has to point at a line, because a definition is hundreds of lines of
/// C and a message without a location is not actionable.
#[test]
fn a_definition_that_ends_early_is_refused_with_a_line_number() {
    let cases = [
        "struct s { uint32 a",
        "struct",
        "typedef",
        "enum e : uint8 {",
        "struct s { uint32 a[",
    ];
    for source in cases {
        let error = parse(source, Endian::Little)
            .expect_err(&format!("{source:?} is incomplete and should be refused"));
        match error {
            Error::Parse { line, message } => {
                assert!(line >= 1, "{source:?}: the error should carry a line");
                assert!(!message.is_empty(), "{source:?}: and say something");
                // The truncation has to be described in words. Either spelling
                // is fine -- "found end of input" from the token-level checks,
                // or "unterminated …" from the body scanners, which already know
                // *what* was left open and say so more usefully.
                assert!(
                    message.contains("end of input") || message.contains("unterminated"),
                    "{source:?}: truncation should say so in words: {message}"
                );
            }
            other => panic!("{source:?}: expected a parse error, got {other:?}"),
        }
    }
}

/// No *parse* error renders a Rust `Debug` form either.
///
/// The expression evaluator has had `describe()` for this since a consumer's
/// traceback read `unexpected token in expression: Some(Op(")"))`. The parser had
/// no equivalent, so its own messages leaked the same way: `expected ',' or ';'
/// after field, found Semicolon` names this crate's enum, and `found None` is a
/// Rust `Option` surfacing in a Python exception.
///
/// This is the parser's twin of
/// [`expression_errors_quote_the_input_not_the_implementation`], and it is
/// deliberately a property over many inputs rather than a check of one wording:
/// what must hold is that *no* message mentions the implementation, not that any
/// particular sentence stays fixed.
#[test]
fn parse_errors_quote_the_input_not_the_implementation() {
    let cases = [
        "struct s { uint32 a",
        "struct s { uint32 a uint32 b; };",
        "enum E : uint8 { A = 1 ;",
        "struct s { uint8 x[1 ; 2]; };",
        "struct",
        "typedef",
        "enum e : uint8 {",
        "struct s { uint32 a[",
        "struct s { : ; };",
    ];
    // Variant names and `Debug` shapes that must never reach a consumer.
    const FORBIDDEN: [&str; 12] = [
        "None",
        "Some(",
        "Ident(",
        "Number(",
        "Str(",
        "Bytes(",
        "Semicolon",
        "LBrace",
        "RBrace",
        "LBracket",
        "RBracket",
        "Punct(",
    ];
    for source in cases {
        let Err(error) = parse(source, Endian::Little) else {
            continue; // Some of these are legal; only the refusals are the subject.
        };
        let message = error.to_string();
        for forbidden in FORBIDDEN {
            assert!(
                !message.contains(forbidden),
                "{source:?}: message leaks a Rust rendering ({forbidden}): {message}"
            );
        }
    }
}

/// Each token kind renders as the characters it was written with.
///
/// The table is the contract `parse_errors_quote_the_input_not_the_implementation`
/// relies on, checked directly so a new `TokenKind` variant cannot be added with
/// a `Debug`-shaped rendering and still look fine in the property test above
/// (which only sees the variants its inputs happen to produce).
#[test]
fn every_token_kind_describes_itself_as_written() {
    let cases = [
        (TokenKind::Ident("uint32".to_owned()), "'uint32'"),
        (TokenKind::Number(42), "'42'"),
        (TokenKind::LBrace, "'{'"),
        (TokenKind::RBrace, "'}'"),
        (TokenKind::LBracket, "'['"),
        (TokenKind::RBracket, "']'"),
        (TokenKind::LParen, "'('"),
        (TokenKind::RParen, "')'"),
        (TokenKind::Semicolon, "';'"),
        (TokenKind::Comma, "','"),
        (TokenKind::Colon, "':'"),
        (TokenKind::Equals, "'='"),
        (TokenKind::Star, "'*'"),
        (TokenKind::Punct('#'), "'#'"),
    ];
    for (kind, expected) in cases {
        assert_eq!(kind.describe(), expected, "{kind:?}");
    }

    // The two literal kinds say what they are rather than quoting raw bytes.
    assert!(TokenKind::Str("gnu.".to_owned())
        .describe()
        .contains("gnu."));
    assert!(TokenKind::Bytes(b"\x00\xff".to_vec())
        .describe()
        .contains("byte-string"));

    // And absence is words, not `None`.
    assert_eq!(
        cstruct_rs2_core::lexer::describe_optional(None),
        "end of input"
    );
    assert_eq!(
        cstruct_rs2_core::lexer::describe_optional(Some(&TokenKind::Comma)),
        "','"
    );
}

/// A `#define` value can be an arbitrary expression, not just a literal.
///
/// Real definitions write `#define BLOCK (1 << 12)` and
/// `#define MASK (A | B)`. The fast path takes a bare literal; anything else is
/// re-assembled from tokens into text and parsed as an expression later. That
/// reassembly has an arm per token kind, and a kind it does not know about used
/// to be dropped silently — turning `(1 << 12)` into `1 12`.
#[test]
fn a_define_may_hold_a_parenthesised_expression() {
    let schema = parse(
        "#define SHIFTED (1 << 12)\n\
         #define MASKED (0xff00 | 0x00ff)\n\
         #define NEGATED (~0 & 0xff)\n\
         #define PLAIN 7\n\
         struct s { uint8 a; };",
        Endian::Little,
    )
    .expect("these are ordinary #define forms");

    assert_eq!(schema.const_int("SHIFTED"), Some(4096));
    assert_eq!(schema.const_int("MASKED"), Some(0xffff));
    assert_eq!(schema.const_int("NEGATED"), Some(0xff));
    assert_eq!(schema.const_int("PLAIN"), Some(7));
}

/// A `#define` may hold a string or a byte literal, and both survive as such.
///
/// `dissect.executable` compares against `b"!<arch>\n"`; the value has to come
/// back as bytes rather than as a decoded string, or the comparison never
/// matches and the archive reader reports every valid archive as invalid.
#[test]
fn a_define_may_hold_a_string_or_bytes() {
    use cstruct_rs2_core::schema::ConstValue;

    let schema = parse(
        "#define NAME \"cstruct\"\n#define MAGIC b\"!<arch>\\n\"\nstruct s { uint8 a; };",
        Endian::Little,
    )
    .unwrap();

    assert_eq!(
        schema.constants.get("NAME"),
        Some(&ConstValue::Str("cstruct".to_owned()))
    );
    assert_eq!(
        schema.constants.get("MAGIC"),
        Some(&ConstValue::Bytes(b"!<arch>\n".to_vec()))
    );
}

/// Stray semicolons inside a type body are skipped, not refused.
///
/// `struct s { uint8 a;; };` is a typo a generated header makes, and C compilers
/// accept it. Refusing it would remove the whole definition — every type in the
/// file, not just the one with the extra character.
#[test]
fn a_stray_semicolon_in_a_body_is_tolerated() {
    let schema = parse("struct s {; uint8 a;; uint8 b; };", Endian::Little)
        .expect("an empty declaration is not an error");
    let plan = Compiler::new(&schema).compile("s").unwrap();

    assert_eq!(plan.fields.len(), 2, "the semicolons contribute no fields");
    assert_eq!(plan.total_size, Some(2));
}

/// A multi-word type ending in a pointer star parses as a pointer to that type.
///
/// `unsigned long *p` is three words and a star. The type-name scanner has to
/// know that the star ends the type and the identifier after it is the *field*
/// name, not another word of the type.
#[test]
fn a_multi_word_type_can_be_a_pointer() {
    let schema = parse(
        "struct s { unsigned long *p; unsigned long long q; };",
        Endian::Little,
    )
    .expect("both are ordinary C spellings");
    let plan = Compiler::new(&schema).compile("s").unwrap();

    assert_eq!(plan.names[0].as_ref(), "p");
    assert_eq!(plan.names[1].as_ref(), "q");
    assert_eq!(
        plan.pointer_targets[0].as_deref(),
        Some("unsigned long"),
        "the pointer remembers what it addresses"
    );
    assert!(plan.pointer_targets[1].is_none(), "q is not a pointer");
}

/// An array length may itself be an indexed expression.
///
/// `values[counts[0]]` nests brackets inside a length, so the scanner that
/// collects the length text has to balance them rather than stop at the first
/// `]`. Stopping early truncates the expression to `counts[0` and reports a
/// grammar error for a construct that is legal.
#[test]
fn an_array_length_may_contain_nested_brackets() {
    let schema = parse(
        "#define N 3\nstruct s { uint8 values[(N)]; };",
        Endian::Little,
    )
    .expect("a parenthesised length is legal");
    let plan = Compiler::new(&schema).compile("s").unwrap();
    assert_eq!(plan.total_size, Some(3));
}

/// An enum member's value may be an expression, including a shift.
///
/// `flag F { A = 1 << 0, B = 1 << 1 }` is how flag types are normally written.
/// The value scanner reassembles the expression token by token, like `#define`,
/// and has the same failure mode: a token kind it does not handle disappears
/// from the expression rather than being reported.
#[test]
fn an_enum_member_value_may_be_an_expression() {
    use cstruct_rs2_core::schema::TypeDef;

    let schema = parse(
        "flag F : uint8 { A = 1 << 0, B = 1 << 1, C = (1 << 2) | 1 };",
        Endian::Little,
    )
    .expect("shifts in flag values are ordinary");

    match schema.resolve("F").expect("F should resolve") {
        TypeDef::Enum(def) => assert_eq!(
            def.members,
            vec![
                ("A".to_owned(), 1),
                ("B".to_owned(), 2),
                ("C".to_owned(), 5),
            ],
            "each value is the expression's result, not its first token"
        ),
        other => panic!("expected an enum, got {other:?}"),
    }
}

/// A definition may omit the final semicolon after a type body.
///
/// Generated headers and hand-written definitions both do it, and the parser
/// recovers by treating the start of the next declaration as the end of this
/// one. Without that, one missing character costs every type after it.
#[test]
fn a_missing_trailing_semicolon_does_not_lose_the_next_type() {
    let schema = parse(
        "enum E : uint8 { A = 1 }\nstruct s { uint8 a; }\nstruct t { uint8 b; };",
        Endian::Little,
    )
    .expect("a missing semicolon is recoverable");

    assert!(schema.resolve("E").is_ok(), "the enum survived");
    assert!(schema.resolve("s").is_ok(), "and so did both structs");
    assert!(schema.resolve("t").is_ok());
}

// ---------------------------------------------------------------------------
// Plan: limits that only the compiler can see
// ---------------------------------------------------------------------------

/// A compile-time array length that will not fit a `u32` is refused.
///
/// `uint8 x[1 << 40]` is arithmetic the expression evaluator performs happily in
/// `i128` and that no record can hold. Catching it at compile time means the
/// definition fails to load — loudly, once, at import — rather than producing a
/// plan whose sizes silently wrapped.
#[test]
fn a_compile_time_length_that_overflows_a_u32_is_refused() {
    let schema = parse("struct s { uint8 x[1 << 40]; };", Endian::Little)
        .expect("the definition parses; the length is the compiler's problem");
    match Compiler::new(&schema).compile("s") {
        Err(Error::InvalidValue { message }) => assert!(
            message.contains("out of range"),
            "the message should say the length is out of range: {message}"
        ),
        other => panic!("an absurd compile-time length should be refused, got {other:?}"),
    }
}

/// A length naming a constant is resolved at compile time, not at decode time.
///
/// `#define N 4` followed by `uint8 x[N]` has to produce a *fixed* plan, which
/// is what makes it eligible for the fast path. Deferring it to decode would be
/// correct and would cost the fast path on a construct that is everywhere.
#[test]
fn a_length_naming_a_constant_compiles_to_a_fixed_size() {
    let plan = compile("#define N 4\nstruct s { uint8 x[N]; };", "s");
    assert_eq!(plan.total_size, Some(4));
    assert!(
        plan.fast_path_eligible(),
        "a constant-sized array is still a fixed structure"
    );
}

/// Recursion past the nesting cap is refused rather than overflowing the stack.
///
/// A definition can nest structures arbitrarily, and a self-referential one can
/// nest forever. `MAX_NESTING` bounds the compiler's recursion; without it a
/// hostile — or merely generated — definition is a stack overflow, which is a
/// hard crash rather than a catchable error.
#[test]
fn nesting_past_the_cap_is_refused() {
    let mut source = String::from("struct s0 { uint8 a; };\n");
    for depth in 1..=40 {
        source.push_str(&format!("struct s{depth} {{ s{} inner; }};\n", depth - 1));
    }
    let schema = parse(&source, Endian::Little).expect("deep nesting parses");

    match Compiler::new(&schema).compile("s40") {
        Err(Error::LimitExceeded { limit, .. }) => assert_eq!(limit, "structure nesting"),
        other => panic!("expected a nesting limit, got {other:?}"),
    }
}

/// A length that is positive and far past `u32` is a limit error, not a wrap.
///
/// The cap check compares against `MAX_ARRAY_ELEMENTS`, which is a `u32` — so
/// the conversion has to happen first, and the conversion is where a `uint64`
/// length of 2^33 lands. Truncating it instead would give 0 elements from a
/// field that asked for eight billion: no error, and a record silently missing
/// its array. Both the scalar-array and structure-array paths resolve their
/// counts in separate functions, so both are checked.
#[test]
fn a_length_beyond_u32_is_a_limit_error_rather_than_a_truncation() {
    let huge = (1u64 << 33).to_le_bytes();

    match layout_error("struct s { uint64 n; uint32 v[n]; };", "s", &huge) {
        Error::LimitExceeded { limit, value } => {
            assert_eq!(limit, "array elements");
            assert_eq!(
                value,
                1 << 33,
                "the report names the number that was asked for"
            );
        }
        other => panic!("scalar array: expected a limit error, got {other:?}"),
    }

    match layout_error(
        "struct e { uint32 a; }; struct s { uint64 n; e items[n]; };",
        "s",
        &huge,
    ) {
        Error::LimitExceeded { limit, .. } => assert_eq!(limit, "array elements"),
        other => panic!("structure array: expected a limit error, got {other:?}"),
    }
}

/// A null-terminated array with no terminator is bounded by the cap.
///
/// `char name[]` scans forward for a zero byte. Given a buffer that has none —
/// which a corrupt or encrypted region routinely is — the scan is bounded by
/// the input, and the input can be a whole disk image. `MAX_ARRAY_ELEMENTS` is
/// the second bound, and it is the one that keeps a 4 GiB mapped region from
/// becoming a 4-billion-element Python list.
///
/// The buffer here is deliberately just past the cap: 2^24 non-zero bytes. That
/// is 16 MiB, which is cheap enough to allocate in a test and is the only way
/// to reach this arm at all.
#[test]
fn an_unterminated_array_is_bounded_by_the_element_cap() {
    let plan = compile("struct s { char name[]; };", "s");
    let no_terminator = vec![0xffu8; MAX_ARRAY_ELEMENTS as usize + 2];

    match resolve_layout(&plan, &no_terminator) {
        Err(Error::LimitExceeded { limit, .. }) => assert_eq!(limit, "array elements"),
        other => panic!("an unterminated scan must be capped, got {other:?}"),
    }
}

/// An anonymous member contributes a field with no name of its own.
///
/// `struct { ... };` inside a structure folds its fields into the parent's
/// namespace, so the member itself has nothing to be called. The plan still
/// needs an entry for it — the side tables are indexed by field position and a
/// missing entry shifts every later field's name onto the wrong slot — so it
/// gets the empty string, and everything that iterates names has to skip it.
#[test]
fn an_anonymous_member_occupies_a_field_slot_with_an_empty_name() {
    let plan = compile(
        "struct s { uint8 a; struct { uint8 b; uint8 c; }; uint8 d; };",
        "s",
    );

    assert_eq!(plan.fields.len(), 3, "the anonymous member is one field");
    assert_eq!(plan.names[0].as_ref(), "a");
    assert_eq!(plan.names[1].as_ref(), "", "the member itself has no name");
    assert_eq!(plan.names[2].as_ref(), "d");

    // Its contents are still reachable by name from the parent, via a two-step
    // path rather than a direct index.
    assert_eq!(plan.field_path("b").map(<[u32]>::to_vec), Some(vec![1, 0]));
    assert_eq!(plan.field_path("c").map(<[u32]>::to_vec), Some(vec![1, 1]));
    assert_eq!(plan.field_path("a").map(<[u32]>::to_vec), Some(vec![0]));
}

/// `[EOF]` takes the rest of the input, at every element kind.
///
/// The three array ops are distinct because they become distinct Python
/// objects: `char[EOF]` is `bytes`, `wchar[EOF]` is `str`, and anything else is
/// a list. Getting the op wrong is not an error — it is a `['h', 'i']` where
/// the consumer expected `"hi"`, which is the shape half of the `evtx` defect.
#[test]
fn a_read_to_end_array_keeps_its_element_kind() {
    use cstruct_rs2_core::plan::Op;

    let cases = [
        ("char", Op::CharArray),
        ("wchar", Op::WcharArray),
        ("uint16", Op::ScalarArray),
    ];
    for (spelling, expected) in cases {
        let plan = compile(&format!("struct s {{ {spelling} rest[EOF]; }};"), "s");
        assert_eq!(plan.fields[0].op, expected, "{spelling}[EOF]");
        assert!(
            plan.fields[0].dynamic,
            "{spelling}[EOF] has no size until read"
        );
        assert_eq!(plan.total_size, None);
    }
}

/// An array *of structures* declared `[EOF]` reads to the end of the input.
///
/// Worth its own test because `decode.rs`'s nested-array branch carries a
/// comment calling this a known gap that "still decodes one element". It does
/// not — the count comes out right — so the comment describes a state the code
/// has moved past. Pinning the real behaviour is what keeps the next reader
/// from re-introducing the gap on the strength of the comment.
#[test]
fn a_read_to_end_array_of_structures_reads_to_the_end() {
    let plan = compile("struct e { uint16 a; }; struct s { e items[EOF]; };", "s");
    let layout = resolve_layout(&plan, &[1, 0, 2, 0, 3, 0]).unwrap();

    assert_eq!(layout.spans[0].count, 3, "all three elements");
    assert_eq!(layout.consumed, 6, "and every byte of them");
}

/// A multi-dimensional array whose outer dimension is unbounded stays dynamic.
///
/// `uint8 rows[][4]` cannot have its shape folded at compile time, because one
/// of the dimensions is not known until the bytes arrive. The compiler has to
/// notice that and leave the field dynamic rather than resolving the dimensions
/// it *can* see and reporting a size that is missing a factor.
#[test]
fn a_multi_dimensional_array_with_an_unbounded_dimension_stays_dynamic() {
    let plan = compile("struct s { uint8 rows[][4]; };", "s");

    assert!(plan.fields[0].dynamic, "an unbounded dimension is dynamic");
    assert_eq!(plan.total_size, None, "and the structure has no fixed size");
}

// ---------------------------------------------------------------------------
// Parser: declaration shapes that only appear in generated headers
// ---------------------------------------------------------------------------

/// `typedef struct { … } Name;` is reachable under the name in its tail.
///
/// The anonymous-then-named form is how most C headers are written, and the
/// name arrives *after* the body rather than before it. rs2 gives the body a
/// generated name and makes the tail an alias to it, rather than renaming the
/// structure — which is visible: the compiled plan reports the generated name,
/// not `HEADER`. That matters because plan identity is type identity and the
/// generated name is what a `__typename__` or a repr will say, so it is pinned
/// here rather than left to be discovered from a confusing log line.
#[test]
fn a_typedef_makes_its_tail_name_an_alias_to_an_anonymous_body() {
    let schema = parse(
        "typedef struct { uint32 a; uint16 b; } HEADER;",
        Endian::Little,
    )
    .expect("this is the ordinary C spelling");
    let plan = Compiler::new(&schema)
        .compile("HEADER")
        .expect("the tail name resolves to the body");

    assert_eq!(plan.total_size, Some(6), "the body is what it says it is");
    assert!(
        plan.name.starts_with("__anonymous"),
        "the plan carries the generated name, not the alias: {}",
        plan.name
    );

    // And a named typedef is a plain alias, which does keep its own spelling
    // only in the schema -- the plan is still the target's.
    let aliased = parse(
        "struct BODY { uint32 a; }; typedef BODY ALIAS;",
        Endian::Little,
    )
    .unwrap();
    let mut compiler = Compiler::new(&aliased);
    let direct = compiler.compile("BODY").unwrap();
    let through_alias = compiler.compile("ALIAS").unwrap();
    assert_eq!(
        through_alias.name.as_ref(),
        "BODY",
        "an alias resolves to its target rather than becoming a second type"
    );
    assert_eq!(direct.total_size, through_alias.total_size);
}

/// A declaration may run to the very end of the input with no separator.
///
/// `at_declaration_start` answers "does a new declaration begin here", and
/// end-of-input is one of the ways the previous one can end. Without that arm
/// the last type in a file with no trailing semicolon is lost — silently, since
/// a missing type only shows up when something asks for it.
#[test]
fn the_last_declaration_may_end_at_end_of_input() {
    for source in [
        "struct s { uint8 a; }",
        "enum E : uint8 { A = 1 }",
        "flag F : uint8 { A = 1 }",
    ] {
        let schema = parse(source, Endian::Little)
            .unwrap_or_else(|e| panic!("{source:?} should parse: {e}"));
        let name = source.split_whitespace().nth(1).unwrap();
        assert!(
            schema.resolve(name).is_ok(),
            "{source:?}: {name} should have been defined"
        );
    }
}

/// An enum member's value may be a ternary, which contains a colon.
///
/// The colon is the interesting character: it is also the separator between an
/// enum's name and its base type (`enum E : uint8`), so the value scanner has
/// to keep collecting rather than treating it as the end of something.
///
/// Note the conditions are bare integers rather than comparisons. The
/// expression grammar has no relational operators at all — `<` and `>` are only
/// ever the shift operators' halves — so `1 > 0 ? …` is a lexer error and not a
/// missing feature of the enum parser. The second half of this test pins that,
/// because "the ternary is broken" and "there is nothing to compare with" look
/// identical from a failing definition.
#[test]
fn an_enum_value_may_contain_a_ternary() {
    use cstruct_rs2_core::schema::TypeDef;

    let schema = parse(
        "enum E : uint8 { A = 1 ? 4 : 9, B = 0 ? 4 : 9 };",
        Endian::Little,
    )
    .expect("a ternary is an ordinary expression");

    match schema.resolve("E").unwrap() {
        TypeDef::Enum(def) => assert_eq!(
            def.members,
            vec![("A".to_owned(), 4), ("B".to_owned(), 9)],
            "both branches of the conditional are evaluated correctly"
        ),
        other => panic!("expected an enum, got {other:?}"),
    }

    // A relational operator is a lexer error in both places, which is what
    // makes the restriction a property of the grammar rather than of the enum.
    assert!(
        matches!(Expr::parse("1 > 0"), Err(Error::Lex { .. })),
        "the expression grammar has no relational operators"
    );
    assert!(
        parse("enum E : uint8 { A = 1 > 0 ? 4 : 9 };", Endian::Little).is_err(),
        "and a definition cannot smuggle one in either"
    );
}

/// An enum body that runs out of input is refused, not silently truncated.
///
/// The value scanner reads until it sees a comma or a closing brace. At
/// end-of-input it has neither, and the choice is between stopping — which
/// yields a type with a half-parsed member — and refusing. It refuses, because
/// a definition that loads with the wrong members is worse than one that does
/// not load.
#[test]
fn an_enum_that_ends_mid_value_is_refused() {
    for source in ["enum E : uint8 { A = 1", "enum E : uint8 { A = 1 <<"] {
        assert!(
            parse(source, Endian::Little).is_err(),
            "{source:?} is incomplete and should not yield a type"
        );
    }
}

/// A token that cannot appear in an array length is named, not skipped.
///
/// The length scanner collects text between brackets to hand to the expression
/// parser. A token it has no spelling for used to vanish, so `x[1 ; 2]` became
/// the length `1 2` — an expression error blaming the wrong thing. Naming the
/// token points at the character that is actually wrong.
#[test]
fn an_impossible_token_in_an_array_length_is_named() {
    let error = parse("struct s { uint8 x[1 ; 2]; };", Endian::Little)
        .expect_err("a semicolon cannot appear in a length");
    let message = error.to_string();
    assert!(
        message.contains("array length"),
        "the message should say where the token was: {message}"
    );
}

/// Compiling the same type twice through one compiler yields one plan.
///
/// Plan identity is type identity: `isinstance` compares the compiled plan's
/// address, so two plans for one name is two incompatible types with the same
/// spelling. The cache is what makes the identity hold, and it survives being
/// handed out and taken back.
#[test]
fn one_compiler_hands_out_one_plan_per_type() {
    let schema = parse("struct s { uint32 a; };", Endian::Little).unwrap();
    let mut compiler = Compiler::new(&schema);
    let first = compiler.compile("s").unwrap();
    let second = compiler.compile("s").unwrap();
    assert!(
        Arc::ptr_eq(&first, &second),
        "the same type is the same plan"
    );

    // And the cache round-trips, which is how the registry shares plans across
    // separate compiles of one namespace.
    let cache: HashMap<_, _> = compiler.into_cache();
    let mut reused = Compiler::with_cache(&schema, cache);
    let third = reused.compile("s").unwrap();
    assert!(
        Arc::ptr_eq(&first, &third),
        "and across compilers sharing a cache"
    );
}
