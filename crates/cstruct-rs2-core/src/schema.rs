//! The schema IR: what a parsed cstruct definition means, before it is compiled.
//!
//! A cstruct definition is a small C-like language. Parsing it produces the
//! types in this module; compiling those produces a [`crate::plan::CompiledPlan`],
//! which is what the decoder actually walks.
//!
//! The split is deliberate. The IR is shaped for *clarity* — named types,
//! resolved lazily, nested structures as trees. The plan is shaped for *the
//! cache* — flat arrays, dense opcodes, every size that can be known already
//! resolved. Keeping them apart means the readable form does not have to
//! compromise for the hot form, and the hot form does not have to stay
//! readable.

use std::collections::{HashMap, HashSet};

use crate::error::{Error, Result};

/// Byte order for multi-byte scalars.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Endian {
    /// Least significant byte first. The default, as in upstream.
    #[default]
    Little,
    /// Most significant byte first.
    Big,
}

/// A primitive type: the leaves of every structure.
///
/// `Hash` so a scalar can key a cache *by what it is*. Keying on its display
/// name instead is what let `cs.uint48` hand back a `uint24` type object: every
/// width without a spelled-out name shared the string `"scalar"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scalar {
    /// Unsigned integer of the given width in bytes (1, 2, 4, 8).
    Uint(u8),
    /// Signed integer of the given width in bytes (1, 2, 4, 8).
    Int(u8),
    /// A single byte, produced as Python `bytes` rather than an int.
    ///
    /// `char` and `uint8` differ only in what Python object they become, which
    /// is precisely the kind of distinction the compatibility contract cares
    /// about and a decoder is tempted to flatten.
    Char,
    /// A UTF-16 code unit, produced as Python `str`.
    Wchar,
    /// A zero-width placeholder that consumes and produces no bytes.
    Void,
    /// IEEE 754 binary32 or binary64, by width in bytes (4 or 8).
    Float(u8),
    /// A LEB128 variable-length integer, signed or unsigned.
    ///
    /// The only scalar whose width is not known before reading: it is
    /// self-delimiting, one continuation bit per byte. `dissect.target`'s
    /// Windows Notepad reader uses it for every length in its tab format.
    Leb128 {
        /// Whether the last value bit is a sign bit.
        signed: bool,
    },
}

impl Scalar {
    /// Alignment in bytes, as upstream declares it.
    ///
    /// Usually the width, but not always: upstream gives `int24` an alignment
    /// of 4 and `uint48` an alignment of 8, so a natural-alignment layout pads
    /// them to the next power of two rather than to their own odd width.
    pub const fn alignment(self) -> u32 {
        match self {
            Self::Uint(3) | Self::Int(3) => 4,
            Self::Uint(6) | Self::Int(6) => 8,
            Self::Leb128 { .. } | Self::Void => 1,
            other => other.size(),
        }
    }

    /// Width in bytes. Always known: these are the fixed-width leaves.
    pub const fn size(self) -> u32 {
        match self {
            Self::Uint(n) | Self::Int(n) | Self::Float(n) => n as u32,
            Self::Char => 1,
            Self::Wchar => 2,
            // Not a width: a LEB128's length is a property of its bytes. Zero
            // is what marks the field dynamic downstream.
            Self::Leb128 { .. } | Self::Void => 0,
        }
    }
}

/// How many elements an array holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArrayLen {
    /// A literal count known at parse time: `uint8 data[16]`.
    Fixed(u32),
    /// A count naming another field or a constant: `wchar name[NameLength]`.
    ///
    /// Held as text and resolved at decode time against the fields already
    /// read. This is what makes a structure dynamic.
    Expr(String),
    /// `uint8 data[]` — read until a null element, or until the input ends.
    NullTerminated,
    /// `char data[EOF]` — read everything that remains.
    ///
    /// A sentinel of its own rather than an identifier, because `EOF` inside
    /// `[...]` is a keyword in the definition language, not a constant the
    /// caller declared. Resolving it as a name would fail on every definition
    /// that uses it.
    ToEnd,
}

/// A reference to a type, as written in a field declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRef {
    /// A named type: a scalar alias, a struct, an enum, or a typedef.
    Named(String),
    /// An array of some element type.
    Array(Box<TypeRef>, ArrayLen),
}

/// One field of a structure or union.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDef {
    /// The field's name. `None` for an anonymous struct or union member, whose
    /// fields are folded into the parent.
    pub name: Option<String>,
    /// The declared type.
    pub ty: TypeRef,
    /// Width in bits for a bitfield member (`uint32 flags : 4`), else `None`.
    ///
    /// A bitfield does not consume its own bytes: consecutive bitfields of the
    /// same underlying type share one storage unit, and the unit is only read
    /// when the previous one is exhausted or the type changes. See
    /// [`crate::decode`] for the packing rules, which are upstream's.
    pub bits: Option<u8>,
    /// The type this member points *at*, when it was declared with a `*`.
    ///
    /// [`Self::ty`] already says how the field decodes -- a pointer-sized
    /// unsigned integer -- and that is all the layout needs. This is the part
    /// the layout does not need and a caller does: without it a decoded pointer
    /// is an address with nothing to dereference, because the fact that the
    /// declaration said `test *ptr` was thrown away at parse time.
    ///
    /// `uint32 **p` records `uint32*`, so a chain of pointers keeps its depth.
    pub points_to: Option<String>,
    /// Byte offset supplied through the programmatic ``Field`` API.
    ///
    /// Parsed C declarations leave this unset because the language has no
    /// offset syntax. Retaining it here ensures a later generation rebuild
    /// preserves a field added with ``offset=N``.
    pub explicit_offset: Option<u32>,
}

/// A structure or a union.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructDef {
    /// The type's name.
    pub name: String,
    /// Fields in declaration order.
    pub fields: Vec<FieldDef>,
    /// True for a union: every field starts at offset zero and the size is the
    /// largest member's.
    pub is_union: bool,
    /// True when this type was declared under `load(..., align=True)`.
    ///
    /// Alignment is a property of the *declaration*, not of the namespace:
    /// upstream stamps `__align__` on the types a given `load()` creates, and
    /// `dissect.target`'s atop reader loads one definition unaligned and the
    /// next aligned into the same namespace.
    pub align: bool,
}

/// An enum or a flag set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumDef {
    /// The type's name.
    pub name: String,
    /// The underlying integer type, named as written (`DWORD`, `uint16`, ...).
    pub base: String,
    /// Members in declaration order.
    pub members: Vec<(String, i128)>,
    /// True for `flag`, which becomes a Python `IntFlag` rather than `IntEnum`.
    pub is_flag: bool,
}

/// Anything a definition can declare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeDef {
    /// A primitive.
    Scalar(Scalar),
    /// A structure or union.
    Struct(StructDef),
    /// An enum or flag set.
    Enum(EnumDef),
    /// A name for another type: `typedef uint32 ULONG;`.
    Alias(TypeRef),
    /// A type the consumer supplied through `add_custom_type`.
    ///
    /// The definition language cannot describe it -- `dissect.target`'s
    /// Everything and docker readers both register a varint whose length is
    /// discovered by reading it -- so the schema records only that the name
    /// exists and leaves the reading to the binding layer.
    Custom,
}

/// The value of a `#define`.
///
/// Integers are the interesting case -- they can be array lengths. Strings are
/// carried because upstream exposes them as module constants and a consumer may
/// read one; they simply cannot size anything, and [`crate::plan`] says so with
/// a clear error if one is used that way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstValue {
    /// An integer constant, usable as an array length.
    Int(i128),
    /// A string constant. Never usable as a length.
    Str(String),
    /// A `b"..."` constant, escapes resolved. Never usable as a length.
    Bytes(Vec<u8>),
    /// An expression over other constants, e.g.
    /// `#define EXT4_XATTR_PAD (1<<EXT4_XATTR_PAD_BITS)` in `dissect.extfs`.
    ///
    /// Held unevaluated because it may refer to a constant defined later in the
    /// same file. Resolved on demand by [`Schema::const_int`]. The original
    /// text is retained because upstream exposes an expression that remains
    /// unresolved as a string constant; `cdef()` must not invent a value.
    Expr {
        /// Parsed form used for integer evaluation.
        parsed: crate::expr::Expr,
        /// Source text used when evaluation cannot produce an integer.
        source: String,
    },
}

/// A parsed definition: every type it declares, plus its constants.
#[derive(Debug, Clone, Default)]
pub struct Schema {
    /// Types by name, including the built-in scalars.
    pub types: HashMap<String, TypeDef>,
    /// `#define`d integer constants, usable as array lengths.
    pub constants: HashMap<String, ConstValue>,
    /// Anonymous enum/flag member name to its hidden owning type.
    ///
    /// Upstream publishes these members through the constant namespace, but
    /// the published value is still an enum member rather than a plain integer.
    /// The integer in `constants` keeps expressions such as
    /// `uint32 values[nElements]` foldable; this companion map lets the Python
    /// binding recover the member's class when the attribute is requested.
    pub anonymous_enum_members: HashMap<String, String>,
    /// Constant declaration order, used when rendering a namespace as cdef.
    ///
    /// Upstream's constants live in an insertion-ordered Python dictionary.
    /// The lookup map above deliberately stays a `HashMap` for cheap expression
    /// evaluation; this small companion list preserves the public order without
    /// putting ordered-map work on the decode path.
    pub constant_order: Vec<String>,
    /// Include spellings seen by the definition parser, in encounter order.
    ///
    /// cstruct records includes for callers but does not read the named files.
    /// Keeping the names in the cold schema mirrors that behavior without
    /// involving them in plan compilation or record decoding.
    pub includes: Vec<String>,
    /// Pointer typedef name to the type it addresses.
    ///
    /// Layout still resolves the typedef to the namespace's pointer-sized
    /// integer. This cold companion metadata lets the Python surface build the
    /// same dereferenceable pointer class as an explicit pointer field.
    pub pointer_aliases: HashMap<String, String>,
    /// Byte order for this schema's scalars.
    pub endian: Endian,
    /// Declaration order, so that generated output and reports are stable.
    pub order: Vec<String>,
    /// Width in bytes that `TYPE *name;` reads at.
    ///
    /// A pointer is a pointer-sized integer, and how big that is belongs to the
    /// namespace rather than to the definition: upstream defaults it from
    /// `sys.maxsize` and lets a consumer set `cs.pointer = cs.uint16` for a
    /// format that stores 16-bit offsets. Zero is not a legal value; it means
    /// "never set" and readers should treat it as eight.
    pub pointer_size: u8,
}

impl Schema {
    /// A schema pre-populated with the built-in scalar types and their aliases.
    ///
    /// The aliases matter as much as the scalars. Definitions are usually
    /// transcribed from C headers, so they are written in whatever spelling the
    /// original used — `ULONG`, `USHORT`, `DWORD` far more often than `uint32`.
    /// A parser that knows only the canonical names cannot read them.
    pub fn with_builtins(endian: Endian) -> Self {
        let mut schema = Self {
            endian,
            // 64-bit unless a consumer says otherwise, which is upstream's
            // default on every platform this runs on.
            pointer_size: 8,
            ..Default::default()
        };

        let scalars: &[(&str, Scalar)] = &[
            ("int8", Scalar::Int(1)),
            ("uint8", Scalar::Uint(1)),
            ("int16", Scalar::Int(2)),
            ("uint16", Scalar::Uint(2)),
            ("int32", Scalar::Int(4)),
            ("uint32", Scalar::Uint(4)),
            ("int64", Scalar::Int(8)),
            ("uint64", Scalar::Uint(8)),
            ("float", Scalar::Float(4)),
            ("double", Scalar::Float(8)),
            ("char", Scalar::Char),
            ("wchar", Scalar::Wchar),
            ("void", Scalar::Void),
            // Non-power-of-two widths, declared directly by formats that pack
            // fields to odd sizes — sqlite3's varints, for instance.
            ("int24", Scalar::Int(3)),
            ("uint24", Scalar::Uint(3)),
            ("int48", Scalar::Int(6)),
            ("uint48", Scalar::Uint(6)),
            // Wider than a machine word. Python integers are arbitrary
            // precision, so these lose nothing crossing the boundary; the Rust
            // side reads them through the u128 path rather than `read_uint`,
            // which caps at 64 bits.
            ("int128", Scalar::Int(16)),
            ("uint128", Scalar::Uint(16)),
            // binary16. Upstream spells it `float16`.
            ("float16", Scalar::Float(2)),
            ("uleb128", Scalar::Leb128 { signed: false }),
            ("ileb128", Scalar::Leb128 { signed: true }),
        ];
        for (name, scalar) in scalars {
            schema.insert((*name).to_owned(), TypeDef::Scalar(*scalar));
        }

        // Transcribed from upstream `cstruct.py`'s `initial_types`, deliberately
        // in full rather than trimmed to the spellings something was observed to
        // use.
        //
        // A definition is parsed when its module is imported, so every name in
        // it must resolve even if nothing ever decodes that type. One missing
        // alias is not a missing feature — it is an import error that takes the
        // whole module with it.
        //
        // Multi-word keys (`unsigned long long`) are looked up by the joined
        // spelling; see `Parser::parse_type_spec`.
        let aliases: &[(&str, &str)] = &[
            // Common C types, exactly as upstream maps them. Note `unsigned
            // char` -> `char`, not `uint8`: upstream produces `bytes` for it,
            // and that is a compatibility fact, not a typo to tidy up.
            ("signed char", "int8"),
            ("unsigned char", "char"),
            ("short", "int16"),
            ("signed short", "int16"),
            ("unsigned short", "uint16"),
            ("int", "int32"),
            ("signed int", "int32"),
            ("unsigned int", "uint32"),
            ("long", "int32"),
            ("signed long", "int32"),
            ("unsigned long", "uint32"),
            ("long long", "int64"),
            ("signed long long", "int64"),
            ("unsigned long long", "uint64"),
            // Convenience spellings.
            ("u8", "uint8"),
            ("u16", "uint16"),
            ("u32", "uint32"),
            ("u64", "uint64"),
            ("__u8", "uint8"),
            ("__u16", "uint16"),
            ("__u32", "uint32"),
            ("__u64", "uint64"),
            ("uchar", "uint8"),
            ("ushort", "uint16"),
            ("uint", "uint32"),
            ("ulong", "uint32"),
            // Windows types.
            ("BYTE", "uint8"),
            ("CHAR", "char"),
            ("SHORT", "int16"),
            ("WORD", "uint16"),
            ("DWORD", "uint32"),
            ("LONG", "int32"),
            ("LONG32", "int32"),
            ("LONG64", "int64"),
            ("LONGLONG", "int64"),
            ("QWORD", "uint64"),
            ("WCHAR", "wchar"),
            ("UCHAR", "uint8"),
            ("USHORT", "uint16"),
            ("ULONG", "uint32"),
            ("ULONG64", "uint64"),
            ("ULONGLONG", "uint64"),
            ("INT", "int32"),
            ("INT8", "int8"),
            ("INT16", "int16"),
            ("INT32", "int32"),
            ("INT64", "int64"),
            ("UINT", "uint32"),
            ("UINT8", "uint8"),
            ("UINT16", "uint16"),
            ("UINT32", "uint32"),
            ("UINT64", "uint64"),
            ("__int8", "int8"),
            ("__int16", "int16"),
            ("__int32", "int32"),
            ("__int64", "int64"),
            ("unsigned __int8", "uint8"),
            ("unsigned __int16", "uint16"),
            ("unsigned __int32", "uint32"),
            ("unsigned __int64", "uint64"),
            ("wchar_t", "wchar"),
            // GNU C types.
            ("int8_t", "int8"),
            ("int16_t", "int16"),
            ("int32_t", "int32"),
            ("int64_t", "int64"),
            ("uint8_t", "uint8"),
            ("uint16_t", "uint16"),
            ("uint32_t", "uint32"),
            ("uint64_t", "uint64"),
            // IDA types.
            ("_BYTE", "uint8"),
            ("_WORD", "uint16"),
            ("_DWORD", "uint32"),
            ("_QWORD", "uint64"),
            ("FILETIME", "uint64"),
        ];
        for (alias, target) in aliases {
            schema.insert(
                (*alias).to_owned(),
                TypeDef::Alias(TypeRef::Named((*target).to_owned())),
            );
        }

        schema
    }

    /// Add or replace a type, recording declaration order for new names.
    pub fn insert(&mut self, name: String, def: TypeDef) {
        if !self.types.contains_key(&name) {
            self.order.push(name.clone());
        }
        self.types.insert(name, def);
    }

    /// Add or replace a constant while preserving its first declaration order.
    pub fn insert_constant(&mut self, name: String, value: ConstValue) {
        if !self.constants.contains_key(&name) {
            self.constant_order.push(name.clone());
        }
        // A later `#define` or explicit `add_const` with the same name replaces
        // the anonymous member in upstream's ordinary constant dictionary.
        self.anonymous_enum_members.remove(&name);
        self.constants.insert(name, value);
    }

    /// Publish one anonymous enum member as both an integer expression and a
    /// typed namespace value.
    pub fn insert_anonymous_enum_member(&mut self, name: String, owner: String, value: i128) {
        self.insert_constant(name.clone(), ConstValue::Int(value));
        self.anonymous_enum_members.insert(name, owner);
    }

    /// Remove a constant from both lookup storage and rendered order.
    pub fn remove_constant(&mut self, name: &str) -> Option<ConstValue> {
        let removed = self.constants.remove(name);
        if removed.is_some() {
            self.constant_order.retain(|entry| entry != name);
            self.anonymous_enum_members.remove(name);
        }
        removed
    }

    /// Insert a declaration while enforcing upstream's duplicate-type rules.
    ///
    /// Repeating an equivalent typedef is accepted because generated C headers
    /// commonly do so. Equivalence follows aliases: for example, the built-in
    /// `FILETIME -> uint64` and a later `typedef QWORD FILETIME` name the same
    /// type. A different typedef or any repeated concrete type is a duplicate,
    /// even if its fields happen to be identical.
    pub fn insert_declaration(&mut self, name: String, def: TypeDef) -> Result<()> {
        if let Some(existing) = self.types.get(&name) {
            if matches!((existing, &def), (TypeDef::Alias(left), TypeDef::Alias(right)) if self.aliases_are_equivalent(left, right))
            {
                return Ok(());
            }
            return Err(Error::InvalidValue {
                message: format!("Duplicate type: {name}"),
            });
        }
        self.insert(name, def);
        Ok(())
    }

    /// Compare typedef targets by meaning rather than by their written alias.
    fn aliases_are_equivalent(&self, left: &TypeRef, right: &TypeRef) -> bool {
        if left == right {
            return true;
        }
        let (TypeRef::Named(left), TypeRef::Named(right)) = (left, right) else {
            return false;
        };
        matches!((self.resolve(left), self.resolve(right)), (Ok(left), Ok(right)) if left == right)
    }

    /// Render user declarations as a semantically equivalent cstruct source.
    ///
    /// Current upstream implements this on its Python type graph. This
    /// implementation renders the retained Rust schema instead, so `cdef()` is
    /// a cold namespace operation and never reparses the original definition.
    pub fn cdef(&self) -> String {
        let builtins = Self::with_builtins(self.endian);
        let mut blocks = Vec::new();

        let defines = self
            .constant_order
            .iter()
            .filter(|name| !self.anonymous_enum_members.contains_key(*name))
            .filter_map(|name| {
                self.constants
                    .get(name)
                    .map(|value| render_define(name, value, self))
            })
            .collect::<Vec<_>>();
        if !defines.is_empty() {
            blocks.push(defines.join("\n"));
        }

        for name in &self.order {
            if builtins.types.contains_key(name) || name.starts_with("__anonymous_") {
                continue;
            }
            let Some(definition) = self.types.get(name) else {
                continue;
            };
            match definition {
                TypeDef::Struct(definition) if definition.name == *name => {
                    blocks.push(render_struct(definition, self));
                }
                TypeDef::Enum(definition) if definition.name == *name => {
                    blocks.push(render_enum(definition));
                }
                TypeDef::Alias(TypeRef::Named(target)) => {
                    let resolved = self.resolve(target).ok();
                    if matches!(resolved, Some(TypeDef::Struct(_) | TypeDef::Enum(_))) {
                        blocks.push(format!("typedef {target} {name};"));
                    }
                }
                _ => {}
            }
        }

        blocks.join("\n\n")
    }

    /// Render one structure, optionally with the declarations it depends on.
    ///
    /// This is the native counterpart of upstream's ``Structure.cdef``.  The
    /// retained schema is already the source of truth, so rendering does not
    /// parse the original definition again and does not involve a compiled read
    /// plan.
    pub fn type_cdef(&self, name: &str, recursive: bool) -> Result<String> {
        let definition = self.resolve(name)?;
        if !matches!(definition, TypeDef::Struct(_)) {
            return Err(Error::InvalidValue {
                message: format!("{name} is not a structure or union"),
            });
        }

        let mut declarations = Vec::new();
        let mut seen = HashSet::new();
        seen.insert(name.to_owned());
        if recursive {
            collect_type_dependencies(name, self, &mut seen, &mut declarations)?;
        }
        declarations.push(name.to_owned());

        let mut referenced_constants = HashSet::new();
        if recursive {
            for dependency in &declarations {
                if let Ok(TypeDef::Struct(structure)) = self.resolve(dependency) {
                    collect_structure_constants(structure, self, &mut referenced_constants);
                }
            }
        }

        let defines = self
            .constant_order
            .iter()
            .filter(|constant| referenced_constants.contains(*constant))
            .filter_map(|constant| {
                self.constants
                    .get(constant)
                    .map(|value| render_define(constant, value, self))
            })
            .collect::<Vec<_>>();

        let mut blocks = Vec::new();
        if !defines.is_empty() {
            blocks.push(defines.join("\n"));
        }
        for dependency in declarations {
            match self.resolve(&dependency)? {
                TypeDef::Struct(structure) => blocks.push(render_type_struct(structure, self)),
                TypeDef::Enum(enumeration) => blocks.push(render_enum(enumeration)),
                _ => {}
            }
        }
        Ok(blocks.join("\n\n"))
    }

    /// Resolve a constant to an integer, evaluating an expression if needed.
    ///
    /// Returns `None` for a string constant or an unknown name. Expression
    /// constants may refer to each other, so evaluation is bounded by depth
    /// rather than trusted to terminate: a definition is input.
    pub fn const_int(&self, name: &str) -> Option<i128> {
        self.const_int_depth(name, 0)
    }

    fn const_int_depth(&self, name: &str, depth: usize) -> Option<i128> {
        const MAX_CONST_DEPTH: usize = 16;
        if depth >= MAX_CONST_DEPTH {
            return None;
        }
        match self.constants.get(name)? {
            ConstValue::Int(value) => Some(*value),
            ConstValue::Str(_) | ConstValue::Bytes(_) => None,
            ConstValue::Expr { parsed: expr, .. } => {
                let scope = ConstScope {
                    schema: self,
                    depth: depth + 1,
                };
                expr.eval(&scope).ok()
            }
        }
    }

    /// Follow aliases until a concrete definition is reached.
    ///
    /// Alias chains are short in practice (`ULONG` -> `uint32`), but the depth
    /// is bounded anyway: a definition is input, and input that can make the
    /// resolver loop forever is a denial of service.
    pub fn resolve<'a>(&'a self, name: &str) -> Result<&'a TypeDef> {
        const MAX_ALIAS_DEPTH: usize = 16;
        let mut current = name;
        for _ in 0..MAX_ALIAS_DEPTH {
            match self.types.get(current) {
                Some(TypeDef::Alias(TypeRef::Named(target))) => current = target,
                Some(def) => return Ok(def),
                None => {
                    return Err(Error::UnknownType {
                        name: current.to_owned(),
                    })
                }
            }
        }
        Err(Error::InvalidValue {
            message: format!("alias chain for {name} is too deep or circular"),
        })
    }
}

/// Append recursive structure/enum dependencies in declaration order.
fn collect_type_dependencies(
    name: &str,
    schema: &Schema,
    seen: &mut HashSet<String>,
    out: &mut Vec<String>,
) -> Result<()> {
    let TypeDef::Struct(structure) = schema.resolve(name)? else {
        return Ok(());
    };
    for field in &structure.fields {
        let Some(dependency) = field_dependency_name(field) else {
            continue;
        };
        let Ok(definition) = schema.resolve(dependency) else {
            continue;
        };
        match definition {
            TypeDef::Struct(nested) if nested.name.starts_with("__anonymous_") => {
                collect_type_dependencies(&nested.name, schema, seen, out)?;
            }
            TypeDef::Struct(nested) => {
                if seen.insert(nested.name.clone()) {
                    collect_type_dependencies(&nested.name, schema, seen, out)?;
                    out.push(nested.name.clone());
                }
            }
            TypeDef::Enum(enumeration) if seen.insert(enumeration.name.clone()) => {
                out.push(enumeration.name.clone());
            }
            _ => {}
        }
    }
    Ok(())
}

/// Return the named base of a field after removing arrays and pointers.
fn field_dependency_name(field: &FieldDef) -> Option<&str> {
    if let Some(target) = field.points_to.as_deref() {
        return Some(target.trim_end_matches('*'));
    }
    let mut current = &field.ty;
    while let TypeRef::Array(inner, _) = current {
        current = inner;
    }
    match current {
        TypeRef::Named(name) => Some(name),
        TypeRef::Array(_, _) => unreachable!("array loop removes every dimension"),
    }
}

/// Find constants named in array expressions, including anonymous members.
fn collect_structure_constants(structure: &StructDef, schema: &Schema, out: &mut HashSet<String>) {
    for field in &structure.fields {
        collect_type_ref_constants(&field.ty, schema, out);
        let Some(dependency) = field_dependency_name(field) else {
            continue;
        };
        if let Ok(TypeDef::Struct(nested)) = schema.resolve(dependency) {
            if nested.name.starts_with("__anonymous_") {
                collect_structure_constants(nested, schema, out);
            }
        }
    }
}

fn collect_type_ref_constants(reference: &TypeRef, schema: &Schema, out: &mut HashSet<String>) {
    let TypeRef::Array(inner, length) = reference else {
        return;
    };
    if let ArrayLen::Expr(source) = length {
        // A bare constant is rendered as its resolved integer, matching
        // upstream, so it no longer needs a definition in the recursive
        // output. Constants inside a larger expression retain their names.
        if source
            .chars()
            .all(|character| character == '_' || character.is_alphanumeric())
            && schema.const_int(source).is_some()
        {
            collect_type_ref_constants(inner, schema, out);
            return;
        }
        let mut identifier = String::new();
        for character in source.chars().chain(std::iter::once(' ')) {
            if character == '_' || character.is_ascii_alphanumeric() {
                identifier.push(character);
            } else if !identifier.is_empty() {
                if schema.constants.contains_key(&identifier) {
                    out.insert(std::mem::take(&mut identifier));
                } else {
                    identifier.clear();
                }
            }
        }
    }
    collect_type_ref_constants(inner, schema, out);
}

fn render_define(name: &str, value: &ConstValue, schema: &Schema) -> String {
    let rendered = match value {
        ConstValue::Int(value) => value.to_string(),
        ConstValue::Expr { source, .. } => schema
            .const_int(name)
            .map_or_else(|| quote_python_string(source), |value| value.to_string()),
        ConstValue::Str(value) => quote_python_string(value),
        ConstValue::Bytes(value) => format!("b{}", quote_python_bytes(value)),
    };
    format!("#define {name} {rendered}")
}

fn quote_python_string(value: &str) -> String {
    let mut out = String::from("'");
    for character in value.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('\'');
    out
}

fn quote_python_bytes(value: &[u8]) -> String {
    let mut out = String::from("'");
    for byte in value {
        match byte {
            b'\\' => out.push_str("\\\\"),
            b'\'' => out.push_str("\\'"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x20..=0x7e => out.push(char::from(*byte)),
            other => out.push_str(&format!("\\x{other:02x}")),
        }
    }
    out.push('\'');
    out
}

fn render_enum(definition: &EnumDef) -> String {
    let keyword = if definition.is_flag { "flag" } else { "enum" };
    let mut lines = vec![format!(
        "{keyword} {} : {} {{",
        definition.name, definition.base
    )];
    for (name, value) in &definition.members {
        let value = if definition.is_flag {
            format!("{value:#x}")
        } else {
            value.to_string()
        };
        lines.push(format!("    {name} = {value},"));
    }
    lines.push("};".to_owned());
    lines.join("\n")
}

fn render_struct(definition: &StructDef, schema: &Schema) -> String {
    let keyword = if definition.is_union {
        "union"
    } else {
        "struct"
    };
    let mut lines = vec![format!("{keyword} {} {{", definition.name)];
    for field in &definition.fields {
        let name = field.name.as_deref().unwrap_or("");
        let mut declaration = render_field_type(field, name, schema);
        if let Some(bits) = field.bits {
            declaration.push_str(&format!(" : {bits}"));
        }
        lines.push(format!("    {declaration};"));
    }
    lines.push("};".to_owned());
    lines.join("\n")
}

/// Render the per-type ``cdef`` surface, preserving symbolic array spelling
/// and inlining anonymous structures rather than exposing their generated
/// schema names.
fn render_type_struct(definition: &StructDef, schema: &Schema) -> String {
    render_type_struct_lines(definition, schema, Some(&definition.name), 0).join("\n")
}

fn render_type_struct_lines(
    definition: &StructDef,
    schema: &Schema,
    visible_name: Option<&str>,
    indent: usize,
) -> Vec<String> {
    let padding = " ".repeat(indent);
    let keyword = if definition.is_union {
        "union"
    } else {
        "struct"
    };
    let title = visible_name.map_or_else(
        || format!("{padding}{keyword} {{"),
        |name| format!("{padding}{keyword} {name} {{"),
    );
    let mut lines = vec![title];
    for field in &definition.fields {
        let mut base = &field.ty;
        while let TypeRef::Array(inner, _) = base {
            base = inner;
        }
        let anonymous = match base {
            TypeRef::Named(name) => schema
                .resolve(name)
                .ok()
                .and_then(|resolved| match resolved {
                    TypeDef::Struct(nested) if nested.name.starts_with("__anonymous_") => {
                        Some(nested)
                    }
                    _ => None,
                }),
            TypeRef::Array(_, _) => None,
        };

        if let Some(nested) = anonymous {
            let mut nested_lines = render_type_struct_lines(nested, schema, None, indent + 4);
            let declarator = render_field_declarator(field, schema);
            let last = nested_lines
                .last_mut()
                .expect("an anonymous structure always has a closing line");
            if declarator.is_empty() {
                last.push(';');
            } else {
                last.push(' ');
                last.push_str(&declarator);
                last.push(';');
            }
            lines.extend(nested_lines);
            continue;
        }

        let mut declaration = render_type_field(field, schema);
        if let Some(bits) = field.bits {
            declaration.push_str(&format!(" : {bits}"));
        }
        lines.push(format!("{}    {declaration};", padding));
    }
    lines.push(format!("{padding}}}"));
    if indent == 0 {
        lines.last_mut().expect("closing line exists").push(';');
    }
    lines
}

fn render_type_field(field: &FieldDef, schema: &Schema) -> String {
    let mut dimensions = Vec::new();
    let mut current = &field.ty;
    while let TypeRef::Array(inner, length) = current {
        dimensions.push(render_array_length(length, schema));
        current = inner;
    }
    let name = field.name.as_deref().unwrap_or("");
    let (base, stars) = match field.points_to.as_deref() {
        Some(target) => {
            let base = target.trim_end_matches('*');
            (base, target.len() - base.len() + 1)
        }
        None => match current {
            TypeRef::Named(base) => (base.as_str(), 0),
            TypeRef::Array(_, _) => unreachable!("array loop removes every dimension"),
        },
    };
    let suffix = dimensions
        .into_iter()
        .map(|length| format!("[{length}]"))
        .collect::<String>();
    format!("{base} {}{name}{suffix}", "*".repeat(stars))
}

fn render_field_declarator(field: &FieldDef, schema: &Schema) -> String {
    let mut dimensions = Vec::new();
    let mut current = &field.ty;
    while let TypeRef::Array(inner, length) = current {
        dimensions.push(render_array_length(length, schema));
        current = inner;
    }
    let suffix = dimensions
        .into_iter()
        .map(|length| format!("[{length}]"))
        .collect::<String>();
    let mut declarator = format!("{}{suffix}", field.name.as_deref().unwrap_or(""));
    if let Some(bits) = field.bits {
        if declarator.is_empty() {
            declarator = format!(": {bits}");
        } else {
            declarator.push_str(&format!(" : {bits}"));
        }
    }
    declarator
}

fn render_field_type(field: &FieldDef, name: &str, schema: &Schema) -> String {
    let mut dimensions = Vec::new();
    let mut current = &field.ty;
    while let TypeRef::Array(inner, length) = current {
        dimensions.push(render_array_length(length, schema));
        current = inner;
    }

    let (base, stars) = match &field.points_to {
        Some(target) => {
            let base = target.trim_end_matches('*');
            let stars = target.len() - base.len() + 1;
            (base, stars)
        }
        None => match current {
            TypeRef::Named(base) => (base.as_str(), 0),
            TypeRef::Array(_, _) => unreachable!("array loop removes every dimension"),
        },
    };
    let suffix = dimensions
        .into_iter()
        .map(|length| format!("[{length}]"))
        .collect::<String>();
    format!("{base} {}{name}{suffix}", "*".repeat(stars))
}

fn render_array_length(length: &ArrayLen, schema: &Schema) -> String {
    match length {
        ArrayLen::Fixed(value) => value.to_string(),
        ArrayLen::Expr(text) => {
            if text
                .chars()
                .all(|character| character == '_' || character.is_alphanumeric())
            {
                if let Some(value) = schema.const_int(text) {
                    return value.to_string();
                }
            }
            space_expression(text)
        }
        ArrayLen::NullTerminated => String::new(),
        ArrayLen::ToEnd => "EOF".to_owned(),
    }
}

pub(crate) fn space_expression(text: &str) -> String {
    let mut out = String::new();
    let characters = text.as_bytes();
    let mut index = 0;
    while index < characters.len() {
        let two = characters.get(index..index + 2);
        if matches!(two, Some(b"<<" | b">>")) {
            out.push(' ');
            out.push(char::from(characters[index]));
            out.push(char::from(characters[index + 1]));
            out.push(' ');
            index += 2;
            continue;
        }
        let character = char::from(characters[index]);
        if matches!(
            character,
            '+' | '-' | '*' | '/' | '%' | '&' | '|' | '^' | '?' | ':'
        ) {
            out.push(' ');
            out.push(character);
            out.push(' ');
        } else if character == ',' {
            out.push_str(", ");
        } else {
            out.push(character);
        }
        index += 1;
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Resolves names against a schema's constants, for evaluating a constant that
/// is itself an expression over other constants.
struct ConstScope<'a> {
    schema: &'a Schema,
    depth: usize,
}

impl crate::expr::Scope for ConstScope<'_> {
    fn lookup(&self, name: &str) -> Option<i128> {
        self.schema.const_int_depth(name, self.depth).or_else(|| {
            let (type_name, member_name) = name.rsplit_once('.')?;
            let TypeDef::Enum(definition) = self.schema.resolve(type_name).ok()? else {
                return None;
            };
            definition
                .members
                .iter()
                .find(|(member, _)| member == member_name)
                .map(|(_, value)| *value)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_aliases_resolve_to_scalars() {
        let schema = Schema::with_builtins(Endian::Little);
        assert_eq!(
            schema.resolve("ULONG").unwrap(),
            &TypeDef::Scalar(Scalar::Uint(4))
        );
        assert_eq!(
            schema.resolve("USHORT").unwrap(),
            &TypeDef::Scalar(Scalar::Uint(2))
        );
        // CHAR must stay distinct from uint8: they differ in the Python type
        // they produce, which the compatibility contract cares about.
        assert_eq!(
            schema.resolve("CHAR").unwrap(),
            &TypeDef::Scalar(Scalar::Char)
        );
    }

    #[test]
    fn unknown_type_is_reported_by_name() {
        let schema = Schema::with_builtins(Endian::Little);
        assert_eq!(
            schema.resolve("NOPE"),
            Err(Error::UnknownType {
                name: "NOPE".to_owned()
            })
        );
    }

    /// A definition is input. Input that makes the resolver spin is a denial of
    /// service, so the depth bound is asserted rather than assumed.
    #[test]
    fn circular_alias_terminates() {
        let mut schema = Schema::with_builtins(Endian::Little);
        schema.insert(
            "A".to_owned(),
            TypeDef::Alias(TypeRef::Named("B".to_owned())),
        );
        schema.insert(
            "B".to_owned(),
            TypeDef::Alias(TypeRef::Named("A".to_owned())),
        );
        assert!(matches!(
            schema.resolve("A"),
            Err(Error::InvalidValue { .. })
        ));
    }

    #[test]
    fn scalar_sizes() {
        assert_eq!(Scalar::Uint(4).size(), 4);
        assert_eq!(Scalar::Wchar.size(), 2);
        assert_eq!(Scalar::Char.size(), 1);
    }

    #[test]
    fn cdef_uses_declaration_order_and_retained_expressions() {
        let mut schema = Schema::with_builtins(Endian::Little);
        schema.insert_constant("WIDTH".to_owned(), ConstValue::Int(4));
        schema.insert(
            "Item".to_owned(),
            TypeDef::Struct(StructDef {
                name: "Item".to_owned(),
                fields: vec![
                    FieldDef {
                        name: Some("fixed".to_owned()),
                        ty: TypeRef::Array(
                            Box::new(TypeRef::Named("char".to_owned())),
                            ArrayLen::Expr("WIDTH".to_owned()),
                        ),
                        bits: None,
                        points_to: None,
                        explicit_offset: None,
                    },
                    FieldDef {
                        name: Some("data".to_owned()),
                        ty: TypeRef::Array(
                            Box::new(TypeRef::Named("uint8".to_owned())),
                            ArrayLen::Expr("count*WIDTH".to_owned()),
                        ),
                        bits: None,
                        points_to: None,
                        explicit_offset: None,
                    },
                ],
                is_union: false,
                align: false,
            }),
        );

        assert_eq!(
            schema.cdef(),
            "#define WIDTH 4\n\nstruct Item {\n    char fixed[4];\n    uint8 data[count * WIDTH];\n};"
        );
    }

    #[test]
    fn duplicate_concrete_types_fail_but_identical_aliases_are_idempotent() {
        let mut schema = Schema::default();
        let alias = TypeDef::Alias(TypeRef::Named("uint32".to_owned()));
        schema
            .insert_declaration("DWORD".to_owned(), alias.clone())
            .unwrap();
        schema
            .insert_declaration("DWORD".to_owned(), alias)
            .unwrap();

        let error = schema
            .insert_declaration("DWORD".to_owned(), TypeDef::Scalar(Scalar::Uint(4)))
            .unwrap_err();
        assert_eq!(error.to_string(), "Duplicate type: DWORD");
    }

    #[test]
    fn duplicate_aliases_may_use_different_names_for_the_same_type() {
        let mut schema = Schema::with_builtins(Endian::Little);
        schema
            .insert_declaration(
                "FILETIME".to_owned(),
                TypeDef::Alias(TypeRef::Named("QWORD".to_owned())),
            )
            .unwrap();

        assert_eq!(schema.resolve("FILETIME"), schema.resolve("uint64"));
    }
}
