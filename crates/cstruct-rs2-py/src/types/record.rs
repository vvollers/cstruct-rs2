//! [`Record`]: one decoded structure instance.
//!
//! # A record is not an instance of its type
//!
//! This is the one structural difference from upstream worth knowing before
//! reading anything else here.
//!
//! Upstream's records *are* instances of the class it generates per structure,
//! so they inherit everything that class defines. Here there is a single
//! `Record` type for every structure, carrying a reference to its compiled plan
//! and its decoded values.
//!
//! `isinstance(record, cs.MY_STRUCT)` still answers correctly — the generated
//! class compares the namespace-owned logical identity shared by the type and
//! its records — but nothing is *inherited*. So anything a record would have
//! got from its class has to be provided on the record itself, which is why
//! `fields`, `dumps` and the rest are implemented here.
//!
//! # Values are built when the record is decoded
//!
//! A record holds Python objects, not bytes. Deferring construction until first
//! access is possible and would skip the fields nobody reads, but it would mean
//! keeping the record's source buffer alive for as long as the record — and a
//! record outlives the 4 KiB block it came from far more often than it goes
//! unread.
//!
//! One consequence: field access is a vector lookup, not a decode. That is why
//! `__getattribute__` can afford to be the entry point at all.

use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString, PyTuple, PyType};

use cstruct_rs2_core::plan::CompiledPlan;

use crate::errors::internal;
use crate::types::decoding::decode_one;
use crate::types::encode::encode_union_member;
use crate::types::encode::{record_to_bytes_with_endian, write_record_to_stream};
use crate::types::field::field_type_object;
use crate::types::field::{fields_mapping, lookup_mapping, visible_member_names};
use crate::types::runtime::PlanRuntime;

/// A decoded record.
///
/// Field access goes through `__getattribute__`, which means one Rust call per
/// read and no name resolution beyond the index the plan already holds.
///
/// Note `__getattribute__` and not `__getattr__`. The latter is a *fallback*,
/// consulted only after ordinary attribute lookup has already failed and
/// raised — so every field read would cost an exception raised and caught.
#[pyclass(name = "Record", module = "dissect.cstruct")]
pub struct Record {
    plan: Arc<CompiledPlan>,
    /// The Python classes this record's fields resolve to.
    ///
    /// Needed so `record.fields` can report each field's *type* — see the module
    /// docs on why a record cannot inherit that from its class.
    runtime: Arc<PlanRuntime>,
    values: Vec<Py<PyAny>>,
    consumed: u32,
    /// Per-field byte spans observed while decoding.
    ///
    /// `None` is meaningful: upstream does not install `__dynamic_sizes__` on
    /// a record built from supplied keyword values because no read occurred.
    /// Decoded and no-argument default records expose a dictionary. An empty
    /// one is created on first access so ordinary fixed-record decoding does
    /// not allocate metadata that almost every consumer ignores.
    dynamic_sizes: Option<Py<PyDict>>,
    /// Whether `dynamic_sizes == None` means "defer an empty dictionary" or
    /// "the attribute is absent because no read occurred".
    exposes_dynamic_sizes: bool,
}

impl Record {
    /// Assemble a record from an already-decoded value vector.
    ///
    /// The only constructor. `values` must have one entry per plan field, in
    /// plan order, because every lookup here indexes it by field index —
    /// `consumed` is what the record occupied in the source, which for a dynamic
    /// type is not the type's declared size.
    pub(crate) fn new(
        plan: Arc<CompiledPlan>,
        runtime: Arc<PlanRuntime>,
        values: Vec<Py<PyAny>>,
        consumed: u32,
        dynamic_sizes: Option<Py<PyDict>>,
        exposes_dynamic_sizes: bool,
    ) -> Self {
        Self {
            plan,
            runtime,
            values,
            consumed,
            dynamic_sizes,
            exposes_dynamic_sizes,
        }
    }

    /// Return the stable per-record dynamic-size map, creating an empty one.
    fn ensure_dynamic_sizes(&mut self, py: Python<'_>) -> PyResult<&Py<PyDict>> {
        if !self.exposes_dynamic_sizes {
            return Err(pyo3::exceptions::PyAttributeError::new_err(format!(
                "'{}' object has no attribute '__dynamic_sizes__'",
                self.plan.name
            )));
        }
        Ok(self
            .dynamic_sizes
            .get_or_insert_with(|| PyDict::new(py).unbind()))
    }

    /// Return the current compiled generation for this logical type.
    ///
    /// Upstream's records keep their generated class, and that class consults
    /// the namespace's current byte order when it writes. A record decoded
    /// before `cs.endian` changes therefore writes in the new order too.
    pub(crate) fn encoding_plan(slf: &Bound<'_, Self>) -> Arc<CompiledPlan> {
        slf.borrow().runtime.identity.current_plan()
    }

    /// How many bytes this record occupied.
    pub(crate) fn consumed_bytes(&self) -> u32 {
        self.consumed
    }

    /// One field's value by index.
    ///
    /// A lookup, not a read: values were built when the record was decoded. See
    /// the module docs on why.
    ///
    /// `None` means there is no such index, which callers treat as "skip this
    /// field" rather than as an error — a plan and a value vector always agree
    /// in length, so it cannot happen for a record this crate built.
    pub(crate) fn field_value_at<'py>(
        slf: &Bound<'py, Self>,
        index: usize,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let py = slf.py();
        let record = slf.borrow();
        Ok(record.values.get(index).map(|value| value.bind(py).clone()))
    }

    /// Default for one field's declared type, used when a caller assigns
    /// ``None`` and later writes the record.
    pub(crate) fn field_default_at<'py>(
        slf: &Bound<'py, Self>,
        index: usize,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let (plan, runtime) = {
            let record = slf.borrow();
            (Arc::clone(&record.plan), Arc::clone(&record.runtime))
        };
        let Some(field) = plan.fields.get(index) else {
            return Ok(None);
        };
        let type_object = field_type_object(slf.py(), &plan, &runtime, index, field)?;
        Ok(Some(
            type_object
                .bind(slf.py())
                .call_method0(pyo3::intern!(slf.py(), "__default__"))?,
        ))
    }

    /// Resolve a field by name, stepping through anonymous members.
    fn lookup<'py>(slf: &Bound<'py, Self>, name: &str) -> PyResult<Option<Bound<'py, PyAny>>> {
        let path = {
            let record = slf.borrow();
            match record.plan.index_of.get(name) {
                Some(path) => path.clone(),
                None => return Ok(None),
            }
        };
        let mut current = slf.clone().into_any();
        for step in path.iter() {
            let next = {
                let Ok(record) = current.cast::<Self>() else {
                    return Ok(None);
                };
                let borrowed = record.borrow();
                match borrowed.values.get(*step as usize) {
                    Some(value) => value.clone_ref(slf.py()),
                    None => return Ok(None),
                }
            };
            current = next.into_bound(slf.py());
        }
        Ok(Some(current))
    }

    /// Assign a declared field, including one folded from an anonymous member.
    ///
    /// Value construction uses this after building nested defaults. Keeping the
    /// helper outside ``#[pymethods]`` avoids adding another public method to
    /// every record; Python reaches it through ``__setattr__`` and
    /// ``__setitem__`` only.
    pub(crate) fn assign_field(
        slf: &Bound<'_, Self>,
        name: &str,
        value: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let path = {
            let record = slf.borrow();
            match record.plan.index_of.get(name) {
                Some(path) => path.clone(),
                None => {
                    return Err(pyo3::exceptions::PyAttributeError::new_err(format!(
                        "{} has no field '{name}'",
                        record.plan.name
                    )))
                }
            }
        };

        let (last, prefix) = path.split_last().expect("index paths are never empty");
        let mut target = slf.clone().into_any();
        for step in prefix {
            let next = {
                let record = target.cast::<Self>().map_err(|_| {
                    internal("an index path stepped through something that is not a record")
                })?;
                let borrowed = record.borrow();
                borrowed
                    .values
                    .get(*step as usize)
                    .map(|value| value.clone_ref(slf.py()))
                    .ok_or_else(|| internal("index path is out of range"))?
            };
            target = next.into_bound(slf.py());
        }

        let record = target.cast::<Self>().map_err(|_| {
            internal("an index path stepped through something that is not a record")
        })?;

        // A static union has one shared byte representation. Rebuild only the
        // member the caller assigned, then decode that small buffer so every
        // overlapping scalar view changes together. This runs on assignment,
        // never while an ordinary record is read.
        let union_parts = {
            let borrowed = record.borrow();
            borrowed
                .plan
                .is_union
                .then(|| (Arc::clone(&borrowed.plan), Arc::clone(&borrowed.runtime)))
        };
        if let Some((plan, runtime)) = union_parts {
            let replacement = if value.is_none() {
                Self::field_default_at(record, *last as usize)?
                    .ok_or_else(|| internal("union member index is out of range"))?
            } else {
                value.clone()
            };
            let raw =
                encode_union_member(slf.py(), &plan, *last as usize, &replacement, plan.endian)?;
            let source = pyo3::types::PyBytes::new(slf.py(), &raw).into_any();
            let decoded = decode_one(slf.py(), &plan, &runtime, &source)?;
            let decoded = decoded
                .cast::<Self>()
                .map_err(|_| internal("union refresh did not produce a record"))?;
            let (values, consumed, dynamic_sizes, exposes_dynamic_sizes) = {
                let decoded = decoded.borrow();
                (
                    decoded
                        .values
                        .iter()
                        .map(|item| item.clone_ref(slf.py()))
                        .collect(),
                    decoded.consumed,
                    decoded
                        .dynamic_sizes
                        .as_ref()
                        .map(|sizes| sizes.clone_ref(slf.py())),
                    decoded.exposes_dynamic_sizes,
                )
            };
            let mut destination = record.borrow_mut();
            destination.values = values;
            destination.consumed = consumed;
            destination.dynamic_sizes = dynamic_sizes;
            destination.exposes_dynamic_sizes = exposes_dynamic_sizes;
            return Ok(());
        }

        let mut borrowed = record.borrow_mut();
        let slot = borrowed
            .values
            .get_mut(*last as usize)
            .ok_or_else(|| internal("index path is out of range"))?;
        *slot = value.clone().unbind();
        Ok(())
    }
}

#[pymethods]
impl Record {
    /// `len(record)` -- the record's size in bytes.
    ///
    /// `dissect.thumbcache` measures entries this way. The value is what the
    /// record actually consumed, which for a dynamic type is not its type's
    /// size and for a fixed one is.
    fn __len__(&self) -> usize {
        self.consumed as usize
    }

    /// `record["field"]` -- records are subscriptable by field name.
    ///
    /// Upstream's own `__repr__` reads fields this way, and callers forward
    /// unknown attributes to a record with `record[name]`. A `KeyError` for a
    /// missing name is what that forwarding expects to catch.
    fn __getitem__<'py>(slf: &Bound<'py, Self>, name: &str) -> PyResult<Bound<'py, PyAny>> {
        match Self::lookup(slf, name)? {
            Some(value) => Ok(value),
            None => Err(pyo3::exceptions::PyKeyError::new_err(name.to_owned())),
        }
    }

    /// `record["field"] = value`, the assignment counterpart.
    fn __setitem__(slf: &Bound<'_, Self>, name: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
        Self::assign_field(slf, name, value)
    }

    fn __contains__(slf: &Bound<'_, Self>, name: &str) -> bool {
        slf.borrow().plan.index_of.contains_key(name)
    }

    /// A live mapping of public field names to values.
    ///
    /// Upstream's `StructureValuesProxy` writes through with `getattr` and
    /// `setattr`. `_RecordValues` provides the same mapping contract around the
    /// native record and uses this plan's folded member order.
    #[getter]
    fn __values__<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        py.import(pyo3::intern!(py, "dissect.cstruct._struct_types"))?
            .getattr(pyo3::intern!(py, "_RecordValues"))?
            .call1((slf,))
    }

    /// Static field sizes shared with the generated type object.
    #[getter]
    fn __static_sizes__(&self, py: Python<'_>) -> Py<PyDict> {
        self.runtime.static_sizes.clone_ref(py)
    }

    /// Dynamic field sizes observed for this particular record.
    #[getter]
    fn __dynamic_sizes__(mut slf: PyRefMut<'_, Self>, py: Python<'_>) -> PyResult<Py<PyDict>> {
        Ok(slf.ensure_dynamic_sizes(py)?.clone_ref(py))
    }

    /// Static and per-record dynamic sizes as upstream's `ChainMap` view.
    #[getter]
    fn __sizes__<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let (static_sizes, dynamic_sizes) = {
            let mut record = slf.borrow_mut();
            let static_sizes = record.runtime.static_sizes.clone_ref(py);
            let dynamic_sizes = record.ensure_dynamic_sizes(py)?.clone_ref(py);
            (static_sizes, dynamic_sizes)
        };
        py.import(pyo3::intern!(py, "collections"))?
            .getattr(pyo3::intern!(py, "ChainMap"))?
            .call1((static_sizes, dynamic_sizes))
    }

    /// `record.fields` -- the declared fields, as upstream exposes them.
    ///
    /// Provided on the record because a record does not inherit it from its
    /// class — see the module docs.
    #[getter]
    fn fields<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyDict>> {
        let (plan, runtime) = {
            let record = slf.borrow();
            (Arc::clone(&record.plan), Arc::clone(&record.runtime))
        };
        fields_mapping(slf.py(), &plan, &runtime)
    }

    /// Canonical member mapping inherited by upstream's Python record class.
    #[getter]
    fn __members__<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyDict>> {
        Self::fields(slf)
    }

    /// Raw declared-field mapping inherited by upstream's Python record class.
    #[getter]
    fn __lookup__<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyDict>> {
        let (plan, runtime) = {
            let record = slf.borrow();
            (Arc::clone(&record.plan), Arc::clone(&record.runtime))
        };
        lookup_mapping(slf.py(), &plan, &runtime)
    }

    /// `record.field = value` -- records are mutable.
    ///
    /// Callers do this — replacing a raw field with a parsed one, for instance
    /// — so a read-only record would not be a drop-in for upstream's.
    ///
    /// Only declared fields are settable, and the value goes into the record's
    /// value vector rather than into an instance dict -- which is why `dumps()`
    /// reflects the change and why a record still carries no `__dict__`.
    fn __setattr__(slf: &Bound<'_, Self>, name: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
        Self::assign_field(slf, name, value)
    }

    /// `record.dumps()` -- the record's bytes.
    ///
    /// See [`crate::types::encode`] on why this matters to readers as well as
    /// writers.
    #[pyo3(name = "dumps")]
    #[pyo3(signature = (*, endian = None))]
    fn dumps_py<'py>(
        slf: &Bound<'py, Self>,
        endian: Option<&str>,
    ) -> PyResult<Bound<'py, pyo3::types::PyBytes>> {
        let order = endian.map(crate::scalars::parse_endian).transpose()?;
        let raw = record_to_bytes_with_endian(slf, order)?;
        Ok(pyo3::types::PyBytes::new(slf.py(), &raw))
    }

    /// ``record.write(stream)`` -- encode through the same checked serializer
    /// as ``dumps`` and return the sink's reported count.
    #[pyo3(signature = (stream, /, *, endian = None))]
    fn write<'py>(
        slf: &Bound<'py, Self>,
        stream: &Bound<'py, PyAny>,
        endian: Option<&str>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let order = endian.map(crate::scalars::parse_endian).transpose()?;
        write_record_to_stream(slf, stream, order)
    }

    /// `bytes(record)` is the same serialization as `record.dumps()`.
    fn __bytes__<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, pyo3::types::PyBytes>> {
        Self::dumps_py(slf, None)
    }

    /// A record is true when at least one public member value is true.
    ///
    /// Public matters for anonymous structures: upstream generates this method
    /// from `__members__`, which contains the folded fields and not the hidden
    /// anonymous container record.
    fn __bool__(slf: &Bound<'_, Self>) -> PyResult<bool> {
        let plan = Arc::clone(&slf.borrow().plan);
        for name in visible_member_names(&plan) {
            if let Some(value) = Self::lookup(slf, &name)? {
                if value.is_truthy()? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Whether this record came from a union plan.
    ///
    /// The Python `Union` marker uses this only for an explicit
    /// `isinstance(record, Union)` check. Ordinary field access and record
    /// construction never consult it.
    #[getter]
    fn _is_union(&self) -> bool {
        self.plan.is_union
    }

    /// Field access.
    ///
    /// Falls through to *generic* attribute lookup for everything that is not a
    /// field, so methods, `_consumed`, and the dunders Python itself needs all
    /// still work.
    ///
    /// The fallback must be `PyObject_GenericGetAttr`, not `getattr`: the latter
    /// re-enters this method and recurses until the native stack is exhausted.
    fn __getattribute__<'py>(
        slf: &Bound<'py, Self>,
        name: &Bound<'py, PyString>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        // `to_str` borrows the interned UTF-8 CPython already holds, so the
        // common path does not allocate.
        let path = match name.to_str() {
            Ok(text) => slf.borrow().plan.field_path(text).map(<[u32]>::to_vec),
            Err(_) => None,
        };
        if let Some(path) = path {
            // One step for a directly declared field, which is almost all of
            // them; more only when the name was folded in from an anonymous
            // member.
            if let [only] = path.as_slice() {
                if let Some(value) = Record::field_value_at(slf, *only as usize)? {
                    return Ok(value);
                }
            } else if let Some(value) = follow_path(slf, &path)? {
                return Ok(value);
            }
        }

        // SAFETY: `slf` and `name` are live, non-null, correctly typed Python
        // objects for as long as their `Bound` handles exist, which spans this
        // call. `PyObject_GenericGetAttr` returns either a new strong reference
        // or NULL with the error indicator set; both are handled below, so no
        // reference is leaked and no null is dereferenced.
        let ptr = unsafe { pyo3::ffi::PyObject_GenericGetAttr(slf.as_ptr(), name.as_ptr()) };
        if ptr.is_null() {
            return Err(PyErr::fetch(py));
        }
        // SAFETY: `ptr` is non-null and is an owned reference returned by
        // CPython, which is exactly what `from_owned_ptr` requires.
        Ok(unsafe { Bound::from_owned_ptr(py, ptr) })
    }

    /// Bytes this record consumed.
    ///
    /// How many bytes this record occupied. For a dynamic type this is not the
    /// type's declared size, and callers use it to advance to the next record.
    #[getter]
    fn _consumed(&self) -> u32 {
        self.consumed
    }

    /// The record's type name.
    #[getter]
    fn __typename__(&self) -> &str {
        &self.plan.name
    }

    /// Stable logical identity of the type that produced this record.
    ///
    /// Used by the struct class's `__instancecheck__` to recognise its own
    /// records exactly. Comparing names would confuse two namespaces that
    /// declare the same type name. This identity survives byte-order plan
    /// replacement, just as upstream keeps its generated class.
    #[getter]
    fn _plan_id(&self) -> usize {
        Arc::as_ptr(&self.runtime.identity) as usize
    }

    /// Public member names in upstream's declaration/folding order.
    ///
    /// Private to the compatibility proxy in `_struct_types`; records cannot
    /// inherit their generated class's `__members__` mapping.
    #[getter]
    fn _member_names<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let names = visible_member_names(&self.plan);
        PyTuple::new(py, names.iter().map(|name| name.as_ref()))
    }

    fn __repr__(slf: &Bound<'_, Self>) -> PyResult<String> {
        let (name, names) = {
            let record = slf.borrow();
            (record.plan.name.to_string(), record.plan.names.clone())
        };
        let mut out = format!("<{name} ");
        for (index, field_name) in names.iter().enumerate() {
            if field_name.is_empty() {
                continue;
            }
            if index > 0 {
                out.push(' ');
            }
            let value = match Record::field_value_at(slf, index)? {
                Some(value) => render_field(&value)?,
                None => "<unavailable>".to_owned(),
            };
            out.push_str(&format!("{field_name}={value}"));
        }
        out.push('>');
        Ok(out)
    }

    /// Hash the visible field values, exactly as upstream's generated method.
    ///
    /// Both are needed together: defining `__eq__` without `__hash__` makes a
    /// type unhashable in Python, and upstream's records can be put in a set.
    /// Note that a record holding an array field is unhashable either way,
    /// because the field's value is a list — upstream behaves the same.
    ///
    /// Type identity is deliberately absent. Python only requires equal objects
    /// to share a hash, and upstream permits different record types with the
    /// same values to collide.
    fn __hash__(slf: &Bound<'_, Self>) -> PyResult<isize> {
        let py = slf.py();
        let plan = Arc::clone(&slf.borrow().plan);
        let tuple = PyTuple::new(
            py,
            visible_member_names(&plan)
                .iter()
                .map(|name| Record::lookup(slf, name))
                .collect::<PyResult<Vec<_>>>()?,
        )?;
        tuple.hash()
    }

    fn __eq__(slf: &Bound<'_, Self>, other: &Bound<'_, PyAny>) -> PyResult<bool> {
        let Ok(other) = other.cast::<Record>() else {
            return Ok(false);
        };
        let (plan, runtime) = {
            let record = slf.borrow();
            (Arc::clone(&record.plan), Arc::clone(&record.runtime))
        };
        if !Arc::ptr_eq(&runtime.identity, &other.borrow().runtime.identity) {
            return Ok(false);
        }
        for name in visible_member_names(&plan) {
            let a = Record::lookup(slf, &name)?;
            let b = Record::lookup(other, &name)?;
            match (a, b) {
                (Some(a), Some(b)) => {
                    if !a.eq(&b)? {
                        return Ok(false);
                    }
                }
                (None, None) => {}
                _ => return Ok(false),
            }
        }
        Ok(true)
    }
}

/// Render one field value the way upstream's `__repr__` does.
///
/// Upstream prints a plain integer field in hex and everything else with
/// `repr`, so enums and floats keep their own formatting. The difference is
/// visible wherever a record is logged, which makes it contract rather than
/// cosmetics.
fn render_field(value: &Bound<'_, PyAny>) -> PyResult<String> {
    let py = value.py();
    // A `bool` is an `int` subclass and an enum member is too, so "plain int"
    // has to be checked by exact type name rather than by `isinstance`.
    let plain_int = value.is_instance_of::<pyo3::types::PyInt>()
        && !value.is_instance_of::<pyo3::types::PyBool>()
        && value.get_type().name()?.to_string_lossy() == "int";
    if !plain_int {
        return Ok(value.repr()?.to_string());
    }
    match value.extract::<i128>() {
        Ok(raw) if raw < 0 => Ok(format!("-0x{:x}", raw.unsigned_abs())),
        Ok(raw) => Ok(format!("0x{raw:x}")),
        // A `uint128` above `i128::MAX`, or a value a caller assigned by hand.
        // Python's own `#x` formatting has no width limit and spells negatives
        // `-0x…` exactly as the arms above do, so repr stays printable instead
        // of raising.
        Err(_) => Ok(value
            .call_method1(pyo3::intern!(py, "__format__"), ("#x",))?
            .to_string()),
    }
}

/// Walk a folded field path inward through nested records.
///
/// Only reached for a name that an anonymous member contributed, so it is off
/// the common path by construction.
fn follow_path<'py>(slf: &Bound<'py, Record>, path: &[u32]) -> PyResult<Option<Bound<'py, PyAny>>> {
    let Some((first, rest)) = path.split_first() else {
        return Ok(None);
    };
    let Some(mut current) = Record::field_value_at(slf, *first as usize)? else {
        return Ok(None);
    };
    for step in rest {
        let Ok(record) = current.cast::<Record>() else {
            return Ok(None);
        };
        match Record::field_value_at(record, *step as usize)? {
            Some(next) => current = next,
            None => return Ok(None),
        }
    }
    Ok(Some(current))
}

/// Build a `dict` of a record's fields, for tests and for `dumpstruct`.
///
/// Anonymous members are skipped: they have no name to key on, and their fields
/// are reachable under their own names on the parent.
#[pyfunction]
pub fn record_as_dict<'py>(record: &Bound<'py, Record>) -> PyResult<Bound<'py, PyDict>> {
    let py = record.py();
    let names = record.borrow().plan.names.clone();
    let out = PyDict::new(py);
    for (index, name) in names.iter().enumerate() {
        if name.is_empty() {
            continue;
        }
        if let Some(value) = Record::field_value_at(record, index)? {
            out.set_item(name.as_ref(), value)?;
        }
    }
    Ok(out)
}

/// Return direct field descriptors and values for the structure formatter.
///
/// Anonymous structures expose their child names through normal record lookup,
/// but the diagnostic view needs the hidden container itself. Keeping this
/// access on the private extension module avoids adding formatter-only
/// descriptors to every record's hot attribute table.
#[pyfunction(name = "_record_dump_fields")]
pub fn record_dump_fields<'py>(record: &Bound<'py, Record>) -> PyResult<Bound<'py, PyTuple>> {
    let py = record.py();
    let (plan, runtime, values) = {
        let record = record.borrow();
        (
            Arc::clone(&record.plan),
            Arc::clone(&record.runtime),
            record
                .values
                .iter()
                .map(|value| value.clone_ref(py))
                .collect::<Vec<_>>(),
        )
    };
    let mapping = lookup_mapping(py, &plan, &runtime)?;
    let fields = PyList::new(py, mapping.values())?;
    let values = PyTuple::new(py, values.iter().map(|value| value.bind(py)))?;
    PyTuple::new(py, [fields.into_any(), values.into_any()])
}

/// The Python type object for records, so tests can assert on it.
#[pyfunction]
pub fn record_type(py: Python<'_>) -> Bound<'_, PyType> {
    py.get_type::<Record>()
}
