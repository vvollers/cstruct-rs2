//! Turns definition text into tokens.
//!
//! The definition language is a small C subset: comments, `#define`, `typedef`,
//! `struct`, `union`, `enum`, `flag`, field declarations, and array brackets.
//! The lexer knows nothing about which of those are legal where — that is the
//! parser's job — and exists mainly so the parser never has to think about
//! whitespace or comments.
//!
//! Tokenising happens once when a definition is loaded, never per record, so
//! this is written for clarity rather than speed.

use crate::error::{Error, Result};

/// A lexical token, with its source location for errors and the public façade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// What the token is.
    pub kind: TokenKind,
    /// 1-based source line, carried so a parse error can name it.
    pub line: u32,
    /// 1-based source column where the token starts.
    pub column: u32,
}

/// The kinds of token the definition language has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    /// An identifier or keyword. Keywords are not distinguished here; the
    /// parser matches on the text, because `struct` is a keyword in one
    /// position and a perfectly good field name in another.
    Ident(String),
    /// An integer literal, already converted from decimal, hex, or octal.
    Number(i128),
    /// A double-quoted string literal, with the quotes removed.
    ///
    /// Only `#define`s use these -- `#define XATTR_HURD_PREFIX "gnu."` in
    /// `dissect.extfs`. They can never be an array length, but upstream exposes
    /// them as module constants, so they are carried rather than rejected.
    Str(String),
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `;`
    Semicolon,
    /// `,`
    Comma,
    /// `:`
    Colon,
    /// A `b"..."` byte-string literal, escapes already resolved.
    Bytes(Vec<u8>),
    /// `=`
    Equals,
    /// `*`
    Star,
    /// Any other punctuation, kept so an expression can be reassembled
    /// verbatim for later evaluation.
    Punct(char),
}

impl TokenKind {
    /// The token as it was *written*, for error messages a caller will read.
    ///
    /// Use this instead of `{:?}` in any message that can escape as an
    /// exception. A `Debug` rendering names this crate's own types — `expected
    /// ',' or ';' after field, found Semicolon` is a sentence about the
    /// implementation, not about the definition somebody wrote.
    ///
    /// Results are quoted so the token reads as a fragment of the input rather
    /// than as a word in the sentence around it.
    pub fn describe(&self) -> String {
        match self {
            Self::Ident(name) => format!("'{name}'"),
            Self::Number(value) => format!("'{value}'"),
            Self::Str(text) => format!("a string literal \"{text}\""),
            Self::Bytes(_) => "a byte-string literal".to_owned(),
            Self::LBrace => "'{'".to_owned(),
            Self::RBrace => "'}'".to_owned(),
            Self::LBracket => "'['".to_owned(),
            Self::RBracket => "']'".to_owned(),
            Self::LParen => "'('".to_owned(),
            Self::RParen => "')'".to_owned(),
            Self::Semicolon => "';'".to_owned(),
            Self::Comma => "','".to_owned(),
            Self::Colon => "':'".to_owned(),
            Self::Equals => "'='".to_owned(),
            Self::Star => "'*'".to_owned(),
            Self::Punct(ch) => format!("'{ch}'"),
        }
    }

    /// Public token category and value used by `dissect.cstruct.lexer`.
    ///
    /// The Rust parser deliberately keeps keywords as identifiers. The Python
    /// façade exposes the categories upstream callers expect without changing
    /// that simpler internal representation.
    pub fn public_parts(&self) -> (&'static str, String) {
        match self {
            Self::Ident(value) => {
                let category = match value.as_str() {
                    "struct" => "STRUCT",
                    "union" => "UNION",
                    "enum" => "ENUM",
                    "flag" => "FLAG",
                    "typedef" => "TYPEDEF",
                    "sizeof" => "SIZEOF",
                    "offsetof" => "OFFSETOF",
                    "#define" => "PP_DEFINE",
                    "#undef" => "PP_UNDEF",
                    "#ifdef" => "PP_IFDEF",
                    "#ifndef" => "PP_IFNDEF",
                    "#else" => "PP_ELSE",
                    "#endif" => "PP_ENDIF",
                    "#include" => "PP_INCLUDE",
                    "#[flags]" => "PP_FLAGS",
                    _ => "IDENTIFIER",
                };
                let public_value = value
                    .strip_prefix('#')
                    .unwrap_or(value)
                    .trim_matches(['[', ']'])
                    .to_owned();
                (category, public_value)
            }
            Self::Number(value) => ("NUMBER", value.to_string()),
            Self::Str(value) => ("STRING", value.clone()),
            Self::Bytes(value) => ("BYTES", format!("b{:?}", String::from_utf8_lossy(value))),
            Self::LBrace => ("LBRACE", "{".to_owned()),
            Self::RBrace => ("RBRACE", "}".to_owned()),
            Self::LBracket => ("LBRACKET", "[".to_owned()),
            Self::RBracket => ("RBRACKET", "]".to_owned()),
            Self::LParen => ("LPAREN", "(".to_owned()),
            Self::RParen => ("RPAREN", ")".to_owned()),
            Self::Semicolon => ("SEMICOLON", ";".to_owned()),
            Self::Comma => ("COMMA", ",".to_owned()),
            Self::Colon => ("COLON", ":".to_owned()),
            Self::Equals => ("EQUALS", "=".to_owned()),
            Self::Star => ("STAR", "*".to_owned()),
            Self::Punct(value) => {
                let category = match value {
                    '+' => "PLUS",
                    '-' => "MINUS",
                    '/' => "SLASH",
                    '%' => "PERCENT",
                    '&' => "AMPERSAND",
                    '|' => "PIPE",
                    '^' => "CARET",
                    '~' => "TILDE",
                    '?' => "QUESTION",
                    _ => "PUNCT",
                };
                (category, value.to_string())
            }
        }
    }
}

/// The same rendering for a token that may not be there.
///
/// End of input is spelled as words rather than as `None`. `found None` is a
/// Rust `Option` surfacing in a Python traceback, which tells the reader nothing
/// about their definition.
pub fn describe_optional(kind: Option<&TokenKind>) -> String {
    match kind {
        Some(kind) => kind.describe(),
        None => "end of input".to_owned(),
    }
}

/// Tokenise a complete definition.
pub fn tokenize(source: &str) -> Result<Vec<Token>> {
    tokenize_with_locations(source, false)
}

/// Tokenise while retaining columns for the public inspection API.
fn tokenize_with_locations(source: &str, retain_columns: bool) -> Result<Vec<Token>> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0usize;
    let mut line = 1u32;

    while i < bytes.len() {
        let c = bytes[i] as char;

        // Whitespace.
        if c == '\n' {
            line += 1;
            i += 1;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            continue;
        }

        // Comments. Both forms appear in real definitions, often as section
        // banners, so neither can be skipped.
        if c == '/' && i + 1 < bytes.len() {
            match bytes[i + 1] as char {
                '/' => {
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                    continue;
                }
                '*' => {
                    let start_line = line;
                    i += 2;
                    loop {
                        if i + 1 >= bytes.len() {
                            return Err(Error::Parse {
                                line: start_line,
                                message: "unterminated block comment".to_owned(),
                            });
                        }
                        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                            i += 2;
                            break;
                        }
                        if bytes[i] == b'\n' {
                            line += 1;
                        }
                        i += 1;
                    }
                    continue;
                }
                _ => {}
            }
        }

        // Preprocessor directives stay in the token stream. The parser filters
        // conditionals before reading declarations, so directives inside a
        // structure body follow the same path as top-level directives.
        if c == '#' {
            let start = i;
            let start_line = line;
            let start_column = token_column(source, start, retain_columns);
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            let mut directive = &source[start..i];
            // Strip a trailing comment before splitting. Real definitions
            // annotate their constants -- `#define EXT2_SBOFF 1024 // offset
            // to superblock` -- and without this the comment words are read as
            // part of the directive.
            if let Some(idx) = comment_start(directive) {
                // A `/*` opened on a directive line may close several lines
                // later: `dissect.jffs` annotates its constants that way. The
                // directive itself ends at the newline, but the *scanner* has
                // to resume after the comment closes, or the comment's prose is
                // lexed as tokens.
                if directive[idx..].starts_with("/*") && !directive[idx..].contains("*/") {
                    if let Some(end) = source[start + idx..].find("*/") {
                        let absolute = start + idx + end + 2;
                        line += source[start + idx..absolute].matches('\n').count() as u32;
                        i = absolute;
                    }
                }
                directive = &directive[..idx];
            }
            let trimmed = directive.trim();
            if let Some(flags) = trimmed
                .strip_prefix("#[")
                .and_then(|rest| rest.strip_suffix(']'))
            {
                tokens.push(Token {
                    kind: TokenKind::Ident("#[flags]".to_owned()),
                    line: start_line,
                    column: start_column,
                });
                tokens.push(Token {
                    kind: TokenKind::Str(flags.to_owned()),
                    line: start_line,
                    column: start_column + 2,
                });
                continue;
            }

            let without_hash = trimmed.strip_prefix('#').unwrap_or(trimmed).trim_start();
            let mut parts = without_hash.split_whitespace();
            let keyword = parts.next().unwrap_or("");
            let head = format!("#{keyword}");
            let supported = matches!(
                keyword,
                "define" | "undef" | "ifdef" | "ifndef" | "else" | "endif" | "include"
            );
            if !supported {
                // Real header fragments contain directives such as `#pragma`
                // that have no cstruct meaning. Definition loading has always
                // ignored them here; retaining that tolerance matters more than
                // mirroring upstream's private Lexer error for an unused line.
                continue;
            }
            tokens.push(Token {
                kind: TokenKind::Ident(head.clone()),
                line: start_line,
                column: start_column,
            });

            if keyword == "define" {
                if let Some(raw_name) = parts.next() {
                    let (name, signature) = raw_name
                        .split_once('(')
                        .map_or((raw_name, None), |(name, rest)| {
                            (name, Some(format!("({rest}")))
                        });
                    tokens.push(Token {
                        kind: classify_word(name),
                        line: start_line,
                        column: start_column + 8,
                    });
                    // The value is whatever follows the name on this line.
                    //
                    // It is *not* split on whitespace when it is a quoted
                    // literal: `#define IMAGE_ARCHIVE_LINKER_MEMBER
                    // b"/               "` has fifteen spaces inside the quotes,
                    // and splitting turns one constant into two tokens and an
                    // error. Unquoted values keep splitting, because expression
                    // constants like `(1 << 15)` rely on it.
                    // `#define NAME = value` is accepted as well as
                    // `#define NAME value`. The `=` is stripped here rather
                    // than in the parser because this is where the value's
                    // text is decided: left in place it was swept into the
                    // value, so `#define B = 6` produced the *string* `"=6"`
                    // instead of the integer 6, and a constant defined that way
                    // silently stopped being a number.
                    let after_keyword = without_hash
                        .strip_prefix(keyword)
                        .unwrap_or(without_hash)
                        .trim_start();
                    let after_name = after_keyword
                        .strip_prefix(raw_name)
                        .map(str::trim)
                        .unwrap_or("")
                        .trim_start_matches('=')
                        .trim();
                    if let Some(signature) = signature {
                        let value = if after_name.is_empty() {
                            signature
                        } else {
                            format!("{signature} {after_name}")
                        };
                        tokens.push(Token {
                            kind: TokenKind::Str(value),
                            line: start_line,
                            column: start_column + 8 + name.len() as u32,
                        });
                        continue;
                    }
                    let opening = if after_name.starts_with("b\"") || after_name.starts_with("b'") {
                        2
                    } else if after_name.starts_with(['"', '\'']) {
                        1
                    } else {
                        0
                    };
                    if opening > 0 {
                        // A literal may run past the end of its line. Dissect's
                        // definitions are ordinary Python strings, so
                        // `b"!<arch>\n"` in the source becomes a real newline in
                        // the definition and the closing quote lands on the next
                        // line. Stopping at the newline reads the value as
                        // unterminated and loses the whole PE definition.
                        let quote = after_name.as_bytes()[opening - 1] as char;
                        let closes_here = after_name[opening..].contains(quote);
                        if closes_here {
                            tokens.push(Token {
                                kind: classify_word(after_name),
                                line: start_line,
                                column: start_column + 8 + raw_name.len() as u32 + 1,
                            });
                        } else {
                            let value_start =
                                after_name.as_ptr() as usize - source.as_ptr() as usize;
                            let mut end = i;
                            while end < bytes.len() && bytes[end] != quote as u8 {
                                if bytes[end] == b'\n' {
                                    line += 1;
                                }
                                end += 1;
                            }
                            if end >= bytes.len() {
                                return Err(Error::Parse {
                                    line,
                                    message: "unterminated string literal".to_owned(),
                                });
                            }
                            tokens.push(Token {
                                kind: classify_word(&source[value_start..=end]),
                                line: start_line,
                                column: start_column + 8 + raw_name.len() as u32 + 1,
                            });
                            i = end + 1;
                            continue;
                        }
                    } else {
                        for part in after_name.split_whitespace() {
                            tokens.push(Token {
                                kind: classify_word(part),
                                line: start_line,
                                column: start_column + 8 + raw_name.len() as u32 + 1,
                            });
                        }
                    }
                }
            } else if matches!(keyword, "undef" | "ifdef" | "ifndef") {
                if let Some(name) = parts.next() {
                    tokens.push(Token {
                        kind: TokenKind::Ident(name.to_owned()),
                        line: start_line,
                        column: start_column + keyword.len() as u32 + 2,
                    });
                }
            } else if keyword == "include" {
                if let Some(path) = parts.next() {
                    let value = if path.starts_with('<') && path.ends_with('>') {
                        path.to_owned()
                    } else {
                        path.trim_matches(['\"', '\'']).to_owned()
                    };
                    tokens.push(Token {
                        kind: TokenKind::Str(value),
                        line: start_line,
                        column: start_column + 9,
                    });
                }
            }
            continue;
        }

        // String and byte-string literals. Only `#define` produces them.
        //
        // `b"!<arch>\n"` is a *bytes* constant: `dissect.executable`'s PE reader
        // declares five of them and compares archive headers against the value.
        // Both forms resolve their escapes here rather than carrying the raw
        // text, because a consumer comparing against `b"\n"` needs one byte and
        // not two characters.
        let is_bytes_literal =
            matches!(c, 'b' | 'B') && matches!(bytes.get(i + 1), Some(b'"' | b'\''));
        let quote = if is_bytes_literal {
            bytes[i + 1]
        } else {
            bytes[i]
        };
        if matches!(c, '"' | '\'') || is_bytes_literal {
            let token_start = i;
            let start_line = line;
            i += if is_bytes_literal { 2 } else { 1 };
            let mut raw = Vec::new();
            let mut terminated = false;
            while i < bytes.len() {
                match bytes[i] {
                    byte if byte == quote => {
                        terminated = true;
                        i += 1;
                        break;
                    }
                    b'\\' => {
                        i += 1;
                        let Some(escape) = bytes.get(i) else { break };
                        match escape {
                            b'n' => raw.push(b'\n'),
                            b'r' => raw.push(b'\r'),
                            b't' => raw.push(b'\t'),
                            b'0' => raw.push(0),
                            b'\\' => raw.push(b'\\'),
                            b'"' => raw.push(b'"'),
                            b'\'' => raw.push(b'\''),
                            // `\xNN`. The only numeric escape supported.
                            b'x' => {
                                let hex = source
                                    .get(i + 1..i + 3)
                                    .and_then(|text| u8::from_str_radix(text, 16).ok());
                                match hex {
                                    Some(byte) => {
                                        raw.push(byte);
                                        i += 2;
                                    }
                                    None => raw.push(b'x'),
                                }
                            }
                            other => raw.push(*other),
                        }
                        i += 1;
                    }
                    byte => {
                        if byte == b'\n' {
                            line += 1;
                        }
                        raw.push(byte);
                        i += 1;
                    }
                }
            }
            if !terminated {
                return Err(Error::Parse {
                    line: start_line,
                    message: "unterminated string literal".to_owned(),
                });
            }
            tokens.push(Token {
                kind: if is_bytes_literal {
                    TokenKind::Bytes(raw)
                } else {
                    // A text literal keeps its characters; invalid UTF-8 cannot
                    // arise because the source itself is a `&str`.
                    TokenKind::Str(String::from_utf8_lossy(&raw).into_owned())
                },
                line: start_line,
                column: token_column(source, token_start, retain_columns),
            });
            continue;
        }

        // Identifiers and keywords.
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < bytes.len() {
                let ch = bytes[i] as char;
                if ch.is_ascii_alphanumeric() || ch == '_' {
                    i += 1;
                } else {
                    break;
                }
            }
            tokens.push(Token {
                kind: TokenKind::Ident(source[start..i].to_owned()),
                line,
                column: token_column(source, start, retain_columns),
            });
            continue;
        }

        // Numbers -- or an identifier that happens to start with a digit.
        //
        // C forbids the latter, but cstruct definitions are not compiled by a C
        // compiler: `dissect.executable`'s PE flags declare a member called
        // `32BIT_MACHINE`, and rejecting it removed the whole PE and ELF stack.
        // The run is scanned first and classified afterwards: if it parses as a
        // number it is one, otherwise it is a name.
        if c.is_ascii_digit() {
            let start = i;
            while i < bytes.len() {
                let ch = bytes[i] as char;
                if ch.is_ascii_alphanumeric() || ch == '_' {
                    i += 1;
                } else {
                    break;
                }
            }
            let text = &source[start..i];
            if parse_int(text).is_none() {
                tokens.push(Token {
                    kind: TokenKind::Ident(text.to_owned()),
                    line,
                    column: token_column(source, start, retain_columns),
                });
                continue;
            }
            let value = parse_int(text).ok_or_else(|| Error::Parse {
                line,
                message: format!("invalid integer literal: {text}"),
            })?;
            tokens.push(Token {
                kind: TokenKind::Number(value),
                line,
                column: token_column(source, start, retain_columns),
            });
            continue;
        }

        let kind = match c {
            '{' => TokenKind::LBrace,
            '}' => TokenKind::RBrace,
            '[' => TokenKind::LBracket,
            ']' => TokenKind::RBracket,
            '(' => TokenKind::LParen,
            ')' => TokenKind::RParen,
            ';' => TokenKind::Semicolon,
            ',' => TokenKind::Comma,
            ':' => TokenKind::Colon,
            '=' => TokenKind::Equals,
            '*' => TokenKind::Star,
            other => TokenKind::Punct(other),
        };
        tokens.push(Token {
            kind,
            line,
            column: token_column(source, i, retain_columns),
        });
        i += 1;
    }

    Ok(tokens)
}

/// One token exposed by the public Python lexer façade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicToken {
    /// Upstream-compatible ``TokenType`` member name.
    pub kind: String,
    /// Source-facing value retained by that token type.
    pub value: String,
    /// 1-based source line.
    pub line: u32,
    /// 1-based source column.
    pub column: u32,
}

/// Tokenise for callers that inspect definitions rather than load them.
///
/// Loading uses the compact semantic tokens above. The public stream retains
/// spellings that do not matter to parsing, such as whether a hexadecimal
/// literal used an upper-case prefix, and groups a macro value into the one
/// string token current upstream exposes. Both views come from this one native
/// scan; the Python façade does not contain another lexer.
pub fn tokenize_public(source: &str) -> Result<Vec<PublicToken>> {
    validate_public_directives(source)?;
    let tokens = tokenize_with_locations(source, true)
        .map_err(|error| public_tokenize_error(source, error))?;
    let mut result = Vec::with_capacity(tokens.len() + 1);
    let mut index = 0usize;

    while index < tokens.len() {
        let token = &tokens[index];
        let (kind, mut value) = token.kind.public_parts();

        if kind == "PP_FLAGS" {
            let flags = tokens
                .get(index + 1)
                .and_then(|next| match &next.kind {
                    TokenKind::Str(value) => Some(value.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            result.push(public_token(token, kind, flags));
            index += 2;
            continue;
        }

        if kind == "PP_DEFINE" {
            result.push(public_token(token, kind, value));
            if let Some(name) = tokens.get(index + 1) {
                let TokenKind::Ident(name_value) = &name.kind else {
                    return Err(public_lex_error(
                        source,
                        token.line,
                        "expected identifier after '#define'",
                    ));
                };
                result.push(public_token(name, "IDENTIFIER", name_value.clone()));
                let line = source_line(source, token.line);
                let start = name.column.saturating_sub(1) as usize + name_value.len();
                let raw = line.get(start..).unwrap_or_default().trim();
                if !raw.is_empty() {
                    result.push(PublicToken {
                        kind: "STRING".to_owned(),
                        value: raw.to_owned(),
                        line: token.line,
                        column: (start + 1) as u32,
                    });
                }
                index += 2;
                while index < tokens.len() && tokens[index].line == token.line {
                    index += 1;
                }
                continue;
            }
            return Err(public_lex_error(
                source,
                token.line,
                "expected identifier after '#define'",
            ));
        }

        if kind == "PP_INCLUDE"
            && !matches!(
                tokens.get(index + 1).map(|next| &next.kind),
                Some(TokenKind::Str(_))
            )
        {
            return Err(public_lex_error(
                source,
                token.line,
                "unexpected end of input",
            ));
        }

        if matches!(token.kind, TokenKind::Number(_)) {
            value = public_number_spelling(source, token);
            if matches!(
                tokens.get(index + 1).map(|next| &next.kind),
                Some(TokenKind::Punct('.'))
            ) {
                value = raw_number_run(source, token);
                index += if matches!(
                    tokens.get(index + 2).map(|next| &next.kind),
                    Some(TokenKind::Number(_))
                ) {
                    3
                } else {
                    2
                };
                result.push(public_token(token, "NUMBER", value));
                continue;
            }
        }

        if let TokenKind::Ident(raw) = &token.kind {
            validate_public_numeric_identifier(source, token, raw)?;
        }

        if matches!(token.kind, TokenKind::Bytes(_)) {
            let raw = raw_quoted_value(source, token).unwrap_or_default();
            value = format!("b'{raw}'");
        } else if matches!(token.kind, TokenKind::Str(_))
            && !matches!(tokens.get(index.wrapping_sub(1)).map(|previous| &previous.kind), Some(TokenKind::Ident(directive)) if directive == "#include")
        {
            if let Some(raw) = raw_quoted_value(source, token) {
                value = raw;
            }
        }

        if matches!(token.kind, TokenKind::Punct('<' | '>'))
            && tokens.get(index + 1).is_some_and(|next| {
                next.line == token.line
                    && next.column == token.column + 1
                    && next.kind == token.kind
            })
        {
            let shift = if matches!(token.kind, TokenKind::Punct('<')) {
                "LSHIFT"
            } else {
                "RSHIFT"
            };
            result.push(public_token(token, shift, value.repeat(2)));
            index += 2;
            continue;
        }

        let kind = if matches!(token.kind, TokenKind::Punct('-')) {
            match result.last().map(|previous| previous.kind.as_str()) {
                Some("IDENTIFIER" | "NUMBER" | "RPAREN" | "RBRACKET") => "MINUS",
                _ => "UNARY_MINUS",
            }
        } else {
            kind
        };

        if kind == "PUNCT" {
            return Err(public_lex_error(
                source,
                token.line,
                &format!("unexpected character '{value}'"),
            ));
        }
        result.push(public_token(token, kind, value));
        index += 1;
    }

    Ok(result)
}

fn public_token(token: &Token, kind: &str, value: String) -> PublicToken {
    PublicToken {
        kind: kind.to_owned(),
        value,
        line: token.line,
        column: token.column,
    }
}

fn validate_public_directives(source: &str) -> Result<()> {
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix('#') else {
            continue;
        };
        if rest.starts_with('[') {
            continue;
        }
        let keyword = rest.split_whitespace().next().unwrap_or("");
        if !matches!(
            keyword,
            "define" | "undef" | "ifdef" | "ifndef" | "else" | "endif" | "include"
        ) {
            return Err(public_lex_error(
                source,
                index as u32 + 1,
                &format!("unknown preprocessor directive '#{keyword}'"),
            ));
        }
        if keyword == "include" {
            let path = rest
                .trim_start()
                .strip_prefix(keyword)
                .unwrap_or_default()
                .trim();
            let complete = (path.starts_with('<') && path.ends_with('>'))
                || (path.starts_with('"') && path.ends_with('"'))
                || (path.starts_with('\'') && path.ends_with('\''));
            if !complete {
                return Err(public_lex_error(
                    source,
                    index as u32 + 1,
                    "unexpected end of input",
                ));
            }
        }
    }
    Ok(())
}

fn validate_public_numeric_identifier(source: &str, token: &Token, raw: &str) -> Result<()> {
    let lower = raw.to_ascii_lowercase();
    let message = if matches!(lower.as_str(), "0x") {
        Some("invalid hexadecimal literal")
    } else if matches!(lower.as_str(), "0b") {
        Some("invalid binary literal")
    } else if raw.len() > 1
        && raw.starts_with('0')
        && raw.chars().all(|character| character.is_ascii_digit())
    {
        Some("invalid octal literal")
    } else {
        None
    };
    match message {
        Some(message) => Err(public_lex_error(source, token.line, message)),
        None => Ok(()),
    }
}

fn public_tokenize_error(source: &str, error: Error) -> Error {
    match error {
        Error::Parse { line, message } => public_lex_error(
            source,
            line,
            if message.contains("unterminated string literal") {
                "unexpected end of input"
            } else {
                &message
            },
        ),
        other => other,
    }
}

fn public_lex_error(source: &str, line: u32, message: &str) -> Error {
    let lines = source.lines().collect::<Vec<_>>();
    let end = (line as usize).min(lines.len());
    let start = end.saturating_sub(3);
    let width = line.to_string().len() + 2;
    let context = lines[start..end]
        .iter()
        .enumerate()
        .map(|(index, text)| format!("{:>width$}: {text}", start + index + 1))
        .collect::<Vec<_>>()
        .join("\n");
    let message = if context.is_empty() {
        format!("line {line}: {message}")
    } else {
        format!("line {line}: {message}\n{context}")
    };
    Error::Lex { message }
}

fn source_line(source: &str, line: u32) -> &str {
    source
        .lines()
        .nth(line.saturating_sub(1) as usize)
        .unwrap_or("")
}

fn byte_offset(source: &str, line: u32, column: u32) -> Option<usize> {
    let mut offset = 0usize;
    for _ in 1..line {
        offset += source.get(offset..)?.find('\n')? + 1;
    }
    let line_text = source.get(offset..)?;
    let column_bytes = line_text
        .char_indices()
        .nth(column.saturating_sub(1) as usize)
        .map_or_else(|| line_text.len(), |(index, _)| index);
    Some(offset + column_bytes)
}

fn raw_number_run(source: &str, token: &Token) -> String {
    let Some(start) = byte_offset(source, token.line, token.column) else {
        return String::new();
    };
    source[start..]
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric() || *character == '.')
        .collect()
}

fn public_number_spelling(source: &str, token: &Token) -> String {
    let raw = raw_number_run(source, token);
    let clean = raw.trim_end_matches(['u', 'U', 'l', 'L']);
    if clean.len() > 1
        && clean.starts_with('0')
        && clean.chars().all(|character| character.is_digit(8))
    {
        format!("0o{}", &clean[1..])
    } else {
        clean.to_owned()
    }
}

fn raw_quoted_value(source: &str, token: &Token) -> Option<String> {
    let mut start = byte_offset(source, token.line, token.column)?;
    if matches!(source.as_bytes().get(start), Some(b'b' | b'B')) {
        start += 1;
    }
    let quote = *source.as_bytes().get(start)?;
    if !matches!(quote, b'\'' | b'"') {
        return None;
    }
    let content_start = start + 1;
    let mut end = content_start;
    while end < source.len() {
        if source.as_bytes()[end] == b'\\' {
            end = (end + 2).min(source.len());
            continue;
        }
        if source.as_bytes()[end] == quote {
            return Some(source[content_start..end].to_owned());
        }
        end += 1;
    }
    None
}

/// Return the 1-based column for one UTF-8 byte offset.
///
/// Definition identifiers are ASCII, while comments and string values may not
/// be. Counting characters after the previous newline keeps public locations
/// meaningful without putting position bookkeeping through every lexer arm.
fn column_at(source: &str, byte_offset: usize) -> u32 {
    let line_start = source[..byte_offset]
        .rfind('\n')
        .map_or(0, |position| position + 1);
    source[line_start..byte_offset].chars().count() as u32 + 1
}

/// Skip column scans on the ordinary definition-loading path.
fn token_column(source: &str, byte_offset: usize, retain_columns: bool) -> u32 {
    if retain_columns {
        column_at(source, byte_offset)
    } else {
        1
    }
}

/// Classify a bare word from a `#define` as a number if it looks like one.
fn classify_word(word: &str) -> TokenKind {
    // `b"..."` is a bytes constant; `"..."` is text. Both resolve their escape
    // sequences here, so a consumer comparing against `b"\n"` gets one byte.
    if let Some(inner) = word
        .strip_prefix("b\"")
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            word.strip_prefix("b'")
                .and_then(|rest| rest.strip_suffix('\''))
        })
    {
        return TokenKind::Bytes(unescape(inner));
    }
    if word.len() >= 2
        && ((word.starts_with('"') && word.ends_with('"'))
            || (word.starts_with('\'') && word.ends_with('\'')))
    {
        let inner = &word[1..word.len() - 1];
        return TokenKind::Str(String::from_utf8_lossy(&unescape(inner)).into_owned());
    }
    match parse_int(word) {
        Some(value) => TokenKind::Number(value),
        None => TokenKind::Ident(word.to_owned()),
    }
}

/// Where a trailing comment starts on a directive line, ignoring quoted text.
///
/// `#define IMAGE_ARCHIVE_LONGNAMES_MEMBER b"//              "` has a `//`
/// inside its value. Cutting there truncates the literal to `b"`, and the
/// scanner then runs to whatever quote it finds next -- which loses the rest of
/// the definition rather than one constant.
fn comment_start(directive: &str) -> Option<usize> {
    let bytes = directive.as_bytes();
    let mut in_quote = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if in_quote => i += 1,
            b'"' => in_quote = !in_quote,
            b'/' if !in_quote && matches!(bytes.get(i + 1), Some(b'/') | Some(b'*')) => {
                return Some(i)
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Resolve C escape sequences in a literal's body.
fn unescape(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        let Some(escape) = bytes.get(i) else {
            out.push(b'\\');
            break;
        };
        match escape {
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'0' => out.push(0),
            b'\\' => out.push(b'\\'),
            b'"' => out.push(b'"'),
            b'\'' => out.push(b'\''),
            b'x' => {
                match text
                    .get(i + 1..i + 3)
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                {
                    Some(byte) => {
                        out.push(byte);
                        i += 2;
                    }
                    None => out.push(b'x'),
                }
            }
            other => out.push(*other),
        }
        i += 1;
    }
    out
}

/// Parse an integer literal in decimal, hex (`0x`), binary (`0b`), or octal.
///
/// Trailing type suffixes (`0x10UL`) appear in real headers, so they are
/// tolerated rather than rejected.
fn parse_int(text: &str) -> Option<i128> {
    let cleaned = text.trim_end_matches(['u', 'U', 'l', 'L']);
    if cleaned.is_empty() {
        return None;
    }
    let lower = cleaned.to_ascii_lowercase();
    if let Some(hex) = lower.strip_prefix("0x") {
        return i128::from_str_radix(hex, 16).ok();
    }
    if let Some(bin) = lower.strip_prefix("0b") {
        return i128::from_str_radix(bin, 2).ok();
    }
    if let Some(octal) = lower.strip_prefix("0o") {
        return i128::from_str_radix(octal, 8).ok();
    }
    if lower.len() > 1 && lower.starts_with('0') {
        return if lower.chars().all(|c| c.is_digit(8)) {
            i128::from_str_radix(&lower[1..], 8).ok()
        } else {
            None
        };
    }
    lower.parse::<i128>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<TokenKind> {
        tokenize(source)
            .unwrap()
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn tokenizes_a_field_declaration() {
        assert_eq!(
            kinds("uint32 Length;"),
            vec![
                TokenKind::Ident("uint32".to_owned()),
                TokenKind::Ident("Length".to_owned()),
                TokenKind::Semicolon,
            ]
        );
    }

    #[test]
    fn skips_both_comment_forms() {
        assert_eq!(
            kinds("/* banner */ a // trailing\nb"),
            vec![
                TokenKind::Ident("a".to_owned()),
                TokenKind::Ident("b".to_owned()),
            ]
        );
    }

    #[test]
    fn parses_integer_bases_and_suffixes() {
        assert_eq!(parse_int("16"), Some(16));
        assert_eq!(parse_int("0x10"), Some(16));
        assert_eq!(parse_int("0b1010"), Some(10));
        assert_eq!(parse_int("020"), Some(16));
        assert_eq!(parse_int("0x10UL"), Some(16));
    }

    #[test]
    fn define_becomes_tokens() {
        assert_eq!(
            kinds("#define MAX 0x20\n"),
            vec![
                TokenKind::Ident("#define".to_owned()),
                TokenKind::Ident("MAX".to_owned()),
                TokenKind::Number(32),
            ]
        );
    }

    /// Line numbers are what makes a parse error actionable, so they are
    /// asserted rather than assumed.
    #[test]
    fn tracks_line_numbers_across_comments() {
        let tokens = tokenize("a\n/* two\nlines */\nb").unwrap();
        assert_eq!(tokens[0].line, 1);
        assert_eq!(tokens[1].line, 4);
    }

    #[test]
    fn unterminated_block_comment_is_an_error() {
        assert!(matches!(tokenize("/* oops"), Err(Error::Parse { .. })));
    }
}
