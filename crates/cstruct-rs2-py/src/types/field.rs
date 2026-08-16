//! What a type says about its own fields: the `Field` object and the two
//! mappings that expose it.
//!
//! This is the introspection side of a compiled type. Nothing here runs while
//! decoding — a decode walks the plan directly, by index. These calls exist
//! because consumers ask a type about its shape: `dissect.target`'s DPAPI reader
//! slices a blob with `lookup["mkVersion"].offset`, and upstream's own parser
//! extends `type.__fields__`.
//!
//! # `fields` and `lookup` are two different questions
//!
//! Upstream exposes both, and they differ for one construct: an *anonymous*
//! member.
//!
//! ```text
//! struct Outer {
//!     struct { uint32 a; uint32 b; };   // anonymous
//!     uint32 c;
//! };
//! ```
//!
//! `fields` **flattens** that member, so it reports `a`, `b` and `c` — the names
//! a caller can actually read off a record. `lookup` keeps the member itself,
//! under the generated type name the parser gave it, and reports it alongside
//! `c`.
//!
//! Aliasing one to the other would be wrong in both directions, so they are
//! built separately: `fields` walks the plan's name index (which already
//! contains the folded names), and `lookup` walks the declared field list.

use std::collections::HashSet;
use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::pyclass::{PyTraverseError, PyVisit};
use pyo3::types::PyDict;

use cstruct_rs2_core::plan::{BitWindow, CompiledPlan, FieldOp, Op, DYNAMIC_COUNT, DYNAMIC_OFFSET};
use cstruct_rs2_core::schema::Scalar;

use crate::errors::internal;
use crate::types::runtime::PlanRuntime;
use crate::types::struct_type::StructType;

/// One declared field, as upstream's `fields` mapping exposes it.
///
/// Upstream's `fields` is a `dict` mapping name to `Field`, so this matches
/// that shape rather than a bare list of names — the two iterate the same but
/// behave differently for everything else a caller might do with a mapping.
#[pyclass(name = "Field", module = "dissect.cstruct")]
pub struct Field {
    #[pyo3(get)]
    name: String,
    /// The field's type object, resolved from the compiled plan.
    type_object: Py<PyAny>,
    bits: Option<u8>,
    /// Byte offset from the start of the record, or `None` when the field's
    /// position depends on content. `dissect.target`'s DPAPI reader slices a
    /// blob with `lookup["mkVersion"].offset`.
    offset: Option<u32>,
}

#[pymethods]
impl Field {
    /// Report the Python type retained by this field description to cyclic GC.
    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.type_object)
    }

    /// Build a field description directly, without a definition string.
    ///
    /// Upstream's tests construct these to drive `_make_union`/`_make_struct`,
    /// so the class needs a Python constructor as well as being something the
    /// compiler produces. A field built this way has no offset: it does not
    /// belong to a compiled layout until a type is made from it.
    #[new]
    #[pyo3(signature = (name, type_object, bits = None, offset = None))]
    fn new(name: String, type_object: Py<PyAny>, bits: Option<u8>, offset: Option<u32>) -> Self {
        Self {
            name,
            type_object,
            bits,
            offset,
        }
    }

    #[getter]
    fn r#type(&self, py: Python<'_>) -> Py<PyAny> {
        self.type_object.clone_ref(py)
    }

    #[getter]
    fn bits(&self) -> Option<u8> {
        self.bits
    }

    /// Name used internally for raw lookup. For ordinary fields it is the
    /// public name; anonymous members are built with their generated schema
    /// name in this implementation.
    #[getter]
    fn _name(&self) -> &str {
        &self.name
    }

    #[getter]
    fn offset(&self) -> Option<u32> {
        self.offset
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        let type_name = self
            .type_object
            .bind(py)
            .getattr(pyo3::intern!(py, "__name__"))
            .and_then(|name| name.extract::<String>())
            .unwrap_or_else(|_| "?".to_owned());
        Ok(match self.bits {
            Some(bits) => format!("<Field {} {type_name} : {bits}>", self.name),
            None => format!("<Field {} {type_name}>", self.name),
        })
    }
}

/// Build one `Field` for a plan entry, resolving its type object.
///
/// The two mappings below differ only in which name they file a field under, so
/// the construction itself is shared.
fn describe_field(
    py: Python<'_>,
    plan: &Arc<CompiledPlan>,
    runtime: &Arc<PlanRuntime>,
    field_index: usize,
    field: &FieldOp,
    window: BitWindow,
    name: String,
) -> PyResult<Py<Field>> {
    Py::new(
        py,
        Field {
            type_object: field_type_object(py, plan, runtime, field_index, field)?,
            bits: window.is_bitfield().then_some(window.width),
            // Upstream reports an offset only for the bitfield that opens a
            // shared storage unit. Continuing members occupy another window
            // in those same bytes, so exposing the opener's offset for each of
            // them would falsely imply that they begin a new field.
            offset: (field.offset != DYNAMIC_OFFSET
                && (!window.is_bitfield() || window.opens_unit))
                .then_some(field.offset),
            name,
        },
    )
}

/// Build the `fields` mapping for a compiled plan: names **as a record sees them**.
///
/// Built from the plan's name index rather than the declared name list, because
/// an anonymous member's fields are reachable by name on the parent and
/// upstream's `fields` includes them. Walking only the declared names would omit
/// exactly those.
pub fn fields_mapping<'py>(
    py: Python<'py>,
    plan: &Arc<CompiledPlan>,
    runtime: &Arc<PlanRuntime>,
) -> PyResult<Bound<'py, PyDict>> {
    let out = PyDict::new(py);

    for name in visible_member_names(plan) {
        let Some(path) = plan.index_of.get(&name) else {
            continue;
        };
        let Some(found) = resolve_path(plan, runtime, path) else {
            continue;
        };
        out.set_item(
            name.as_ref(),
            describe_field(
                py,
                &found.plan,
                &found.runtime,
                found.field_index,
                &found.field,
                found.window,
                name.to_string(),
            )?,
        )?;
    }
    Ok(out)
}

/// Public member names in the order upstream exposes through `__members__`.
///
/// Direct fields retain declaration order. An anonymous structure or union does
/// not contribute its generated name; its own public members are spliced into
/// the parent's sequence at that position. Upstream builds the mapping with
/// repeated `dict.update` calls, so a later duplicate replaces the value but
/// keeps the first insertion position. The plan's `index_of` map already points
/// at the last value; the set here preserves that first position.
pub(crate) fn visible_member_names(plan: &CompiledPlan) -> Vec<Arc<str>> {
    fn append(plan: &CompiledPlan, names: &mut Vec<Arc<str>>, seen: &mut HashSet<Arc<str>>) {
        for (index, name) in plan.names.iter().enumerate() {
            if !name.is_empty() {
                if seen.insert(Arc::clone(name)) {
                    names.push(Arc::clone(name));
                }
                continue;
            }

            let Some(field) = plan.fields.get(index) else {
                continue;
            };
            if field.op != Op::Nested {
                continue;
            }
            if let Some(subplan) = plan.subplans.get(field.aux as usize) {
                append(subplan, names, seen);
            }
        }
    }

    let mut names = Vec::with_capacity(plan.index_of.len());
    let mut seen = HashSet::with_capacity(plan.index_of.len());
    append(plan, &mut names, &mut seen);
    names
}

/// A directly declared field's raw mapping key.
///
/// Anonymous structures have no public field name of their own, but upstream's
/// raw lookup and size maps key them by their generated nested type name.
pub(crate) fn declared_field_name(plan: &CompiledPlan, index: usize) -> Option<&str> {
    let name = plan.names.get(index)?;
    if !name.is_empty() {
        return Some(name.as_ref());
    }

    let field = plan.fields.get(index)?;
    if field.op != Op::Nested {
        return None;
    }
    plan.subplans
        .get(field.aux as usize)
        .map(|subplan| subplan.name.as_ref())
}

/// Build the `lookup` mapping: fields **as declared**, not folded.
///
/// See the module header on why this is not the same mapping as
/// [`fields_mapping`]. Insertion follows plan order, and a Python dict preserves
/// that, so the values of this mapping are the declaration order — which is what
/// `StructType::__fields__` turns into a list.
pub fn lookup_mapping<'py>(
    py: Python<'py>,
    plan: &Arc<CompiledPlan>,
    runtime: &Arc<PlanRuntime>,
) -> PyResult<Bound<'py, PyDict>> {
    let out = PyDict::new(py);
    for (index, field) in plan.fields.iter().enumerate() {
        let declared = plan.names.get(index).map(|name| name.to_string());
        let key = match declared {
            Some(name) if !name.is_empty() => name,
            // An anonymous member is declared under its generated type name.
            _ => match plan.subplans.get(field.aux as usize) {
                Some(sub) => sub.name.to_string(),
                None => continue,
            },
        };
        let window = plan.bit_windows.get(index).copied().unwrap_or_default();
        out.set_item(
            key.clone(),
            describe_field(py, plan, runtime, index, field, window, key)?,
        )?;
    }
    Ok(out)
}

/// A field found at the end of a name path, with the plan it actually belongs to.
///
/// A folded name reaches a field inside a sub-plan, so reporting it needs the
/// sub-plan and sub-runtime too — the field's type object is resolved against
/// them, not against the outermost plan.
struct Located {
    plan: Arc<CompiledPlan>,
    runtime: Arc<PlanRuntime>,
    field_index: usize,
    field: FieldOp,
    window: BitWindow,
}

/// Walk a name path to the field it names, descending into sub-plans.
///
/// Returns `None` for a path that does not lead to a field. That is treated as
/// "nothing to report for this name" rather than an error: the mappings are
/// introspection, and a name that cannot be resolved is better omitted than
/// raised from a `fields` access.
fn resolve_path(
    plan: &Arc<CompiledPlan>,
    runtime: &Arc<PlanRuntime>,
    path: &[u32],
) -> Option<Located> {
    let mut current_plan = Arc::clone(plan);
    let mut current_runtime = Arc::clone(runtime);

    for (depth, step) in path.iter().enumerate() {
        let candidate = *current_plan.fields.get(*step as usize)?;
        let window = current_plan
            .bit_windows
            .get(*step as usize)
            .copied()
            .unwrap_or_default();

        if depth + 1 == path.len() {
            return Some(Located {
                plan: current_plan,
                runtime: current_runtime,
                field_index: *step as usize,
                field: candidate,
                window,
            });
        }

        let sub = current_plan.subplans.get(candidate.aux as usize)?.clone();
        let sub_runtime = current_runtime
            .subplans
            .get(candidate.aux as usize)?
            .clone();
        current_plan = sub;
        current_runtime = sub_runtime;
    }
    None
}

/// The type object for one field, built from its op.
///
/// A field yields the canonical object from its owning namespace where one is
/// available. This makes `cs.Outer.fields["inner"].type is cs.Inner` and the
/// corresponding scalar/enum checks agree with current upstream. A bare
/// `StructType` has no namespace; in that uncommon path a nested type is built
/// directly and a scalar is represented by a standalone
/// [`ScalarType`](crate::scalars::ScalarType).
pub(crate) fn field_type_object(
    py: Python<'_>,
    plan: &Arc<CompiledPlan>,
    runtime: &Arc<PlanRuntime>,
    field_index: usize,
    field: &FieldOp,
) -> PyResult<Py<PyAny>> {
    if plan
        .pointer_targets
        .get(field_index)
        .and_then(Option::as_ref)
        .is_some()
    {
        let pointer = runtime
            .pointer_classes
            .get(field_index)
            .and_then(Option::as_ref)
            .map(|class| class.clone_ref(py))
            .ok_or_else(|| internal("missing pointer class"))?;
        return if matches!(field.op, Op::ScalarArray) {
            subscript_type(
                py,
                pointer,
                field.count,
                plan.array_length_sources
                    .get(field_index)
                    .and_then(Option::as_deref),
            )
        } else {
            Ok(pointer)
        };
    }
    Ok(match field.op {
        Op::Enum | Op::Bits if field.op == Op::Enum || field.elem == Op::Enum => runtime
            .enum_classes
            .get(field.aux as usize)
            .map(|class| class.clone_ref(py))
            .ok_or_else(|| internal("missing enum class"))?,
        Op::ScalarArray if field.elem == Op::Enum => {
            let enum_type = runtime
                .enum_classes
                .get(field.aux as usize)
                .map(|class| class.clone_ref(py))
                .ok_or_else(|| internal("missing enum class"))?;
            subscript_type(
                py,
                enum_type,
                field.count,
                plan.array_length_sources
                    .get(field_index)
                    .and_then(Option::as_deref),
            )?
        }
        Op::Nested | Op::NestedArray => {
            let sub = plan
                .subplans
                .get(field.aux as usize)
                .ok_or_else(|| internal("missing sub-plan"))?;
            let sub_runtime = runtime
                .subplans
                .get(field.aux as usize)
                .ok_or_else(|| internal("missing sub-runtime"))?;
            let nested_type = namespace_type(py, runtime, &sub.name)
                .unwrap_or(Py::new(py, StructType::from_parts(sub, sub_runtime))?.into_any());
            if field.op == Op::NestedArray {
                subscript_type(
                    py,
                    nested_type,
                    field.count,
                    plan.array_length_sources
                        .get(field_index)
                        .and_then(Option::as_deref),
                )?
            } else {
                nested_type
            }
        }
        Op::Custom => runtime
            .custom_types
            .get(field.aux as usize)
            .map(|class| class.clone_ref(py))
            .ok_or_else(|| internal("missing custom type"))?,
        Op::Void | Op::VoidArray => {
            let void = namespace_type(py, runtime, "void").unwrap_or(
                py.import(pyo3::intern!(py, "dissect.cstruct"))?
                    .getattr(pyo3::intern!(py, "Void"))?
                    .unbind(),
            );
            if field.op == Op::VoidArray {
                subscript_type(py, void, field.count, None)?
            } else {
                void
            }
        }
        Op::CharArray | Op::WcharArray | Op::ScalarArray => {
            let scalar = scalar_for(field.elem, field.elem_size.max(1))
                .unwrap_or(Scalar::Uint(field.elem_size.max(1)));
            let scalar_type = canonical_or_standalone_scalar(py, plan, runtime, scalar)?;
            subscript_type(
                py,
                scalar_type,
                field.count,
                plan.array_length_sources
                    .get(field_index)
                    .and_then(Option::as_deref),
            )?
        }
        op => {
            // A plain scalar carries its width in `size`; an array element
            // carries it in `elem_size`, and a bitfield describes its storage
            // type in `elem` rather than in `op`.
            let width = if matches!(op, Op::Uint | Op::Int | Op::Float) {
                field.size as u8
            } else {
                field.elem_size.max(1)
            };
            let scalar = scalar_for(if op == Op::Bits { field.elem } else { op }, width)
                .unwrap_or(Scalar::Uint(width.max(1)));
            canonical_or_standalone_scalar(py, plan, runtime, scalar)?
        }
    })
}

/// Look up a declared type on the runtime's owning namespace.
fn namespace_type(py: Python<'_>, runtime: &PlanRuntime, name: &str) -> Option<Py<PyAny>> {
    runtime
        .namespace
        .as_ref()
        .and_then(|namespace| namespace.bind(py).getattr(name).ok())
        .map(Bound::unbind)
}

/// Return a namespace's canonical scalar or a self-contained fallback.
fn canonical_or_standalone_scalar(
    py: Python<'_>,
    plan: &CompiledPlan,
    runtime: &PlanRuntime,
    scalar: Scalar,
) -> PyResult<Py<PyAny>> {
    let name = crate::registry::canonical_scalar_name(scalar);
    if let Some(type_object) = namespace_type(py, runtime, name) {
        return Ok(type_object);
    }
    Ok(Py::new(
        py,
        crate::scalars::ScalarType::new(scalar, plan.endian, name),
    )?
    .into_any())
}

/// Apply one field's declared array length to its element type.
fn subscript_type(
    py: Python<'_>,
    element: Py<PyAny>,
    count: u32,
    expression: Option<&str>,
) -> PyResult<Py<PyAny>> {
    if let Some(expression) = expression {
        // The source spelling remains in the compiled plan for rendering, but
        // a constant expression has already folded to an exact count. Current
        // upstream exposes that count through the field's array type rather
        // than presenting it as a still-dynamic expression.
        if count != DYNAMIC_COUNT {
            return Ok(element.bind(py).get_item(count as usize)?.unbind());
        }
        if let Ok(array) = crate::scalars::expression_array_type(py, element.bind(py), expression) {
            return Ok(array);
        }
        if element.bind(py).hasattr("__members__")? {
            return Ok(py
                .import(pyo3::intern!(py, "dissect.cstruct._enum_types"))?
                .getattr(pyo3::intern!(py, "_expression_array_type"))?
                .call1((element.bind(py), expression))?
                .unbind());
        }
        return Ok(element.bind(py).get_item(expression)?.unbind());
    }
    let array = if count == DYNAMIC_COUNT {
        element.bind(py).get_item(py.None())?
    } else {
        element.bind(py).get_item(count as usize)?
    };
    Ok(array.unbind())
}

/// Map a decode op and a width back onto the scalar that produces it.
///
/// `None` for an op that is not a scalar at all, which the caller treats as an
/// opaque unsigned field.
fn scalar_for(op: Op, width: u8) -> Option<Scalar> {
    match op {
        Op::Uint => Some(Scalar::Uint(width)),
        Op::Int => Some(Scalar::Int(width)),
        Op::Float => Some(Scalar::Float(width)),
        Op::Char | Op::CharArray => Some(Scalar::Char),
        Op::Wchar | Op::WcharArray => Some(Scalar::Wchar),
        Op::Void | Op::VoidArray => Some(Scalar::Void),
        _ => None,
    }
}
