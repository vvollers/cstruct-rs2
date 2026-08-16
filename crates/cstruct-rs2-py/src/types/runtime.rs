//! The Python objects a compiled plan needs but cannot hold.
//!
//! A [`CompiledPlan`] lives in `cstruct-rs2-core` and knows nothing about
//! Python. That is deliberate — it is what lets the parser and decoder be tested
//! and fuzzed with no interpreter in the loop. But some fields cannot be decoded
//! into a plain number: an enum field has to produce a real `IntEnum` member,
//! because `dissect.ntfs` reads `.value` off one, and a pointer field has to
//! produce a `Pointer` subclass that knows what it addresses.
//!
//! Those classes have to live somewhere, so they live here, in a structure that
//! runs alongside the plan and mirrors its shape: one entry per enum, one per
//! consumer-supplied type, one per pointer field, and one nested runtime per
//! sub-plan. Index into this with the same `aux` or field index you would use on
//! the plan and you get the Python object for it.
//!
//! Everything here is built **once per type**, when the type is compiled, and
//! then shared behind an `Arc` by every record decoded from it. Nothing in this
//! module runs per record.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use pyo3::prelude::*;
use pyo3::pyclass::{PyTraverseError, PyVisit};
use pyo3::types::PyDict;

use cstruct_rs2_core::plan::CompiledPlan;

use crate::types::field::declared_field_name;

/// Stable identity for one declared structure across compiled generations.
///
/// Changing a namespace's byte order recompiles a structure, but upstream keeps
/// the generated Python class itself. Records decoded before and after that
/// change are therefore still instances of the same type and compare as that
/// same type. The identity stays fixed while `current_plan` moves to the newly
/// compiled generation used for later serialization.
pub struct TypeIdentity {
    current_plan: RwLock<Arc<CompiledPlan>>,
}

impl TypeIdentity {
    /// Start a logical type at one compiled generation.
    pub fn new(plan: Arc<CompiledPlan>) -> Self {
        Self {
            current_plan: RwLock::new(plan),
        }
    }

    /// Make later operations use a replacement generation of the same type.
    pub fn replace_plan(&self, plan: Arc<CompiledPlan>) {
        match self.current_plan.write() {
            Ok(mut current) => *current = plan,
            // No code runs while holding this lock except an `Arc` assignment,
            // so poisoning can only follow an unrelated panic. Recover the
            // guarded value rather than turning that into a second failure at
            // the Python boundary.
            Err(poisoned) => *poisoned.into_inner() = plan,
        }
    }

    /// The generation used by operations performed now, notably `dumps()`.
    pub fn current_plan(&self) -> Arc<CompiledPlan> {
        match self.current_plan.read() {
            Ok(current) => Arc::clone(&current),
            Err(poisoned) => Arc::clone(&poisoned.into_inner()),
        }
    }
}

/// Logical structure identities in one cstruct namespace, keyed canonically.
pub type TypeIdentities = HashMap<String, Arc<TypeIdentity>>;

/// Python objects a plan needs at materialisation time.
///
/// Sub-plans get their own runtime, mirroring [`CompiledPlan::subplans`], so a
/// nested structure's enums and pointers resolve the same way its parent's do.
pub struct PlanRuntime {
    /// Stable logical identity shared by this type object and all its records.
    pub identity: Arc<TypeIdentity>,
    /// Owning namespace for canonical field-type introspection.
    ///
    /// This reference is never consulted while decoding. It is used only when
    /// a caller asks for `fields` or `lookup`, so a scalar or nested field names
    /// the same object exposed as `cs.uint32` or `cs.Inner`. Runtime owners
    /// report this hidden reference to cyclic GC through [`Self::traverse`].
    pub namespace: Option<Py<PyAny>>,
    /// One class per entry in [`CompiledPlan::enums`].
    pub enum_classes: Vec<Py<PyAny>>,
    /// One runtime per entry in [`CompiledPlan::subplans`].
    pub subplans: Vec<Arc<PlanRuntime>>,
    /// One consumer-supplied class per entry in [`CompiledPlan::custom_names`].
    pub custom_types: Vec<Py<PyAny>>,
    /// A `Pointer` subclass per pointer field, parallel to [`CompiledPlan::fields`].
    ///
    /// Empty when the plan was built without a namespace to build them from --
    /// a pointer class needs `cs._make_pointer`, and some entry points (tests,
    /// a bare `StructType::new`) have no namespace. A pointer field then
    /// decodes to its plain integer address because there is no owning
    /// namespace from which to construct a target-aware pointer class.
    pub pointer_classes: Vec<Option<Py<PyAny>>>,
    /// Static field sizes, shared by the generated type and all its records.
    ///
    /// Upstream stores this exact dictionary on the generated structure class.
    /// Keeping one Python object here preserves that identity even though an rs2
    /// record is not a Python instance of its generated class.
    pub static_sizes: Py<PyDict>,
}

impl PlanRuntime {
    /// Report Python objects hidden behind this Rust runtime to cyclic GC.
    ///
    /// The runtime is held through `Arc`, which Python cannot inspect. In
    /// particular, a pointer class retains its namespace and can therefore
    /// close a cycle through a generated structure class or decoded record.
    /// Every Python-facing owner of a runtime delegates its traversal here.
    pub fn traverse(&self, visit: &PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.namespace)?;
        for class in &self.enum_classes {
            visit.call(class)?;
        }
        for runtime in &self.subplans {
            runtime.traverse(visit)?;
        }
        for class in &self.custom_types {
            visit.call(class)?;
        }
        for class in self.pointer_classes.iter().flatten() {
            visit.call(class)?;
        }
        visit.call(&self.static_sizes)
    }

    /// Build the Python classes a plan's enum fields need.
    ///
    /// Use this where there is no namespace and no consumer-supplied type to
    /// resolve — a plan compiled straight from a definition string, as tests do.
    /// Pointer fields built this way decode to plain integers.
    pub fn build(py: Python<'_>, plan: &Arc<CompiledPlan>) -> PyResult<Arc<Self>> {
        Self::build_with(py, plan, &HashMap::new(), None)
    }

    /// Build a runtime, resolving consumer-supplied types from `custom`.
    ///
    /// `namespace` is the `cstruct` object the plan belongs to, when there is
    /// one. It is needed only for pointer fields: a `Pointer` subclass carries
    /// the namespace so it can resolve its target type and the pointer width,
    /// and neither is derivable from the plan alone.
    ///
    /// Fails when the definition named a custom type that was never registered,
    /// because decoding such a field would have nothing to call.
    pub fn build_with(
        py: Python<'_>,
        plan: &Arc<CompiledPlan>,
        custom: &HashMap<String, Py<PyAny>>,
        namespace: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Arc<Self>> {
        let mut identities = HashMap::new();
        Self::build_with_identities(py, plan, custom, namespace, &mut identities)
    }

    /// Build a runtime using the namespace's stable logical type identities.
    pub fn build_with_identities(
        py: Python<'_>,
        plan: &Arc<CompiledPlan>,
        custom: &HashMap<String, Py<PyAny>>,
        namespace: Option<&Bound<'_, PyAny>>,
        identities: &mut TypeIdentities,
    ) -> PyResult<Arc<Self>> {
        let identity = match identities.get(plan.name.as_ref()) {
            Some(existing) => {
                existing.replace_plan(Arc::clone(plan));
                Arc::clone(existing)
            }
            None => {
                let created = Arc::new(TypeIdentity::new(Arc::clone(plan)));
                identities.insert(plan.name.to_string(), Arc::clone(&created));
                created
            }
        };
        let mut enum_classes = Vec::with_capacity(plan.enums.len());
        for enum_plan in &plan.enums {
            enum_classes.push(match namespace {
                Some(namespace) => namespace.getattr(enum_plan.name.as_ref())?.unbind(),
                None => crate::registry::build_enum_class_from_plan(py, enum_plan, None, None)?,
            });
        }
        let mut subplans = Vec::with_capacity(plan.subplans.len());
        for sub in &plan.subplans {
            subplans.push(Self::build_with_identities(
                py, sub, custom, namespace, identities,
            )?);
        }
        let mut custom_types = Vec::with_capacity(plan.custom_names.len());
        for name in &plan.custom_names {
            if let Some(class) = custom.get(name.as_ref()) {
                custom_types.push(class.clone_ref(py));
                continue;
            }

            // Custom arrays retain `base[selector]` in the plan. Resolve the
            // class once while constructing the runtime; record decoding then
            // holds the exact class handle and invokes one array callback.
            let (base, selector) = name
                .strip_suffix(']')
                .and_then(|value| value.rsplit_once('['))
                .ok_or_else(|| {
                    pyo3::exceptions::PyRuntimeError::new_err(format!(
                        "custom type {name} was used in a definition but is not registered"
                    ))
                })?;
            let base_class = custom.get(base).ok_or_else(|| {
                pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "custom type {base} was used in a definition but is not registered"
                ))
            })?;
            let selector = if selector.is_empty() {
                py.None()
            } else if let Ok(count) = selector.parse::<u32>() {
                count.into_pyobject(py)?.into_any().unbind()
            } else {
                selector.into_pyobject(py)?.into_any().unbind()
            };
            custom_types.push(base_class.bind(py).get_item(selector.bind(py))?.unbind());
        }

        Ok(Arc::new(Self {
            identity,
            namespace: namespace.cloned().map(Bound::unbind),
            enum_classes,
            subplans,
            custom_types,
            pointer_classes: pointer_classes_for(py, plan, namespace)?,
            static_sizes: static_sizes_for(py, plan)?.unbind(),
        }))
    }
}

/// Build upstream's `__static_sizes__` dictionary in raw field order.
///
/// Anonymous members are keyed by the generated nested type name in this raw
/// view; their folded public names belong to `__members__`, not to the size map.
fn static_sizes_for<'py>(py: Python<'py>, plan: &CompiledPlan) -> PyResult<Bound<'py, PyDict>> {
    let sizes = PyDict::new(py);
    for (index, field) in plan.fields.iter().enumerate() {
        if field.dynamic {
            continue;
        }
        if let Some(key) = declared_field_name(plan, index) {
            sizes.set_item(key, field.size)?;
        }
    }
    Ok(sizes)
}

/// Build one `Pointer` subclass per pointer field, or nothing if there are none.
///
/// Split out because the common case is worth seeing at a glance: almost no type
/// has a pointer field, and the whole function then returns an empty `Vec`
/// without touching the interpreter.
///
/// The class resolves its *target* lazily (see `Pointer._target`), which is what
/// lets `struct Node { Node *next; }` work: this runs while `Node` is being
/// compiled, so asking the namespace for `Node` now would recurse.
fn pointer_classes_for(
    py: Python<'_>,
    plan: &CompiledPlan,
    namespace: Option<&Bound<'_, PyAny>>,
) -> PyResult<Vec<Option<Py<PyAny>>>> {
    let Some(namespace) = namespace else {
        return Ok(Vec::new());
    };
    if !plan.pointer_targets.iter().any(Option::is_some) {
        return Ok(Vec::new());
    }

    let mut classes = Vec::with_capacity(plan.pointer_targets.len());
    for target in &plan.pointer_targets {
        classes.push(match target {
            Some(target) => Some(
                namespace
                    .call_method1(pyo3::intern!(py, "_make_pointer"), (target.as_ref(),))?
                    .unbind(),
            ),
            None => None,
        });
    }
    Ok(classes)
}
