//! A small integer expression evaluator.
//!
//! Two things in a definition need one, and they need the same one:
//!
//! * **Array lengths.** `wchar FileName[FileNameLength]` sizes an array from a
//!   field read earlier in the same record. This is what makes a structure
//!   dynamic, and `_FILE_NAME` — 35,090 decodes in a bounded `walkfs` — depends
//!   on it.
//! * **Constant definitions.** `#define EXT4_XATTR_PAD (1<<EXT4_XATTR_PAD_BITS)`
//!   in `dissect.extfs` is an expression over other constants.
//!
//! The grammar is C's integer subset, with the usual precedence:
//!
//! ```text
//! ternary   := or ['?' expr ':' expr]
//! or        := xor {'|' xor}
//! xor       := and {'^' and}
//! and       := shift {'&' shift}
//! shift     := additive {('<<' | '>>') additive}
//! additive  := term {('+' | '-') term}
//! term      := unary {('*' | '/' | '%') unary}
//! unary     := ['-' | '~'] primary
//! primary   := NUMBER | IDENT | '(' ternary ')'
//! ```
//!
//! # This runs per record
//!
//! For a dynamic field, the expression is evaluated on every decode. It is
//! therefore parsed *once* at compile time into [`Expr`] and only evaluated
//! here — re-parsing text per record would put string handling on the hot path.

use std::collections::HashMap;

use crate::error::{Error, Result};

/// A parsed integer expression.
///
/// A tree rather than a flat form on purpose: expressions in real definitions
/// are tiny — usually a bare identifier — so tree-walk overhead is irrelevant,
/// and the common case is short-circuited by [`Expr::Name`] anyway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// A literal.
    Const(i128),
    /// A name: another field in the same record, or a `#define`d constant.
    Name(String),
    /// A unary operation.
    Unary(UnaryOp, Box<Expr>),
    /// A binary operation.
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    /// `cond ? a : b`.
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `sizeof(TYPE)`.
    ///
    /// Held as a name rather than folded at parse time: the parser sees the
    /// text, and only a [`Scope`] backed by a schema knows how big a type is.
    SizeOf(String),
    /// `offsetof(TYPE, member)`.
    ///
    /// Boxed, and the box is the point: two inline `String`s are 48 bytes and
    /// made *every* `Expr` node that size, including the `Name` that
    /// `wchar FileName[FileNameLength]` evaluates on every record. `offsetof`
    /// is rare enough that one allocation when it is parsed costs nothing and
    /// keeps the common node at 32 bytes.
    OffsetOf(Box<(String, String)>),
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// Arithmetic negation.
    Neg,
    /// Bitwise complement.
    Not,
}

/// Binary operators, in C's integer subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Rem,
    /// `<<`
    Shl,
    /// `>>`
    Shr,
    /// `&`
    BitAnd,
    /// `|`
    BitOr,
    /// `^`
    BitXor,
}

/// Where an expression's names are looked up.
///
/// Two scopes, checked in this order: fields already decoded in the current
/// record, then schema constants. Fields win, because C scoping says the
/// nearest declaration does and because a field named after a constant is a
/// real possibility in generated headers.
pub trait Scope {
    /// Resolve a name to an integer, or `None` if it is not in scope.
    fn lookup(&self, name: &str) -> Option<i128>;

    /// Why [`Self::lookup`] returned `None`, when the scope can be specific.
    ///
    /// Optional. The default `None` keeps the generic "not defined in this
    /// scope" message, which is right for a scope that only holds numbers. A
    /// scope backed by a whole namespace can distinguish "no such member" from
    /// "that member is not a number", and those are different problems for
    /// whoever wrote the expression.
    fn describe_failure(&self, _name: &str) -> Option<String> {
        None
    }

    /// `sizeof(TYPE)` -- the size in bytes of a named type.
    ///
    /// A scope that has no types (a bag of constants, a decoded record's
    /// fields) says `None` and the expression fails, which is correct: the name
    /// really is unresolvable there.
    fn size_of(&self, _type_name: &str) -> Option<i128> {
        None
    }

    /// `offsetof(TYPE, member)` -- a member's byte offset within a type.
    fn offset_of(&self, _type_name: &str, _member: &str) -> Option<i128> {
        None
    }
}

impl Scope for HashMap<String, i128> {
    fn lookup(&self, name: &str) -> Option<i128> {
        self.get(name).copied()
    }
}

/// An empty scope, for expressions that must be resolvable from literals alone.
pub struct NoScope;

impl Scope for NoScope {
    fn lookup(&self, _name: &str) -> Option<i128> {
        None
    }
}

impl Expr {
    /// Parse expression text.
    pub fn parse(text: &str) -> Result<Self> {
        let tokens = lex(text)?;
        let mut parser = ExprParser {
            tokens: &tokens,
            pos: 0,
        };
        let expr = parser.ternary()?;
        if parser.pos != parser.tokens.len() {
            // Naming the token that stopped the parse, not just the whole
            // expression: "trailing tokens in expression: 0()" left the reader
            // to work out which part was the problem.
            let stopper = parser.tokens.get(parser.pos);
            let message = match stopper {
                // Two values in a row: one operand more than the operators can
                // consume. Upstream says exactly this and it is the clearer
                // description.
                Some(Tok::Num(_) | Tok::Name(_)) => {
                    "Invalid expression: too many operands".to_owned()
                }
                Some(token) => format!(
                    "Invalid expression: Parser expected sizeof or an arithmetic \
                     operator instead got: {}",
                    describe(token)
                ),
                None => "Invalid expression".to_owned(),
            };
            return Err(Error::InvalidValue { message });
        }
        Ok(expr)
    }

    /// If this is a bare literal, return it.
    ///
    /// The compiler uses this to fold a constant length at build time so the
    /// decoder never evaluates it.
    pub fn as_const(&self) -> Option<i128> {
        match self {
            Self::Const(value) => Some(*value),
            _ => None,
        }
    }

    /// Evaluate against a scope.
    pub fn eval(&self, scope: &dyn Scope) -> Result<i128> {
        match self {
            Self::Const(value) => Ok(*value),
            Self::Name(name) => match scope.lookup(name) {
                Some(value) => Ok(value),
                // A scope that knows *why* the name failed says so. A dotted
                // name has two ways to fail -- the member is absent, or it is
                // present and not a number -- and "not defined in this scope"
                // describes only the first.
                None => Err(Error::InvalidValue {
                    message: scope.describe_failure(name).unwrap_or_else(|| {
                        if name.contains('.') {
                            format!("Unknown identifier: '{name}'")
                        } else {
                            format!("{name} is not defined in this scope")
                        }
                    }),
                }),
            },
            Self::SizeOf(type_name) => {
                scope.size_of(type_name).ok_or_else(|| Error::InvalidValue {
                    message: format!("Unknown type in sizeof: '{type_name}'"),
                })
            }
            Self::OffsetOf(names) => {
                let (type_name, member) = names.as_ref();
                scope
                    .offset_of(type_name, member)
                    .ok_or_else(|| Error::InvalidValue {
                        message: format!("Unknown member in offsetof: '{type_name}.{member}'"),
                    })
            }
            Self::Unary(op, inner) => {
                let value = inner.eval(scope)?;
                Ok(match op {
                    UnaryOp::Neg => value.wrapping_neg(),
                    UnaryOp::Not => !value,
                })
            }
            Self::Binary(op, lhs, rhs) => {
                let a = lhs.eval(scope)?;
                let b = rhs.eval(scope)?;
                Ok(match op {
                    BinaryOp::Add => a.wrapping_add(b),
                    BinaryOp::Sub => a.wrapping_sub(b),
                    BinaryOp::Mul => a.wrapping_mul(b),
                    // Division by zero comes from a malformed record, not a bug.
                    // It must be an error, never a panic that escapes as a crash.
                    BinaryOp::Div => {
                        if b == 0 {
                            return Err(Error::InvalidValue {
                                message: "division by zero in an array length".to_owned(),
                            });
                        }
                        a.wrapping_div(b)
                    }
                    BinaryOp::Rem => {
                        if b == 0 {
                            return Err(Error::InvalidValue {
                                message: "modulo by zero in an array length".to_owned(),
                            });
                        }
                        a.wrapping_rem(b)
                    }
                    // Shift counts are attacker-controlled. An out-of-range
                    // shift is UB-adjacent in C and a panic in Rust, so it is
                    // clamped to a defined result instead.
                    BinaryOp::Shl => {
                        if !(0..128).contains(&b) {
                            return Err(Error::InvalidValue {
                                message: format!("shift count out of range: {b}"),
                            });
                        }
                        a.wrapping_shl(b as u32)
                    }
                    BinaryOp::Shr => {
                        if !(0..128).contains(&b) {
                            return Err(Error::InvalidValue {
                                message: format!("shift count out of range: {b}"),
                            });
                        }
                        a.wrapping_shr(b as u32)
                    }
                    BinaryOp::BitAnd => a & b,
                    BinaryOp::BitOr => a | b,
                    BinaryOp::BitXor => a ^ b,
                })
            }
            Self::Ternary(cond, when_true, when_false) => {
                if cond.eval(scope)? != 0 {
                    when_true.eval(scope)
                } else {
                    when_false.eval(scope)
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Num(i128),
    Name(String),
    Op(&'static str),
}

/// How a token reads in an error message.
///
/// Exists so no error path reaches for `{:?}`. A `Debug` rendering of an
/// internal enum is fine in a log and wrong in an exception a consumer sees:
/// `Some(Op(")"))` names this crate's data structures, not the input.
fn describe(token: &Tok) -> String {
    match token {
        Tok::Num(value) => format!("'{value}'"),
        Tok::Name(name) => format!("'{name}'"),
        Tok::Op(text) => format!("'{text}'"),
    }
}

fn lex(text: &str) -> Result<Vec<Tok>> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i] as char).is_ascii_alphanumeric() {
                i += 1;
            }
            let raw = &text[start..i];
            let value = parse_int(raw).ok_or_else(|| Error::Lex {
                // Named by base, because that is what the reader got wrong:
                // "invalid integer in expression: 0x" made them work out which
                // part of `0x` was the problem.
                message: match raw.to_ascii_lowercase() {
                    text if text.starts_with("0x") => {
                        format!("invalid hexadecimal literal: {raw}")
                    }
                    text if text.starts_with("0b") => format!("invalid binary literal: {raw}"),
                    _ => format!("invalid integer literal: {raw}"),
                },
            })?;
            tokens.push(Tok::Num(value));
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < bytes.len() {
                let ch = bytes[i] as char;
                if ch.is_ascii_alphanumeric() || ch == '_' {
                    i += 1;
                } else if ch == '.'
                    && i + 1 < bytes.len()
                    && ((bytes[i + 1] as char).is_ascii_alphabetic() || bytes[i + 1] == b'_')
                {
                    // `color.RED` is one name, not two with an operator
                    // between: cstruct has no member-access operator, and the
                    // resolver is what knows how to walk the dots. The
                    // look-ahead keeps a trailing `.` an error rather than
                    // swallowing it into the name.
                    i += 1;
                } else {
                    break;
                }
            }
            tokens.push(Tok::Name(text[start..i].to_owned()));
            continue;
        }
        // Two-character operators first, so `<<` is not read as two `<`.
        let two = if i + 1 < bytes.len() {
            &text[i..i + 2]
        } else {
            ""
        };
        let op = match two {
            "<<" => Some("<<"),
            ">>" => Some(">>"),
            _ => None,
        };
        if let Some(op) = op {
            tokens.push(Tok::Op(op));
            i += 2;
            continue;
        }
        let single = match c {
            '+' => "+",
            '-' => "-",
            '*' => "*",
            '/' => "/",
            '%' => "%",
            '&' => "&",
            '|' => "|",
            '^' => "^",
            '~' => "~",
            '(' => "(",
            ')' => ")",
            '?' => "?",
            ':' => ":",
            // Only ever an argument separator: `offsetof(test, a)`. cstruct has
            // no comma operator, so a comma anywhere else is a parse error,
            // which is what the trailing-token check reports.
            ',' => ",",
            other => {
                return Err(Error::Lex {
                    message: format!("unexpected character '{other}'"),
                })
            }
        };
        tokens.push(Tok::Op(single));
        i += 1;
    }
    Ok(tokens)
}

fn parse_int(text: &str) -> Option<i128> {
    let cleaned = text.trim_end_matches(['u', 'U', 'l', 'L']);
    let lower = cleaned.to_ascii_lowercase();
    if let Some(hex) = lower.strip_prefix("0x") {
        return i128::from_str_radix(hex, 16).ok();
    }
    if let Some(bin) = lower.strip_prefix("0b") {
        return i128::from_str_radix(bin, 2).ok();
    }
    // C octal: a leading zero with digits after it is base 8, so `010` is 8 and
    // not 10. This parsed as decimal, which is silently wrong for any
    // definition that writes permissions or flags the way C does.
    if let Some(octal) = lower.strip_prefix('0') {
        if !octal.is_empty() {
            return i128::from_str_radix(octal, 8).ok();
        }
    }
    lower.parse::<i128>().ok()
}

struct ExprParser<'a> {
    tokens: &'a [Tok],
    pos: usize,
}

impl ExprParser<'_> {
    fn peek_op(&self) -> Option<&'static str> {
        match self.tokens.get(self.pos) {
            Some(Tok::Op(op)) => Some(op),
            _ => None,
        }
    }

    fn eat(&mut self, op: &str) -> bool {
        if self.peek_op() == Some(op) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn ternary(&mut self) -> Result<Expr> {
        let cond = self.binary(0)?;
        if self.eat("?") {
            let when_true = self.ternary()?;
            if !self.eat(":") {
                return Err(Error::InvalidValue {
                    message: "expected ':' in ternary expression".to_owned(),
                });
            }
            let when_false = self.ternary()?;
            return Ok(Expr::Ternary(
                Box::new(cond),
                Box::new(when_true),
                Box::new(when_false),
            ));
        }
        Ok(cond)
    }

    /// Precedence climbing. Level 0 is the loosest binding.
    fn binary(&mut self, level: usize) -> Result<Expr> {
        const LEVELS: &[&[(&str, BinaryOp)]] = &[
            &[("|", BinaryOp::BitOr)],
            &[("^", BinaryOp::BitXor)],
            &[("&", BinaryOp::BitAnd)],
            &[("<<", BinaryOp::Shl), (">>", BinaryOp::Shr)],
            &[("+", BinaryOp::Add), ("-", BinaryOp::Sub)],
            &[
                ("*", BinaryOp::Mul),
                ("/", BinaryOp::Div),
                ("%", BinaryOp::Rem),
            ],
        ];

        if level >= LEVELS.len() {
            return self.unary();
        }

        let mut lhs = self.binary(level + 1)?;
        // Not a `while let`: the loop also breaks on the *second* condition
        // below, when the operator is not at this precedence level.
        #[allow(clippy::while_let_loop)]
        loop {
            let Some(op_text) = self.peek_op() else { break };
            let Some((_, op)) = LEVELS[level].iter().find(|(text, _)| *text == op_text) else {
                break;
            };
            self.pos += 1;
            let rhs = self.binary(level + 1)?;
            lhs = Expr::Binary(*op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Expr> {
        if self.eat("-") {
            return Ok(Expr::Unary(UnaryOp::Neg, Box::new(self.unary()?)));
        }
        if self.eat("~") {
            return Ok(Expr::Unary(UnaryOp::Not, Box::new(self.unary()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr> {
        match self.tokens.get(self.pos) {
            Some(Tok::Num(value)) => {
                self.pos += 1;
                Ok(Expr::Const(*value))
            }
            // `sizeof(TYPE)` and `offsetof(TYPE, member)` look like calls and
            // are the only two in the language, so they are recognised here
            // rather than by a general call rule that nothing else would use.
            // Their arguments are *type and member names*, not expressions --
            // `sizeof(uint32 + 1)` is not a thing -- so they are read as bare
            // identifiers and the whole node resolves through the scope.
            Some(Tok::Name(name)) if name == "sizeof" || name == "offsetof" => {
                let is_offset = name == "offsetof";
                self.pos += 1;
                if !self.eat("(") {
                    return Err(Error::InvalidValue {
                        message: "Mismatched parentheses: expected (, got )".to_owned(),
                    });
                }
                let Some(Tok::Name(type_name)) = self.tokens.get(self.pos).cloned() else {
                    if self.pos >= self.tokens.len() {
                        // The input ran out with the bracket still open.
                        return Err(Error::InvalidValue {
                            message: "Mismatched parentheses: expected ), got end of input"
                                .to_owned(),
                        });
                    }
                    // Something is there, it is just not a type name.
                    // `sizeof(0 +)` is a *resolution* failure, not a syntax one:
                    // the argument of `sizeof` is a type, and no type is
                    // spelled that way.
                    let raw: Vec<String> = self.tokens[self.pos..]
                        .iter()
                        .take_while(|token| !matches!(token, Tok::Op(")")))
                        .map(|token| match token {
                            Tok::Num(value) => value.to_string(),
                            Tok::Name(name) => name.clone(),
                            Tok::Op(op) => (*op).to_owned(),
                        })
                        .collect();
                    return Err(Error::UnknownType {
                        name: raw.join(" "),
                    });
                };
                self.pos += 1;
                let member = if is_offset {
                    if !self.eat(",") {
                        return Err(Error::InvalidValue {
                            message: "Invalid expression: offsetof takes a type and a member"
                                .to_owned(),
                        });
                    }
                    let Some(Tok::Name(member)) = self.tokens.get(self.pos).cloned() else {
                        return Err(Error::InvalidValue {
                            message: "Invalid expression: offsetof takes a type and a member"
                                .to_owned(),
                        });
                    };
                    self.pos += 1;
                    Some(member)
                } else {
                    None
                };
                if !self.eat(")") {
                    return Err(Error::InvalidValue {
                        message: "Mismatched parentheses: expected ), got end of input".to_owned(),
                    });
                }
                Ok(match member {
                    Some(member) => Expr::OffsetOf(Box::new((type_name, member))),
                    None => Expr::SizeOf(type_name),
                })
            }
            Some(Tok::Name(name)) => {
                self.pos += 1;
                Ok(Expr::Name(name.clone()))
            }
            Some(Tok::Op("(")) => {
                self.pos += 1;
                if matches!(self.tokens.get(self.pos), Some(Tok::Op(")"))) {
                    return Err(Error::InvalidValue {
                        message: "Mismatched parentheses: Parser expected an expression, \
                                  instead received empty parenthesis"
                            .to_owned(),
                    });
                }
                // Any failure *inside* a parenthesised group is reported as a
                // parenthesis problem, because from the reader's point of view
                // that is what it is: `sizeof(` runs out of input in `ternary`
                // and would otherwise be blamed on the missing operand rather
                // than on the bracket that is still open.
                let inner = self.ternary().map_err(|_| Error::InvalidValue {
                    message: "Mismatched parentheses: expected ), got end of input".to_owned(),
                })?;
                if !self.eat(")") {
                    return Err(Error::InvalidValue {
                        message: "Mismatched parentheses: expected ), got end of input".to_owned(),
                    });
                }
                Ok(inner)
            }
            // A closing bracket with nothing open.
            Some(Tok::Op(")")) => Err(Error::InvalidValue {
                message: "Mismatched parentheses: expected (, got )".to_owned(),
            }),
            // An operator where a value belongs: `1 +` has an operator and one
            // operand too few, which is worth saying directly.
            //
            // This is the last arm because it is the last thing a token can be.
            // `Tok` has three variants and the arms above cover `Num`, `Name`
            // and the two brackets, so an operator is all that is left -- there
            // is no "some other token" case to write, and one written here
            // would be dead code the compiler rejects.
            Some(Tok::Op(_)) => Err(Error::InvalidValue {
                message: "Invalid expression: not enough operands".to_owned(),
            }),
            // Nothing left to parse: an operator was given operands it does not
            // have, or the expression ended mid-way.
            None => Err(Error::InvalidValue {
                message: "Invalid expression: not enough operands".to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Expr` is evaluated per record for every dynamic array, so its size is
    /// not cosmetic: `wchar FileName[FileNameLength]` runs 35,090 times in a
    /// bounded NTFS walk, and a bigger node is more cache lines for the tree
    /// walk. Pinned so a new variant cannot quietly grow it -- which is exactly
    /// what `OffsetOf(String, String)` did, taking it from 32 bytes to 56 until
    /// its payload was boxed.
    #[test]
    fn expr_nodes_stay_small() {
        assert!(
            std::mem::size_of::<Expr>() <= 32,
            "Expr grew to {} bytes; box the payload of whichever variant did it",
            std::mem::size_of::<Expr>()
        );
    }

    fn eval(text: &str) -> i128 {
        Expr::parse(text).unwrap().eval(&NoScope).unwrap()
    }

    fn eval_with(text: &str, vars: &[(&str, i128)]) -> i128 {
        let scope: HashMap<String, i128> =
            vars.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect();
        Expr::parse(text).unwrap().eval(&scope).unwrap()
    }

    #[test]
    fn literals_and_bases() {
        assert_eq!(eval("42"), 42);
        assert_eq!(eval("0x20"), 32);
        assert_eq!(eval("0b101"), 5);
    }

    /// The `dissect.extfs` constant that forced this module to exist.
    #[test]
    fn shifts_and_parentheses() {
        assert_eq!(
            eval_with("(1<<EXT4_XATTR_PAD_BITS)", &[("EXT4_XATTR_PAD_BITS", 2)]),
            4
        );
        assert_eq!(eval_with("(EXT4_XATTR_PAD-1)", &[("EXT4_XATTR_PAD", 4)]), 3);
    }

    #[test]
    fn precedence_matches_c() {
        assert_eq!(eval("2 + 3 * 4"), 14);
        assert_eq!(eval("(2 + 3) * 4"), 20);
        assert_eq!(eval("1 | 2 & 3"), 3);
        assert_eq!(eval("1 << 2 + 1"), 8);
    }

    #[test]
    fn a_bare_field_name_is_the_common_case() {
        assert_eq!(eval_with("FileNameLength", &[("FileNameLength", 11)]), 11);
    }

    #[test]
    fn unary_operators() {
        assert_eq!(eval("-5"), -5);
        assert_eq!(eval("~0"), -1);
    }

    #[test]
    fn ternary() {
        assert_eq!(eval_with("n ? 4 : 8", &[("n", 1)]), 4);
        assert_eq!(eval_with("n ? 4 : 8", &[("n", 0)]), 8);
    }

    /// Lengths come out of forensic images. Every one of these is a malformed
    /// record producing an error, not a panic that escapes as a crash.
    #[test]
    fn hostile_input_errors_rather_than_panics() {
        assert!(Expr::parse("1/0").unwrap().eval(&NoScope).is_err());
        assert!(Expr::parse("1%0").unwrap().eval(&NoScope).is_err());
        assert!(Expr::parse("1<<999").unwrap().eval(&NoScope).is_err());
        assert!(Expr::parse("1>>-1").unwrap().eval(&NoScope).is_err());
        assert!(Expr::parse("(1").is_err());
        assert!(Expr::parse("1 +").is_err());
        assert!(Expr::parse("@").is_err());
        // An unknown name is an error, not a silent zero -- a silent zero would
        // decode a zero-length array and produce a wrong record.
        assert!(Expr::parse("Missing").unwrap().eval(&NoScope).is_err());
    }

    #[test]
    fn constant_folding_is_detectable() {
        assert_eq!(Expr::parse("16").unwrap().as_const(), Some(16));
        assert_eq!(Expr::parse("Length").unwrap().as_const(), None);
    }
}
