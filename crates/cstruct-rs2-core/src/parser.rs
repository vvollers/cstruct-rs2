//! Turns tokens into a [`Schema`].
//!
//! The grammar covers the constructs upstream's own parser accepts. Anything
//! outside it produces [`Error::Unsupported`] with a message naming the
//! construct, rather than being quietly misparsed — differing from upstream
//! without saying so is the one outcome worth avoiding at any cost.
//!
//! Supported:
//!
//! * `#define NAME <int>`
//! * `struct NAME { ... };` and `typedef struct NAME { ... } ALIAS;`
//! * `union { ... };`, named or anonymous, nested or top level
//! * `enum NAME : BASE { ... };` and `flag NAME : BASE { ... };`
//! * `typedef EXISTING ALIAS;`, including multi-word types
//!   (`typedef unsigned long long time_t;`)
//! * fields: plain, fixed array `[16]`, expression array `[NameLength]`,
//!   and null-terminated `[]`
//! * anonymous struct and union members, whose fields fold into the parent
//! * bitfields (`uint32 flags : 4`), packed as upstream packs them
//! * elaborated type specifiers (`struct exit_status ut_exit;`)
//! * pointers, read as pointer-sized integers
//!
//! # Why the grammar is not narrower
//!
//! It is tempting to support only the constructs something actually decodes and
//! reject the rest. That does not work here, because a definition is parsed when
//! its module is imported, not when a record is read. A construct nothing ever
//! decodes still has to parse, or the import fails and the module disappears
//! along with everything else it defined.
//!
//! So the grammar follows upstream's, and each construct is implemented against
//! upstream's semantics rather than inferred from its syntax. Where the two
//! could plausibly differ — bitfield packing, negative array lengths, what `[]`
//! means — the choice is documented at the code that makes it.

use std::borrow::Cow;
use std::collections::HashSet;

use crate::error::{Error, Result};
use crate::lexer::{describe_optional, tokenize, Token, TokenKind};
use crate::schema::{
    ArrayLen, ConstValue, Endian, EnumDef, FieldDef, Schema, StructDef, TypeDef, TypeRef,
};

/// Parse a definition into a schema, starting from the built-in scalars.
pub fn parse(source: &str, endian: Endian) -> Result<Schema> {
    let mut schema = Schema::with_builtins(endian);
    parse_into(source, &mut schema)?;
    Ok(schema)
}

/// Parse a definition whose structures use natural alignment.
pub fn parse_aligned_into(source: &str, schema: &mut Schema) -> Result<()> {
    parse_with(source, schema, true).map(|_| ())
}

/// Parse a definition into an existing schema.
///
/// `dissect` consumers call `cs.load()` more than once on the same object, and
/// later definitions refer to types from earlier ones, so accumulating into a
/// schema is the normal case rather than a special one.
pub fn parse_into(source: &str, schema: &mut Schema) -> Result<()> {
    parse_with(source, schema, false).map(|_| ())
}

/// Definition-time details used by the public parser façade.
///
/// This is returned by the same parse that mutates the schema. Keeping it here
/// avoids a second lexer/parser pass merely to populate Python attributes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParseMetadata {
    /// Configuration flags left after the final declaration.
    pub flags: Vec<String>,
    /// Constants removed by active `#undef` directives.
    pub undefined_constants: Vec<String>,
}

/// Parse an unaligned definition and return its public metadata.
pub fn parse_into_with_metadata(source: &str, schema: &mut Schema) -> Result<ParseMetadata> {
    parse_with(source, schema, false)
}

/// Parse an aligned definition and return its public metadata.
pub fn parse_aligned_into_with_metadata(
    source: &str,
    schema: &mut Schema,
) -> Result<ParseMetadata> {
    parse_with(source, schema, true)
}

fn parse_with(source: &str, schema: &mut Schema, align: bool) -> Result<ParseMetadata> {
    // Upstream removes a backslash-newline pair before lexing. This is what
    // makes a multiline macro one directive rather than a value followed by
    // two stray expression lines.
    let source = if source.contains("\\\n") {
        Cow::Owned(source.replace("\\\n", ""))
    } else {
        Cow::Borrowed(source)
    };
    let first_new_type = schema.order.len();
    let tokens = with_error_context(tokenize(&source), &source)?;
    let tokens = with_error_context(filter_conditionals(tokens, schema), &source)?;
    let mut parser = Parser {
        tokens: &tokens,
        pos: 0,
        anonymous_counter: 0,
        align,
        flags: Vec::new(),
        undefined_constants: Vec::new(),
        // Copied onto the parser rather than read from the schema at each `*`:
        // the pointer width is fixed for the duration of one parse, and
        // threading `&Schema` into `pointer_type_name` would put a borrow
        // through every declaration parser for a value that cannot change.
        pointer_size: match schema.pointer_size {
            0 => 8,
            size => size,
        },
    };
    with_error_context(parser.parse_all(schema), &source)?;
    validate_new_type_references(schema, first_new_type)?;
    Ok(ParseMetadata {
        flags: parser.flags,
        undefined_constants: parser.undefined_constants,
    })
}

/// Add upstream's compact source excerpt to a parser error.
fn with_error_context<T>(result: Result<T>, source: &str) -> Result<T> {
    result.map_err(|error| match error {
        Error::Parse { line, message } if !message.contains("\n  1:") => {
            let context = format_error_context(source, line);
            let message = if context.is_empty() {
                message
            } else {
                format!("{message}\n{context}")
            };
            Error::Parse { line, message }
        }
        other => other,
    })
}

/// Format the current line and at most two preceding lines.
fn format_error_context(source: &str, line: u32) -> String {
    let line = line.max(1) as usize;
    let lines = source.lines().collect::<Vec<_>>();
    if lines.is_empty() {
        return String::new();
    }
    let end = line.min(lines.len());
    let start = end.saturating_sub(3);
    let width = line.to_string().len() + 2;
    lines[start..end]
        .iter()
        .enumerate()
        .map(|(index, text)| format!("{:>width$}: {text}", start + index + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug)]
struct ConditionalFrame {
    line: u32,
    parent_active: bool,
    condition_true: bool,
    active: bool,
    saw_else: bool,
}

/// Remove inactive conditional branches while retaining all active directives.
///
/// Conditional expressions depend only on whether a constant exists. The
/// declaration parser still evaluates active `#define` values and applies
/// `#undef`; this pass merely decides which token branch reaches it.
fn filter_conditionals(tokens: Vec<Token>, schema: &Schema) -> Result<Vec<Token>> {
    if !tokens.iter().any(|token| {
        matches!(
            &token.kind,
            TokenKind::Ident(value)
                if matches!(value.as_str(), "#ifdef" | "#ifndef" | "#else" | "#endif")
        )
    }) {
        return Ok(tokens);
    }

    let mut defined = schema.constants.keys().cloned().collect::<HashSet<_>>();
    let mut stack = Vec::<ConditionalFrame>::new();
    let mut output = Vec::with_capacity(tokens.len());
    let mut index = 0usize;

    while index < tokens.len() {
        let token = &tokens[index];
        let directive = match &token.kind {
            TokenKind::Ident(value) if value.starts_with('#') => Some(value.as_str()),
            _ => None,
        };
        let parent_active = stack.last().is_none_or(|frame| frame.active);

        match directive {
            Some("#ifdef" | "#ifndef") => {
                let name = match tokens.get(index + 1).map(|next| &next.kind) {
                    Some(TokenKind::Ident(name)) => name,
                    _ => {
                        return Err(Error::Parse {
                            line: token.line,
                            message: "expected an identifier after conditional directive"
                                .to_owned(),
                        })
                    }
                };
                let mut condition_true = defined.contains(name);
                if directive == Some("#ifndef") {
                    condition_true = !condition_true;
                }
                stack.push(ConditionalFrame {
                    line: token.line,
                    parent_active,
                    condition_true,
                    active: parent_active && condition_true,
                    saw_else: false,
                });
                index += 2;
                continue;
            }
            Some("#else") => {
                let Some(frame) = stack.last_mut() else {
                    return Err(Error::Parse {
                        line: token.line,
                        message: "#else without matching #ifdef/#ifndef".to_owned(),
                    });
                };
                if frame.saw_else {
                    return Err(Error::Parse {
                        line: token.line,
                        message: "duplicate #else for conditional statement".to_owned(),
                    });
                }
                frame.saw_else = true;
                frame.active = frame.parent_active && !frame.condition_true;
                index += 1;
                continue;
            }
            Some("#endif") => {
                if stack.pop().is_none() {
                    return Err(Error::Parse {
                        line: token.line,
                        message: "#endif without matching #ifdef/#ifndef".to_owned(),
                    });
                }
                index += 1;
                continue;
            }
            Some("#define") if parent_active => {
                if let Some(TokenKind::Ident(name)) = tokens.get(index + 1).map(|next| &next.kind) {
                    defined.insert(name.clone());
                }
            }
            Some("#undef") if parent_active => {
                if let Some(TokenKind::Ident(name)) = tokens.get(index + 1).map(|next| &next.kind) {
                    defined.remove(name);
                }
            }
            _ => {}
        }

        if parent_active {
            output.push(token.clone());
        }
        index += 1;
    }

    if let Some(frame) = stack.last() {
        return Err(Error::Parse {
            line: frame.line,
            message: "unclosed conditional statement".to_owned(),
        });
    }
    Ok(output)
}

/// Resolve every type name introduced by this load after forward declarations
/// have had a chance to appear.
///
/// Plans are compiled lazily, but upstream rejects an unknown field type while
/// loading the definition. Deferring this check would make a malformed module
/// import successfully and fail only when somebody first touched its type.
fn validate_new_type_references(schema: &Schema, first: usize) -> Result<()> {
    fn validate_ref(schema: &Schema, reference: &TypeRef) -> Result<()> {
        match reference {
            TypeRef::Named(name) => {
                schema.resolve(name)?;
            }
            TypeRef::Array(element, _) => validate_ref(schema, element)?,
        }
        Ok(())
    }

    for name in schema.order.get(first..).unwrap_or_default() {
        match schema.types.get(name) {
            Some(TypeDef::Struct(definition)) => {
                for field in &definition.fields {
                    validate_ref(schema, &field.ty)?;
                }
            }
            Some(TypeDef::Enum(definition)) => {
                schema.resolve(&definition.base)?;
            }
            Some(TypeDef::Alias(reference)) => validate_ref(schema, reference)?,
            Some(TypeDef::Scalar(_) | TypeDef::Custom) | None => {}
        }
    }
    Ok(())
}

/// Reject a bitfield run that cannot fit in one storage unit.
///
/// Current upstream reports this while `load()` is parsing the declaration,
/// before a caller asks for the generated structure class. Plans in rs2 are
/// otherwise compiled lazily, so leaving the check to the plan compiler would
/// make the same invalid definition appear to load successfully. Only the
/// unit's signedness and byte width matter: aliases and enums resolve to their
/// underlying integer, and `char` is the one byte-valued non-integer spelling
/// that upstream permits for a bitfield.
fn validate_bitfield_runs(schema: &Schema, fields: &[FieldDef]) -> Result<()> {
    let mut open_unit: Option<(bool, u32)> = None;
    let mut remaining = 0u32;

    for field in fields {
        let Some(width) = field.bits else {
            open_unit = None;
            remaining = 0;
            continue;
        };
        let Some(unit) = bitfield_unit(schema, &field.ty) else {
            // The plan compiler supplies the useful unsupported-base
            // diagnostic after type resolution. This pass exists only to move
            // the source-compatible straddle check to load time.
            open_unit = None;
            remaining = 0;
            continue;
        };
        if open_unit != Some(unit) || remaining == 0 {
            open_unit = Some(unit);
            remaining = unit.1 * 8;
        }
        let width = u32::from(width);
        if width > remaining {
            return Err(Error::InvalidValue {
                message: "Straddled bit fields are unsupported".to_owned(),
            });
        }
        remaining -= width;
    }
    Ok(())
}

/// Resolve the storage-unit identity used by [`validate_bitfield_runs`].
fn bitfield_unit(schema: &Schema, ty: &TypeRef) -> Option<(bool, u32)> {
    let TypeRef::Named(name) = ty else {
        return None;
    };
    match schema.resolve(name).ok()? {
        TypeDef::Scalar(crate::schema::Scalar::Uint(size)) => {
            (*size <= 8).then_some((false, u32::from(*size)))
        }
        TypeDef::Scalar(crate::schema::Scalar::Int(size)) => {
            (*size <= 8).then_some((true, u32::from(*size)))
        }
        TypeDef::Scalar(crate::schema::Scalar::Char) => Some((false, 1)),
        TypeDef::Enum(definition) => {
            bitfield_unit(schema, &TypeRef::Named(definition.base.clone()))
        }
        _ => None,
    }
}

/// Turn the lexer-level `[EOF]` sentinel back into a normal expression when a
/// nearer value named `EOF` exists.
fn bind_eof_dimension(ty: &mut TypeRef) {
    if let TypeRef::Array(element, length) = ty {
        if matches!(length, ArrayLen::ToEnd) {
            *length = ArrayLen::Expr("EOF".to_owned());
        }
        bind_eof_dimension(element);
    }
}

/// Whether an unbounded outer dimension lacks a fixed inner row width.
fn array_requires_depth(ty: &TypeRef) -> bool {
    let mut dimensions = Vec::new();
    let mut current = ty;
    while let TypeRef::Array(element, length) = current {
        dimensions.push(length);
        current = element;
    }
    dimensions.iter().enumerate().any(|(index, dimension)| {
        matches!(dimension, ArrayLen::NullTerminated | ArrayLen::ToEnd)
            && dimensions[index + 1..]
                .iter()
                .any(|inner| !matches!(inner, ArrayLen::Fixed(_)))
    })
}

/// Resolves names inside an enum member's value expression.
struct EnumScope<'a> {
    schema: &'a Schema,
    members: &'a [(String, i128)],
}

impl crate::expr::Scope for EnumScope<'_> {
    fn lookup(&self, name: &str) -> Option<i128> {
        self.members
            .iter()
            .rev()
            .find(|(member, _)| member == name)
            .map(|(_, value)| *value)
            .or_else(|| self.schema.const_int(name))
    }
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    /// Counter for naming anonymous structs and unions.
    ///
    /// Upstream names them `__anonymous_0__`, `__anonymous_1__`, and so on.
    /// These names surface in reprs and error messages, so the scheme is copied
    /// rather than invented.
    anonymous_counter: u32,
    /// Whether structures declared by this parse use natural alignment.
    align: bool,
    /// Width in bytes a `*` member reads at, from the schema. Never zero.
    pointer_size: u8,
    /// Configuration flags that have not yet been consumed by a declaration.
    flags: Vec<String>,
    /// Active `#undef` names, returned so the binding can update Python views.
    undefined_constants: Vec<String>,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&'a TokenKind> {
        self.tokens.get(self.pos).map(|t| &t.kind)
    }

    fn line(&self) -> u32 {
        self.tokens
            .get(self.pos)
            .or_else(|| self.tokens.last())
            .map(|t| t.line)
            .unwrap_or(0)
    }

    fn next(&mut self) -> Option<&'a TokenKind> {
        let kind = self.tokens.get(self.pos).map(|t| &t.kind);
        if kind.is_some() {
            self.pos += 1;
        }
        kind
    }

    fn error<T>(&self, message: impl Into<String>) -> Result<T> {
        Err(Error::Parse {
            line: self.line(),
            message: message.into(),
        })
    }

    fn expect(&mut self, want: &TokenKind) -> Result<()> {
        match self.next() {
            Some(got) if got == want => Ok(()),
            Some(got) => {
                let got = got.describe();
                self.error(format!("expected {}, found {got}", want.describe()))
            }
            None => self.error(format!("expected {}, found end of input", want.describe())),
        }
    }

    fn expect_ident(&mut self) -> Result<String> {
        match self.next() {
            Some(TokenKind::Ident(name)) => Ok(name.clone()),
            Some(other) => {
                let other = other.describe();
                self.error(format!("expected an identifier, found {other}"))
            }
            None => self.error("expected an identifier, found end of input"),
        }
    }

    fn eat_ident(&mut self, word: &str) -> bool {
        if matches!(self.peek(), Some(TokenKind::Ident(name)) if name == word) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn next_anonymous_name(&mut self) -> String {
        let name = format!("__anonymous_{}__", self.anonymous_counter);
        self.anonymous_counter += 1;
        name
    }

    fn parse_all(&mut self, schema: &mut Schema) -> Result<()> {
        while self.pos < self.tokens.len() {
            // Stray semicolons between declarations are common and harmless.
            if matches!(self.peek(), Some(TokenKind::Semicolon)) {
                self.pos += 1;
                continue;
            }
            self.parse_declaration(schema)?;
        }
        Ok(())
    }

    fn parse_declaration(&mut self, schema: &mut Schema) -> Result<()> {
        if self.parse_preprocessor(schema)? {
            Ok(())
        } else if self.eat_ident("typedef") {
            self.parse_typedef(schema)
        } else if self.eat_ident("struct") {
            let def = self.parse_struct_body(schema, false, None)?;
            self.consume_trailing_names(schema, &def, false)?;
            Ok(())
        } else if self.eat_ident("union") {
            let def = self.parse_struct_body(schema, true, None)?;
            self.consume_trailing_names(schema, &def, false)?;
            Ok(())
        } else if self.eat_ident("enum") {
            self.parse_enum(schema, false)
        } else if self.eat_ident("flag") {
            self.parse_enum(schema, true)
        } else {
            let found = describe_optional(self.peek());
            self.error(format!("unexpected token {found}"))
        }
    }

    /// Apply one active non-conditional preprocessor directive.
    fn parse_preprocessor(&mut self, schema: &mut Schema) -> Result<bool> {
        let line = self.line();
        if self.eat_ident("#define") {
            let name = self.expect_ident()?;
            let value = self.parse_define_value(&name, line)?;
            schema.insert_constant(name, value);
            return Ok(true);
        }
        if self.eat_ident("#undef") {
            let name = self.expect_ident()?;
            if schema.remove_constant(&name).is_none() {
                return self.error(format!("constant '{name}' not defined"));
            }
            self.undefined_constants.push(name);
            return Ok(true);
        }
        if self.eat_ident("#include") {
            match self.next() {
                Some(TokenKind::Str(path)) => schema.includes.push(path.clone()),
                _ => return self.error("expected include path after '#include'"),
            }
            return Ok(true);
        }
        if self.eat_ident("#[flags]") {
            match self.next() {
                Some(TokenKind::Str(flags)) => self.flags.extend(
                    flags
                        .split(',')
                        .map(str::trim)
                        .filter(|flag| !flag.is_empty())
                        .map(str::to_owned),
                ),
                _ => return self.error("expected configuration flags inside '#[...]'"),
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// Whether the next token begins a new top-level declaration.
    fn at_declaration_start(&self) -> bool {
        match self.peek() {
            None => true,
            Some(TokenKind::Ident(word)) => matches!(
                word.as_str(),
                "#define"
                    | "#undef"
                    | "#include"
                    | "#[flags]"
                    | "typedef"
                    | "struct"
                    | "union"
                    | "enum"
                    | "flag"
            ),
            _ => false,
        }
    }

    /// Read the value of a `#define`, which is not always a literal.
    ///
    /// Three forms have to be carried:
    ///
    /// * an integer — `#define BLOCK_SIZE 4096`
    /// * a string — `#define XATTR_PREFIX "gnu."`
    /// * an expression over other constants —
    ///   `#define XATTR_PAD (1 << XATTR_PAD_BITS)`
    ///
    /// Only the first can size an array, but all three have to *parse*: a
    /// definition that declares a string constant is unusable if the parser
    /// rejects it, even when nothing reads that constant.
    fn parse_define_value(&mut self, name: &str, directive_line: u32) -> Result<ConstValue> {
        // `#define NAME = 6` is accepted as well as `#define NAME 6`. Upstream
        // takes both, and without this the `=` was swept into the expression
        // text: `#define B = 6` produced the *string* `"=6"` rather than the
        // integer 6, so a constant defined that way silently stopped being a
        // number and could no longer size an array.
        // The `=` arrives as `Ident("=")`, not `Equals`: `#define` values are
        // split on whitespace by the lexer and each part goes through
        // `classify_word`, which never sees it as punctuation. Both spellings
        // are matched so the skip does not depend on that detail.
        let skip = match self.peek() {
            Some(TokenKind::Equals) => true,
            Some(TokenKind::Ident(word)) => word == "=",
            _ => false,
        };
        if skip {
            self.pos += 1;
        }

        // A lone integer or string is the common case.
        if let Some(TokenKind::Number(value)) = self.peek() {
            let value = *value;
            if self.define_value_ends_here(1, directive_line) {
                self.pos += 1;
                return Ok(ConstValue::Int(value));
            }
        }
        if let Some(TokenKind::Str(text)) = self.peek() {
            let text = text.clone();
            if self.define_value_ends_here(1, directive_line) {
                self.pos += 1;
                return Ok(ConstValue::Str(text));
            }
        }
        if let Some(TokenKind::Bytes(raw)) = self.peek() {
            let raw = raw.clone();
            if self.define_value_ends_here(1, directive_line) {
                self.pos += 1;
                return Ok(ConstValue::Bytes(raw));
            }
        }

        // Otherwise gather the rest of the value and parse it as an expression.
        let mut text = String::new();
        while !self.define_value_ends_here(0, directive_line) {
            match self.next() {
                Some(TokenKind::Ident(word)) => text.push_str(word),
                Some(TokenKind::Number(value)) => text.push_str(&value.to_string()),
                Some(TokenKind::LParen) => text.push('('),
                Some(TokenKind::RParen) => text.push(')'),
                Some(TokenKind::Star) => text.push('*'),
                Some(TokenKind::Colon) => text.push(':'),
                Some(TokenKind::Punct(c)) => text.push(*c),
                Some(other) => {
                    let other = other.clone();
                    return self.error(format!(
                        "unexpected token in #define {name}: {}",
                        other.describe()
                    ));
                }
                None => break,
            }
        }
        if text.is_empty() {
            return Ok(ConstValue::Str(String::new()));
        }
        // If it is not an expression either, keep the raw text.
        //
        // `dissect.evidence` writes `#define ADSEGMENTEDFILE_MAGIC
        // ADSEGMENTEDFILE\00` -- a bare token sequence with an escape, neither
        // an integer nor a quoted string. Current upstream carries such a value
        // as raw constant text. Rejecting an opaque constant would make the
        // surrounding definition unloadable even when no decode path reads it.
        match crate::expr::Expr::parse(&text) {
            Ok(parsed) => Ok(ConstValue::Expr {
                parsed,
                source: crate::schema::space_expression(&text),
            }),
            Err(_) => Ok(ConstValue::Str(text)),
        }
    }

    /// Whether the `#define` value ends `offset` tokens ahead.
    ///
    /// The lexer drops newlines, so a directive's end is inferred: the next
    /// token starts a new declaration. In practice a `#define` is always
    /// followed by another `#define`, a `typedef`, `struct`, `union`, `enum`,
    /// or `flag`, or the input ends.
    fn define_value_ends_here(&self, offset: usize, directive_line: u32) -> bool {
        self.tokens
            .get(self.pos + offset)
            .is_none_or(|token| token.line != directive_line)
    }

    /// Handle the `} NAME, OTHER;` tail that follows a struct or union body.
    ///
    /// C lets one definition introduce several names. `dissect.ntfs` relies on
    /// this constantly — `typedef struct _FOO { ... } FOO;` is its house style
    /// — so both the tag and the trailing names must resolve to the same type.
    fn consume_trailing_names(
        &mut self,
        schema: &mut Schema,
        def: &StructDef,
        is_typedef: bool,
    ) -> Result<()> {
        let mut declarations = Vec::<(String, u8)>::new();
        loop {
            // A missing trailing semicolon is legal here: a definition may
            // write `struct HEADER { ... }` and start the next declaration
            // straight after. Upstream accepts it, so requiring one would make
            // definitions unparseable that upstream reads without complaint —
            // and since definitions are parsed at import time, that costs the
            // whole module rather than one type.
            if self.at_declaration_start() {
                break;
            }
            match self.peek() {
                Some(TokenKind::Semicolon) => {
                    self.pos += 1;
                    break;
                }
                Some(TokenKind::Comma) => {
                    self.pos += 1;
                }
                Some(TokenKind::Star) => {
                    let indirection = self.eat_star();
                    let alias = self.expect_ident()?;
                    declarations.push((alias, indirection));
                }
                Some(TokenKind::Ident(_)) => {
                    let alias = self.expect_ident()?;
                    declarations.push((alias, 0));
                }
                other => {
                    let other = other.cloned();
                    return self.error(format!(
                        "unexpected token after type body: {}",
                        describe_optional(other.as_ref())
                    ));
                }
            }
        }

        // Current upstream treats names after a non-typedef body as C variable
        // declarations and ignores them. A few released Dissect definitions
        // use one anonymous body plus one name as a convenient type spelling;
        // retaining only that unambiguous legacy form preserves those imports
        // without turning ordinary variables into namespace types.
        let keep_single_anonymous = !is_typedef
            && def.name.starts_with("__anonymous_")
            && declarations.len() == 1
            && declarations[0].1 == 0;
        if !is_typedef && !keep_single_anonymous {
            return Ok(());
        }

        for (alias, indirection) in declarations {
            if indirection > 0 {
                let target = format!(
                    "{}{}",
                    def.name,
                    "*".repeat(indirection.saturating_sub(1) as usize)
                );
                schema.pointer_aliases.insert(alias.clone(), target);
                let referent = self.pointer_type_name();
                schema.insert_declaration(alias, TypeDef::Alias(TypeRef::Named(referent)))?;
            } else if alias != def.name {
                schema
                    .insert_declaration(alias, TypeDef::Alias(TypeRef::Named(def.name.clone())))?;
            }
        }
        Ok(())
    }

    fn parse_typedef(&mut self, schema: &mut Schema) -> Result<()> {
        for (word, is_union) in [("struct", false), ("union", true)] {
            if self.eat_ident(word) {
                // `typedef struct prange prange_t;` -- an alias for a struct
                // already declared, with no body of its own. `dissect.apfs`
                // declares every one of its types this way.
                if self.is_elaborated_reference() {
                    let referenced = self.expect_ident()?;
                    return self.consume_trailing_names(
                        schema,
                        &StructDef {
                            name: referenced,
                            fields: Vec::new(),
                            is_union,
                            align: self.align,
                        },
                        true,
                    );
                }
                let def = self.parse_struct_body(schema, is_union, None)?;
                return self.consume_trailing_names(schema, &def, true);
            }
        }
        if self.eat_ident("enum") {
            return self.parse_enum(schema, false);
        }
        if self.eat_ident("flag") {
            return self.parse_enum(schema, true);
        }

        // `typedef EXISTING ALIAS;`, where EXISTING may be several words
        // (`typedef unsigned long long time_t;` in `dissect.target`'s atop),
        // ALIAS may be a pointer (`typedef void *PVOID;`), and ALIAS may carry
        // array dimensions (`typedef char uuid_t[16];`).
        let target = self.parse_type_spec(schema)?;
        let indirection = self.eat_star();
        let alias = self.expect_ident()?;

        if matches!(self.peek(), Some(TokenKind::Colon)) {
            return self.error("typedefs cannot have bitfields");
        }

        // The subscripts bind to the *alias*, not to the target: `uuid_t` names
        // "sixteen chars", so a field declared `uuid_t id;` is the array. Folded
        // in reverse for the same reason field declarations are -- the
        // dimensions apply outside-in, so the last subscript is the innermost
        // array.
        let mut dimensions = Vec::new();
        while matches!(self.peek(), Some(TokenKind::LBracket)) {
            self.pos += 1;
            dimensions.push(self.parse_array_length()?);
        }
        self.expect(&TokenKind::Semicolon)?;

        let referent = if indirection > 0 {
            let points_to = format!(
                "{}{}",
                target,
                "*".repeat(indirection.saturating_sub(1) as usize)
            );
            schema.pointer_aliases.insert(alias.clone(), points_to);
            self.pointer_type_name()
        } else {
            target
        };
        let mut ty = TypeRef::Named(referent);
        for len in dimensions.into_iter().rev() {
            ty = TypeRef::Array(Box::new(ty), len);
        }
        schema.insert_declaration(alias, TypeDef::Alias(ty))?;
        Ok(())
    }

    /// Parse `[NAME] { fields }`, registering the type and returning it.
    ///
    /// `forced_name` is used for anonymous members, which still need a name to
    /// be registered under so the parent can reference them.
    fn parse_struct_body(
        &mut self,
        schema: &mut Schema,
        is_union: bool,
        forced_name: Option<String>,
    ) -> Result<StructDef> {
        let name = match forced_name {
            Some(name) => name,
            None => match self.peek() {
                Some(TokenKind::Ident(_)) => self.expect_ident()?,
                _ => self.next_anonymous_name(),
            },
        };

        self.expect(&TokenKind::LBrace)?;
        let mut fields = Vec::new();
        loop {
            if self.parse_preprocessor(schema)? {
                continue;
            }
            match self.peek() {
                Some(TokenKind::RBrace) => {
                    self.pos += 1;
                    break;
                }
                Some(TokenKind::Semicolon) => {
                    self.pos += 1;
                }
                None => return self.error("unterminated type body"),
                _ => fields.extend(self.parse_member(schema)?),
            }
        }

        // `[EOF]` normally means "consume the remaining input", but upstream's
        // expression lookup gives a previously declared field or constant of
        // that name precedence. Preserve that lexical spelling during parsing,
        // then bind it once the structure's declaration order is known.
        let mut eof_is_value = schema.const_int("EOF").is_some();
        for field in &mut fields {
            if eof_is_value {
                bind_eof_dimension(&mut field.ty);
            }
            if field.name.as_deref() == Some("EOF") {
                eof_is_value = true;
            }
            if array_requires_depth(&field.ty) {
                return self.error("Depth required for multi-dimensional array");
            }
        }

        // Upstream rejects repeated public field names while permitting the
        // discard name ``_`` more than once. Detect this before registering the
        // type so a failed load cannot leave a partly usable declaration.
        let mut names = HashSet::new();
        for field in &fields {
            let Some(field_name) = field.name.as_deref() else {
                continue;
            };
            if field_name != "_" && !names.insert(field_name) {
                return Err(Error::InvalidValue {
                    message: format!("Duplicate field name: {field_name}"),
                });
            }
        }

        let def = StructDef {
            name: name.clone(),
            fields,
            is_union,
            align: self.align,
        };
        validate_bitfield_runs(schema, &def.fields)?;
        schema.insert_declaration(name, TypeDef::Struct(def.clone()))?;
        self.flags.clear();
        Ok(def)
    }

    /// Parse one member, which may expand to several fields.
    ///
    /// An anonymous struct or union contributes a single unnamed field holding
    /// the generated type; the compiler folds its fields into the parent later.
    fn parse_member(&mut self, schema: &mut Schema) -> Result<Vec<FieldDef>> {
        // A named enum or flag declared inside a structure enters the
        // namespace but does not itself occupy a field. Current upstream
        // permits this so later directives in the same body can refer to its
        // members.
        if self.eat_ident("enum") {
            self.parse_enum(schema, false)?;
            return Ok(Vec::new());
        }
        if self.eat_ident("flag") {
            self.parse_enum(schema, true)?;
            return Ok(Vec::new());
        }

        // Nested struct or union.
        for (word, is_union) in [("struct", false), ("union", true)] {
            if self.eat_ident(word) {
                // `struct EXISTING name;` -- an elaborated type specifier, not a
                // definition. C allows naming an already-declared type with its
                // tag keyword, and `dissect.target`'s `utmp` and `lastlog`
                // definitions both do (`struct exit_status ut_exit;`). Without
                // this the parser demands a `{` and the whole module is lost.
                if self.is_elaborated_reference() {
                    let referenced = self.expect_ident()?;
                    let is_pointer = self.eat_star() > 0;
                    let member_name = self.expect_ident()?;
                    let mut ty = if is_pointer {
                        TypeRef::Named(self.pointer_type_name())
                    } else {
                        TypeRef::Named(referenced.clone())
                    };
                    if matches!(self.peek(), Some(TokenKind::LBracket)) {
                        self.pos += 1;
                        let len = self.parse_array_length()?;
                        ty = TypeRef::Array(Box::new(ty), len);
                    }
                    self.expect(&TokenKind::Semicolon)?;
                    return Ok(vec![FieldDef {
                        name: Some(member_name),
                        ty,
                        bits: None,
                        // `struct foo *bar;` -- the elaborated form. Same
                        // recording as the plain one below.
                        points_to: is_pointer.then(|| referenced.clone()),
                        explicit_offset: None,
                    }]);
                }

                // A named nested type is still just a type; an anonymous one
                // gets a generated name. Its closing brace may be followed by
                // several ordinary declarators, including arrays.
                let nested = self.parse_struct_body(schema, is_union, None)?;
                if matches!(self.peek(), Some(TokenKind::Semicolon)) {
                    self.pos += 1;
                    return Ok(vec![FieldDef {
                        name: None,
                        ty: TypeRef::Named(nested.name),
                        bits: None,
                        points_to: None,
                        explicit_offset: None,
                    }]);
                }

                let mut fields = Vec::new();
                loop {
                    let indirection = self.eat_star();
                    let member_name = self.expect_ident()?;
                    let mut ty = if indirection > 0 {
                        TypeRef::Named(self.pointer_type_name())
                    } else {
                        TypeRef::Named(nested.name.clone())
                    };
                    let mut dimensions = Vec::new();
                    while matches!(self.peek(), Some(TokenKind::LBracket)) {
                        self.pos += 1;
                        dimensions.push(self.parse_array_length()?);
                    }
                    for len in dimensions.into_iter().rev() {
                        ty = TypeRef::Array(Box::new(ty), len);
                    }
                    fields.push(FieldDef {
                        name: Some(member_name),
                        ty,
                        bits: None,
                        points_to: (indirection > 0).then(|| {
                            format!(
                                "{}{}",
                                nested.name,
                                "*".repeat(indirection.saturating_sub(1) as usize)
                            )
                        }),
                        explicit_offset: None,
                    });

                    match self.peek() {
                        Some(TokenKind::Comma) => self.pos += 1,
                        Some(TokenKind::Semicolon) => {
                            self.pos += 1;
                            break;
                        }
                        other => {
                            let other = other.cloned();
                            return self.error(format!(
                                "expected ',' or ';' after field, found {}",
                                describe_optional(other.as_ref())
                            ));
                        }
                    }
                }
                return Ok(fields);
            }
        }

        // A plain field: TYPE name [array] [: bits] ;
        let type_name = self.parse_type_spec(schema)?;

        let mut fields = Vec::new();
        loop {
            // `TYPE *name;` -- a pointer member. Upstream reads a pointer-sized
            // integer; see `parse_pointer_width`.
            let indirection = self.eat_star();
            let field_name = self.expect_ident()?;
            let mut ty = if indirection > 0 {
                TypeRef::Named(self.pointer_type_name())
            } else {
                TypeRef::Named(type_name.clone())
            };
            // `uint32 **p` addresses a `uint32*`, which addresses a `uint32`.
            let points_to = if indirection > 0 {
                let mut target = type_name.clone();
                for _ in 1..indirection {
                    target.push('*');
                }
                Some(target)
            } else {
                schema.pointer_aliases.get(&type_name).cloned()
            };

            // Multi-dimensional arrays: `CHAR Hash[29][16];` in
            // `dissect.database`'s NTDS definition. The dimensions apply
            // outside-in, so they are folded in reverse: the innermost array is
            // the last subscript.
            let mut dimensions = Vec::new();
            while matches!(self.peek(), Some(TokenKind::LBracket)) {
                self.pos += 1;
                dimensions.push(self.parse_array_length()?);
            }
            for len in dimensions.into_iter().rev() {
                ty = TypeRef::Array(Box::new(ty), len);
            }

            let bits = if matches!(self.peek(), Some(TokenKind::Colon)) {
                self.pos += 1;
                Some(self.parse_bit_width()?)
            } else {
                None
            };

            fields.push(FieldDef {
                name: Some(field_name),
                ty,
                bits,
                points_to,
                explicit_offset: None,
            });

            match self.peek() {
                // `uint32 a, b;` declares two fields of the same type.
                Some(TokenKind::Comma) => {
                    self.pos += 1;
                }
                Some(TokenKind::Semicolon) => {
                    self.pos += 1;
                    break;
                }
                other => {
                    let other = other.cloned();
                    return self.error(format!(
                        "expected ',' or ';' after field, found {}",
                        describe_optional(other.as_ref())
                    ));
                }
            }
        }

        Ok(fields)
    }

    /// True when `struct`/`union` was just eaten and what follows names an
    /// existing type rather than opening a definition body.
    ///
    /// `struct exit_status ut_exit;` versus `struct { ... } ut_exit;` or
    /// `struct exit_status { ... };`. The discriminator is simply whether a
    /// `{` follows the tag.
    fn is_elaborated_reference(&self) -> bool {
        matches!(self.peek(), Some(TokenKind::Ident(_)))
            && !matches!(
                self.tokens.get(self.pos + 1).map(|t| &t.kind),
                Some(TokenKind::LBrace)
            )
    }

    /// Consume a `*` if one is present.
    /// How many `*` a member declaration carries; zero when it is not a pointer.
    ///
    /// The *count* matters, not just the fact: every level reads the same
    /// pointer-sized integer, so the layout is unaffected, but `uint32 **p`
    /// points at a `uint32*` and `uint32 *p` points at a `uint32`. Collapsing
    /// them lost the difference, and dereferencing the first one twice is
    /// exactly what a caller does with it.
    fn eat_star(&mut self) -> u8 {
        let mut levels = 0u8;
        while matches!(self.peek(), Some(TokenKind::Star)) {
            self.pos += 1;
            levels = levels.saturating_add(1);
        }
        levels
    }

    /// The type a pointer member decodes as.
    ///
    /// A pointer decodes as an unsigned integer of the namespace's pointer
    /// width. It is not dereferenced during decode: a forensic image is not an
    /// address space, so following an address only means anything when a caller
    /// asks for it, which `Pointer.dereference()` is for.
    ///
    /// Eight bytes is upstream's default but not its only value:
    /// `cstruct(pointer="uint16")` and `cs.pointer = cs.uint16` select two-byte
    /// pointer fields. The schema's namespace-specific width therefore controls
    /// the scalar type used for layout.
    fn pointer_type_name(&self) -> String {
        format!("uint{}", u32::from(self.pointer_size) * 8)
    }

    /// Parse the width after `:` in a bitfield declaration.
    fn parse_bit_width(&mut self) -> Result<u8> {
        let Some(TokenKind::Number(value)) = self.peek() else {
            let found = self.peek().cloned();
            return self.error(format!("expected a bit width after ':', found {found:?}"));
        };
        let value = *value;
        self.pos += 1;
        // A width of zero is C's "align to the next unit" idiom. Upstream has no
        // handling for it, and guessing would silently shift every later field.
        if value <= 0 || value > 64 {
            return Err(Error::InvalidValue {
                message: format!("bit width out of range: {value}"),
            });
        }
        Ok(value as u8)
    }

    /// Parse a type specifier, which may be several words (`unsigned long long`).
    ///
    /// The ambiguity is real: in `unsigned long long time_t;` every word is an
    /// identifier. The final identifier before a declarator delimiter is the
    /// field name; every earlier C type word belongs to the type spelling. Type
    /// resolution happens afterwards so an invalid multipart name is reported
    /// whole (`unsigned long long unsigned`), as upstream reports it, while
    /// malformed `uint32 a b` still fails as a declaration rather than being
    /// reinterpreted as a type named `uint32 a`.
    fn parse_type_spec(&mut self, schema: &Schema) -> Result<String> {
        let mut parts = vec![self.expect_ident()?];

        while matches!(self.peek(), Some(TokenKind::Ident(_))) {
            match self.tokens.get(self.pos + 1).map(|t| &t.kind) {
                // The next identifier is followed by something that ends a
                // declarator, so it is the field name.
                Some(
                    TokenKind::Semicolon
                    | TokenKind::LBracket
                    | TokenKind::Colon
                    | TokenKind::Comma
                    | TokenKind::RBrace,
                ) => {
                    let accumulated = parts.join(" ");
                    let last_is_type_word = parts.last().is_some_and(|part| {
                        matches!(
                            part.as_str(),
                            "signed" | "unsigned" | "short" | "long" | "int" | "char"
                        )
                    });
                    if schema.types.contains_key(&accumulated) || last_is_type_word {
                        break;
                    }
                    parts.push(self.expect_ident()?);
                }
                // `unsigned long *p` -- this is the last word of the type.
                Some(TokenKind::Star) => {
                    parts.push(self.expect_ident()?);
                    break;
                }
                Some(TokenKind::Ident(_)) => parts.push(self.expect_ident()?),
                _ => break,
            }
        }

        Ok(parts.join(" "))
    }

    /// Parse an enum member's value, which may be an expression.
    ///
    /// Flag enums are usually written as shifts — `LEAF_NODE = 1 << 1` — so a
    /// parser that accepts only literals here cannot read them.
    ///
    /// Earlier members of the same enum are in scope, as are the schema's
    /// constants, because C allows both:
    ///
    /// ```text
    /// enum E : uint8 { A = 1, B = A << 1, C = B | A };
    /// ```
    fn parse_enum_member_value(
        &mut self,
        schema: &Schema,
        members: &[(String, i128)],
    ) -> Result<i128> {
        let mut text = String::new();
        let mut depth = 0i32;
        loop {
            match self.peek() {
                Some(TokenKind::Comma) | Some(TokenKind::RBrace) | None if depth == 0 => break,
                // `EDITING_ENABLED = 1` on one line and `MACROS_ENABLED = 2` on
                // the next, with no comma between them -- which upstream
                // tolerates and `dissect.target`'s trusteddocs relies on. An
                // identifier followed by `=` starts a new member and cannot be
                // part of this one's value; identifiers *inside* an expression
                // are never followed by `=`.
                Some(TokenKind::Ident(_))
                    if depth == 0
                        && matches!(
                            self.tokens.get(self.pos + 1).map(|t| &t.kind),
                            Some(TokenKind::Equals)
                        ) =>
                {
                    break
                }
                _ => {}
            }
            match self.next() {
                Some(TokenKind::Ident(word)) => text.push_str(word),
                Some(TokenKind::Number(value)) => text.push_str(&value.to_string()),
                Some(TokenKind::LParen) => {
                    depth += 1;
                    text.push('(');
                }
                Some(TokenKind::RParen) => {
                    depth -= 1;
                    text.push(')');
                }
                Some(TokenKind::Star) => text.push('*'),
                Some(TokenKind::Colon) => text.push(':'),
                Some(TokenKind::Punct(ch)) => text.push(*ch),
                Some(other) => {
                    let other = other.clone();
                    return self.error(format!(
                        "unexpected token in enum value: {}",
                        other.describe()
                    ));
                }
                None => break,
            }
        }

        let trimmed = text.trim();
        if trimmed.is_empty() {
            return self.error("enum member has an empty value");
        }
        // A bare integer is the common case and needs no evaluator.
        if let Ok(value) = trimmed.parse::<i128>() {
            return Ok(value);
        }

        let expr = crate::expr::Expr::parse(trimmed)?;
        let scope = EnumScope { schema, members };
        expr.eval(&scope)
    }

    /// Parse the inside of `[...]`, up to and including the closing bracket.
    fn parse_array_length(&mut self) -> Result<ArrayLen> {
        // `[]` -- null-terminated or read-to-end.
        if matches!(self.peek(), Some(TokenKind::RBracket)) {
            self.pos += 1;
            return Ok(ArrayLen::NullTerminated);
        }

        // `[EOF]` -- everything that remains.
        if matches!(self.peek(), Some(TokenKind::Ident(word)) if word == "EOF")
            && matches!(
                self.tokens.get(self.pos + 1).map(|t| &t.kind),
                Some(TokenKind::RBracket)
            )
        {
            self.pos += 2;
            return Ok(ArrayLen::ToEnd);
        }

        // A single integer literal is the common case and is resolved now.
        if let Some(TokenKind::Number(value)) = self.peek() {
            let value = *value;
            if matches!(
                self.tokens.get(self.pos + 1).map(|t| &t.kind),
                Some(TokenKind::RBracket)
            ) {
                self.pos += 2;
                let count = u32::try_from(value).map_err(|_| Error::InvalidValue {
                    message: format!("array length out of range: {value}"),
                })?;
                return Ok(ArrayLen::Fixed(count));
            }
        }

        // Anything else is an expression naming other fields or constants. It
        // is kept as text and evaluated at decode time, when the values it
        // refers to exist.
        let mut expr = String::new();
        let mut depth = 0usize;
        loop {
            match self.next() {
                Some(TokenKind::RBracket) if depth == 0 => break,
                Some(TokenKind::LBracket) => {
                    depth += 1;
                    expr.push('[');
                }
                Some(TokenKind::RBracket) => {
                    depth -= 1;
                    expr.push(']');
                }
                Some(TokenKind::Ident(name)) => expr.push_str(name),
                Some(TokenKind::Number(value)) => expr.push_str(&value.to_string()),
                Some(TokenKind::Str(_)) => {
                    return self.error("a string literal cannot be an array length")
                }
                Some(TokenKind::LParen) => expr.push('('),
                Some(TokenKind::RParen) => expr.push(')'),
                Some(TokenKind::Star) => expr.push('*'),
                // The only thing a comma can be inside a length is
                // `offsetof(TYPE, member)`'s argument separator -- cstruct has
                // no comma operator. Without this arm `char d[offsetof(h, b)]`
                // was a parse error even though the evaluator and
                // `SchemaConstScope::offset_of` were both built to resolve it,
                // so `sizeof` worked in a definition and its twin did not.
                Some(TokenKind::Comma) => expr.push(','),
                Some(TokenKind::Punct(c)) => expr.push(*c),
                Some(other) => {
                    let other = other.clone();
                    return self.error(format!(
                        "unexpected token in array length: {}",
                        other.describe()
                    ));
                }
                None => return self.error("unterminated array length"),
            }
        }
        Ok(ArrayLen::Expr(expr))
    }

    fn parse_enum(&mut self, schema: &mut Schema, is_flag: bool) -> Result<()> {
        let (schema_name, public_name, anonymous) = match self.peek() {
            Some(TokenKind::Ident(_)) => {
                let name = self.expect_ident()?;
                (name.clone(), name, false)
            }
            _ => (self.next_anonymous_name(), String::new(), true),
        };

        // `enum NAME : BASE {`. The base is optional; upstream defaults it to
        // `uint32`, so this does too.
        let base = if matches!(self.peek(), Some(TokenKind::Colon)) {
            self.pos += 1;
            let mut parts = vec![self.expect_ident()?];
            while !matches!(self.peek(), Some(TokenKind::LBrace)) {
                match self.peek() {
                    Some(TokenKind::Ident(_)) => parts.push(self.expect_ident()?),
                    other => {
                        let other = other.cloned();
                        return self.error(format!(
                            "expected an enum base type, found {}",
                            describe_optional(other.as_ref())
                        ));
                    }
                }
            }
            let name = parts.join(" ");
            // Upstream reports an unknown complete base as a resolution error,
            // not a parser error at the second word of a multipart spelling.
            schema.resolve(&name)?;
            name
        } else {
            "uint32".to_owned()
        };

        self.expect(&TokenKind::LBrace)?;
        let mut members = Vec::new();
        // Enums count upward. Flags choose the next unused power of two, which
        // is upstream's rule even after an explicit value such as `A = 2`.
        let mut next_value: i128 = if is_flag { 1 } else { 0 };
        loop {
            if self.parse_preprocessor(schema)? {
                continue;
            }
            match self.peek() {
                Some(TokenKind::RBrace) => {
                    self.pos += 1;
                    break;
                }
                Some(TokenKind::Comma) => {
                    self.pos += 1;
                }
                None => return self.error("unterminated enum body"),
                _ => {
                    // An enum member may be spelled as a bare number:
                    // `dissect.executable`'s PE virtual-key enum declares
                    // members `0` through `9`. C forbids it, cstruct does not,
                    // and refusing loses the whole PE definition.
                    let member = match self.peek() {
                        Some(TokenKind::Number(value)) => {
                            let text = value.to_string();
                            self.pos += 1;
                            text
                        }
                        _ => self.expect_ident()?,
                    };
                    let value = if matches!(self.peek(), Some(TokenKind::Equals)) {
                        self.pos += 1;
                        self.parse_enum_member_value(schema, &members)?
                    } else {
                        next_value
                    };
                    next_value = if is_flag {
                        let bit_length = i128::BITS - value.unsigned_abs().leading_zeros();
                        1i128
                            .checked_shl(bit_length)
                            .ok_or_else(|| Error::InvalidValue {
                                message: format!(
                                    "flag member {member} leaves no representable automatic value"
                                ),
                            })?
                    } else {
                        value.checked_add(1).ok_or_else(|| Error::InvalidValue {
                            message: format!(
                                "enum member {member} leaves no representable automatic value"
                            ),
                        })?
                    };
                    members.push((member, value));
                }
            }
        }

        let def = EnumDef {
            name: public_name,
            base,
            members: members.clone(),
            is_flag,
        };
        schema.insert_declaration(schema_name.clone(), TypeDef::Enum(def))?;
        if anonymous {
            for (member, value) in members {
                schema.insert_anonymous_enum_member(member, schema_name.clone(), value);
            }
        }

        // Enums accept the same trailing-alias tail as structs, and the same
        // tolerance for a missing semicolon.
        loop {
            if self.at_declaration_start() {
                return Ok(());
            }
            match self.peek() {
                Some(TokenKind::Semicolon) => {
                    self.pos += 1;
                    return Ok(());
                }
                Some(TokenKind::Comma) => {
                    self.pos += 1;
                }
                Some(TokenKind::Ident(_)) => {
                    let alias = self.expect_ident()?;
                    let target = TypeRef::Named(schema_name.clone());
                    schema.insert_declaration(alias, TypeDef::Alias(target))?;
                }
                other => {
                    let other = other.cloned();
                    return self.error(format!(
                        "unexpected token after enum body: {}",
                        describe_optional(other.as_ref())
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Scalar;

    fn struct_of(schema: &Schema, name: &str) -> StructDef {
        match schema.resolve(name).unwrap() {
            TypeDef::Struct(def) => def.clone(),
            other => panic!("{name} is not a struct: {other:?}"),
        }
    }

    #[test]
    fn parses_a_simple_struct() {
        let schema = parse("struct entry { uint16 a; uint32 b; };", Endian::Little).unwrap();
        let def = struct_of(&schema, "entry");
        assert_eq!(def.fields.len(), 2);
        assert_eq!(def.fields[0].name.as_deref(), Some("a"));
        assert!(!def.is_union);
    }

    /// `typedef struct _FOO { ... } FOO;` is `dissect.ntfs`'s house style, so
    /// both the tag and the alias must resolve to the same type.
    #[test]
    fn typedef_struct_registers_tag_and_alias() {
        let schema = parse(
            "typedef struct _MFT_SEGMENT_REFERENCE {
                 ULONG SegmentNumberLowPart;
                 USHORT SegmentNumberHighPart;
                 USHORT SequenceNumber;
             } MFT_SEGMENT_REFERENCE;",
            Endian::Little,
        )
        .unwrap();
        let tag = struct_of(&schema, "_MFT_SEGMENT_REFERENCE");
        let alias = struct_of(&schema, "MFT_SEGMENT_REFERENCE");
        assert_eq!(tag, alias);
        assert_eq!(tag.fields.len(), 3);
    }

    #[test]
    fn parses_flags_with_a_base_type() {
        let schema = parse(
            "flag FILE_ATTRIBUTE : DWORD { READONLY = 0x1, HIDDEN = 0x2, };",
            Endian::Little,
        )
        .unwrap();
        match schema.resolve("FILE_ATTRIBUTE").unwrap() {
            TypeDef::Enum(def) => {
                assert!(def.is_flag);
                assert_eq!(def.base, "DWORD");
                assert_eq!(
                    def.members,
                    vec![("READONLY".to_owned(), 1), ("HIDDEN".to_owned(), 2),]
                );
            }
            other => panic!("expected an enum, got {other:?}"),
        }
    }

    /// C enums continue from the previous value when one is omitted.
    #[test]
    fn enum_members_auto_increment() {
        let schema = parse("enum E : uint8 { A, B, C = 7, D };", Endian::Little).unwrap();
        match schema.resolve("E").unwrap() {
            TypeDef::Enum(def) => assert_eq!(
                def.members,
                vec![
                    ("A".to_owned(), 0),
                    ("B".to_owned(), 1),
                    ("C".to_owned(), 7),
                    ("D".to_owned(), 8),
                ]
            ),
            other => panic!("expected an enum, got {other:?}"),
        }
    }

    #[test]
    fn parses_the_three_array_forms() {
        let schema = parse(
            "struct s { char fixed[8]; wchar named[NameLength]; uint8 rest[]; };",
            Endian::Little,
        )
        .unwrap();
        let def = struct_of(&schema, "s");
        assert!(matches!(
            &def.fields[0].ty,
            TypeRef::Array(_, ArrayLen::Fixed(8))
        ));
        assert!(
            matches!(&def.fields[1].ty, TypeRef::Array(_, ArrayLen::Expr(e)) if e == "NameLength")
        );
        assert!(matches!(
            &def.fields[2].ty,
            TypeRef::Array(_, ArrayLen::NullTerminated)
        ));
    }

    /// Mirrors `dissect.ntfs.c_ntfs._INDEX_ENTRY`, where an anonymous union
    /// contains both a named record and an anonymous structure.
    #[test]
    fn parses_the_index_entry_shape() {
        let schema = parse(
            "typedef struct _MFT_SEGMENT_REFERENCE {
                 ULONG Low; USHORT High; USHORT Seq;
             } FILE_REFERENCE;
             typedef struct _INDEX_ENTRY {
                 union {
                     FILE_REFERENCE FileReference;
                     struct {
                         USHORT DataOffset;
                         USHORT DataLength;
                         ULONG  _;
                     };
                 };
                 USHORT Length;
                 USHORT KeyLength;
                 USHORT Flags;
                 USHORT Reserved;
             } INDEX_ENTRY;",
            Endian::Little,
        )
        .unwrap();
        let def = struct_of(&schema, "INDEX_ENTRY");
        // The anonymous union is one member; the four trailing shorts follow.
        assert_eq!(def.fields.len(), 5);
        assert!(
            def.fields[0].name.is_none(),
            "the union member is anonymous"
        );
        assert_eq!(def.fields[1].name.as_deref(), Some("Length"));
    }

    /// A `#define` whose value is neither an integer, a quoted string, nor an
    /// expression is carried as raw text rather than refused.
    #[test]
    fn unparseable_define_values_are_carried_as_text() {
        let schema = parse(
            "#define ADSEGMENTEDFILE_MAGIC ADSEGMENTEDFILE\\00\nstruct s { uint8 a; };",
            Endian::Little,
        )
        .expect("an odd constant must not make the definition unloadable");
        assert!(schema.constants.contains_key("ADSEGMENTEDFILE_MAGIC"));
        assert!(schema.resolve("s").is_ok());
    }

    /// `dissect.eventlog` closes a struct with `}` and no `;`, then starts the
    /// next declaration. Upstream accepts it.
    #[test]
    fn a_missing_trailing_semicolon_is_accepted() {
        let schema = parse(
            "struct PRVA_HEADER { uint32 data_offset; }\n\nstruct PRVA { uint32 unknown; };",
            Endian::Little,
        )
        .expect("a missing semicolon must not make the definition unparseable");
        assert!(schema.resolve("PRVA_HEADER").is_ok());
        assert!(schema.resolve("PRVA").is_ok());
    }

    #[test]
    fn defines_become_constants() {
        let schema = parse(
            "#define MAX_PATH 260\nstruct s { uint8 p[MAX_PATH]; };",
            Endian::Little,
        )
        .unwrap();
        assert_eq!(
            schema.constants.get("MAX_PATH"),
            Some(&ConstValue::Int(260))
        );
    }

    #[test]
    fn multiple_names_per_declaration() {
        let schema = parse("struct s { uint32 a, b; };", Endian::Little).unwrap();
        let def = struct_of(&schema, "s");
        assert_eq!(def.fields.len(), 2);
        assert_eq!(def.fields[1].name.as_deref(), Some("b"));
    }

    #[test]
    fn typedef_alias_resolves_to_a_scalar() {
        let schema = parse("typedef uint32 MYINT;", Endian::Little).unwrap();
        assert_eq!(
            schema.resolve("MYINT").unwrap(),
            &TypeDef::Scalar(Scalar::Uint(4))
        );
    }

    /// Bitfield widths come through the schema, not just the syntax: a parser
    /// that accepts `uint32 a : 4` but discards the `4` produces a type whose
    /// fields all claim the full storage width.
    #[test]
    fn bitfields_parse_with_their_widths() {
        let schema = parse("struct s { uint32 a : 4; uint32 b : 12; };", Endian::Little).unwrap();
        let TypeDef::Struct(def) = schema.resolve("s").unwrap() else {
            panic!("expected a struct");
        };
        assert_eq!(def.fields[0].bits, Some(4));
        assert_eq!(def.fields[1].bits, Some(12));
    }

    #[test]
    fn a_zero_or_oversized_bit_width_is_refused() {
        // C's `int : 0;` alignment idiom has no upstream handling, and guessing
        // would shift every later field.
        for source in [
            "struct s { uint32 a : 0; };",
            "struct s { uint32 a : 65; };",
        ] {
            assert!(
                matches!(
                    parse(source, Endian::Little),
                    Err(Error::InvalidValue { .. })
                ),
                "{source} should be refused"
            );
        }
    }

    /// `typedef unsigned long long time_t;` — every word is an identifier, and
    /// only the type table says where the type ends and the name begins.
    #[test]
    fn multi_word_integer_types_resolve() {
        let schema = parse(
            "typedef unsigned long long u64_t;\ntypedef long long s64_t;\nstruct s { unsigned long a; long b; };",
            Endian::Little,
        )
        .unwrap();
        assert_eq!(
            schema.resolve("u64_t").unwrap(),
            &TypeDef::Scalar(Scalar::Uint(8))
        );
        assert_eq!(
            schema.resolve("s64_t").unwrap(),
            &TypeDef::Scalar(Scalar::Int(8))
        );
        let TypeDef::Struct(def) = schema.resolve("s").unwrap() else {
            panic!("expected a struct");
        };
        assert_eq!(def.fields[0].ty, TypeRef::Named("unsigned long".to_owned()));
        assert_eq!(def.fields[1].ty, TypeRef::Named("long".to_owned()));
    }

    #[test]
    fn multipart_enum_bases_and_unknown_field_types_are_reported_whole() {
        let schema = parse(
            "enum E : unsigned int { A = 1 }; struct s { unsigned long long value; };",
            Endian::Little,
        )
        .unwrap();
        let TypeDef::Enum(definition) = schema.resolve("E").unwrap() else {
            panic!("expected an enum");
        };
        assert_eq!(definition.base, "unsigned int");

        let error = parse(
            "struct bad { unsigned long long unsigned value; };",
            Endian::Little,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            Error::UnknownType { ref name } if name == "unsigned long long unsigned"
        ));
    }

    /// `unsigned char` is `char`, not `uint8`: upstream produces `bytes` for it.
    /// A tidier mapping would silently change a value's Python type.
    #[test]
    fn unsigned_char_stays_bytes_like_upstream() {
        let schema = parse("struct s { unsigned char a; };", Endian::Little).unwrap();
        assert_eq!(
            schema.resolve("unsigned char").unwrap(),
            &TypeDef::Scalar(Scalar::Char)
        );
        let _ = schema;
    }

    /// `struct exit_status ut_exit;` — an elaborated specifier, not a
    /// definition. `utmp` and `lastlog` both use it.
    #[test]
    fn elaborated_struct_reference_is_a_field_not_a_definition() {
        let schema = parse(
            "struct exit_status { uint16 e_termination; uint16 e_exit; };\n\
             struct entry { struct exit_status ut_exit; uint32 ut_session; };",
            Endian::Little,
        )
        .unwrap();
        let TypeDef::Struct(def) = schema.resolve("entry").unwrap() else {
            panic!("expected a struct");
        };
        assert_eq!(def.fields.len(), 2);
        assert_eq!(def.fields[0].name.as_deref(), Some("ut_exit"));
        assert_eq!(def.fields[0].ty, TypeRef::Named("exit_status".to_owned()));
    }

    /// A pointer is a pointer-sized integer. There is nothing to dereference:
    /// a forensic image is not an address space.
    #[test]
    fn pointers_read_as_pointer_sized_integers() {
        let schema = parse(
            "typedef uint32 *PULONG;\nstruct s { uint32 *p; char **argv; };",
            Endian::Little,
        )
        .unwrap();
        assert_eq!(
            schema.resolve("PULONG").unwrap(),
            &TypeDef::Scalar(Scalar::Uint(8))
        );
        let TypeDef::Struct(def) = schema.resolve("s").unwrap() else {
            panic!("expected a struct");
        };
        assert_eq!(def.fields[0].name.as_deref(), Some("p"));
        assert_eq!(def.fields[1].name.as_deref(), Some("argv"));
    }

    #[test]
    fn parse_errors_name_a_line() {
        let err = parse("struct s {\n  uint32 a\n", Endian::Little).unwrap_err();
        match err {
            Error::Parse { line, .. } => assert!(line >= 2, "error should point past line 1"),
            other => panic!("expected a parse error, got {other:?}"),
        }
    }
}
