//! The `cstruct()` object consumers hold, and `load()`.
//!
//! Callers write `cs = cstruct(); cs.load(definition)` and then reach types as
//! attributes — `cs.MY_STRUCT`. That shape comes from upstream and is
//! reproduced rather than improved on.
//!
//! Everything here happens when a definition is loaded or a type is first
//! reached, never per record, so none of it is written for speed. The per-record
//! work is in [`crate::types`].

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::pyclass::{PyTraverseError, PyVisit};
use pyo3::types::{PyBytes, PyDict, PyString, PyType, PyWeakrefReference};

use cstruct_rs2_core::parser::{
    parse_aligned_into_with_metadata, parse_into, parse_into_with_metadata, ParseMetadata,
};
use cstruct_rs2_core::plan::Compiler;
use cstruct_rs2_core::schema::{
    ArrayLen, ConstValue, Endian, FieldDef, Scalar, Schema, TypeDef, TypeRef,
};

use crate::scalars::ScalarType;

use crate::errors::to_py_err;
use crate::types::runtime::{PlanRuntime, TypeIdentities};
use crate::types::StructType;

/// A dict-like view of a namespace's declared types.
///
/// Upstream's `typedefs` is a plain `dict` of name to type object, and
/// `dissect.executable`'s `copy_cstruct` clones a namespace with
/// `dst.typedefs = src.typedefs.copy()`. rs2 keeps its types in a Rust schema,
/// so the dict has to be a view rather than the storage — but it has to *behave*
/// like the dict, because that assignment is the whole mechanism by which the
/// ELF reader builds its 32- and 64-bit namespaces.
#[pyclass(name = "TypeDefs", module = "dissect.cstruct", mapping)]
pub struct TypeDefs {
    schema: Arc<Schema>,
    /// The namespace these types came from, for building a type object on
    /// demand. A copy keeps pointing at the original, which is where the type
    /// objects legitimately live.
    owner: Py<CStruct>,
}

#[pymethods]
impl TypeDefs {
    /// The view keeps its source namespace alive while a caller holds the view.
    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.owner)
    }

    /// `src.typedefs.copy()` — the call `copy_cstruct` actually makes.
    ///
    /// The schema is immutable behind an `Arc`, so a copy shares it rather than
    /// duplicating 105 entries.
    fn copy(&self, py: Python<'_>) -> Self {
        Self {
            schema: Arc::clone(&self.schema),
            owner: self.owner.clone_ref(py),
        }
    }

    fn keys(&self) -> Vec<String> {
        self.schema.order.clone()
    }

    fn __len__(&self) -> usize {
        self.schema.types.len()
    }

    fn __contains__(&self, name: &str) -> bool {
        self.schema.types.contains_key(name)
    }

    fn __iter__(slf: &Bound<'_, Self>) -> PyResult<Py<PyAny>> {
        let names = slf.borrow().schema.order.clone();
        let list = pyo3::types::PyList::new(slf.py(), names)?;
        Ok(list.try_iter()?.into_any().unbind())
    }

    fn __getitem__(&self, py: Python<'_>, name: &str) -> PyResult<Py<PyAny>> {
        self.owner
            .bind(py)
            .call_method1("_resolve_type", (name,))
            .map(Bound::unbind)
    }

    fn __repr__(&self) -> String {
        format!("<TypeDefs {} types>", self.schema.types.len())
    }
}

/// A definition namespace: types, constants, and the byte order they use.
#[pyclass(name = "cstruct", module = "dissect.cstruct", subclass, weakref)]
pub struct CStruct {
    schema: Schema,
    /// Live byte order shared by scalar and scalar-array type objects.
    endian_is_little: Arc<AtomicBool>,
    /// Compiled plans, built lazily on first access and then reused.
    ///
    /// Lazily because a definition usually declares many more types than any
    /// one caller reads. Compiling all of them when the definition loads would
    /// charge import time for work most callers never use.
    /// Generated Python classes, one per struct, built on first access.
    compiled: HashMap<String, Py<PyAny>>,
    /// Enum and flag types, built as real Python `IntEnum`/`IntFlag` classes.
    ///
    /// Built once per type and cached, because constructing an `Enum` subclass
    /// is expensive and consumers reach the *class* at import time --
    /// `dissect.ntfs` does `ATTRIBUTE_TYPE_CODE = c_ntfs.ATTRIBUTE_TYPE_CODE`
    /// at module scope, then compares members against it for every attribute of
    /// every MFT record.
    enum_types: HashMap<String, Py<PyAny>>,
    /// One weak back-reference shared by every enum class in this namespace.
    ///
    /// Each class exposes the namespace as ``__cs__``. Sharing the weakref
    /// keeps that metadata from allocating one helper object per enum while
    /// still avoiding the namespace-to-class ownership cycle.
    enum_owner_ref: Option<Py<PyAny>>,
    /// Constants assigned directly by a consumer, as opposed to parsed.
    ///
    /// Upstream's `consts` is an ordinary mutable dict; rs2 derives it from the
    /// schema, so assignment needs somewhere of its own to live.
    custom_consts: HashMap<String, Py<PyAny>>,
    /// The dict handed out by the `consts` getter.
    ///
    /// Current upstream exposes one ordinary mutable dictionary. Keeping one
    /// persistent object makes `cs.consts["A"] = 8` visible to later expression
    /// evaluation instead of mutating a temporary view.
    consts_view: Option<Py<PyDict>>,
    /// Enum reverse lookups, carried for compatibility. See the getter.
    lookups: Option<Py<PyDict>>,
    /// Consumer-supplied types, by the name they were registered under.
    custom_types: HashMap<String, Py<PyAny>>,
    /// Compiled plans, shared across every compile in this namespace so that a
    /// type has exactly one identity. See `Compiler::with_cache`.
    plan_cache: HashMap<String, Arc<cstruct_rs2_core::plan::CompiledPlan>>,
    /// Scalar type objects, so `cs.uint32 is cs.uint32` holds. Their shared
    /// endian cell changes in place with the namespace.
    scalar_types: HashMap<cstruct_rs2_core::schema::Scalar, Py<PyAny>>,
    /// Pointer classes keyed by their canonical target spelling.
    pointer_types: HashMap<String, Py<PyAny>>,
    /// Stable logical identities for classes kept across endian recompilation.
    type_identities: TypeIdentities,
    /// Bypass constants while `resolve()` asks for a type with the same name.
    ///
    /// Attribute access follows upstream's last-constant-wins rule, while
    /// `resolve("INT")` must still return the built-in type after a constant
    /// called `INT` is declared. The flag is held only for one direct Rust
    /// method call and never participates in decoding.
    resolving_type_only: bool,
}

impl CStruct {
    /// Drop every object derived from the schema after namespace mutation.
    fn clear_generated_types(&mut self) {
        self.compiled.clear();
        self.enum_types.clear();
        self.plan_cache.clear();
        self.pointer_types.clear();
        self.type_identities.clear();
    }

    /// Apply one definition and return metadata from that same native parse.
    ///
    /// `CStyleParser` uses the metadata, while ordinary `load()` discards it.
    /// Both call this helper, so exposing parser state cannot accidentally make
    /// a definition run through the lexer twice.
    fn parse_definition(
        &mut self,
        py: Python<'_>,
        definition: &str,
        aligned: bool,
    ) -> PyResult<ParseMetadata> {
        // Absorb direct assignments to the public constants dictionary before
        // evaluating conditionals or array expressions.
        if let Some(view) = self.consts_view.as_ref().map(|dict| dict.bind(py).clone()) {
            for (name, value) in view.iter() {
                if let (Ok(name), Ok(int)) = (name.extract::<String>(), value.extract::<i128>()) {
                    if !self.schema.constants.contains_key(&name) {
                        self.schema.insert_constant(name, ConstValue::Int(int));
                    }
                }
            }
        }

        let metadata = if aligned {
            parse_aligned_into_with_metadata(definition, &mut self.schema)
        } else {
            parse_into_with_metadata(definition, &mut self.schema)
        }
        .map_err(to_py_err)?;

        // The schema owns parsed constants, while the Python dictionary also
        // carries values assigned by a caller. `#undef` removes the name from
        // both views, matching upstream's one mutable constant namespace.
        for name in &metadata.undefined_constants {
            self.custom_consts.remove(name);
            if let Some(view) = &self.consts_view {
                let view = view.bind(py);
                if view.contains(name)? {
                    view.del_item(name)?;
                }
            }
        }

        self.clear_generated_types();
        Ok(metadata)
    }

    /// Return the one Python type object for a scalar in this namespace.
    fn scalar_type_object(&mut self, py: Python<'_>, scalar: Scalar) -> PyResult<Py<PyAny>> {
        if let Some(existing) = self.scalar_types.get(&scalar) {
            return Ok(existing.clone_ref(py));
        }
        let type_object = Py::new(
            py,
            ScalarType::with_endian_state(
                scalar,
                Arc::clone(&self.endian_is_little),
                canonical_scalar_name(scalar),
            ),
        )?
        .into_any();
        self.scalar_types.insert(scalar, type_object.clone_ref(py));
        Ok(type_object)
    }

    /// Build or fetch one named or hidden enum class.
    ///
    /// Anonymous enum members point at their hidden schema name, while the
    /// class itself keeps an empty public name. Named aliases use the original
    /// declaration's cache entry so every spelling resolves to the same class.
    fn enum_class(
        &mut self,
        py: Python<'_>,
        owner: &Bound<'_, PyAny>,
        schema_name: &str,
    ) -> PyResult<Py<PyAny>> {
        let definition = match self.schema.resolve(schema_name) {
            Ok(TypeDef::Enum(definition)) => definition.clone(),
            _ => {
                return Err(pyo3::exceptions::PyAttributeError::new_err(format!(
                    "{schema_name} is not an enum in this cstruct namespace"
                )))
            }
        };
        let cache_name = if definition.name.is_empty() {
            schema_name
        } else {
            &definition.name
        };
        if let Some(existing) = self.enum_types.get(cache_name) {
            return Ok(existing.clone_ref(py));
        }

        let scalar = match self.schema.resolve(&definition.base) {
            Ok(TypeDef::Scalar(Scalar::Uint(size))) => (Scalar::Uint(*size), *size, false),
            Ok(TypeDef::Scalar(Scalar::Int(size))) => (Scalar::Int(*size), *size, true),
            _ => {
                return Err(pyo3::exceptions::PyTypeError::new_err(
                    "Enum can only be created from int type",
                ))
            }
        };
        let base_type = self.scalar_type_object(py, scalar.0)?;
        let plan = cstruct_rs2_core::plan::EnumPlan {
            name: Arc::from(definition.name.as_str()),
            size: scalar.1,
            signed: scalar.2,
            endian: self.schema.endian,
            is_flag: definition.is_flag,
            members: definition
                .members
                .iter()
                .map(|(name, value)| (Arc::from(name.as_str()), *value))
                .collect(),
        };
        let owner_ref = match &self.enum_owner_ref {
            Some(existing) => existing.clone_ref(py),
            None => {
                let reference = PyWeakrefReference::new(owner)?.into_any().unbind();
                self.enum_owner_ref = Some(reference.clone_ref(py));
                reference
            }
        };
        let class = build_enum_class_from_plan(
            py,
            &plan,
            Some(owner_ref.bind(py)),
            Some(base_type.bind(py)),
        )?;
        self.enum_types
            .insert(cache_name.to_owned(), class.clone_ref(py));
        if schema_name != cache_name {
            self.enum_types
                .insert(schema_name.to_owned(), class.clone_ref(py));
        }
        Ok(class)
    }
}

/// Build the cold Python class published for an array typedef.
///
/// The compiled plan already understands the alias as a native array. This
/// function exists only for direct namespace access, where current upstream
/// exposes a class and permits checks such as ``issubclass(cs.uuid_t,
/// BaseArray)``.
fn array_alias_class(
    bound: &Bound<'_, CStruct>,
    alias: &str,
    reference: &TypeRef,
) -> PyResult<Py<PyAny>> {
    let py = bound.py();
    let built = match reference {
        TypeRef::Named(name) => CStruct::_resolve_type(bound, name)?,
        TypeRef::Array(element, length) => {
            let element_type = array_alias_class(bound, alias, element)?;
            let count = match length {
                ArrayLen::Fixed(value) => (*value as usize).into_pyobject(py)?.into_any().unbind(),
                ArrayLen::Expr(expression) => {
                    expression.clone().into_pyobject(py)?.into_any().unbind()
                }
                ArrayLen::NullTerminated => py.None(),
                ArrayLen::ToEnd => "EOF".into_pyobject(py)?.into_any().unbind(),
            };
            if element_type.bind(py).is_instance_of::<PyType>() {
                bound
                    .as_any()
                    .call_method1("_make_array", (element_type, count))?
                    .unbind()
            } else {
                let native_array = element_type.bind(py).get_item(count.bind(py))?;
                bound
                    .as_any()
                    .call_method1(
                        "_make_native_array_alias",
                        (alias, native_array, element_type, count),
                    )?
                    .unbind()
            }
        }
    };
    if built.bind(py).is_instance_of::<PyType>() {
        built.bind(py).setattr("__name__", alias)?;
        built.bind(py).setattr("__qualname__", alias)?;
    }
    Ok(built)
}

#[pymethods]
impl CStruct {
    /// Tell Python's cyclic collector about every Python object retained here.
    ///
    /// Pointer classes deliberately keep their owning namespace in `__cs__`,
    /// matching upstream identity and pointer-width behaviour. The namespace
    /// caches those same classes, forming a cycle that reference counting alone
    /// cannot release. Generated structure classes can reach the pointer class
    /// through their runtime too, so both caches are visited.
    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        for class in self.compiled.values() {
            visit.call(class)?;
        }
        for class in self.enum_types.values() {
            visit.call(class)?;
        }
        visit.call(&self.enum_owner_ref)?;
        for value in self.custom_consts.values() {
            visit.call(value)?;
        }
        visit.call(&self.consts_view)?;
        visit.call(&self.lookups)?;
        for class in self.custom_types.values() {
            visit.call(class)?;
        }
        for type_object in self.scalar_types.values() {
            visit.call(type_object)?;
        }
        for class in self.pointer_types.values() {
            visit.call(class)?;
        }
        Ok(())
    }

    /// Break Python reference cycles after the collector proves this namespace
    /// is unreachable.
    fn __clear__(&mut self) {
        self.compiled.clear();
        self.enum_types.clear();
        self.enum_owner_ref = None;
        self.custom_consts.clear();
        self.consts_view = None;
        self.lookups = None;
        self.custom_types.clear();
        self.scalar_types.clear();
        self.pointer_types.clear();
    }

    /// Create an empty namespace.
    ///
    /// `endian` accepts the same spellings upstream does, because definitions
    /// and consumers both pass them through.
    /// The first positional argument is a **definition to load**, not the byte
    /// order: upstream's signature is
    /// `cstruct(load="", *, endian="<", pointer=None)`.
    ///
    /// Treating it as the endianness made `cstruct(ds_def)` — which
    /// `dissect.database`'s NTDS reader writes — fail with "unknown endianness"
    /// followed by an entire struct definition, and took the module with it.
    #[new]
    #[pyo3(signature = (load = "", *, endian = "<", pointer = None))]
    fn new(py: Python<'_>, load: &str, endian: &str, pointer: Option<&str>) -> PyResult<Self> {
        // `pointer` selects the width a `TYPE *name;` member reads at.
        // Accepting it and then ignoring it would be worse than rejecting it:
        // reading a 16-bit pointer as 64 bits misplaces every field after it,
        // with no error.
        let pointer_size = match pointer {
            None => {
                // From `sys.maxsize`, which is where upstream reads it -- not
                // from `usize::BITS`. The two agree on every real host, and
                // reading Python's value is what makes the width follow a
                // patched `sys.maxsize`, which is how upstream's suite (and
                // anything emulating a 32-bit target) selects a 32-bit pointer.
                let maxsize: u128 = py
                    .import("sys")?
                    .getattr("maxsize")?
                    .extract()
                    .unwrap_or(usize::MAX as u128);
                // `> 2**32`, upstream's exact threshold -- not `> u32::MAX`.
                // They differ by one, and `sys.maxsize == 2**32` is the boundary
                // value the upstream suite uses to model a 32-bit target.
                if maxsize > (1u128 << 32) {
                    8
                } else {
                    4
                }
            }
            Some(name) => match name
                .strip_prefix("uint")
                .and_then(|b| b.parse::<u32>().ok())
            {
                Some(bits) if matches!(bits, 8 | 16 | 32 | 64) => (bits / 8) as u8,
                _ => {
                    return Err(pyo3::exceptions::PyValueError::new_err(format!(
                        "not a usable pointer type: {name}"
                    )))
                }
            },
        };

        let endian = crate::scalars::parse_endian(endian)?;
        let endian_is_little = Arc::new(AtomicBool::new(matches!(endian, Endian::Little)));
        let mut schema = Schema::with_builtins(endian);
        schema.pointer_size = pointer_size;
        if !load.is_empty() {
            parse_into(load, &mut schema).map_err(to_py_err)?;
        }
        Ok(Self {
            schema,
            endian_is_little,
            compiled: HashMap::new(),
            enum_types: HashMap::new(),
            enum_owner_ref: None,
            custom_consts: HashMap::new(),
            consts_view: None,
            lookups: None,
            custom_types: HashMap::new(),
            plan_cache: HashMap::new(),
            scalar_types: HashMap::new(),
            pointer_types: HashMap::new(),
            type_identities: HashMap::new(),
            resolving_type_only: false,
        })
    }

    /// Parse a definition into this namespace.
    ///
    /// Definitions accumulate: consumers call this more than once and later
    /// definitions reference earlier types.
    /// Returns `self`, because consumers chain it.
    ///
    /// `dissect.ntfs` writes `c_ntfs = cstruct().load(ntfs_def)` at module
    /// scope. Returning `None` makes `c_ntfs` `None` and every later attribute
    /// access fails with an error that names the *consumer's* variable rather
    /// than the cause -- which is exactly how this was found.
    #[pyo3(signature = (definition, deftype = None, **kwargs))]
    fn load<'py>(
        mut slf: PyRefMut<'py, Self>,
        definition: &str,
        deftype: Option<&Bound<'py, PyAny>>,
        kwargs: Option<&Bound<'py, pyo3::types::PyDict>>,
    ) -> PyResult<Py<Self>> {
        // `deftype` selects the definition dialect. Upstream defaults to its C
        // parser, which is the only one implemented here.
        let _ = deftype;

        // `align=True` asks for natural alignment, and it is a property of this
        // *load* rather than of the namespace: `dissect.target`'s atop reader
        // loads one definition unaligned and the next aligned into the same
        // object.
        let aligned = match kwargs {
            Some(kwargs) => match kwargs.get_item("align")? {
                Some(value) => value.is_truthy()?,
                None => false,
            },
            None => false,
        };

        let py = slf.py();
        slf.parse_definition(py, definition, aligned)?;
        Ok(slf.into())
    }

    /// Parse once and return the cold metadata needed by `CStyleParser`.
    #[pyo3(signature = (definition, *, compiled = true, align = false))]
    fn _parse_definition<'py>(
        &mut self,
        py: Python<'py>,
        definition: &str,
        compiled: bool,
        align: bool,
    ) -> PyResult<Py<PyDict>> {
        // A native plan is neither upstream's compiled nor interpreted Python
        // path. The flag remains accepted so source-compatible callers need no
        // branch, but it cannot change the one native parser.
        let _ = compiled;
        let metadata = self.parse_definition(py, definition, align)?;
        let result = PyDict::new(py);
        result.set_item("flags", metadata.flags)?;
        result.set_item("includes", &self.schema.includes)?;
        Ok(result.unbind())
    }

    /// Resolve a type or constant by name.
    ///
    /// `__getattr__` rather than `__getattribute__`: this object is touched
    /// once per type per process, not per record, so the fallback cost that
    /// matters so much in [`crate::types::Record`] is irrelevant here.
    fn __getattr__(bound: &Bound<'_, Self>, name: &str) -> PyResult<Py<PyAny>> {
        let py = bound.py();
        // The borrow is scoped rather than held for the whole function: building
        // a struct type calls back into Python, and that call reaches
        // `cs._make_pointer`, which reaches this method again. A borrow still
        // live at that point is a `BorrowMutError` at runtime -- and only for
        // definitions containing a pointer, which is the worst kind of latent
        // failure. `mut slf` covers everything up to that call and is dropped
        // before it.
        let mut slf = bound.borrow_mut();

        if let Some(existing) = slf.compiled.get(name) {
            return Ok(existing.clone_ref(py));
        }

        if !slf.resolving_type_only {
            if let Some(value) = slf.custom_consts.get(name) {
                return Ok(value.clone_ref(py));
            }

            if let Some(enum_name) = slf.schema.anonymous_enum_members.get(name).cloned() {
                let class = slf.enum_class(py, bound.as_any(), &enum_name)?;
                return class.bind(py).getattr(name).map(Bound::unbind);
            }

            // Constants come out as plain Python values. Upstream keeps
            // constants and types in separate maps, then lets a constant win
            // ordinary attribute access when their names collide.
            if let Some(value) = slf.schema.constants.get(name).cloned() {
                return match value {
                    ConstValue::Str(text) => Ok(text.into_pyobject(py)?.into_any().unbind()),
                    ConstValue::Bytes(raw) => Ok(PyBytes::new(py, &raw).into_any().unbind()),
                    ConstValue::Expr { source, .. } => match slf.schema.const_int(name) {
                        Some(int) => Ok(int.into_pyobject(py)?.into_any().unbind()),
                        None => Ok(source.into_pyobject(py)?.into_any().unbind()),
                    },
                    ConstValue::Int(int) => Ok(int.into_pyobject(py)?.into_any().unbind()),
                };
            }
        }

        if let Some(existing) = slf.enum_types.get(name) {
            return Ok(existing.clone_ref(py));
        }

        // A registered custom type is already a namespace-bound Python class.
        // Returning it directly keeps mutation such as `EtwPointer.as_32bit()`
        // visible to the compiled plan that holds the same class handle.
        if matches!(slf.schema.resolve(name), Ok(TypeDef::Custom)) {
            if let Some(class) = slf.custom_types.get(name) {
                return Ok(class.clone_ref(py));
            }
        }

        // Pointer and array typedefs have native layout definitions, plus a
        // cold Python class surface for callers that reach the typedef itself.
        // Build either class once and keep ordinary field decoding on the
        // existing compiled operation.
        if let Some(target) = slf.schema.pointer_aliases.get(name).cloned() {
            drop(slf);
            let class = bound
                .as_any()
                .call_method1("_make_pointer", (target,))?
                .unbind();
            bound
                .borrow_mut()
                .compiled
                .insert(name.to_owned(), class.clone_ref(py));
            return Ok(class);
        }
        if let Some(TypeDef::Alias(reference @ TypeRef::Array(_, _))) =
            slf.schema.types.get(name).cloned()
        {
            drop(slf);
            let class = array_alias_class(bound, name, &reference)?;
            bound
                .borrow_mut()
                .compiled
                .insert(name.to_owned(), class.clone_ref(py));
            return Ok(class);
        }

        // Scalars are namespace attributes too. `dissect.extfs` writes
        // `c_ext.uint32[15](self.inode.i_block)` to reinterpret an inode's
        // block list, which needs the scalar, the subscript, and the call.
        if let Ok(TypeDef::Scalar(scalar)) = slf.schema.resolve(name) {
            let scalar = *scalar;
            return slf.scalar_type_object(py, scalar);
        }

        // Enums and flags become real Python classes, because that is what
        // consumers compare against.
        if matches!(slf.schema.resolve(name), Ok(TypeDef::Enum(_))) {
            return slf.enum_class(py, bound.as_any(), name);
        }

        // Types are compiled on demand.
        let is_struct = matches!(slf.schema.resolve(name), Ok(TypeDef::Struct(_)));
        if !is_struct {
            return Err(pyo3::exceptions::PyAttributeError::new_err(format!(
                "{name} is not defined in this cstruct namespace"
            )));
        }

        let plan = {
            // The plan cache lives on the namespace, not on one compile call:
            // two plans for one type means two identities, and `isinstance`
            // compares identity.
            let cache = std::mem::take(&mut slf.plan_cache);
            let mut compiler = Compiler::with_cache(&slf.schema, cache);
            let compiled = compiler.compile(name).map_err(to_py_err);
            slf.plan_cache = compiler.into_cache();
            compiled?
        };
        let canonical_name = plan.name.to_string();
        if let Some(existing) = slf.compiled.get(&canonical_name) {
            let class = existing.clone_ref(py);
            slf.compiled.insert(name.to_owned(), class.clone_ref(py));
            return Ok(class);
        }
        // Cloned so the borrow can be released before anything calls back into
        // Python. One clone per *type*, not per record, and the values are
        // handles.
        let custom: HashMap<String, Py<PyAny>> = slf
            .custom_types
            .iter()
            .map(|(key, value)| (key.clone(), value.clone_ref(py)))
            .collect();
        let mut identities = std::mem::take(&mut slf.type_identities);
        drop(slf);

        let namespace = bound.as_any().clone();
        let built = StructType::with_custom(
            py,
            Arc::clone(&plan),
            &custom,
            Some(&namespace),
            Some(&mut identities),
        );
        bound.borrow_mut().type_identities = identities;
        let struct_type = Py::new(py, built?)?;
        // Build the Python class once, in the thin scaffolding. Nothing here
        // runs per record: `cls(stream)` reaches Rust through `__new__`.
        let class = py
            .import("dissect.cstruct")?
            .getattr("_make_struct_class")?
            .call1((
                if canonical_name.starts_with("__anonymous_") {
                    name
                } else {
                    &canonical_name
                },
                struct_type,
            ))?
            .unbind();
        let mut slf = bound.borrow_mut();
        slf.compiled.insert(canonical_name, class.clone_ref(py));
        slf.compiled.insert(name.to_owned(), class.clone_ref(py));
        Ok(class)
    }

    /// Resolve a type without ordinary Python attribute precedence.
    ///
    /// Calling `Self::__getattr__` directly also bypasses real class methods,
    /// so a structure named `load` remains reachable through `resolve` while
    /// `cs.load` itself stays callable.
    fn _resolve_type(bound: &Bound<'_, Self>, name: &str) -> PyResult<Py<PyAny>> {
        bound.borrow_mut().resolving_type_only = true;
        let result = Self::__getattr__(bound, name);
        bound.borrow_mut().resolving_type_only = false;
        result
    }

    /// Names this namespace defines, for `dir()` and for diagnostics.
    fn __dir__(&self) -> Vec<String> {
        let mut names: Vec<String> = self.schema.order.clone();
        names.extend(self.schema.constants.keys().cloned());
        names.sort();
        names.dedup();
        names
    }

    /// Register a type or alias in this namespace.
    ///
    /// Upstream accepts a name or an already-bound type object. Concrete type
    /// construction remains elsewhere; this method only makes that type
    /// reachable under another name and invalidates definition-time caches.
    #[pyo3(signature = (name, type_, replace = false))]
    fn add_type(&mut self, name: &str, type_: &Bound<'_, PyAny>, replace: bool) -> PyResult<()> {
        let target_name = type_.extract::<String>().or_else(|_| {
            type_
                .getattr("name")
                .or_else(|_| type_.getattr("__name__"))?
                .extract::<String>()
        });

        if let Ok(target_name) = target_name {
            if let Ok(target) = self.schema.resolve(&target_name).cloned() {
                let same_type = self.schema.resolve(name).ok() == Some(&target);
                if self.schema.types.contains_key(name) && !same_type && !replace {
                    return Err(pyo3::exceptions::PyValueError::new_err(format!(
                        "Duplicate type: {name}"
                    )));
                }
                if !same_type {
                    self.schema.insert(
                        name.to_owned(),
                        TypeDef::Alias(cstruct_rs2_core::schema::TypeRef::Named(target_name)),
                    );
                    self.clear_generated_types();
                }
                return Ok(());
            }
        }

        if self.schema.types.contains_key(name) && !replace {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "Duplicate type: {name}"
            )));
        }
        self.schema.insert(name.to_owned(), TypeDef::Custom);
        self.custom_types
            .insert(name.to_owned(), type_.clone().unbind());
        self.clear_generated_types();
        Ok(())
    }

    /// Historical spelling retained by upstream.
    #[pyo3(signature = (name, type_, replace = false))]
    fn addtype(&mut self, name: &str, type_: &Bound<'_, PyAny>, replace: bool) -> PyResult<()> {
        self.add_type(name, type_, replace)
    }

    /// Add a constant to both expression and attribute namespaces.
    fn add_const(&mut self, name: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
        let parsed = if let Ok(integer) = value.extract::<i128>() {
            Some(ConstValue::Int(integer))
        } else if let Ok(string) = value.cast::<PyString>() {
            Some(ConstValue::Str(string.to_str()?.to_owned()))
        } else if let Ok(bytes) = value.cast::<PyBytes>() {
            Some(ConstValue::Bytes(bytes.as_bytes().to_vec()))
        } else {
            None
        };
        if let Some(parsed) = parsed {
            self.schema.insert_constant(name.to_owned(), parsed);
        }
        self.custom_consts
            .insert(name.to_owned(), value.clone().unbind());
        if let Some(view) = &self.consts_view {
            view.bind(value.py()).set_item(name, value)?;
        }
        Ok(())
    }

    /// Historical spelling retained by upstream.
    fn addconst(&mut self, name: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
        self.add_const(name, value)
    }

    /// Remove a constant without disturbing a type of the same name.
    fn del_const(&mut self, py: Python<'_>, name: &str) -> PyResult<()> {
        let existed =
            self.schema.remove_constant(name).is_some() | self.custom_consts.remove(name).is_some();
        if !existed {
            return Err(pyo3::exceptions::PyKeyError::new_err(name.to_owned()));
        }
        if let Some(view) = &self.consts_view {
            view.bind(py).del_item(name)?;
        }
        Ok(())
    }

    /// Historical spelling retained by upstream.
    fn delconst(&mut self, py: Python<'_>, name: &str) -> PyResult<()> {
        self.del_const(py, name)
    }

    /// Return an independent namespace with the same retained schema.
    ///
    /// Generated classes are intentionally rebuilt on first access so their
    /// `__cs__` points at the copy, as current upstream's `__copy__` requires.
    fn copy(slf: &Bound<'_, Self>) -> PyResult<Py<Self>> {
        let py = slf.py();
        let source = slf.borrow();
        let endian_is_little = Arc::new(AtomicBool::new(matches!(
            source.schema.endian,
            Endian::Little
        )));
        let custom_consts = source
            .custom_consts
            .iter()
            .map(|(name, value)| (name.clone(), value.clone_ref(py)))
            .collect();
        let custom_types = source
            .custom_types
            .iter()
            .map(|(name, value)| (name.clone(), value.clone_ref(py)))
            .collect();
        let lookups = source.lookups.as_ref().map(|value| value.clone_ref(py));
        Py::new(
            py,
            Self {
                schema: source.schema.clone(),
                endian_is_little,
                compiled: HashMap::new(),
                enum_types: HashMap::new(),
                enum_owner_ref: None,
                custom_consts,
                consts_view: None,
                lookups,
                custom_types,
                plan_cache: HashMap::new(),
                scalar_types: HashMap::new(),
                pointer_types: HashMap::new(),
                type_identities: HashMap::new(),
                resolving_type_only: false,
            },
        )
    }

    /// Render retained declarations without parsing their source again.
    fn _render_cdef(&self) -> String {
        self.schema.cdef()
    }

    /// Render one structure or union from the retained schema.
    ///
    /// Generated type classes delegate their ``cdef`` method here. This stays
    /// separate from the compiled plan because source-facing details such as
    /// symbolic array expressions are intentionally absent from the hot read
    /// operations.
    #[pyo3(signature = (name, *, recursive = false))]
    fn _render_type_cdef(&self, name: &str, recursive: bool) -> PyResult<String> {
        self.schema.type_cdef(name, recursive).map_err(to_py_err)
    }

    /// Append fields and publish one new immutable plan generation.
    ///
    /// The Python metaclass batches calls made inside ``start_update()`` and
    /// invokes this once when the context closes. Existing records retain their
    /// old plan and values; the generated class and future records move to the
    /// replacement generation.
    fn _add_struct_fields(
        slf: &Bound<'_, Self>,
        type_object: &Bound<'_, StructType>,
        entries: Vec<(String, String, Option<u8>, Option<u32>)>,
    ) -> PyResult<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let py = slf.py();
        let canonical_name = type_object.borrow().plan().name.to_string();

        {
            let mut namespace = slf.borrow_mut();
            let pointer_width = namespace.schema.pointer_size;
            let definition = namespace
                .schema
                .types
                .get_mut(&canonical_name)
                .and_then(|definition| match definition {
                    TypeDef::Struct(structure) => Some(structure),
                    _ => None,
                })
                .ok_or_else(|| {
                    pyo3::exceptions::PyValueError::new_err(format!(
                        "{canonical_name} is not a mutable structure or union"
                    ))
                })?;

            let mut names = definition
                .fields
                .iter()
                .filter_map(|field| field.name.as_deref())
                .filter(|name| *name != "_")
                .map(str::to_owned)
                .collect::<HashSet<_>>();
            for (field_name, type_name, bits, offset) in entries {
                if field_name != "_" && !names.insert(field_name.clone()) {
                    return Err(pyo3::exceptions::PyValueError::new_err(format!(
                        "Duplicate field name: {field_name}"
                    )));
                }
                let (ty, points_to) = programmatic_type_ref(&type_name, pointer_width)?;
                definition.fields.push(FieldDef {
                    name: Some(field_name),
                    ty,
                    bits,
                    points_to,
                    explicit_offset: offset,
                });
            }
            namespace.plan_cache.clear();
        }

        // A changed nested type can alter a materialised parent's layout, so
        // rebuild every live generated class from the updated schema. The work
        // is definition-time only and publishes each plan atomically through
        // its stable type identity.
        let (materialised, custom, mut identities) = {
            let mut namespace = slf.borrow_mut();
            let mut seen = HashSet::new();
            let materialised = namespace
                .compiled
                .values()
                .filter(|class| seen.insert(class.as_ptr() as usize))
                .map(|class| {
                    let type_object = class
                        .bind(py)
                        .getattr("_cstruct_type_")?
                        .cast_into::<StructType>()?
                        .unbind();
                    let name = type_object.bind(py).borrow().plan().name.to_string();
                    Ok((name, type_object))
                })
                .collect::<PyResult<Vec<_>>>()?;
            let custom = namespace
                .custom_types
                .iter()
                .map(|(name, value)| (name.clone(), value.clone_ref(py)))
                .collect::<HashMap<_, _>>();
            let identities = std::mem::take(&mut namespace.type_identities);
            (materialised, custom, identities)
        };

        let plans = {
            let mut namespace = slf.borrow_mut();
            let cache = std::mem::take(&mut namespace.plan_cache);
            let mut compiler = Compiler::with_cache(&namespace.schema, cache);
            let plans = materialised
                .iter()
                .map(|(name, _)| compiler.compile(name).map_err(to_py_err))
                .collect::<PyResult<Vec<_>>>()?;
            namespace.plan_cache = compiler.into_cache();
            plans
        };

        let namespace_object = slf.as_any().clone();
        for ((_, holder), plan) in materialised.into_iter().zip(plans) {
            let runtime = PlanRuntime::build_with_identities(
                py,
                &plan,
                &custom,
                Some(&namespace_object),
                &mut identities,
            )?;
            holder
                .bind(py)
                .borrow_mut()
                .replace_generation(plan, runtime);
        }
        slf.borrow_mut().type_identities = identities;
        Ok(())
    }

    /// Register a consumer-supplied type with its own `_read`.
    ///
    /// `dissect.target`'s Everything and docker readers both declare a varint
    /// this way: the definition language cannot express "read bytes until the
    /// high bit clears", so the consumer supplies a class and cstruct calls it.
    ///
    /// `size`, `alignment` and any further keywords are accepted because
    /// upstream's signature takes them. A declared `size` changes nothing here:
    /// the length of a custom field is whatever the caller's own reader
    /// consumed, which is measured rather than trusted.
    #[pyo3(signature = (name, type_, size = None, alignment = None, **kwargs))]
    fn add_custom_type(
        &mut self,
        name: &str,
        type_: &Bound<'_, PyAny>,
        size: Option<&Bound<'_, PyAny>>,
        alignment: Option<&Bound<'_, PyAny>>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<()> {
        let _ = (size, alignment, kwargs);
        self.schema.insert(name.to_owned(), TypeDef::Custom);
        self.custom_types
            .insert(name.to_owned(), type_.clone().unbind());
        self.compiled.clear();
        self.plan_cache.clear();
        self.pointer_types.clear();
        self.type_identities.clear();
        Ok(())
    }

    /// Upstream spells it both ways.
    #[pyo3(signature = (name, type_, size = None, alignment = None, **kwargs))]
    fn addcustomtype(
        &mut self,
        name: &str,
        type_: &Bound<'_, PyAny>,
        size: Option<&Bound<'_, PyAny>>,
        alignment: Option<&Bound<'_, PyAny>>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<()> {
        self.add_custom_type(name, type_, size, alignment, kwargs)
    }

    /// Register an integer type created through the source-compatible factory.
    ///
    /// Upstream's private-but-supported `_make_int_type` can create any byte
    /// width. Enum fixtures use it for widths such as 40 and 72 bits, which
    /// have no built-in spelling. The decoder represents integers in at most
    /// 128 bits, so wider types are rejected here, before a schema can compile
    /// a field whose value would be truncated.
    fn _register_int_type(&mut self, name: &str, size: u8, signed: bool) -> PyResult<()> {
        if !(1..=16).contains(&size) {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "integer widths must be between 1 and 16 bytes",
            ));
        }
        let scalar = if signed {
            Scalar::Int(size)
        } else {
            Scalar::Uint(size)
        };
        self.schema.insert(name.to_owned(), TypeDef::Scalar(scalar));
        self.compiled.clear();
        self.enum_types.clear();
        self.plan_cache.clear();
        self.pointer_types.clear();
        self.type_identities.clear();
        self.scalar_types.remove(&scalar);
        Ok(())
    }

    /// Types declared by the definitions loaded so far.
    ///
    /// A dict-like view, not a list of names: upstream's is a `dict` and
    /// `copy_cstruct` calls `.copy()` on it.
    #[getter]
    fn typedefs(slf: &Bound<'_, Self>) -> PyResult<TypeDefs> {
        Ok(TypeDefs {
            schema: Arc::new(slf.borrow().schema.clone()),
            owner: slf.clone().unbind(),
        })
    }

    /// Include spellings recorded by active `#include` directives.
    #[getter]
    fn includes(&self) -> Vec<String> {
        self.schema.includes.clone()
    }

    /// `cs.typedefs = other.typedefs.copy()` — adopt another namespace's types.
    #[setter]
    fn set_typedefs(&mut self, value: &Bound<'_, PyAny>) -> PyResult<()> {
        if let Ok(view) = value.extract::<PyRef<'_, TypeDefs>>() {
            // Adopt the source's types wholesale, preserving declaration order.
            // The builtins are already present in both and simply overwrite.
            for name in &view.schema.order {
                if let Some(def) = view.schema.types.get(name) {
                    self.schema.insert(name.clone(), def.clone());
                }
            }
            for name in &view.schema.constant_order {
                if let Some(value) = view.schema.constants.get(name) {
                    self.schema.insert_constant(name.clone(), value.clone());
                }
            }
            for (member, owner) in &view.schema.anonymous_enum_members {
                self.schema
                    .anonymous_enum_members
                    .insert(member.clone(), owner.clone());
            }
            self.compiled.clear();
            self.enum_types.clear();
            self.plan_cache.clear();
            self.pointer_types.clear();
            self.type_identities.clear();
            return Ok(());
        }
        Err(pyo3::exceptions::PyTypeError::new_err(
            "cs.typedefs expects another namespace's typedefs view",
        ))
    }

    /// Reverse lookups for enum types. Upstream exposes this as an ordinary
    /// mutable dict, and code that clones one namespace into another assigns to
    /// it, so it has to be both readable and settable.
    #[getter]
    fn lookups<'py>(&self, py: Python<'py>) -> Bound<'py, PyDict> {
        match &self.lookups {
            Some(existing) => existing.bind(py).clone(),
            None => PyDict::new(py),
        }
    }

    #[setter]
    fn set_lookups(&mut self, value: &Bound<'_, PyDict>) {
        self.lookups = Some(value.clone().unbind());
    }

    /// Constants, as a plain dict.
    #[getter]
    fn consts<'py>(bound: &Bound<'py, Self>) -> PyResult<Bound<'py, PyDict>> {
        let py = bound.py();
        let mut slf = bound.borrow_mut();
        // One dict for the lifetime of the namespace. Entries parsed since the
        // last access are merged in; entries the consumer added are left alone,
        // which is what makes an item assignment stick.
        let out = match &slf.consts_view {
            Some(existing) => existing.bind(py).clone(),
            None => {
                let fresh = PyDict::new(py);
                slf.consts_view = Some(fresh.clone().unbind());
                fresh
            }
        };
        let names = slf.schema.constants.keys().cloned().collect::<Vec<_>>();
        for name in names {
            if let Some(enum_name) = slf.schema.anonymous_enum_members.get(&name).cloned() {
                let class = slf.enum_class(py, bound.as_any(), &enum_name)?;
                out.set_item(&name, class.bind(py).getattr(&name)?)?;
                continue;
            }
            match slf.schema.constants.get(&name) {
                Some(ConstValue::Str(text)) => out.set_item(name, text)?,
                Some(ConstValue::Bytes(raw)) => {
                    out.set_item(name, pyo3::types::PyBytes::new(py, raw))?
                }
                Some(ConstValue::Expr { source, .. }) => {
                    if let Some(value) = slf.schema.const_int(&name) {
                        out.set_item(name, value)?;
                    } else {
                        out.set_item(name, source)?;
                    }
                }
                Some(ConstValue::Int(value)) => out.set_item(name, value)?,
                None => unreachable!("name came from schema.constants"),
            }
        }
        // Anything the consumer assigned wins over a parsed constant of the
        // same name, which is what plain attribute assignment would do.
        for (name, value) in &slf.custom_consts {
            out.set_item(name, value.bind(py))?;
        }
        Ok(out)
    }

    /// `cs.consts = {...}` — upstream's `consts` is a plain instance attribute,
    /// and `dissect.executable`'s `copy_cstruct` assigns to it wholesale.
    ///
    /// A read-only property is not a smaller version of that; it is a
    /// `AttributeError` that removes the ELF stack from `target-query`.
    #[setter]
    fn set_consts(&mut self, py: Python<'_>, value: &Bound<'_, PyDict>) -> PyResult<()> {
        self.custom_consts.clear();
        self.schema.constants.clear();
        self.schema.constant_order.clear();
        for (name, item) in value.iter() {
            let name: String = name.extract()?;
            // Integers go into the schema so they can still size an array;
            // everything else is carried for attribute access only.
            if let Ok(int) = item.extract::<i128>() {
                self.schema
                    .insert_constant(name.clone(), ConstValue::Int(int));
            }
            self.custom_consts.insert(name, item.unbind());
        }
        let _ = py;
        Ok(())
    }

    /// The namespace's byte order, spelled as upstream spells it.
    #[getter]
    fn endian(&self) -> &'static str {
        match self.schema.endian {
            Endian::Little => "<",
            Endian::Big => ">",
        }
    }

    /// Set the namespace's byte order and recompile materialised structures.
    ///
    /// Current upstream keeps every type object stable and makes both touched
    /// and untouched types use the new order. A compiled plan fixes bitfield
    /// shifts as well as scalar byte order, so changing one flag inside an old
    /// plan would be incomplete. Rebuilding here is a cold namespace operation;
    /// record decoding still follows one immutable plan with no per-field
    /// namespace lookup.
    #[setter]
    fn set_endian(slf: &Bound<'_, Self>, value: &str) -> PyResult<()> {
        let py = slf.py();
        let endian = crate::scalars::parse_endian(value)?;

        let (classes, enum_classes, custom) = {
            let mut namespace = slf.borrow_mut();
            namespace.schema.endian = endian;
            namespace
                .endian_is_little
                .store(matches!(endian, Endian::Little), Ordering::Relaxed);
            namespace.plan_cache.clear();

            let mut seen = HashSet::new();
            let classes = namespace
                .compiled
                .values()
                .filter(|class| seen.insert(class.as_ptr() as usize))
                .map(|class| class.clone_ref(py))
                .collect::<Vec<_>>();
            let enum_classes = namespace
                .enum_types
                .values()
                .map(|class| class.clone_ref(py))
                .collect::<Vec<_>>();
            let custom = namespace
                .custom_types
                .iter()
                .map(|(name, class)| (name.clone(), class.clone_ref(py)))
                .collect::<HashMap<_, _>>();
            (classes, enum_classes, custom)
        };

        let byteorder = if matches!(endian, Endian::Little) {
            "little"
        } else {
            "big"
        };
        for class in enum_classes {
            class.bind(py).setattr("__byteorder__", byteorder)?;
        }

        let mut materialised = Vec::with_capacity(classes.len());
        for class in classes {
            let type_object = class
                .bind(py)
                .getattr("_cstruct_type_")?
                .cast_into::<StructType>()?
                .unbind();
            let name = type_object.bind(py).borrow().plan().name.to_string();
            materialised.push((name, type_object));
        }

        let (plans, mut identities) = {
            let mut namespace = slf.borrow_mut();
            let cache = std::mem::take(&mut namespace.plan_cache);
            let mut compiler = Compiler::with_cache(&namespace.schema, cache);
            let plans = materialised
                .iter()
                .map(|(name, _)| compiler.compile(name).map_err(to_py_err))
                .collect::<PyResult<Vec<_>>>()?;
            namespace.plan_cache = compiler.into_cache();
            (plans, std::mem::take(&mut namespace.type_identities))
        };

        let namespace = slf.as_any().clone();
        let mut result = Ok(());
        for ((_, type_object), plan) in materialised.into_iter().zip(plans) {
            let runtime = PlanRuntime::build_with_identities(
                py,
                &plan,
                &custom,
                Some(&namespace),
                &mut identities,
            );
            match runtime {
                Ok(runtime) => {
                    type_object
                        .bind(py)
                        .borrow_mut()
                        .replace_generation(plan, runtime);
                }
                Err(error) => {
                    result = Err(error);
                    break;
                }
            }
        }
        slf.borrow_mut().type_identities = identities;
        result
    }

    /// Canonical target spelling for a generated pointer class.
    ///
    /// Upstream resolves typedefs before naming a pointer, so `u_int *` is a
    /// `uint32*` when `u_int` aliases `uint32`. Structure aliases similarly use
    /// their tag. This resolves names without materialising the target type,
    /// which is essential for `struct Node { Node *next; }` while `Node` itself
    /// is still being compiled.
    fn _canonical_type_name(&self, name: &str) -> String {
        let base = name.trim_end_matches('*');
        let indirection = name.len() - base.len();
        let canonical = match self.schema.resolve(base) {
            Ok(TypeDef::Scalar(scalar)) => canonical_scalar_name(*scalar).to_owned(),
            Ok(TypeDef::Struct(structure)) => structure.name.clone(),
            Ok(TypeDef::Enum(definition)) => definition.name.clone(),
            _ => base.to_owned(),
        };
        format!("{canonical}{}", "*".repeat(indirection))
    }

    /// Fetch a pointer class already built for this namespace and target.
    fn _pointer_type(&self, py: Python<'_>, target: &str) -> Option<Py<PyAny>> {
        self.pointer_types
            .get(target)
            .map(|class| class.clone_ref(py))
    }

    /// Retain one pointer class so rebuilt plan runtimes preserve its identity.
    fn _cache_pointer_type(&mut self, target: &str, class: Py<PyAny>) {
        self.pointer_types.insert(target.to_owned(), class);
    }

    /// The type a `TYPE *name;` member decodes as.
    ///
    /// Returned as the namespace's own scalar type object, not a fresh one, so
    /// `cs.pointer is cs.uint64` holds -- consumers and upstream's tests both
    /// compare it by identity.
    #[getter]
    fn pointer<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyAny>> {
        let size = match slf.borrow().schema.pointer_size {
            0 => 8,
            size => size,
        };
        slf.getattr(format!("uint{}", u32::from(size) * 8).as_str())
    }

    /// Set the pointer width from a scalar type object or a type name.
    ///
    /// Like `endian`, it applies to types defined *after* the assignment: a
    /// type compiles its plan at definition time and carries the width it was
    /// compiled with. Upstream is the same, for the same reason.
    #[setter]
    fn set_pointer(&mut self, value: &Bound<'_, PyAny>) -> PyResult<()> {
        // A type object carries its width on `size`; a string is the name of
        // one. Both spellings appear in real code.
        let size = if let Ok(size) = value.getattr("size").and_then(|s| s.extract::<u8>()) {
            size
        } else {
            let name: String = value.extract()?;
            match name
                .strip_prefix("uint")
                .and_then(|bits| bits.parse::<u32>().ok())
            {
                Some(bits) if bits % 8 == 0 => (bits / 8) as u8,
                _ => {
                    return Err(pyo3::exceptions::PyValueError::new_err(format!(
                        "not a usable pointer type: {name}"
                    )))
                }
            }
        };
        if !matches!(size, 1 | 2 | 4 | 8) {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "pointer width must be 1, 2, 4 or 8 bytes, not {size}"
            )));
        }
        self.schema.pointer_size = size;
        Ok(())
    }

    fn __repr__(&self) -> String {
        format!(
            "<cstruct endian={} types={}>",
            match self.schema.endian {
                Endian::Little => "<",
                Endian::Big => ">",
            },
            self.schema.order.len()
        )
    }
}

/// Convert a generated type's ``__name__`` back to a schema reference.
///
/// This is used only by the programmatic ``add_field`` surface. Names produced
/// by rs2 use the same compact spellings as upstream (``uint16``, ``Item[4]``
/// and ``Item*``), so this narrow parser need not accept a full declaration.
fn programmatic_type_ref(spelling: &str, pointer_width: u8) -> PyResult<(TypeRef, Option<String>)> {
    let mut pieces = spelling.split('[');
    let base = pieces.next().unwrap_or_default();
    if base.is_empty() {
        return Err(pyo3::exceptions::PyValueError::new_err(
            "field type has no name",
        ));
    }
    let mut dimensions = Vec::new();
    for piece in pieces {
        let Some(length) = piece.strip_suffix(']') else {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "invalid array type name: {spelling}"
            )));
        };
        dimensions.push(if length.is_empty() {
            ArrayLen::NullTerminated
        } else if let Ok(count) = length.parse::<u32>() {
            ArrayLen::Fixed(count)
        } else {
            ArrayLen::Expr(length.to_owned())
        });
    }

    let pointer_base = base.trim_end_matches('*');
    let indirection = base.len() - pointer_base.len();
    let points_to = (indirection > 0).then(|| {
        format!(
            "{pointer_base}{}",
            "*".repeat(indirection.saturating_sub(1))
        )
    });
    let mut reference = if indirection > 0 {
        TypeRef::Named(format!("uint{}", u32::from(pointer_width) * 8))
    } else {
        TypeRef::Named(base.to_owned())
    };
    for length in dimensions.into_iter().rev() {
        reference = TypeRef::Array(Box::new(reference), length);
    }
    Ok((reference, points_to))
}

/// Build one Python enum class for its owning namespace.
///
/// # Why the cache is on `CStruct`, not here
///
/// The class a consumer imports (`c_regf.KEY`) and the class a decoded field's
/// value belongs to **must be the same object**. `dissect.regf` writes
/// `KEY.COMP_NAME in self.cell.Flags`, and Python's `in` on two members of
/// *different* `IntFlag` classes raises
/// `TypeError: unsupported operand type(s) for 'in': 'KEY' and 'KEY'` -- an
/// error that names the same class twice, because there are two of them.
///
/// Building the class in two places produced exactly that, and it stopped every
/// registry hive from opening. `PlanRuntime` therefore asks its namespace for
/// the enum, reaching the namespace-local `enum_types` map. A process-global
/// cache would make two independent namespaces return the same Python class,
/// while current upstream returns distinct classes and members.
/// Build or fetch the Python class for a compiled enum.
pub fn build_enum_class_from_plan(
    py: Python<'_>,
    plan: &cstruct_rs2_core::plan::EnumPlan,
    owner_ref: Option<&Bound<'_, PyAny>>,
    base_type: Option<&Bound<'_, PyAny>>,
) -> PyResult<Py<PyAny>> {
    let members = PyDict::new(py);
    for (name, value) in &plan.members {
        // A duplicate value is an alias in Python's enum model, which is what
        // upstream produces too, so duplicates are inserted rather than skipped.
        members.set_item(name.as_ref(), *value)?;
    }

    // Built in the Python scaffolding, not with `enum.IntEnum` here: what has
    // to match upstream is `__str__` and the treatment of values with no
    // declared name, and both are Python-level behaviour. This runs once per
    // enum per process, never per record.
    Ok(py
        .import("dissect.cstruct")?
        .getattr("_make_enum_class")?
        .call1((
            plan.name.as_ref(),
            members,
            plan.is_flag,
            plan.size,
            matches!(plan.endian, Endian::Little),
            plan.signed,
            Option::<&Bound<'_, PyAny>>::None,
            base_type,
            owner_ref,
        ))?
        .unbind())
}

/// Canonical name for a scalar, independent of which alias was used.
pub(crate) fn canonical_scalar_name(scalar: cstruct_rs2_core::schema::Scalar) -> &'static str {
    use cstruct_rs2_core::schema::Scalar;
    match scalar {
        Scalar::Uint(1) => "uint8",
        Scalar::Uint(2) => "uint16",
        Scalar::Uint(4) => "uint32",
        Scalar::Uint(8) => "uint64",
        Scalar::Int(1) => "int8",
        Scalar::Int(2) => "int16",
        Scalar::Int(4) => "int32",
        Scalar::Int(8) => "int64",
        Scalar::Uint(3) => "uint24",
        Scalar::Uint(6) => "uint48",
        Scalar::Uint(16) => "uint128",
        Scalar::Int(3) => "int24",
        Scalar::Int(6) => "int48",
        Scalar::Int(16) => "int128",
        Scalar::Float(2) => "float16",
        Scalar::Float(4) => "float",
        Scalar::Float(8) => "double",
        Scalar::Char => "char",
        Scalar::Wchar => "wchar",
        Scalar::Void => "void",
        Scalar::Leb128 { signed: false } => "uleb128",
        Scalar::Leb128 { signed: true } => "ileb128",
        // A width `_make_int_type` invented, which has no built-in spelling.
        // Display only -- nothing keys on this, and the comment at the cache
        // says why that matters.
        _ => "scalar",
    }
}

/// Expose the namespace classes on the extension module.
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<CStruct>()?;
    module.add_class::<TypeDefs>()?;
    Ok(())
}
