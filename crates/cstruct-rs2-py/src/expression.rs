//! `Expression`: evaluating a length or constant expression from Python.
//!
//! Array lengths in a definition are expressions — `char name[n]`,
//! `uint8 pad[(len + 3) & ~3]` — and the decoder evaluates them for every record
//! that has one. Upstream also exposes the evaluator on its own, as
//! `Expression(text).evaluate(cs)`, and its suite drives the arithmetic through
//! that entry point directly.
//!
//! This module is that entry point. It wraps the *same* Rust evaluator the
//! decoder uses rather than reimplementing the grammar, because two
//! implementations of one grammar are two places for it to drift — and
//! expression semantics decide array lengths, so a drift there is a wrong
//! record rather than a wrong error message.
//!
//! # What a name can resolve to
//!
//! The evaluator itself knows only about integers. Everything that turns a
//! *name* into an integer lives in this module's `ConstScope`, and there is more
//! of it than there looks:
//!
//! * a `#define` in the definition, or an entry a consumer put in `cs.consts`;
//! * a dotted path such as `color.RED`, which is an enum member — an `int`
//!   subclass, so it extracts directly;
//! * `sizeof(TYPE)`, answered by `len()` on whatever type object the namespace
//!   hands out, so scalars, arrays and structures all answer the same way;
//! * `offsetof(TYPE, member)`, answered from the type's `__lookup__` mapping.
//!
//! # Error classes
//!
//! Both entry points here translate a core `InvalidValue` into
//! `ExpressionParserError` rather than letting the shared mapping produce a bare
//! `ValueError`. The shared mapping cannot tell a malformed *expression* from a
//! malformed *definition* — it sees only the error variant — and these two
//! functions know which they are looking at. `ExpressionParserError` derives
//! from `ValueError`, so anything already catching the broader class is
//! unaffected.

use std::collections::HashMap;

use pyo3::prelude::*;
use pyo3::types::PyDict;

use cstruct_rs2_core::error::Error;
use cstruct_rs2_core::expr::{Expr, Scope};

use crate::errors::{to_py_err, typed_err};

/// A parsed length/constant expression, evaluated against a namespace.
#[pyclass(module = "dissect.cstruct", name = "Expression")]
pub struct Expression {
    text: String,
    expr: Expr,
}

#[pymethods]
impl Expression {
    /// Parse an expression, or fail naming the grammar problem.
    ///
    /// A *grammar* failure is raised as `ExpressionParserError`. Everything else
    /// keeps its own class: blanketing all of them meant a malformed literal
    /// (`0x`) and an unknown type (`sizeof(0 +)`) both reported as grammar
    /// errors, which tells the reader the wrong thing about their input.
    #[new]
    fn new(expression: &str) -> PyResult<Self> {
        let expr = Expr::parse(expression).map_err(as_expression_error)?;
        Ok(Self {
            text: expression.to_owned(),
            expr,
        })
    }

    /// Evaluate against a namespace's constants.
    ///
    /// Takes the namespace rather than a bare mapping because that is what
    /// upstream's callers pass, and because a name can resolve to a const
    /// declared by a `#define`, to an enum member, or to a type's size.
    ///
    /// With no namespace, only literal arithmetic can succeed: any name is then
    /// unresolvable and raises rather than reading as zero.
    #[pyo3(signature = (context = None))]
    fn evaluate(&self, py: Python<'_>, context: Option<&Bound<'_, PyAny>>) -> PyResult<i128> {
        let consts = consts_of(py, context);
        self.expr
            .eval(&ConstScope {
                consts: &consts,
                context,
                py,
            })
            .map_err(as_expression_error)
    }

    fn __repr__(&self) -> String {
        format!("<Expression {}>", self.text)
    }
}

/// Report a core failure as an expression failure where it is one.
///
/// See the module header on why this is not left to the shared error mapping.
fn as_expression_error(error: Error) -> PyErr {
    match error {
        Error::InvalidValue { ref message } => typed_err("ExpressionParserError", message.clone()),
        other => to_py_err(other),
    }
}

/// Read a namespace's integer constants into a plain map.
///
/// Only entries that are actually integers are carried across; anything else in
/// `consts` is left for [`ConstScope::walk`] to resolve by name, which is where
/// the "resolved but not a number" case gets its own message.
fn consts_of(py: Python<'_>, context: Option<&Bound<'_, PyAny>>) -> HashMap<String, i128> {
    let mut consts = HashMap::new();
    let Some(context) = context else {
        return consts;
    };
    let Ok(mapping) = context.getattr(pyo3::intern!(py, "consts")) else {
        return consts;
    };
    let Ok(dict) = mapping.cast::<PyDict>() else {
        return consts;
    };
    for (key, value) in dict.iter() {
        if let (Ok(name), Ok(number)) = (key.extract::<String>(), value.extract::<i128>()) {
            consts.insert(name, number);
        }
    }
    consts
}

/// Constants supplied by a namespace, for `Expr::eval`.
struct ConstScope<'a, 'py> {
    consts: &'a HashMap<String, i128>,
    /// The namespace the expression is being evaluated against, when there is
    /// one. Carried so a dotted name can be walked -- `color.RED` is an enum
    /// member, and only the namespace knows that.
    context: Option<&'a Bound<'py, PyAny>>,
    py: Python<'py>,
}

impl ConstScope<'_, '_> {
    /// Walk `a.b.c` through the namespace, returning the object it names.
    ///
    /// The first segment comes from the namespace's own attributes (an enum
    /// type, a struct type) or from `consts` (a dict a consumer assigned);
    /// every later segment is an attribute or an item of what came before.
    /// Both spellings are needed: `color.RED` is `getattr`, `mydict.nested` is
    /// `__getitem__`.
    fn walk(&self, name: &str) -> Option<Bound<'_, PyAny>> {
        let context = self.context?;
        let mut parts = name.split('.');
        let head = parts.next()?;
        let mut current = context.getattr(head).ok().or_else(|| {
            context
                .getattr(pyo3::intern!(self.py, "consts"))
                .ok()?
                .get_item(head)
                .ok()
        })?;
        for part in parts {
            current = current
                .getattr(part)
                .ok()
                .or_else(|| current.get_item(part).ok())?;
        }
        Some(current)
    }
}

impl Scope for ConstScope<'_, '_> {
    fn lookup(&self, name: &str) -> Option<i128> {
        if let Some(value) = self.consts.get(name) {
            return Some(*value);
        }
        if !name.contains('.') {
            return None;
        }
        // An enum member is an `int` subclass, so `extract` gets it directly;
        // anything else that is not a number is a resolution failure rather
        // than a zero, and `describe_failure` says which.
        self.walk(name)?.extract::<i128>().ok()
    }

    /// Every unresolved name, dotted or not, reads as "Unknown identifier".
    ///
    /// The decode path keeps its own wording ("X is not defined in this
    /// scope"), which is right there: a length expression names a *field*, and
    /// saying so points at the record rather than at the namespace. Here the
    /// name really is an identifier in a namespace, and this is the entry point
    /// upstream's callers use.
    fn describe_failure(&self, name: &str) -> Option<String> {
        if !name.contains('.') {
            return Some(format!("Unknown identifier: '{name}'"));
        }
        match self.walk(name) {
            // It resolved to something; it just is not a number.
            Some(_) => Some(format!("'{name}' does not resolve to an integer")),
            None => Some(format!("Unknown identifier: '{name}'")),
        }
    }

    /// `len(cs.TYPE)` is the size, for every type object the namespace hands
    /// out -- scalars, arrays and structures alike. Asking the namespace rather
    /// than reaching into the schema keeps one answer to the question.
    fn size_of(&self, type_name: &str) -> Option<i128> {
        let type_object = self.context?.getattr(type_name).ok()?;
        type_object.len().ok().map(|size| size as i128)
    }

    fn offset_of(&self, type_name: &str, member: &str) -> Option<i128> {
        self.context?
            .getattr(type_name)
            .ok()?
            .getattr(pyo3::intern!(self.py, "__lookup__"))
            .ok()?
            .get_item(member)
            .ok()?
            .getattr(pyo3::intern!(self.py, "offset"))
            .ok()?
            .extract()
            .ok()
    }
}
