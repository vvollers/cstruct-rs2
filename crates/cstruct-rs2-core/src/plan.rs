//! The compiled plan: what the decoder actually walks.
//!
//! A [`Schema`] is shaped for clarity — named types, nested trees, resolved
//! lazily. A [`CompiledPlan`] is shaped for the cache. Everything that can be
//! known before any bytes arrive is resolved here, once per type per process,
//! so the decoder does no name lookups, no alias chasing, and no arithmetic it
//! could have avoided.
//!
//! # Why the fields are one packed array
//!
//! Decoding walks fields `0..n` in order and reads *every* attribute of each
//! one. That argues for keeping a field's attributes together: a packed array
//! of 20-byte [`FieldOp`] records puts an eight-field structure in 160
//! contiguous bytes, which the hardware prefetches in order.
//!
//! Splitting the attributes into parallel arrays — one for ops, one for sizes,
//! one for offsets — would be the better shape for a pass that reads *one*
//! attribute across many records. This pass is the opposite: all attributes,
//! few records.
//!
//! The width of [`FieldOp`] is therefore load-bearing, and anything rare enough
//! goes in a side table instead: [`CompiledPlan::bit_windows`],
//! [`CompiledPlan::array_dims`], [`CompiledPlan::lengths`]. Each is parallel to
//! `fields` and empty for an ordinary type.
//!
//! One consequence is worth stating explicitly, because it is easy to undo:
//! [`FieldOp::aux`] and [`FieldOp::count`] are separate members even though
//! only one is ever used at a time. Merging them saves four bytes and breaks
//! arrays of structures, which need the sub-plan index *and* the element count
//! at once.
//!
//! # Sub-plans are indices, not pointers
//!
//! A nested structure refers to its plan by index into
//! [`CompiledPlan::subplans`] rather than by `Arc`. That keeps pointers out of
//! the packed array, and the sub-plan is only reached when a nested field is
//! actually decoded.

use std::collections::HashMap;
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::expr::Expr;
use crate::schema::{ArrayLen, ConstValue, Endian, Scalar, Schema, TypeDef, TypeRef};
use crate::telemetry::ShapeSummary;

/// Marker for an offset that is not known until the record is decoded.
pub const DYNAMIC_OFFSET: u32 = u32::MAX;

/// Marker for an element count that comes from an expression.
pub const DYNAMIC_COUNT: u32 = u32::MAX;

/// Deepest structure nesting the compiler will accept.
///
/// A definition is input. Without a bound, a self-referential structure would
/// recurse until the stack ran out, which is a crash rather than an error.
pub const MAX_NESTING: u8 = 32;

/// What to do with one field's bytes.
///
/// A dense `u8` enum so the decoder's `match` becomes a jump table rather than
/// a chain of comparisons, and so a [`FieldOp`] stays small.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Op {
    /// Unsigned integer, width in [`FieldOp::size`].
    Uint,
    /// Signed integer, width in [`FieldOp::size`].
    Int,
    /// IEEE 754 float, width in [`FieldOp::size`].
    Float,
    /// A single byte, produced as Python `bytes` of length 1.
    Char,
    /// `size` bytes, produced as Python `bytes`.
    CharArray,
    /// One UTF-16 code unit, produced as Python `str`.
    Wchar,
    /// `size` bytes of UTF-16, produced as Python `str`.
    WcharArray,
    /// A zero-width placeholder value.
    Void,
    /// Any declared array of zero-width placeholders; its value is empty.
    VoidArray,
    /// An array of fixed-width scalars; element op in [`FieldOp::elem`].
    ScalarArray,
    /// A nested structure or union; sub-plan index in [`FieldOp::aux`].
    Nested,
    /// An array of nested structures.
    NestedArray,
    /// An integer that becomes an enum or flag member.
    ///
    /// A separate op rather than a flag on [`Op::Uint`], because building the
    /// member is a large enough share of the work to be worth specialising on
    /// its own.
    Enum,
    /// A LEB128 variable-length integer. Signedness is in [`FieldOp::elem`].
    Leb128,
    /// A zero-terminated run of LEB128 values.
    ///
    /// Each element has its own byte width, so treating this as a scalar array
    /// with a single stride cannot work. Layout and materialization walk it on
    /// the uncommon self-delimiting path.
    Leb128Array,
    /// A consumer-supplied type; index into [`CompiledPlan::custom_names`] in
    /// [`FieldOp::aux`]. Read by calling back into Python.
    Custom,
    /// A field whose shape could not be resolved while compiling.
    ///
    /// Compiling still succeeds, because upstream defers length evaluation to
    /// read time and a type that will not build is a module that will not
    /// import. Decoding one of these raises.
    Unsupported,
    /// A bitfield member: some bits of a shared storage unit.
    ///
    /// The underlying op is in [`FieldOp::elem`], the unit width in
    /// [`FieldOp::size`], and the shift and width in
    /// [`CompiledPlan::bit_windows`]. Consecutive bitfields of one unit all
    /// carry the *same* [`FieldOp::offset`] -- they are different windows onto
    /// the same bytes, not different bytes.
    Bits,
}

/// Where a bitfield sits inside its storage unit, resolved at compile time.
///
/// Upstream extracts bitfields with a running `BitBuffer` that shifts as it
/// goes. Doing the same at decode time would mean carrying mutable state
/// through the hot loop for a feature that is rare and never on the fast path.
/// Since the shift depends only on the declaration and the byte order -- both
/// known while compiling -- it is computed once here and the decoder becomes a
/// stateless shift-and-mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BitWindow {
    /// How far to shift the storage unit right before masking.
    pub shift: u8,
    /// How many bits the field occupies. Zero when the field is not a bitfield.
    pub width: u8,
    /// True when this field is the one that opens its storage unit.
    ///
    /// Only the opener consumes bytes. Without this the layout walk advances
    /// the cursor once per bitfield instead of once per unit, which puts every
    /// field after a bitfield run at the wrong offset — and a wrong offset is a
    /// silently wrong record, not a crash.
    pub opens_unit: bool,
}

/// One dimension of a multi-dimensional array that needs runtime layout.
///
/// Fixed matrices keep their compact `u32` dimensions in
/// [`CompiledPlan::array_dims`]. This richer form is only stored when at least
/// one dimension depends on record content or has a terminator, so ordinary
/// arrays keep the same plan and materialization path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArrayDimension {
    /// A definition-time element count.
    Fixed(u32),
    /// A count evaluated against fields already decoded in this record.
    Expr(Expr),
    /// The innermost row ends at an all-zero element.
    NullTerminated,
    /// The innermost row takes the remaining input.
    ToEnd,
}

impl BitWindow {
    /// True when this field is a bitfield at all.
    pub const fn is_bitfield(self) -> bool {
        self.width != 0
    }

    /// The mask to apply after shifting.
    pub const fn mask(self) -> u64 {
        if self.width >= 64 {
            u64::MAX
        } else {
            (1u64 << self.width) - 1
        }
    }
}

/// One field, packed to 20 bytes.
///
/// Size is load-bearing: see the module docs. Changing a member's width changes
/// how many cache lines an eight-field record occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct FieldOp {
    /// What to do with the bytes.
    pub op: Op,
    /// For an array, the element's op.
    pub elem: Op,
    /// Element width in bytes, for arrays. Zero when the element is a nested
    /// type, whose width lives on its sub-plan instead.
    pub elem_size: u8,
    /// True when this field's length comes from an expression.
    pub dynamic: bool,
    /// Byte offset from the start of the record, or [`DYNAMIC_OFFSET`].
    pub offset: u32,
    /// Total byte size, when known. Zero for a dynamic field.
    pub size: u32,
    /// Sub-plan index for a nested type, or enum index for [`Op::Enum`].
    ///
    /// This index has only one meaning. Array element counts live in [`Self::count`]
    /// because a nested array needs both its sub-plan index and its count.
    pub aux: u32,
    /// Element count for arrays, one for scalars, or [`DYNAMIC_COUNT`].
    pub count: u32,
}

impl FieldOp {
    /// Wrap a bare op as a compiled field with its alignment and dimensions.
    fn into_compiled(self, alignment: u32, dims: Box<[u32]>) -> CompiledField {
        CompiledField {
            op: self,
            length: None,
            to_end: false,
            alignment,
            dims,
            runtime_dims: Box::from([]),
            length_source: None,
        }
    }
}

/// A resolved enum or flag type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumPlan {
    /// The type's name, as consumers see it.
    pub name: Arc<str>,
    /// Width of the underlying integer, in bytes.
    pub size: u8,
    /// Whether the underlying integer is signed.
    pub signed: bool,
    /// Byte order of the underlying integer.
    ///
    /// Carried because an enum type is *callable on a stream* — upstream's
    /// `c_mlocate.DBE_TYPE(fh)` reads the integer and returns the member — so
    /// the type has to know how to read it, not merely how to name it.
    pub endian: Endian,
    /// True for `flag` (Python `IntFlag`), false for `enum` (`IntEnum`).
    pub is_flag: bool,
    /// Members in declaration order.
    pub members: Vec<(Arc<str>, i128)>,
}

/// Everything the decoder needs for one type.
#[derive(Debug, Clone)]
pub struct CompiledPlan {
    /// The type's name.
    pub name: Arc<str>,
    /// Fields in declaration order, packed and contiguous.
    pub fields: Vec<FieldOp>,
    /// Field names, parallel to [`Self::fields`].
    ///
    /// Kept out of the hot array on purpose: the decoder never needs a name,
    /// only attribute access does, and the names would otherwise evict the ops
    /// that decoding does need.
    pub names: Vec<Arc<str>>,
    /// Name to a path of field indices, built once so attribute access never
    /// scans and never resolves a name at read time.
    ///
    /// A *path* rather than a single index because anonymous members fold their
    /// fields into the parent's namespace. `_INDEX_ENTRY` declares an anonymous
    /// union whose first member is `FileReference`; a consumer writes
    /// `entry.FileReference`, and reaching it means stepping into the union
    /// first. One index would return the union itself, which is the wrong
    /// object -- caught by decoding a real `_INDEX_ENTRY` rather than a
    /// synthetic one.
    ///
    /// Paths are one element long for every directly declared field, which is
    /// the overwhelming majority.
    pub index_of: HashMap<Arc<str>, Box<[u32]>>,
    /// Length expressions for dynamic fields, parallel to [`Self::fields`].
    ///
    /// `None` for every fixed field, which is nearly all of them.
    pub lengths: Vec<Option<Expr>>,
    /// Array-length spelling used by cold type metadata.
    ///
    /// The parsed expression in [`Self::lengths`] is optimized for evaluation
    /// and deliberately loses whitespace. Upstream exposes a readable form in
    /// an array type's `__name__`, so the canonical source is retained once
    /// here rather than reconstructed during introspection.
    pub array_length_sources: Vec<Option<Arc<str>>>,
    /// Bit windows, parallel to [`Self::fields`], all-zero for the usual type.
    ///
    /// A side table rather than two more bytes on [`FieldOp`], because
    /// bitfields are rare and that struct's width is load-bearing — see the
    /// module docs. Parallel rather than sparse: indexing a vector of zeroes
    /// costs less than probing a map.
    pub bit_windows: Vec<BitWindow>,
    /// Natural byte alignment of each field, parallel to [`Self::fields`].
    ///
    /// Fixed layouts bake this into [`FieldOp::offset`]. Dynamic aligned
    /// layouts cannot: every field after a content-sized value needs to round
    /// the runtime cursor before it is read. Keeping the boundary beside the
    /// plan avoids resolving a Python type or revisiting the schema per record.
    pub field_alignments: Vec<u32>,
    /// Plans for nested types, referenced by [`FieldOp::aux`].
    pub subplans: Vec<Arc<CompiledPlan>>,
    /// Enum types, referenced by [`FieldOp::aux`] for [`Op::Enum`].
    pub enums: Vec<Arc<EnumPlan>>,
    /// Total size when fixed; `None` when the record's size depends on content.
    pub total_size: Option<u32>,
    /// Byte order.
    pub endian: Endian,
    /// True for a union: fields overlap at offset zero.
    pub is_union: bool,
    /// True for each `[EOF]` field: read whatever remains rather than scanning
    /// for a terminator. Parallel to [`Self::fields`].
    pub to_end: Vec<bool>,
    /// For each field declared with a `*`, the name of the type it addresses.
    ///
    /// Parallel to [`Self::fields`] and `None` for almost every field, so a
    /// side table for the same reason as the others: `FieldOp` is 20 bytes and
    /// its size is load-bearing. Layout only needs the pointer-sized integer;
    /// materialisation needs the separate target name to construct a value that
    /// can dereference itself like upstream's `Pointer`.
    pub pointer_targets: Vec<Option<Arc<str>>>,
    /// Dimensions of multi-dimensional array fields, outermost first.
    ///
    /// Parallel to [`Self::fields`] and empty for every one-dimensional field,
    /// which is almost all of them. A side table for the same reason as
    /// [`Self::bit_windows`].
    pub array_dims: Vec<Box<[u32]>>,
    /// Runtime dimensions for uncommon content-sized matrices.
    ///
    /// Parallel to [`Self::fields`] and empty for every scalar, one-dimensional
    /// array, and fully fixed matrix. Layout evaluates these expressions in
    /// Rust and reports the resolved shape to the materializer.
    pub runtime_array_dims: Vec<Box<[ArrayDimension]>>,
    /// Names of consumer-supplied types, referenced by [`FieldOp::aux`].
    pub custom_names: Vec<Arc<str>>,
    /// Natural alignment of this type, in bytes: the largest member's.
    ///
    /// Meaningful only when [`Self::aligned`] is set, but computed either way
    /// so that a nested type can report it to an aligned parent.
    pub alignment: u32,
    /// True when this type was declared under `load(..., align=True)`.
    pub aligned: bool,
    /// True when layout must resolve a content-sized matrix.
    ///
    /// This plan-level discriminator keeps matrix side-table lookups out of
    /// the ordinary dynamic-record loop. It is computed once with the plan;
    /// callers never scan the parallel table per record.
    pub has_runtime_matrices: bool,
    /// Summary used by the fast-path predicate and the fallback counter.
    pub shape: ShapeSummary,
}

impl CompiledPlan {
    /// Clone this plan tree for a one-call byte-order override.
    ///
    /// Offsets and sizes do not depend on byte order, but nested plans, enum
    /// readers, and precomputed bitfield shifts do. The Python binding caches
    /// the returned immutable tree only when an opposite-endian call is made;
    /// ordinary types therefore keep one plan exactly as before.
    pub fn with_endian(&self, endian: Endian) -> Arc<Self> {
        if self.endian == endian {
            return Arc::new(self.clone());
        }

        let mut alternate = self.clone();
        alternate.endian = endian;
        alternate.subplans = self
            .subplans
            .iter()
            .map(|plan| plan.with_endian(endian))
            .collect();
        alternate.enums = self
            .enums
            .iter()
            .map(|plan| {
                let mut plan = plan.as_ref().clone();
                plan.endian = endian;
                Arc::new(plan)
            })
            .collect();

        let mut consumed = 0u8;
        for (field, window) in alternate.fields.iter().zip(&mut alternate.bit_windows) {
            if !window.is_bitfield() {
                consumed = 0;
                continue;
            }
            if window.opens_unit {
                consumed = 0;
            }
            let unit_bits = (field.size as u8).saturating_mul(8);
            window.shift = match endian {
                Endian::Little => consumed,
                Endian::Big => unit_bits - consumed - window.width,
            };
            consumed += window.width;
        }

        Arc::new(alternate)
    }

    /// True when a dynamic array is delimited by a terminator rather than by a
    /// length expression -- `char d[]`, not `char d[n]`.
    ///
    /// Decoder and encoder share this predicate because they must agree on
    /// whether the terminator occupies bytes. `FieldOp::dynamic` alone cannot
    /// distinguish the two forms: a count expression also makes `char d[n]`
    /// dynamic, but current upstream does not append a terminator to it.
    ///
    /// The `dynamic` check also excludes a scalar `char c;`. Like a terminated
    /// array, that field has no length expression and is not marked `[EOF]`, but
    /// it represents exactly one byte.
    pub fn is_null_terminated(&self, index: usize) -> bool {
        self.fields.get(index).is_some_and(|field| {
            field.dynamic
                && field.count == DYNAMIC_COUNT
                && matches!(
                    field.op,
                    Op::CharArray | Op::WcharArray | Op::ScalarArray | Op::NestedArray
                )
        }) && self.lengths.get(index).and_then(Option::as_ref).is_none()
            && self
                .runtime_array_dims
                .get(index)
                .is_none_or(|dimensions| dimensions.is_empty())
            && !self.to_end.get(index).copied().unwrap_or(false)
    }

    /// Whether this type can take a fast path, and why not if it cannot.
    ///
    /// Delegates to the single predicate in [`crate::telemetry`] so that
    /// "decided it was ineligible" and "counted it as ineligible" can never
    /// become two different judgements.
    pub fn fast_path_eligible(&self) -> bool {
        crate::telemetry::fast_path_eligible(&self.shape).is_ok()
    }

    /// Look up a field's access path by name.
    ///
    /// Returns a slice of indices to follow from this record inward. Empty is
    /// never returned: a name either resolves to at least one step or not at
    /// all.
    pub fn field_path(&self, name: &str) -> Option<&[u32]> {
        self.index_of.get(name).map(|path| path.as_ref())
    }

    /// Look up a directly declared field's index by name.
    ///
    /// Convenience for the common single-step case; returns `None` for a name
    /// that only resolves through an anonymous member.
    pub fn field_index(&self, name: &str) -> Option<usize> {
        match self.index_of.get(name)?.as_ref() {
            [only] => Some(*only as usize),
            _ => None,
        }
    }
}

/// Compiles schema types into plans, memoising as it goes.
pub struct Compiler<'a> {
    schema: &'a Schema,
    cache: HashMap<String, Arc<CompiledPlan>>,
}

impl<'a> Compiler<'a> {
    /// Start a compiler over a schema.
    pub fn new(schema: &'a Schema) -> Self {
        Self {
            schema,
            cache: HashMap::new(),
        }
    }

    /// Start a compiler that reuses plans compiled earlier.
    ///
    /// The cache has to outlive a single `compile` call because **plan identity
    /// is type identity**: `isinstance` compares the address of the compiled
    /// plan, not the type's name.
    ///
    /// Without a shared cache, compiling a type on its own and then again as a
    /// nested field produces two plans for one type. A record decoded through
    /// the nested route then fails `isinstance` against the very class that
    /// produced it — with no error, just a check that quietly answers `False`.
    pub fn with_cache(schema: &'a Schema, cache: HashMap<String, Arc<CompiledPlan>>) -> Self {
        Self { schema, cache }
    }

    /// Take back the compiled plans, to be handed to the next compiler.
    pub fn into_cache(self) -> HashMap<String, Arc<CompiledPlan>> {
        self.cache
    }

    /// Compile a named type, reusing an earlier result if there is one.
    pub fn compile(&mut self, name: &str) -> Result<Arc<CompiledPlan>> {
        self.compile_at_depth(name, 0)
    }

    fn compile_at_depth(&mut self, name: &str, depth: u8) -> Result<Arc<CompiledPlan>> {
        if depth > MAX_NESTING {
            return Err(Error::LimitExceeded {
                limit: "structure nesting",
                value: depth as u64,
            });
        }
        if let Some(plan) = self.cache.get(name) {
            return Ok(Arc::clone(plan));
        }

        let def = self.schema.resolve(name)?.clone();
        let TypeDef::Struct(structure) = def else {
            return Err(Error::InvalidValue {
                message: format!("{name} is not a structure; only structures have plans"),
            });
        };

        // An alias is a second *spelling*, not a second type, so it has to
        // reach the same plan — see `with_cache` on why identity matters.
        //
        // `typedef struct _REF { ... } FILE_REFERENCE;` registers both names.
        // Compiling through the alias must not build a fresh plan, because a
        // nested member of the same type always compiles through the tag (see
        // `compile_named`). Two plans would mean
        // `isinstance(record.r, cs.FILE_REFERENCE)` is false for a value that
        // type just produced.
        //
        // The recursion terminates: the canonical entry is registered under its
        // own `structure.name`, so the second call takes the equal-name path.
        if structure.name != name {
            let plan = self.compile_at_depth(&structure.name, depth)?;
            self.cache.insert(name.to_owned(), Arc::clone(&plan));
            return Ok(plan);
        }

        let mut fields = Vec::with_capacity(structure.fields.len());
        let mut names = Vec::with_capacity(structure.fields.len());
        let mut lengths = Vec::with_capacity(structure.fields.len());
        let mut array_length_sources = Vec::with_capacity(structure.fields.len());
        let mut bit_windows = Vec::with_capacity(structure.fields.len());
        let mut field_alignments = Vec::with_capacity(structure.fields.len());
        let mut array_dims: Vec<Box<[u32]>> = Vec::with_capacity(structure.fields.len());
        let mut runtime_array_dims: Vec<Box<[ArrayDimension]>> =
            Vec::with_capacity(structure.fields.len());
        let mut to_end: Vec<bool> = Vec::with_capacity(structure.fields.len());
        let mut pointer_targets: Vec<Option<Arc<str>>> = Vec::with_capacity(structure.fields.len());
        let mut subplans = Vec::new();
        let mut enums = Vec::new();
        let mut custom_names: Vec<Arc<str>> = Vec::new();
        let mut index_of = HashMap::new();

        let mut offset = 0u32;
        let mut fixed_so_far = true;
        let mut max_member_size = 0u32;
        // A structure's alignment is its largest member's, and it applies even
        // when this type is not itself aligned -- an aligned parent needs it.
        let mut alignment = 1u32;
        let mut nesting_depth = 0u8;
        let mut dependent_lengths = false;

        // Bitfield packing state, mirroring upstream `structure.py`'s `commit`.
        // `unit` identifies the current storage unit by the *underlying
        // integer*'s (signedness, width); a change of either starts a new one,
        // as does exhausting the current.
        //
        // Signedness and width, and nothing else. Current upstream's structure
        // compiler follows this rule: `uint8 a : 4; uint8 b : 4;` is one byte,
        // `int8 a : 4; uint8 b : 4;` is two, and `enum K : uint8` shares a byte
        // with a `uint8` bitfield because an enum's underlying integer *is* its
        // base type.
        let mut bit_unit: Option<(bool, u32)> = None;
        let mut bits_remaining: i32 = 0;
        let mut bit_unit_offset = 0u32;

        for field in &structure.fields {
            let compiled = self.compile_field(
                &field.ty,
                depth,
                &mut subplans,
                &mut enums,
                &mut custom_names,
                &mut nesting_depth,
            )?;

            alignment = alignment.max(compiled.alignment);

            // ``add_field(..., offset=N)`` pins the layout cursor before the
            // normal alignment rule is applied. Parsed declarations never set
            // this value, so ordinary compilation remains unchanged.
            if let Some(explicit) = field.explicit_offset {
                offset = explicit;
                fixed_so_far = true;
            }

            // Natural alignment: pad up to the next multiple of this field's
            // alignment before placing it. `-offset & (a - 1)` is the padding,
            // which is upstream's own expression and only correct for a
            // power-of-two alignment -- which is why `int24` aligns to 4.
            if structure.align && fixed_so_far && !structure.is_union {
                let step = compiled.alignment.max(1);
                debug_assert!(step.is_power_of_two());
                offset = offset
                    .checked_add(offset.wrapping_neg() & (step - 1))
                    .ok_or(Error::LimitExceeded {
                        limit: "structure size",
                        value: u64::from(offset),
                    })?;
            }

            let mut field_offset = if fixed_so_far {
                if structure.is_union && field.explicit_offset.is_none() {
                    0
                } else {
                    offset
                }
            } else {
                DYNAMIC_OFFSET
            };

            let mut op = compiled.op;
            let mut window = BitWindow::default();

            if let Some(width) = field.bits {
                // A bitfield's storage unit must be a fixed-width integer;
                // anything else has no bit count to divide up.
                if !matches!(op.op, Op::Uint | Op::Int | Op::Char | Op::Enum) || op.dynamic {
                    return Err(Error::InvalidValue {
                        message: format!(
                            "bitfield {} must have an integer type",
                            field.name.as_deref().unwrap_or("<anonymous>")
                        ),
                    });
                }
                let unit_bits = i32::from(op.size as u8) * 8;
                // An enum's signedness lives on its plan, not on its op: the op
                // is `Op::Enum` for a signed and an unsigned base alike. Keying
                // the unit on `op.op` instead made `Op::Enum` differ from
                // `Op::Uint`, so `enum K : uint8 k : 4; uint8 rest : 4;` opened
                // *two* bytes where upstream opens one -- the structure was a
                // byte larger and every field after it sat at the wrong offset,
                // with no error to notice it by.
                let unit_signed = match op.op {
                    Op::Int => true,
                    Op::Enum => enums.get(op.aux as usize).is_some_and(|plan| plan.signed),
                    _ => false,
                };
                let unit = (unit_signed, op.size);
                let opens_unit = bits_remaining == 0 || bit_unit != Some(unit);

                if opens_unit {
                    // Start a new storage unit. This is the only point at which
                    // a run of bitfields consumes any bytes.
                    bit_unit = Some(unit);
                    bits_remaining = unit_bits;
                    bit_unit_offset = field_offset;
                    if fixed_so_far {
                        if structure.is_union {
                            max_member_size =
                                max_member_size.max(field_offset.saturating_add(op.size));
                        } else {
                            offset = offset.checked_add(op.size).ok_or(Error::LimitExceeded {
                                limit: "structure size",
                                value: u64::from(offset) + u64::from(op.size),
                            })?;
                        }
                    }
                }

                let consumed = unit_bits - bits_remaining;
                bits_remaining -= i32::from(width);
                if bits_remaining < 0 {
                    // Upstream refuses rather than splitting across units, and a
                    // silently different split would be a wrong record.
                    return Err(Error::InvalidValue {
                        message: "Straddled bit fields are unsupported".to_owned(),
                    });
                }

                window = BitWindow {
                    // Little-endian fills from the low bits up; big-endian from
                    // the high bits down. Both are upstream's `BitBuffer`.
                    shift: match self.schema.endian {
                        Endian::Little => consumed as u8,
                        Endian::Big => (unit_bits - consumed - i32::from(width)) as u8,
                    },
                    width,
                    opens_unit,
                };

                // Every field of a unit reads the same bytes.
                field_offset = bit_unit_offset;
                op.elem = op.op;
                op.op = Op::Bits;
                op.count = 1;
            } else {
                bit_unit = None;
                bits_remaining = 0;
                bit_unit_offset = 0;
            }

            op.offset = field_offset;

            if window.is_bitfield() {
                // Bytes were accounted for when the unit opened; fall through to
                // recording the field without advancing the offset again.
                bit_windows.push(window);
                field_alignments.push(compiled.alignment.max(1));
                array_dims.push(compiled.dims.clone());
                runtime_array_dims.push(compiled.runtime_dims.clone());
                to_end.push(compiled.to_end);
                // Exactly one push per field, like every other side table here.
                // A stray second one made `pointer_targets` longer than
                // `fields` for any type containing a bitfield, so every entry
                // after it named the wrong field -- and a side table that is
                // silently off by one is read without complaint.
                pointer_targets.push(field.points_to.as_deref().map(Arc::from));
                let field_name: Arc<str> = match &field.name {
                    Some(name) => Arc::from(name.as_str()),
                    None => Arc::from(""),
                };
                if !field_name.is_empty() {
                    index_of.insert(
                        Arc::clone(&field_name),
                        vec![fields.len() as u32].into_boxed_slice(),
                    );
                }
                fields.push(op);
                names.push(field_name);
                lengths.push(compiled.length);
                array_length_sources.push(compiled.length_source.clone());
                continue;
            }
            bit_windows.push(window);
            field_alignments.push(compiled.alignment.max(1));
            array_dims.push(compiled.dims.clone());
            runtime_array_dims.push(compiled.runtime_dims.clone());
            to_end.push(compiled.to_end);
            pointer_targets.push(field.points_to.as_deref().map(Arc::from));

            if op.dynamic {
                dependent_lengths = true;
                // Everything after a dynamic field sits at an offset that
                // depends on that field's length, so the rest of the record is
                // dynamic too. Marking only the dynamic field itself would let
                // the trailing fields keep stale compile-time offsets, and the
                // record's total size would then omit them.
                fixed_so_far = false;
            } else if fixed_so_far {
                if structure.is_union {
                    max_member_size = max_member_size.max(field_offset.saturating_add(op.size));
                } else {
                    offset = offset.checked_add(op.size).ok_or(Error::LimitExceeded {
                        limit: "structure size",
                        value: u64::from(offset) + u64::from(op.size),
                    })?;
                }
            }

            let field_name: Arc<str> = match &field.name {
                Some(name) => Arc::from(name.as_str()),
                // An anonymous member has no name of its own. Upstream folds
                // its fields into the parent's namespace; the same is done
                // below, after the member itself is recorded.
                None => Arc::from(""),
            };

            if !field_name.is_empty() {
                index_of.insert(
                    Arc::clone(&field_name),
                    vec![fields.len() as u32].into_boxed_slice(),
                );
            }

            fields.push(op);
            names.push(field_name);
            lengths.push(compiled.length);
            array_length_sources.push(compiled.length_source.clone());
        }

        // Fold anonymous members' field names into this type's namespace, so
        // `entry.DataOffset` reaches through the anonymous union that declares
        // it. Upstream does the same, and definitions rely on it — an anonymous
        // member exists precisely so its fields read as the parent's.
        for (position, field_op) in fields.iter().enumerate() {
            if !names[position].is_empty() {
                continue;
            }
            if matches!(field_op.op, Op::Nested) {
                let sub = &subplans[field_op.aux as usize];
                for (sub_name, sub_path) in sub.index_of.iter() {
                    // Prepend this anonymous member's index so the path reaches
                    // *through* it to the field the consumer named.
                    let mut path = Vec::with_capacity(sub_path.len() + 1);
                    path.push(position as u32);
                    path.extend_from_slice(sub_path);
                    index_of
                        .entry(Arc::clone(sub_name))
                        .or_insert_with(|| path.into_boxed_slice());
                }
            }
        }

        // An aligned structure's size is rounded up to its own alignment, so
        // that an array of them puts every element on a boundary.
        if structure.align && fixed_so_far && !structure.is_union {
            let step = alignment.max(1);
            offset = offset
                .checked_add(offset.wrapping_neg() & (step - 1))
                .ok_or(Error::LimitExceeded {
                    limit: "structure size",
                    value: u64::from(offset),
                })?;
        }

        // A union's members all begin at zero, but an aligned union still has
        // tail padding so an array of unions places every element on the
        // union's natural boundary. Upstream rounds the widest member just as
        // it rounds a structure's running offset.
        if structure.align && fixed_so_far && structure.is_union {
            let step = alignment.max(1);
            max_member_size = max_member_size
                .checked_add(max_member_size.wrapping_neg() & (step - 1))
                .ok_or(Error::LimitExceeded {
                    limit: "structure size",
                    value: u64::from(max_member_size),
                })?;
        }

        let total_size = if !fixed_so_far {
            None
        } else if structure.is_union {
            Some(max_member_size)
        } else {
            Some(offset)
        };

        let has_runtime_matrices = runtime_array_dims
            .iter()
            .any(|dimensions| !dimensions.is_empty());
        let shape = ShapeSummary {
            fixed_size: total_size.is_some(),
            dependent_lengths,
            contains_union: structure.is_union,
            nesting_depth,
        };

        let plan = Arc::new(CompiledPlan {
            name: Arc::from(structure.name.as_str()),
            fields,
            names,
            index_of,
            lengths,
            array_length_sources,
            bit_windows,
            field_alignments,
            subplans,
            enums,
            total_size,
            endian: self.schema.endian,
            custom_names,
            array_dims,
            runtime_array_dims,
            to_end,
            pointer_targets,
            is_union: structure.is_union,
            alignment: alignment.max(1),
            aligned: structure.align,
            has_runtime_matrices,
            shape,
        });

        self.cache.insert(name.to_owned(), Arc::clone(&plan));
        Ok(plan)
    }

    fn compile_field(
        &mut self,
        ty: &TypeRef,
        depth: u8,
        subplans: &mut Vec<Arc<CompiledPlan>>,
        enums: &mut Vec<Arc<EnumPlan>>,
        custom_names: &mut Vec<Arc<str>>,
        nesting_depth: &mut u8,
    ) -> Result<CompiledField> {
        match ty {
            TypeRef::Named(name) => {
                self.compile_named(name, depth, subplans, enums, custom_names, nesting_depth)
            }
            TypeRef::Array(element, len) => {
                // `CHAR Hash[29][16]` nests arrays. Collect every dimension
                // outermost-first and compile the base type once, rather than
                // building an array of arrays that a flat `FieldOp` cannot
                // describe.
                let mut dims = vec![len];
                let mut base = element.as_ref();
                while let TypeRef::Array(inner, inner_len) = base {
                    dims.push(inner_len);
                    base = inner.as_ref();
                }

                let inner = self.compile_named_ref(
                    base,
                    depth,
                    subplans,
                    enums,
                    custom_names,
                    nesting_depth,
                )?;

                if dims.len() == 1 {
                    if inner.op.op == Op::Custom {
                        // A custom type may replace its array class and attach
                        // meaning to a symbolic selector such as `[lower]`.
                        // Preserve that selector in the plan's cold custom-name
                        // table and let the binding invoke the array reader
                        // once for the whole field. Built-in arrays never take
                        // this branch.
                        let index = inner.op.aux as usize;
                        let base = custom_names.get(index).ok_or_else(|| Error::InvalidValue {
                            message: "custom array refers to a missing type name".to_owned(),
                        })?;
                        let selector = match len {
                            ArrayLen::Fixed(count) => count.to_string(),
                            ArrayLen::Expr(source) => source.clone(),
                            ArrayLen::NullTerminated => String::new(),
                            ArrayLen::ToEnd => "EOF".to_owned(),
                        };
                        custom_names[index] = Arc::from(format!("{base}[{selector}]"));
                        return Ok(inner);
                    }
                    return self.compile_array(inner, len);
                }
                self.compile_matrix(inner, &dims)
            }
        }
    }

    fn compile_named_ref(
        &mut self,
        ty: &TypeRef,
        depth: u8,
        subplans: &mut Vec<Arc<CompiledPlan>>,
        enums: &mut Vec<Arc<EnumPlan>>,
        custom_names: &mut Vec<Arc<str>>,
        nesting_depth: &mut u8,
    ) -> Result<CompiledField> {
        match ty {
            TypeRef::Named(name) => {
                self.compile_named(name, depth, subplans, enums, custom_names, nesting_depth)
            }
            // Reached only for a base type that is itself an array, which
            // `compile_field` has already flattened.
            TypeRef::Array(..) => Err(Error::InvalidValue {
                message: "nested array reached the element compiler".to_owned(),
            }),
        }
    }

    fn compile_named(
        &mut self,
        name: &str,
        depth: u8,
        subplans: &mut Vec<Arc<CompiledPlan>>,
        enums: &mut Vec<Arc<EnumPlan>>,
        custom_names: &mut Vec<Arc<str>>,
        nesting_depth: &mut u8,
    ) -> Result<CompiledField> {
        match self.schema.resolve(name)? {
            TypeDef::Scalar(scalar) => Ok(CompiledField {
                op: scalar_op(*scalar),
                length: None,
                to_end: false,
                alignment: scalar.alignment(),
                dims: Box::from([]),
                runtime_dims: Box::from([]),
                length_source: None,
            }),
            TypeDef::Enum(def) => {
                let base = match self.schema.resolve(&def.base)? {
                    TypeDef::Scalar(scalar) => *scalar,
                    _ => {
                        return Err(Error::InvalidValue {
                            message: format!("enum {} has a non-scalar base type", def.name),
                        })
                    }
                };
                let (size, signed) = match base {
                    Scalar::Uint(n) => (n, false),
                    Scalar::Int(n) => (n, true),
                    other => {
                        return Err(Error::InvalidValue {
                            message: format!(
                                "enum {} has base {other:?}, which is not an integer",
                                def.name
                            ),
                        })
                    }
                };
                let index = enums.len() as u32;
                enums.push(Arc::new(EnumPlan {
                    name: Arc::from(def.name.as_str()),
                    size,
                    signed,
                    endian: self.schema.endian,
                    is_flag: def.is_flag,
                    members: def
                        .members
                        .iter()
                        .map(|(member, value)| (Arc::from(member.as_str()), *value))
                        .collect(),
                }));
                Ok(CompiledField {
                    op: FieldOp {
                        op: Op::Enum,
                        elem: Op::Uint,
                        elem_size: size,
                        dynamic: false,
                        offset: 0,
                        size: u32::from(size),
                        aux: index,
                        count: 1,
                    },
                    length: None,
                    to_end: false,
                    // An enum aligns like the integer underneath it.
                    alignment: u32::from(size),
                    dims: Box::from([]),
                    runtime_dims: Box::from([]),
                    length_source: None,
                })
            }
            TypeDef::Struct(structure) => {
                let sub_name = structure.name.clone();
                let sub = self.compile_at_depth(&sub_name, depth + 1)?;
                *nesting_depth = (*nesting_depth).max(sub.shape.nesting_depth + 1);
                let index = subplans.len() as u32;
                let dynamic = sub.total_size.is_none();
                let size = sub.total_size.unwrap_or(0);
                let alignment = sub.alignment;
                subplans.push(sub);
                Ok(CompiledField {
                    op: FieldOp {
                        op: Op::Nested,
                        elem: Op::Uint,
                        elem_size: 0,
                        dynamic,
                        offset: 0,
                        size,
                        aux: index,
                        count: 1,
                    },
                    length: None,
                    to_end: false,
                    alignment,
                    dims: Box::from([]),
                    runtime_dims: Box::from([]),
                    length_source: None,
                })
            }
            TypeDef::Custom => {
                let index = custom_names.len() as u32;
                custom_names.push(Arc::from(name));
                Ok(CompiledField {
                    op: FieldOp {
                        op: Op::Custom,
                        elem: Op::Uint,
                        elem_size: 0,
                        // Length is whatever the consumer's reader consumes.
                        dynamic: true,
                        offset: 0,
                        size: 0,
                        aux: index,
                        count: 1,
                    },
                    length: None,
                    to_end: false,
                    alignment: 1,
                    dims: Box::from([]),
                    runtime_dims: Box::from([]),
                    length_source: None,
                })
            }
            // An alias for an *array*: `typedef char uuid_t[16];`.
            //
            // `Schema::resolve` follows a chain of plain name aliases and stops
            // here, because an array is not a name and there is nothing further
            // to look up. So this is where the array gets compiled, and it
            // compiles exactly as the same shape written out longhand would --
            // `uuid_t id;` and `char id[16];` produce the same `FieldOp`.
            //
            // That is also what makes `uuid_t many[2];` work: the outer
            // subscript is compiled by `compile_field`, which asks for the
            // element type and gets a 16-byte char array back from here, so the
            // result is an array of arrays rather than 32 loose bytes.
            TypeDef::Alias(TypeRef::Array(element, len)) => {
                let (element, len) = (element.as_ref().clone(), len.clone());
                self.compile_field(
                    &TypeRef::Array(Box::new(element), len),
                    depth,
                    subplans,
                    enums,
                    custom_names,
                    nesting_depth,
                )
            }
            // A chain of plain name aliases is followed by `Schema::resolve`, so
            // by the time a definition reaches here it has already been reduced
            // to whatever it ultimately names.
            TypeDef::Alias(TypeRef::Named(target)) => Err(Error::InvalidValue {
                message: format!("alias chain for {name} did not resolve past {target}"),
            }),
        }
    }

    /// Compile `T[a][b]...` -- an array whose elements are themselves arrays.
    ///
    /// Every dimension must be a literal. A content-dependent length in a
    /// multi-dimensional array has no upstream example, and guessing at which
    /// dimension a length field sizes would be a silently wrong stride.
    fn compile_matrix(
        &mut self,
        inner: CompiledField,
        dims: &[&ArrayLen],
    ) -> Result<CompiledField> {
        // A dimension may be a constant rather than a literal --
        // `zones[SD_COUNT][SM_DATAZONE_ALLOCZONE_COUNT]` in `dissect.apfs` --
        // so each one is folded against the schema's constants exactly as a
        // one-dimensional array's length is.
        let mut counts = Vec::with_capacity(dims.len());
        let mut runtime_dims = Vec::with_capacity(dims.len());
        for len in dims {
            let (count, runtime) = match len {
                ArrayLen::Fixed(count) => (Some(*count), ArrayDimension::Fixed(*count)),
                ArrayLen::Expr(text) => {
                    let expr = Expr::parse(text)?;
                    let count = expr
                        .eval(&SchemaConstScope {
                            schema: self.schema,
                        })
                        .ok()
                        .and_then(|value| u32::try_from(value).ok());
                    let runtime =
                        count.map_or_else(|| ArrayDimension::Expr(expr), ArrayDimension::Fixed);
                    (count, runtime)
                }
                ArrayLen::NullTerminated => (None, ArrayDimension::NullTerminated),
                ArrayLen::ToEnd => (None, ArrayDimension::ToEnd),
            };
            runtime_dims.push(runtime);
            if let Some(count) = count {
                counts.push(count);
            }
        }

        let element = inner.op;
        if element.dynamic || element.size == 0 {
            return Err(Error::Unsupported {
                construct: "multi-dimensional array of a variable-width type".to_owned(),
            });
        }

        let all_fixed = counts.len() == dims.len();
        if !all_fixed {
            let terminal_is_unbounded = matches!(
                runtime_dims.last(),
                Some(ArrayDimension::NullTerminated | ArrayDimension::ToEnd)
            );
            if runtime_dims[..runtime_dims.len().saturating_sub(1)]
                .iter()
                .any(|dimension| {
                    matches!(
                        dimension,
                        ArrayDimension::NullTerminated | ArrayDimension::ToEnd
                    )
                })
            {
                // The source parser accepts `uint8 rows[][4]`; there is no
                // safe stride to infer for the outer terminator, so retain a
                // dynamic placeholder and fail only if a caller decodes it.
                // This preserves importability without inventing a layout.
                return Ok(FieldOp {
                    op: Op::Unsupported,
                    elem: Op::Uint,
                    elem_size: 0,
                    dynamic: true,
                    offset: 0,
                    size: 0,
                    aux: 0,
                    count: DYNAMIC_COUNT,
                }
                .into_compiled(1, Box::from([])));
            }
            if terminal_is_unbounded && !matches!(element.op, Op::Char | Op::Wchar) {
                // Other scalar terminators are representable, but no current
                // consumer or latest-head test uses the matrix form. Refuse it
                // rather than inventing nested zero-value semantics.
                return Err(Error::Unsupported {
                    construct: "a terminated matrix of non-character values".to_owned(),
                });
            }
            return Ok(CompiledField {
                op: FieldOp {
                    op: Op::ScalarArray,
                    elem: element.op,
                    elem_size: u8::try_from(element.size).unwrap_or(0),
                    dynamic: true,
                    offset: 0,
                    size: 0,
                    aux: element.aux,
                    count: DYNAMIC_COUNT,
                },
                length: None,
                to_end: false,
                alignment: inner.alignment,
                dims: Box::from([]),
                runtime_dims: runtime_dims.into_boxed_slice(),
                length_source: None,
            });
        }

        let total: u64 = counts.iter().map(|count| u64::from(*count)).product();
        let bytes = total * u64::from(element.size);
        if total > u64::from(crate::decode::MAX_ARRAY_ELEMENTS) || bytes > u64::from(u32::MAX) {
            return Err(Error::LimitExceeded {
                limit: "array byte size",
                value: bytes,
            });
        }

        // An array of structures stays an array of structures; only the
        // element *count* changes. Calling it a scalar array would read the
        // sub-records as integers.
        let op = if matches!(element.op, Op::Nested | Op::NestedArray) {
            Op::NestedArray
        } else {
            Op::ScalarArray
        };

        Ok(FieldOp {
            op,
            elem: element.op,
            elem_size: u8::try_from(element.size).unwrap_or(0),
            dynamic: false,
            offset: 0,
            size: bytes as u32,
            aux: element.aux,
            count: total as u32,
        }
        .into_compiled(inner.alignment, counts.into_boxed_slice()))
    }

    fn compile_array(&mut self, inner: CompiledField, len: &ArrayLen) -> Result<CompiledField> {
        let element = inner.op;
        let element_alignment = inner.alignment;
        if element.op == Op::Void {
            return Ok(CompiledField {
                op: FieldOp {
                    op: Op::VoidArray,
                    elem: Op::Void,
                    elem_size: 0,
                    dynamic: false,
                    offset: 0,
                    size: 0,
                    aux: 0,
                    count: 0,
                },
                length: None,
                to_end: false,
                alignment: 1,
                dims: Box::from([]),
                runtime_dims: Box::from([]),
                length_source: None,
            });
        }
        if element.op == Op::Leb128 && matches!(len, ArrayLen::NullTerminated) {
            return Ok(CompiledField {
                op: FieldOp {
                    op: Op::Leb128Array,
                    elem: element.elem,
                    elem_size: 0,
                    dynamic: true,
                    offset: 0,
                    size: 0,
                    aux: 0,
                    count: DYNAMIC_COUNT,
                },
                length: None,
                to_end: false,
                alignment: element_alignment,
                dims: Box::from([]),
                runtime_dims: Box::from([]),
                length_source: None,
            });
        }
        // Resolve the count now if it can be: a fixed literal, or an expression
        // over constants alone. Folding here means the decoder never evaluates
        // an expression for the overwhelming majority of arrays.
        // `[EOF]` has no count until the input is seen.
        if matches!(len, ArrayLen::ToEnd) {
            let element = inner.op;
            return Ok(FieldOp {
                // Preserve the element category just as `compile_matrix` does.
                // In particular, `E items[EOF]` must materialise `E` records;
                // treating it as a scalar array would return integers even if
                // layout found the correct element count.
                op: if matches!(element.op, Op::Nested | Op::NestedArray) {
                    Op::NestedArray
                } else if matches!(element.op, Op::Char) {
                    Op::CharArray
                } else if matches!(element.op, Op::Wchar) {
                    Op::WcharArray
                } else {
                    Op::ScalarArray
                },
                elem: element.op,
                elem_size: u8::try_from(element.size).unwrap_or(1).max(1),
                dynamic: true,
                offset: 0,
                size: 0,
                aux: element.aux,
                count: DYNAMIC_COUNT,
            }
            .into_compiled(inner.alignment, Box::from([]))
            .marked_to_end());
        }

        let fixed_count = match len {
            ArrayLen::Fixed(count) => Some(*count),
            // Handled above; listed so a new variant cannot slip through.
            ArrayLen::ToEnd => None,
            ArrayLen::Expr(text) => {
                let expr = Expr::parse(text)?;
                match expr.as_const() {
                    Some(value) => Some(u32::try_from(value).map_err(|_| Error::InvalidValue {
                        message: format!("array length out of range: {value}"),
                    })?),
                    None => {
                        // Try to fold against schema constants; a length naming
                        // only `#define`s is still fixed.
                        let scope = SchemaConstScope {
                            schema: self.schema,
                        };
                        match expr.eval(&scope) {
                            Ok(value) => {
                                Some(u32::try_from(value).map_err(|_| Error::InvalidValue {
                                    message: format!("array length out of range: {value}"),
                                })?)
                            }
                            Err(_) => None,
                        }
                    }
                }
            }
            ArrayLen::NullTerminated => None,
        };

        let length_expr = match (fixed_count, len) {
            (None, ArrayLen::Expr(text)) => Some(Expr::parse(text)?),
            _ => None,
        };
        let length_source = match len {
            ArrayLen::Expr(text) => Some(Arc::from(crate::schema::space_expression(text))),
            _ => None,
        };
        let null_terminated = fixed_count.is_none() && matches!(len, ArrayLen::NullTerminated);

        // `char[n]` and `wchar[n]` are not arrays of one-element values: they
        // are a single `bytes` or `str`. Getting this wrong would return a list
        // of one-character strings where a consumer expects a name.
        let op = match element.op {
            Op::Char => Op::CharArray,
            Op::Wchar => Op::WcharArray,
            Op::Nested => Op::NestedArray,
            _ => Op::ScalarArray,
        };

        // A nested element's width lives on its sub-plan, which can exceed a
        // `u8` -- `ext4_inode` is 256 bytes. Truncating it silently produced a
        // stride of zero.
        let elem_size = u8::try_from(element.size).unwrap_or(0);

        let (size, dynamic, count) = match fixed_count {
            Some(count) if !element.dynamic => {
                let total = u64::from(count) * u64::from(element.size);
                let total = u32::try_from(total).map_err(|_| Error::LimitExceeded {
                    limit: "array byte size",
                    value: total,
                })?;
                (total, false, count)
            }
            // The byte width remains dynamic, but the declared element count
            // does not. Keeping the literal count here lets layout distinguish
            // `Item values[2]` from `Item values[]` when `Item` contains a
            // consumer-supplied or otherwise variable-width field.
            Some(count) => (0, true, count),
            _ => (0, true, DYNAMIC_COUNT),
        };

        Ok(CompiledField {
            op: FieldOp {
                op,
                elem: element.op,
                elem_size,
                dynamic: dynamic || null_terminated,
                offset: 0,
                size,
                // `aux` is the sub-plan or enum index and nothing else.
                aux: element.aux,
                count,
            },
            length: length_expr,
            to_end: false,
            // An array aligns like one of its elements, not like the whole.
            alignment: element_alignment,
            dims: Box::from([]),
            runtime_dims: Box::from([]),
            length_source,
        })
    }
}

/// A field mid-compilation: its op, plus a length expression if it has one.
struct CompiledField {
    op: FieldOp,
    length: Option<Expr>,
    /// True for `[EOF]`: read whatever remains.
    to_end: bool,
    /// Natural alignment of this field's type, in bytes.
    alignment: u32,
    /// Dimensions, outermost first, for a multi-dimensional array.
    dims: Box<[u32]>,
    /// Rich dimensions for a matrix whose shape is resolved per record.
    runtime_dims: Box<[ArrayDimension]>,
    /// Canonical source for a one-dimensional expression count.
    length_source: Option<Arc<str>>,
}

impl CompiledField {
    /// Mark a field as `[EOF]`.
    fn marked_to_end(mut self) -> Self {
        self.to_end = true;
        self
    }
}

/// Resolves names against schema constants only, for folding array lengths at
/// compile time.
struct SchemaConstScope<'a> {
    schema: &'a Schema,
}

impl crate::expr::Scope for SchemaConstScope<'_> {
    fn lookup(&self, name: &str) -> Option<i128> {
        // Deliberately *only* `#define` constants. Upstream does not resolve
        // enum members here — `uint16 vals[SD_COUNT]` raises
        // `ExpressionParserError` on upstream — and making rs2 more permissive
        // would decode a record where the oracle produces none. Being wrong in
        // a generous direction is still being different.
        match self.schema.constants.get(name) {
            Some(ConstValue::Str(_)) | None => None,
            Some(_) => self.schema.const_int(name),
        }
    }

    /// `char a[sizeof(uint32)]` -- resolved from the schema at compile time.
    ///
    /// A struct's size is whatever compiling it says, so this compiles it. That
    /// is not as circular as it looks: a type can only name types declared
    /// before it, and `MAX_NESTING` bounds the depth, so a definition that
    /// somehow referred to itself hits that guard rather than recursing
    /// forever. A type with no fixed size has no `sizeof`, which is correct --
    /// its size depends on the bytes.
    fn size_of(&self, type_name: &str) -> Option<i128> {
        match self.schema.resolve(type_name).ok()? {
            TypeDef::Scalar(scalar) => Some(i128::from(scalar.size())),
            _ => {
                let plan = Compiler::new(self.schema).compile(type_name).ok()?;
                plan.total_size.map(i128::from)
            }
        }
    }

    fn offset_of(&self, type_name: &str, member: &str) -> Option<i128> {
        let plan = Compiler::new(self.schema).compile(type_name).ok()?;
        let index = plan.names.iter().position(|name| name.as_ref() == member)?;
        plan.fields.get(index).map(|field| i128::from(field.offset))
    }
}

fn scalar_op(scalar: Scalar) -> FieldOp {
    // A LEB128 has no width until it is read, so it is the one scalar that
    // makes its structure dynamic.
    if let Scalar::Leb128 { signed } = scalar {
        return FieldOp {
            op: Op::Leb128,
            elem: if signed { Op::Int } else { Op::Uint },
            elem_size: 0,
            dynamic: true,
            offset: 0,
            size: 0,
            aux: 0,
            count: 1,
        };
    }
    let (op, size) = match scalar {
        Scalar::Uint(n) => (Op::Uint, n),
        Scalar::Int(n) => (Op::Int, n),
        Scalar::Float(n) => (Op::Float, n),
        Scalar::Char => (Op::Char, 1),
        Scalar::Wchar => (Op::Wchar, 2),
        Scalar::Void => (Op::Void, 0),
        Scalar::Leb128 { .. } => unreachable!("handled above"),
    };
    FieldOp {
        op,
        elem: op,
        elem_size: size,
        dynamic: false,
        offset: 0,
        size: u32::from(size),
        aux: 0,
        count: 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn compile(source: &str, type_name: &str) -> Arc<CompiledPlan> {
        let schema = parse(source, Endian::Little).unwrap();
        Compiler::new(&schema).compile(type_name).unwrap()
    }

    /// The layout claim in the module docs is load-bearing, so it is asserted.
    /// An eight-field record must fit in two cache lines.
    #[test]
    fn field_op_stays_small() {
        assert_eq!(std::mem::size_of::<FieldOp>(), 20);
        assert_eq!(
            std::mem::size_of::<FieldOp>() * 8,
            160,
            "an eight-field record must stay within three cache lines"
        );
    }

    /// `aux` must mean exactly one thing. When it doubled as the element count,
    /// an array of structures decoded as a single element -- which is how the
    /// MBR's four-entry partition table became one, and every partitioned image
    /// became undetectable.
    #[test]
    fn an_array_of_structs_keeps_both_its_subplan_and_its_count() {
        let plan = compile(
            "struct part { uint8 flag; uint8 chs[3]; uint8 type; uint8 chs2[3];              uint32 lba; uint32 sectors; };
             struct mbr { char boot[446]; part partitions[4]; uint16 signature; };",
            "mbr",
        );
        let partitions = &plan.fields[1];
        assert_eq!(partitions.op, Op::NestedArray);
        assert_eq!(partitions.count, 4, "four partition entries, not one");
        assert_eq!(partitions.size, 64, "4 x 16 bytes");
        assert_eq!(
            plan.fields[2].offset, 510,
            "the boot signature must sit after all four entries"
        );
        assert_eq!(plan.total_size, Some(512));
    }

    #[test]
    fn fixed_struct_gets_offsets_and_a_total_size() {
        let plan = compile("struct s { uint16 a; uint32 b; uint8 c; };", "s");
        assert_eq!(plan.total_size, Some(7));
        assert_eq!(plan.fields[0].offset, 0);
        assert_eq!(plan.fields[1].offset, 2);
        assert_eq!(plan.fields[2].offset, 6);
        assert!(plan.fast_path_eligible());
    }

    #[test]
    fn union_members_all_start_at_zero() {
        let plan = compile("union u { uint32 a; uint16 b; };", "u");
        assert!(plan.is_union);
        assert_eq!(
            plan.total_size,
            Some(4),
            "a union is as big as its largest member"
        );
        assert_eq!(plan.fields[0].offset, 0);
        assert_eq!(plan.fields[1].offset, 0);
    }

    #[test]
    fn char_and_wchar_arrays_are_not_scalar_arrays() {
        let plan = compile("struct s { char name[8]; wchar wide[4]; };", "s");
        assert_eq!(plan.fields[0].op, Op::CharArray);
        assert_eq!(plan.fields[0].size, 8);
        assert_eq!(plan.fields[1].op, Op::WcharArray);
        assert_eq!(plan.fields[1].size, 8, "4 code units of 2 bytes");
    }

    /// A length naming only `#define`s is still fixed, and folding it here
    /// means the decoder never evaluates an expression for it.
    #[test]
    fn constant_lengths_fold_at_compile_time() {
        let plan = compile("#define N 4\nstruct s { uint32 v[N]; };", "s");
        assert!(!plan.fields[0].dynamic);
        assert_eq!(plan.fields[0].size, 16);
        assert_eq!(plan.total_size, Some(16));
        assert!(
            plan.lengths[0].is_none(),
            "a folded length needs no expression"
        );
    }

    /// The trap this avoids: a dynamic array followed by
    /// another field must not leave that field at a fixed offset.
    #[test]
    fn fields_after_a_dynamic_field_are_dynamic() {
        let plan = compile(
            "struct s { uint16 len; wchar name[len]; uint32 trailing; };",
            "s",
        );
        assert_eq!(plan.total_size, None, "the record has no fixed size");
        assert_eq!(plan.fields[0].offset, 0);
        assert!(plan.fields[1].dynamic);
        assert_eq!(
            plan.fields[2].offset, DYNAMIC_OFFSET,
            "the trailing field's offset depends on the array's length"
        );
        assert!(!plan.fast_path_eligible());
    }

    #[test]
    fn anonymous_union_members_fold_into_the_parent_namespace() {
        let plan = compile(
            "typedef struct _REF { ULONG Low; USHORT High; USHORT Seq; } FILE_REFERENCE;
             typedef struct _INDEX_ENTRY {
                 union {
                     FILE_REFERENCE FileReference;
                     struct { USHORT DataOffset; USHORT DataLength; ULONG _; };
                 };
                 USHORT Length;
                 USHORT KeyLength;
             } INDEX_ENTRY;",
            "INDEX_ENTRY",
        );
        // Named fields resolve directly, in one step.
        assert_eq!(plan.field_path("Length").map(<[u32]>::len), Some(1));
        // A folded name resolves through the anonymous member that declares it,
        // so its path is longer than one. Returning a single index here would
        // hand back the union rather than the field the consumer asked for.
        let reference = plan
            .field_path("FileReference")
            .expect("folded name must resolve");
        assert!(
            reference.len() > 1,
            "a folded name must step through its anonymous member, got {reference:?}"
        );
        // And a folded name two levels deep still resolves.
        assert!(plan.field_path("DataOffset").is_some());
    }

    #[test]
    fn enums_carry_their_members_and_width() {
        let plan = compile(
            "flag F : uint16 { A = 1, B = 2 };
             struct s { F flags; };",
            "s",
        );
        assert_eq!(plan.fields[0].op, Op::Enum);
        assert_eq!(plan.fields[0].size, 2);
        let enum_plan = &plan.enums[plan.fields[0].aux as usize];
        assert!(enum_plan.is_flag);
        assert_eq!(enum_plan.members.len(), 2);
    }

    #[test]
    fn an_alternate_endian_plan_updates_nested_enums_and_bitfields() {
        let plan = compile(
            "enum E : uint16 { A = 1 }; struct inner { E e; }; \
             struct outer { uint16 high : 4; uint16 low : 12; inner child; };",
            "outer",
        );
        let alternate = plan.with_endian(Endian::Big);

        assert_eq!(alternate.endian, Endian::Big);
        assert_eq!(alternate.bit_windows[0].shift, 12);
        assert_eq!(alternate.bit_windows[1].shift, 0);
        assert_eq!(alternate.subplans[0].endian, Endian::Big);
        assert_eq!(alternate.subplans[0].enums[0].endian, Endian::Big);
        assert_eq!(
            plan.endian,
            Endian::Little,
            "the original plan is immutable"
        );
    }

    #[test]
    fn nested_structs_become_subplans_not_pointers() {
        let plan = compile(
            "struct inner { uint32 a; uint32 b; };
             struct outer { inner first; uint16 tail; };",
            "outer",
        );
        assert_eq!(plan.fields[0].op, Op::Nested);
        assert_eq!(plan.subplans.len(), 1);
        assert_eq!(plan.subplans[0].total_size, Some(8));
        assert_eq!(plan.total_size, Some(10));
        assert_eq!(plan.shape.nesting_depth, 1);
    }

    /// A definition is input. Self-reference must be an error, not a stack
    /// overflow.
    #[test]
    fn self_referential_structures_are_bounded() {
        let schema = parse("struct s { uint32 a; };", Endian::Little).unwrap();
        let mut compiler = Compiler::new(&schema);
        // Compiling a non-structure is refused rather than mis-planned.
        assert!(compiler.compile("uint32").is_err());
    }

    #[test]
    fn the_real_index_entry_compiles_to_a_fixed_sixteen_bytes() {
        let ntfs = include_str!("../tests/fixtures/ntfs.txt");
        let schema = parse(ntfs, Endian::Little).unwrap();
        let plan = Compiler::new(&schema).compile("_INDEX_ENTRY").unwrap();
        assert_eq!(
            plan.total_size,
            Some(16),
            "_INDEX_ENTRY is 16 bytes: an 8-byte union plus four shorts"
        );
    }
}
