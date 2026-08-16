//! [`StructType`]: a compiled structure type, and the entry point that decodes it.
//!
//! `cs.MY_STRUCT` is a generated Python class holding one of these, and calling
//! that class decodes a record. This module is the type side of that pair; the
//! record side is [`crate::types::record`].
//!
//! # Why the decode entry point is `__new__`
//!
//! Callers write `isinstance(value, cs.X)`, and `isinstance` requires its second
//! argument to be a type before any hook it might define is consulted. So `cs.X`
//! has to be a real class, which rules out making it a factory object.
//!
//! The natural way to make a class callable-that-decodes is a metaclass whose
//! `__call__` does the work, but PyO3 cannot extend `PyType`, so a metaclass
//! written in Rust is not available. Writing `__call__` in Python instead would
//! add a Python frame to every decode.
//!
//! [`struct_new`] avoids both. `cls(stream)` goes `type.__call__` → `cls.__new__`
//! entirely in C and lands there. The returned record is deliberately *not* an
//! instance of `cls`, so CPython skips `__init__` — one less call per record.
//!
//! # Byte order is fixed for one compiled generation
//!
//! Upstream threads byte order through every read as a parameter, so it can
//! change an existing type when its namespace changes. Here each plan still
//! fixes offsets, byte order and bitfield shifts ahead of time; the namespace
//! replaces that plan as one cold operation while the Python class and logical
//! type identity remain stable. A per-call opposite-endian read lazily derives
//! and caches a second immutable plan; ordinary reads never inspect or build it.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use pyo3::prelude::*;
use pyo3::pyclass::{PyTraverseError, PyVisit};
use pyo3::types::{PyBytes, PyDict, PyList, PyTuple, PyType};

use crate::errors::internal;
use crate::input;
use crate::types::decoding::{decode_one, decode_one_classified};
use crate::types::encode::{
    encode_union_member, record_to_bytes_with_endian, write_record_to_stream,
};
use crate::types::field::{declared_field_name, field_type_object, fields_mapping, lookup_mapping};
use crate::types::record::Record;
use crate::types::runtime::{PlanRuntime, TypeIdentities, TypeIdentity};
use cstruct_rs2_core::plan::CompiledPlan;

/// A cstruct type: callable, and the thing `cs.<Name>` resolves to.
#[pyclass(name = "StructType", module = "dissect.cstruct")]
pub struct StructType {
    pub(crate) plan: Arc<CompiledPlan>,
    pub(crate) runtime: Arc<PlanRuntime>,
    pub(crate) identity: Arc<TypeIdentity>,
    /// Opposite-endian plan, built only after the first per-call override.
    alternate_plan: OnceLock<Arc<CompiledPlan>>,
}

impl StructType {
    /// Wrap a compiled plan, building the Python classes its fields need.
    ///
    /// For a plan with no consumer-supplied types and no namespace. A pointer
    /// field on such a type decodes to a plain integer, because a `Pointer`
    /// subclass needs a namespace to resolve its target against.
    pub fn new(py: Python<'_>, plan: Arc<CompiledPlan>) -> PyResult<Self> {
        Self::with_custom(py, plan, &HashMap::new(), None, None)
    }

    /// Wrap a plan whose definition referenced consumer-supplied types.
    pub fn with_custom(
        py: Python<'_>,
        plan: Arc<CompiledPlan>,
        custom: &HashMap<String, Py<PyAny>>,
        namespace: Option<&Bound<'_, PyAny>>,
        identities: Option<&mut TypeIdentities>,
    ) -> PyResult<Self> {
        let runtime = match identities {
            Some(identities) => {
                PlanRuntime::build_with_identities(py, &plan, custom, namespace, identities)?
            }
            None => PlanRuntime::build_with(py, &plan, custom, namespace)?,
        };
        let identity = Arc::clone(&runtime.identity);
        Ok(Self {
            plan,
            runtime,
            identity,
            alternate_plan: OnceLock::new(),
        })
    }

    /// Wrap a plan and a runtime that were already built together.
    ///
    /// Used where a sub-plan is being exposed as a type in its own right — a
    /// nested field's `type` — so that the sub-runtime the parent already built
    /// is reused instead of building a second set of enum classes.
    pub(crate) fn from_parts(plan: &Arc<CompiledPlan>, runtime: &Arc<PlanRuntime>) -> Self {
        Self {
            plan: Arc::clone(plan),
            runtime: Arc::clone(runtime),
            identity: Arc::clone(&runtime.identity),
            alternate_plan: OnceLock::new(),
        }
    }

    /// Replace the compiled generation while preserving Python type identity.
    pub fn replace_generation(&mut self, plan: Arc<CompiledPlan>, runtime: Arc<PlanRuntime>) {
        self.identity.replace_plan(Arc::clone(&plan));
        self.plan = plan;
        self.runtime = runtime;
        self.alternate_plan = OnceLock::new();
    }

    /// The plan behind this type, for tests and for the registry.
    pub fn plan(&self) -> &Arc<CompiledPlan> {
        &self.plan
    }

    /// The plan and runtime as fresh handles, with the borrow released.
    ///
    /// Every method that decodes or builds Python objects needs both and must
    /// not hold a `PyRef` while re-entering the interpreter, so they all start
    /// this way.
    fn parts(slf: &Bound<'_, Self>) -> (Arc<CompiledPlan>, Arc<PlanRuntime>) {
        let this = slf.borrow();
        (Arc::clone(&this.plan), Arc::clone(&this.runtime))
    }

    /// Select the default or lazily cached alternate plan for one call.
    fn parts_for_endian(
        slf: &Bound<'_, Self>,
        requested: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<(Arc<CompiledPlan>, Arc<PlanRuntime>)> {
        let wanted = requested
            .filter(|value| !value.is_none())
            .map(|value| value.extract::<String>())
            .transpose()?
            .map(|value| crate::scalars::parse_endian(&value))
            .transpose()?;
        let this = slf.borrow();
        let plan = match wanted {
            None => Arc::clone(&this.plan),
            Some(endian) if endian == this.plan.endian => Arc::clone(&this.plan),
            Some(endian) => Arc::clone(
                this.alternate_plan
                    .get_or_init(|| this.plan.with_endian(endian)),
            ),
        };
        Ok((plan, Arc::clone(&this.runtime)))
    }
}

#[pymethods]
impl StructType {
    /// Expose Python objects retained inside the shared runtime to cyclic GC.
    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        self.runtime.traverse(&visit)
    }

    /// Decode one record.
    ///
    /// Upstream reaches the equivalent point through `__call__` → `_read` → each
    /// field's own `_read`. Here the sizes are already on the plan, so this is
    /// one call.
    ///
    /// `endian` mirrors the generated class's per-call byte-order override.
    #[pyo3(signature = (source, /, *, endian = None))]
    fn __call__<'py>(
        slf: &Bound<'py, Self>,
        source: &Bound<'py, PyAny>,
        endian: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let (plan, runtime) = Self::parts_for_endian(slf, endian)?;
        decode_one(slf.py(), &plan, &runtime, source)
    }

    /// Decode from a `bytes`-like object.
    #[pyo3(signature = (data, /, *, endian = None))]
    fn reads<'py>(
        slf: &Bound<'py, Self>,
        data: &Bound<'py, PyAny>,
        endian: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        Self::__call__(slf, data, endian)
    }

    /// Decode from a stream or a buffer.
    #[pyo3(signature = (obj, /, *, endian = None))]
    fn read<'py>(
        slf: &Bound<'py, Self>,
        obj: &Bound<'py, PyAny>,
        endian: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        Self::__call__(slf, obj, endian)
    }

    /// Fixed size in bytes, or `None` for a type whose size depends on content.
    #[getter]
    fn size(&self) -> Option<u32> {
        self.plan.total_size
    }

    /// True when the record's size depends on its content.
    #[getter]
    fn dynamic(&self) -> bool {
        self.plan.total_size.is_none()
    }

    /// The type's name.
    #[getter]
    fn __name__(&self) -> &str {
        &self.plan.name
    }

    /// Owning namespace, retained by the compiled runtime.
    #[getter]
    fn __cs__(&self, py: Python<'_>) -> Option<Py<PyAny>> {
        self.runtime
            .namespace
            .as_ref()
            .map(|namespace| namespace.clone_ref(py))
    }

    /// Fields keyed by the names a record answers to. See [`fields_mapping`].
    #[getter]
    fn fields<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyDict>> {
        let (plan, runtime) = Self::parts(slf);
        fields_mapping(slf.py(), &plan, &runtime)
    }

    /// Sizes of fields whose width is known when the type is compiled.
    ///
    /// This is the same dictionary the generated class and every decoded record
    /// expose, matching upstream's class-level `__static_sizes__` cache.
    #[getter]
    fn __static_sizes__(&self, py: Python<'_>) -> Py<PyDict> {
        self.runtime.static_sizes.clone_ref(py)
    }

    /// Stable logical identity matching `Record._plan_id`.
    #[getter]
    fn _plan_id(&self) -> usize {
        Arc::as_ptr(&self.identity) as usize
    }

    /// Whether this type qualifies for a fast path.
    ///
    /// Exposed so a test can assert the fast path was *selected*. Behaviour alone
    /// cannot show that: the general path produces identical records, so a suite
    /// that only checks values stays green if the shortcut is lost.
    #[getter]
    fn __fast_path__(&self) -> bool {
        self.plan.fast_path_eligible()
    }

    /// Whether this type was declared as a `union`.
    ///
    /// Union members overlay at offset zero, which the decoder already handles.
    /// This getter is what lets the *Python* class carry the distinction too, so
    /// `issubclass(t, Union)` can be true.
    #[getter]
    fn __is_union__(&self) -> bool {
        self.plan.is_union
    }

    /// A type's size in bytes.
    ///
    /// Callers use `len(cs.TYPE)` where they need a declared size, typically to
    /// fall back on it when a size read from the data is zero.
    fn __len__(&self) -> PyResult<usize> {
        match self.plan.total_size {
            Some(size) => Ok(size as usize),
            None => Err(pyo3::exceptions::PyTypeError::new_err(format!(
                "{} has no fixed size, so len() is undefined",
                self.plan.name
            ))),
        }
    }

    /// Alignment, in bytes, or `None` when the type is not aligned.
    ///
    /// Upstream reports `None` for a type that was not loaded with `align=True`,
    /// and the alignment itself for one that was. The compiled plan carries both
    /// facts, so this reports them rather than guessing.
    #[getter]
    fn alignment(&self) -> Option<u32> {
        if self.plan.aligned {
            Some(self.plan.alignment)
        } else {
            None
        }
    }

    /// Whether this type was declared under natural alignment.
    ///
    /// A **bool**, not the alignment. The two dunders are easy to mix up:
    /// upstream's `__align__` answers "was this loaded with `align=True`" and
    /// `__alignment__` gives the number of bytes. Returning the number here would
    /// make `if t.__align__:` true for every aligned type *and* for an unaligned
    /// one that happened to report a non-zero alignment — the wrong question
    /// answered plausibly.
    #[getter]
    fn __align__(&self) -> bool {
        self.plan.aligned
    }

    /// Alignment in bytes -- upstream's spelling of [`Self::alignment`].
    ///
    /// This is the type's natural alignment even when aligned layout was not
    /// requested. Upstream separates placement policy (`__align__`) from the
    /// largest member boundary (`__alignment__`); a packed struct containing a
    /// `uint32` therefore reports four here while still placing fields tightly.
    #[getter]
    fn __alignment__(&self) -> u32 {
        self.plan.alignment.max(1)
    }

    /// Field descriptors **in declaration order**, as a list.
    ///
    /// A list, not a mapping. Upstream's `__fields__` is the ordered sequence its
    /// compiler and its parser both iterate (`for field in structure.__fields__`,
    /// `type.__fields__.extend(fields)`), and `type.__fields__[0]` is how a caller
    /// asks for the first field. Against a dict that is a `KeyError: 0`. The
    /// name-keyed mapping is `__lookup__`, below.
    #[getter]
    fn __fields__<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyList>> {
        // `lookup_mapping` inserts in plan order, and a Python dict preserves
        // insertion order, so its values are the declaration order.
        let mapping = Self::lookup(slf)?;
        PyList::new(slf.py(), mapping.values())
    }

    /// Fields keyed by name -- upstream's spelling of [`Self::lookup`].
    #[getter]
    fn __lookup__<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyDict>> {
        Self::lookup(slf)
    }

    /// Pointer members, mapped to the name of the type each addresses.
    ///
    /// `struct S { uint32 *p; test **q; }` reports `{"p": "uint32", "q": "test*"}`.
    /// Empty for a type with no pointers, which is nearly all of them.
    ///
    /// This is the *declared* target type, which is what a caller needs to know
    /// what a pointer addresses — an address alone is enough to decode a pointer
    /// and not enough to follow it.
    #[getter]
    fn __pointers__<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyDict>> {
        let plan = Arc::clone(&slf.borrow().plan);
        let out = PyDict::new(slf.py());
        for (index, target) in plan.pointer_targets.iter().enumerate() {
            let (Some(target), Some(name)) = (target, plan.names.get(index)) else {
                continue;
            };
            if !name.is_empty() {
                out.set_item(name.as_ref(), target.as_ref())?;
            }
        }
        Ok(out)
    }

    /// `cs.TYPE.lookup` -- fields as declared, keyed by name.
    #[getter]
    fn lookup<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyDict>> {
        let (plan, runtime) = Self::parts(slf);
        lookup_mapping(slf.py(), &plan, &runtime)
    }

    /// `cs.TYPE.dumps(record)` -- upstream exposes encoding on the type too.
    #[pyo3(signature = (value, /, *, endian = None))]
    fn dumps<'py>(
        slf: &Bound<'py, Self>,
        value: &Bound<'py, PyAny>,
        endian: Option<&str>,
    ) -> PyResult<Bound<'py, pyo3::types::PyBytes>> {
        let record = value
            .cast::<Record>()
            .map_err(|_| internal("dumps expects a record of this type"))?;
        let order = endian.map(crate::scalars::parse_endian).transpose()?;
        let raw = record_to_bytes_with_endian(record, order)?;
        Ok(pyo3::types::PyBytes::new(slf.py(), &raw))
    }

    /// ``cs.TYPE.write(stream, record)`` -- encode through the checked record
    /// serializer and return the stream's reported byte count.
    #[pyo3(signature = (stream, value, /, *, endian = None))]
    fn write<'py>(
        _slf: &Bound<'py, Self>,
        stream: &Bound<'py, PyAny>,
        value: &Bound<'py, PyAny>,
        endian: Option<&str>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let record = value
            .cast::<Record>()
            .map_err(|_| internal("write expects a record of this type"))?;
        let order = endian.map(crate::scalars::parse_endian).transpose()?;
        write_record_to_stream(record, stream, order)
    }

    fn __repr__(&self) -> String {
        format!("<StructType {}>", self.plan.name)
    }
}

/// `__new__` for a generated struct class. See the module header on why.
///
/// Two shapes, decided by whether a source was given:
///
/// * `cs.X(buf_or_stream)` decodes a record;
/// * `cs.X(field=value, ...)` builds one from values, decoding nothing.
#[pyfunction]
#[pyo3(signature = (cls, *args, **kwargs))]
pub fn struct_new<'py>(
    cls: &Bound<'py, PyType>,
    args: &Bound<'py, PyTuple>,
    kwargs: Option<&Bound<'py, PyDict>>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = cls.py();
    let holder = cls
        .getattr(pyo3::intern!(py, "_cstruct_type_"))
        .map_err(|_| internal("struct class is missing its compiled type"))?;
    let struct_type = holder
        .cast::<StructType>()
        .map_err(|_| internal("_cstruct_type_ is not a StructType"))?;
    let source_kind = if args.len() == 1 {
        input::classify(&args.get_item(0)?).ok()
    } else {
        None
    };
    if let Some(source_kind) = source_kind {
        let source = args.get_item(0)?;
        // Every keyword other than `endian` is tolerated and ignored. Current upstream source accepts
        // `cs.S(buf, anything=1)` without complaint, so tightening that would
        // be a deliberate compatibility difference.
        let requested = match kwargs {
            Some(values) => values.get_item(pyo3::intern!(py, "endian"))?,
            None => None,
        };
        let (plan, runtime) = StructType::parts_for_endian(struct_type, requested.as_ref())?;
        return decode_one_classified(py, &plan, &runtime, &source, source_kind);
    }

    let (plan, runtime) = StructType::parts(struct_type);
    build_from_values(py, &plan, &runtime, args, kwargs)
}

/// Build a record from positional and keyword values rather than from bytes.
///
/// `CIEXYZ(ciexyzX=0, ciexyzY=0, ciexyzZ=0)` — `dissect.target`'s rdpcache plugin
/// constructs an empty colour space this way at import time, and refusing removed
/// the plugin. Nothing is decoded: the values are taken as given, and a field
/// nobody named gets its type's zero, which is what upstream leaves in an unset
/// field.
///
/// A keyword that names no field is a `TypeError`, so a misspelled field name
/// does not silently vanish.
fn build_from_values<'py>(
    py: Python<'py>,
    plan: &Arc<CompiledPlan>,
    runtime: &Arc<PlanRuntime>,
    args: &Bound<'py, PyTuple>,
    kwargs: Option<&Bound<'py, PyDict>>,
) -> PyResult<Bound<'py, PyAny>> {
    if args.len() > plan.fields.len() {
        return Err(pyo3::exceptions::PyTypeError::new_err(format!(
            "{}() takes at most {} positional arguments ({} given)",
            plan.name,
            plan.fields.len(),
            args.len()
        )));
    }

    let mut values = Vec::with_capacity(plan.fields.len());
    for index in 0..plan.fields.len() {
        let positional = if index < args.len() {
            Some(args.get_item(index)?)
        } else {
            None
        };
        if positional.is_some() {
            if let (Some(kwargs), Some(name)) = (kwargs, declared_field_name(plan, index)) {
                if kwargs.contains(name)? {
                    return Err(pyo3::exceptions::PyTypeError::new_err(format!(
                        "{}() got multiple values for argument '{name}'",
                        plan.name
                    )));
                }
            }
        }
        match positional.filter(|value| !value.is_none()) {
            Some(value) => values.push(value.unbind()),
            None => values.push(default_value(py, plan, runtime, index)?.unbind()),
        }
    }

    if let Some(kwargs) = kwargs {
        reject_unknown_keyword(plan, kwargs)?;
    }

    let record = Record::new(
        Arc::clone(plan),
        Arc::clone(runtime),
        values,
        plan.total_size.unwrap_or(0),
        None,
        args.is_empty() && !matches!(kwargs, Some(kwargs) if !kwargs.is_empty()),
    );
    let record = Bound::new(py, record)?;
    if plan.is_union {
        let selected = if !args.is_empty() {
            Some((0usize, args.get_item(0)?))
        } else {
            match kwargs.and_then(|values| values.iter().next()) {
                Some((key, value)) => {
                    let key: String = key.extract()?;
                    let path = plan
                        .index_of
                        .get(key.as_str())
                        .ok_or_else(|| internal("validated union keyword has no field path"))?;
                    match path.as_ref() {
                        [index] => Some((*index as usize, value)),
                        _ => {
                            return Err(pyo3::exceptions::PyNotImplementedError::new_err(
                                "Initializing a folded union member is not yet supported",
                            ))
                        }
                    }
                }
                None => None,
            }
        };
        if let Some((index, selected_value)) = selected {
            if plan.total_size.is_none() {
                return Err(pyo3::exceptions::PyNotImplementedError::new_err(
                    "Initializing a dynamic union is not yet supported",
                ));
            }
            let value = if selected_value.is_none() {
                Record::field_default_at(&record, index)?
                    .ok_or_else(|| internal("union member index is out of range"))?
            } else {
                selected_value
            };
            let raw = encode_union_member(py, plan, index, &value, plan.endian)?;
            let source = PyBytes::new(py, &raw).into_any();
            return decode_one(py, plan, runtime, &source);
        }
        return Ok(record.into_any());
    }

    if let Some(kwargs) = kwargs {
        for (key, value) in kwargs.iter() {
            if value.is_none() {
                continue;
            }
            Record::assign_field(&record, key.extract::<&str>()?, &value)?;
        }
    }
    Ok(record.into_any())
}

/// Raise for the first keyword that does not name a declared field.
fn reject_unknown_keyword(plan: &CompiledPlan, kwargs: &Bound<'_, PyDict>) -> PyResult<()> {
    for (key, _) in kwargs.iter() {
        let key: String = key.extract()?;
        let direct = plan.names.iter().any(|candidate| candidate.as_ref() == key);
        if !direct && !plan.index_of.contains_key(key.as_str()) {
            return Err(pyo3::exceptions::PyTypeError::new_err(format!(
                "{} has no field '{key}'",
                plan.name
            )));
        }
    }
    Ok(())
}

/// The value an unset field takes when a record is built from keywords.
///
/// The type's zero: empty bytes of the declared width for `char`, an empty
/// string for `wchar`, an empty list for an array, `0.0` for a float, and `0`
/// for everything else.
fn default_value<'py>(
    py: Python<'py>,
    plan: &Arc<CompiledPlan>,
    runtime: &Arc<PlanRuntime>,
    index: usize,
) -> PyResult<Bound<'py, PyAny>> {
    let Some(field) = plan.fields.get(index) else {
        return Ok(py.None().into_bound(py));
    };
    field_type_object(py, plan, runtime, index, field)?
        .bind(py)
        .call_method0(pyo3::intern!(py, "__default__"))
}
