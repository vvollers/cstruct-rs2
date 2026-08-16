//! What the tokenizer does with the shapes real definitions are written in.
//!
//! The lexer's job is narrow -- turn definition text into tokens and know
//! nothing about which of them are legal where -- but the *edge* cases are
//! where every historical defect in it lived, and none of them were exotic.
//! Each one came from a real `dissect.*` package:
//!
//! * a `//` inside a quoted `#define` value (`dissect.executable`'s PE archive
//!   member names) truncated the literal and desynchronised the whole scan;
//! * a string literal running past its line (`b"!<arch>\n"` is a *real*
//!   newline once Python has parsed the source) read as unterminated;
//! * an identifier starting with a digit (`32BIT_MACHINE`) was refused as a bad
//!   number, which deleted the PE and ELF stack.
//!
//! So these tests are less about grammar coverage than about pinning the
//! recovery rules, because when this module is wrong the symptom is never a
//! lexer error -- it is a plugin that silently stops existing.
//!
//! Tokenising happens once per definition, so nothing here is about speed.

use cstruct_rs2_core::error::Error;
use cstruct_rs2_core::lexer::{tokenize, tokenize_public, Token, TokenKind};

/// Just the token kinds, which is what almost every assertion here is about.
fn kinds(source: &str) -> Vec<TokenKind> {
    tokenize(source)
        .expect("source should tokenize")
        .into_iter()
        .map(|token| token.kind)
        .collect()
}

fn ident(name: &str) -> TokenKind {
    TokenKind::Ident(name.to_owned())
}

/// Every punctuation token the definition language names, in one declaration.
///
/// The point is coverage of the *classification*, not of any grammar: the
/// parser is the thing that decides a `:` means a bitfield, and it can only do
/// that if the lexer hands it a distinct token rather than a generic `Punct`.
#[test]
fn each_punctuation_mark_gets_its_own_token() {
    assert_eq!(
        kinds("{}[]();,:=*"),
        vec![
            TokenKind::LBrace,
            TokenKind::RBrace,
            TokenKind::LBracket,
            TokenKind::RBracket,
            TokenKind::LParen,
            TokenKind::RParen,
            TokenKind::Semicolon,
            TokenKind::Comma,
            TokenKind::Colon,
            TokenKind::Equals,
            TokenKind::Star,
        ]
    );
}

/// Anything the language has no name for is carried verbatim rather than
/// refused.
///
/// Expression text inside `[...]` and `#define` values is reassembled from
/// tokens, so an operator the lexer dropped would change the expression rather
/// than fail it -- `[a+b]` becoming `[ab]` is a wrong length, not an error.
#[test]
fn unnamed_punctuation_is_carried_so_expressions_can_be_reassembled() {
    assert_eq!(
        kinds("+ - < > ? @"),
        vec![
            TokenKind::Punct('+'),
            TokenKind::Punct('-'),
            TokenKind::Punct('<'),
            TokenKind::Punct('>'),
            TokenKind::Punct('?'),
            TokenKind::Punct('@'),
        ]
    );
}

/// C forbids an identifier starting with a digit. cstruct definitions are not
/// compiled by a C compiler, and `dissect.executable`'s PE flag set declares a
/// member called `32BIT_MACHINE`.
///
/// The rule is "scan the run, then classify": if it parses as a number it is
/// one, otherwise it is a name.
#[test]
fn a_digit_leading_run_that_is_not_a_number_is_an_identifier() {
    assert_eq!(kinds("32BIT_MACHINE"), vec![ident("32BIT_MACHINE")]);
    assert_eq!(kinds("16"), vec![TokenKind::Number(16)]);
    // `0x` with no digits is not a number either, so it stays a name rather
    // than becoming a lexer error at this stage. The expression lexer, which
    // *does* know it is reading a number, is where `0x` is reported -- see
    // `expressions.rs`.
    assert_eq!(kinds("0x"), vec![ident("0x")]);
}

/// Every integer base and suffix a real header writes.
///
/// Octal matters more than it looks: `010` is eight, and reading it as ten is
/// silently wrong for any definition that writes permissions the way C does.
#[test]
fn integer_literals_cover_every_base_and_suffix() {
    for (source, want) in [
        ("42", 42i128),
        ("0x20", 32),
        ("0X20", 32),
        ("0b1010", 10),
        ("020", 16),
        ("0x10UL", 16),
        ("100u", 100),
        ("100L", 100),
    ] {
        assert_eq!(
            kinds(source),
            vec![TokenKind::Number(want)],
            "{source} should lex as {want}"
        );
    }
}

/// Both comment forms disappear, and a block comment may span lines.
///
/// Real definitions use block comments as section banners, so neither form can
/// be skipped and the line counter has to survive them -- a parse error that
/// names the wrong line is worse than one that names none.
#[test]
fn comments_vanish_and_line_numbers_survive_them() {
    assert_eq!(
        kinds("/* banner */ a // trailing\nb"),
        vec![ident("a"), ident("b")]
    );

    let tokens = tokenize("a\n/* two\nlines */\nb").unwrap();
    assert_eq!(tokens[0].line, 1);
    assert_eq!(tokens[1].line, 4, "the token after a two-line comment");
}

/// An unterminated block comment is an error that names where it *opened*.
///
/// Naming the opening line rather than the end of input is the useful half: the
/// reader needs to find the `/*` they forgot to close.
#[test]
fn an_unterminated_block_comment_names_its_opening_line() {
    match tokenize("a\nb\n/* oops") {
        Err(Error::Parse { line, message }) => {
            assert_eq!(line, 3);
            assert!(message.contains("unterminated"), "got {message:?}");
        }
        other => panic!("expected a parse error, got {other:?}"),
    }
}

/// `#define NAME value` becomes three tokens, with the value already classified.
#[test]
fn a_define_becomes_a_directive_a_name_and_a_value() {
    assert_eq!(
        kinds("#define MAX 0x20\n"),
        vec![ident("#define"), ident("MAX"), TokenKind::Number(32)]
    );
}

/// `#define NAME = value` is accepted, and the `=` is not part of the value.
///
/// Leaving it in produced the *string* `"=6"` where an integer was meant, so a
/// constant declared that way silently stopped being a number and could no
/// longer size an array. The `=` arrives as `Ident("=")` rather than `Equals`
/// because a directive's value is whitespace-split, which is exactly why both
/// spellings have to be handled downstream.
#[test]
fn a_define_may_be_written_with_an_equals_sign() {
    let tokens = kinds("#define B = 6\n");
    assert_eq!(tokens[0], ident("#define"));
    assert_eq!(tokens[1], ident("B"));
    // Whatever the middle token is, the *value* must survive as a number.
    assert!(
        tokens.contains(&TokenKind::Number(6)),
        "the value must still lex as the integer 6, got {tokens:?}"
    );
}

/// A trailing comment on a directive line is stripped before the value is read.
///
/// Without this, `#define EXT2_SBOFF 1024 // offset to superblock` reads the
/// comment's words as further value tokens.
#[test]
fn a_trailing_comment_does_not_become_part_of_a_define_value() {
    assert_eq!(
        kinds("#define EXT2_SBOFF 1024 // offset to superblock\n"),
        vec![
            ident("#define"),
            ident("EXT2_SBOFF"),
            TokenKind::Number(1024),
        ]
    );
}

/// A `/*` opened on a directive line may close several lines later.
///
/// `dissect.jffs` annotates its constants that way. The directive itself ends
/// at the newline, but the *scanner* has to resume after the comment closes or
/// the comment's prose is lexed as declarations.
#[test]
fn a_block_comment_opened_on_a_define_line_is_skipped_entirely() {
    assert_eq!(
        kinds("#define A 1 /* this comment\n   runs on for a while */\n#define B 2\n"),
        vec![
            ident("#define"),
            ident("A"),
            TokenKind::Number(1),
            ident("#define"),
            ident("B"),
            TokenKind::Number(2),
        ]
    );
}

/// A `//` *inside* a quoted value is part of the value, not a comment.
///
/// `#define IMAGE_ARCHIVE_LONGNAMES_MEMBER b"//              "` is a real
/// constant in `dissect.executable`. Cutting at the `//` truncates the literal
/// to `b"`, and the scanner then runs to whatever quote it finds next -- which
/// loses the rest of the file rather than one constant.
#[test]
fn a_comment_marker_inside_a_quoted_define_value_is_not_a_comment() {
    let tokens = kinds("#define LONGNAMES b\"//              \"\n#define AFTER 7\n");
    assert_eq!(tokens[2], TokenKind::Bytes(b"//              ".to_vec()));
    assert_eq!(
        tokens[5],
        TokenKind::Number(7),
        "the declaration after the literal must still be reached"
    );
}

/// A quoted `#define` value keeps its interior spaces.
///
/// Splitting the value on whitespace turns one constant into several tokens and
/// an error. `IMAGE_ARCHIVE_LINKER_MEMBER` has fifteen spaces inside its quotes.
#[test]
fn a_quoted_define_value_is_not_whitespace_split() {
    assert_eq!(
        kinds("#define LINKER b\"/               \"\n")[2],
        TokenKind::Bytes(b"/               ".to_vec())
    );
    assert_eq!(
        kinds("#define PREFIX \"gnu.\"\n")[2],
        TokenKind::Str("gnu.".to_owned())
    );
}

/// An *unquoted* value is still whitespace-split, because expression constants
/// rely on it: `#define PAD (1 << 15)` has to arrive as several tokens for the
/// parser to reassemble.
#[test]
fn an_unquoted_define_value_keeps_its_parts() {
    assert_eq!(
        kinds("#define PAD (1 << 15)\n"),
        vec![
            ident("#define"),
            ident("PAD"),
            ident("(1"),
            ident("<<"),
            ident("15)"),
        ]
    );
}

/// A literal may run past the end of its line.
///
/// Dissect's definitions are ordinary Python strings, so `b"!<arch>\n"` in the
/// source contains a *real* newline by the time the lexer sees it and the
/// closing quote lands on the next line. Stopping at the newline reads the
/// value as unterminated and loses the whole PE definition.
#[test]
fn a_define_literal_may_span_lines() {
    let tokens = kinds("#define ARCHIVE_START b\"!<arch>\n\"\n#define AFTER 3\n");
    assert_eq!(tokens[2], TokenKind::Bytes(b"!<arch>\n".to_vec()));
    assert_eq!(tokens[5], TokenKind::Number(3));
}

#[test]
fn a_single_quoted_define_literal_may_span_lines() {
    let tokens = tokenize("#define NEWLINE '\n'\n").unwrap();

    assert_eq!(tokens[2].kind, TokenKind::Str("\n".to_owned()));
}

/// A literal that never closes is an error rather than a silent run to the end
/// of the file.
#[test]
fn an_unterminated_string_literal_is_an_error() {
    assert!(matches!(
        tokenize("#define A \"never closed\n#define B 1\n"),
        Err(Error::Parse { .. })
    ));
    assert!(matches!(
        tokenize("struct s { char a[\"oops; };"),
        Err(Error::Parse { .. })
    ));
}

/// Escape sequences are resolved by the lexer, not carried as text.
///
/// Code comparing a constant against `b"\n"` needs one byte, not a backslash
/// followed by an `n`.
///
/// `\xNN` is the only numeric escape. A malformed one degrades to a literal `x`
/// rather than failing the load: a constant nothing reads should not be able to
/// make a whole definition unusable.
#[test]
fn escape_sequences_are_resolved_in_both_literal_forms() {
    assert_eq!(
        kinds("#define E b\"\\n\\r\\t\\0\\\\\\\"\\'\\x41\"\n")[2],
        TokenKind::Bytes(vec![b'\n', b'\r', b'\t', 0, b'\\', b'"', b'\'', b'A'])
    );
    // A text literal resolves the same escapes and becomes a `Str`.
    assert_eq!(
        kinds("#define T \"a\\tb\"\n")[2],
        TokenKind::Str("a\tb".to_owned())
    );
    // `\q` has no meaning; the character is kept rather than the load failing.
    assert_eq!(
        kinds("#define U \"a\\qb\"\n")[2],
        TokenKind::Str("aqb".to_owned())
    );
    // `\x` with no usable hex digits keeps a literal `x`.
    assert_eq!(
        kinds("#define V \"a\\xzz\"\n")[2],
        TokenKind::Str("axzz".to_owned())
    );
}

/// A directive that is not `#define` is skipped to end of line.
///
/// Skipping rather than refusing is the point: `#ifdef` in a definition is not
/// something this implementation acts on, and misparsing it as a field would be
/// far worse than ignoring it.
#[test]
fn include_is_retained_while_unrelated_directives_are_skipped() {
    assert_eq!(
        kinds("#pragma once\n#include <stdint.h>\nstruct s"),
        vec![
            ident("#include"),
            TokenKind::Str("<stdint.h>".to_owned()),
            ident("struct"),
            ident("s"),
        ]
    );
}

/// A `#define` with no value at all produces just the directive and the name.
///
/// The parser is what decides this is an error; the lexer's contract is only
/// that it does not invent a value or run into the next declaration.
#[test]
fn a_valueless_define_stops_at_its_name() {
    assert_eq!(
        kinds("#define GUARD\n"),
        vec![ident("#define"), ident("GUARD")]
    );
}

/// Tokens carry the line they started on, which is the whole basis of a useful
/// parse error.
#[test]
fn every_token_carries_its_line() {
    let tokens = tokenize("struct s {\n  uint32 a;\n};").unwrap();
    let lines: Vec<(TokenKind, u32)> = tokenize("struct s {\n  uint32 a;\n};")
        .unwrap()
        .into_iter()
        .map(|Token { kind, line, .. }| (kind, line))
        .collect();
    assert_eq!(lines[0], (ident("struct"), 1));
    assert_eq!(lines[3], (ident("uint32"), 2));
    assert_eq!(lines[6], (TokenKind::RBrace, 3));
    assert_eq!(tokens.len(), lines.len());
}

/// Empty and whitespace-only input is not an error.
///
/// `cs.load("")` is a legal no-op and consumers do it when a definition is
/// assembled conditionally.
#[test]
fn empty_input_produces_no_tokens() {
    assert!(kinds("").is_empty());
    assert!(kinds("   \n\t\n  ").is_empty());
    assert!(kinds("// only a comment\n").is_empty());
}

/// The public token stream retains source spellings that the semantic parser
/// has already reduced to values.
#[test]
fn public_tokens_preserve_spelling_and_group_macro_values() {
    let tokens =
        tokenize_public("0xff 0755 3.14 a << b\n#define FOO (1 + 2)\n#[one, two]").unwrap();
    let values = tokens
        .iter()
        .map(|token| (token.kind.as_str(), token.value.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        values,
        [
            ("NUMBER", "0xff"),
            ("NUMBER", "0o755"),
            ("NUMBER", "3.14"),
            ("IDENTIFIER", "a"),
            ("LSHIFT", "<<"),
            ("IDENTIFIER", "b"),
            ("PP_DEFINE", "define"),
            ("IDENTIFIER", "FOO"),
            ("STRING", "(1 + 2)"),
            ("PP_FLAGS", "one, two"),
        ]
    );
}

/// Public lexer errors use their own exception category and point at the
/// source line, while definition loading retains its more tolerant header
/// rules.
#[test]
fn public_token_errors_are_lexer_errors_with_context() {
    for (source, phrase) in [
        ("0888", "invalid octal literal"),
        ("#include <missing", "unexpected end of input"),
        ("#unknown", "unknown preprocessor directive"),
        ("a\n@bad", "line 2: unexpected character '@'"),
    ] {
        match tokenize_public(source) {
            Err(Error::Lex { message }) => assert!(message.contains(phrase), "{message}"),
            other => panic!("expected a public lexer error, got {other:?}"),
        }
    }
}
