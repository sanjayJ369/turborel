//! [ISO/IEC 9075-2:2023 §7.17 SR3f] Whether a query specification simply
//! contains row pattern recognition, which a recursive query's element may
//! not.

use super::selected;
use crate::ast::{QueryExpression, TableReference, ValueExpression, WindowOver};

/// [ISO/IEC 9075-2:2023 §7.17 SR3f] Whether `q` is a query specification
/// that simply contains row pattern recognition: of a table primary, or
/// of a window of its window clause or its select list.
pub(super) fn recognizes(q: &QueryExpression) -> bool {
    let QueryExpression::Spec(spec) = q else { return false };
    let te = &spec.table_expression;
    if te.window.iter().any(|d| d.specification.recognition.is_some()) {
        return true;
    }
    let inline = |v: &ValueExpression| {
        let ValueExpression::Window(w) = v else { return false };
        let WindowOver::Specification(s) = &w.over else { return false };
        s.recognition.is_some()
    };
    if selected(spec).into_iter().any(|v| v.any(&inline)) {
        return true;
    }
    let mut refs: Vec<&TableReference> = te.from.iter().collect();
    while let Some(r) = refs.pop() {
        match r {
            TableReference::Recognized(_) => return true,
            TableReference::Joined(j) => refs.extend([&j.left, &j.right]),
            TableReference::Table(_)
            | TableReference::Derived(_)
            | TableReference::Unnest(_)
            | TableReference::Polymorphic(_)
            | TableReference::JsonTable(_) => {}
        }
    }
    false
}
