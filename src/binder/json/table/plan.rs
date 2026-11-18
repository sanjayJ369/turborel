//! [ISO/IEC 9075-2:2023 §7.11 SR1i, SR1j] The plan of a `<JSON table>`:
//! its `<JSON table specific plan>`, each path name its primitive, checked
//! \[SR1j\]; else the implicit one, of COLTREE and the `<JSON table default
//! plan>`'s choices \[SR1i\].
//!
//! \[DESIGN\] Beyond SR1j ii, the transformation gives a path name of a
//! nested primitive the context item ANCESTORPN.JTPNi [SR1k iii 6 B II]:
//! the chaining column of the first operand of the nearest OUTER or INNER
//! whose second operand it is in, which only the primitive it is nested
//! in has [SR1k iii 6 C I]. A plan that puts it elsewhere names no column
//! the query has, so it is refused, as SR1j ii refuses its like.
//!
//! The parser nests plans and columns clauses at most `MAX_NESTING_DEPTH`
//! deep, so the recursion here is bounded (RULES.md R-CF-6).

use std::sync::Arc;

use super::super::super::rule;
use super::Node;
use crate::ast::{JsonPathName, JsonPlanClause, JsonTablePlan};
use crate::error::Result;
use crate::json::{Join, Plan, Siblings};

/// [SR1i, SR1j] The plan of the table whose COLTREE is `nodes`, of its
/// plan clause `clause`, if written.
///
/// # Errors
///
/// SQLCODE -103 if a specific plan breaks SR1j or names a path name of no
/// path.
pub(super) fn plan(clause: Option<&JsonPlanClause>, nodes: &[Node]) -> Result<Plan> {
    match clause {
        // [SR1i i, SR1i ii] OUTER and UNION, unless PLAN DEFAULT says
        // INNER or CROSS.
        None => Ok(implicit(nodes, 0, (Join::Outer, Siblings::Union))),
        Some(JsonPlanClause::Default(join, siblings)) => Ok(implicit(nodes, 0, (*join, *siblings))),
        Some(JsonPlanClause::Specific(written, at)) => specific(written, nodes, *at),
    }
}

/// [SR1i viii] CPLANc, of the columns clause `c`: its path name, of no
/// child; else it joined by `join` to its one child's plan, or to the
/// `siblings` of its children's plans.
fn implicit(nodes: &[Node], c: usize, (join, siblings): (Join, Siblings)) -> Plan {
    let children = nodes.get(c).map_or(&[][..], |node| node.children.as_slice());
    match children {
        [] => Plan::Path(c),
        [only] => Plan::Join(c, join, Arc::new(implicit(nodes, *only, (join, siblings)))),
        [_, _, ..] => {
            let operands = children.iter().map(|&d| Arc::new(implicit(nodes, d, (join, siblings)))).collect();
            Plan::Join(c, join, Arc::new(Plan::Siblings(siblings, operands)))
        }
    }
}

/// A path name of a specific plan: its primitive, the first operand of
/// the nearest OUTER or INNER it is in the second operand of, if any, and
/// where it is written.
type Named = (usize, Option<usize>, usize);

/// \[SR1j\] The specific plan `written`, of PLAN written at `at`, over
/// COLTREE `nodes`: its path names checked, each named once [SR1j i], then
/// each within the columns of the first operand of its OUTER or INNER
/// [SR1j ii], then simply [SR1k iii 6 B II].
fn specific(written: &JsonTablePlan, nodes: &[Node], at: usize) -> Result<Plan> {
    let mut names = Vec::new();
    let plan = translated(written, (nodes, None), &mut names)?;
    // [SR1j i] every path name, of the outermost too.
    if let Some(missing) = (0..nodes.len()).find(|&p| !names.iter().any(|&(n, _, _)| n == p)) {
        let msg = match nodes.get(missing).and_then(|node| node.name.as_ref()) {
            Some(name) => format!("[ISO/IEC 9075-2:2023 §7.11 SR1j i] the PLAN of JSON_TABLE does not name the path {}", name.name),
            None => "[ISO/IEC 9075-2:2023 §7.11 SR1j i] the PLAN of JSON_TABLE names every path, so its row path has AS a <JSON table path name>"
                .to_owned(),
        };
        return Err(rule(msg).or_at(at));
    }
    for &(p, left, place) in &names {
        if let Some(l) = left
            && !within(nodes, p, l)
        {
            let msg = format!(
                "[ISO/IEC 9075-2:2023 §7.11 SR1j ii] the columns of the path {} of the PLAN of JSON_TABLE are not within those of {}, the first operand of the OUTER or INNER it is in",
                name_of(nodes, p),
                name_of(nodes, l)
            );
            return Err(rule(msg).or_at(place));
        }
    }
    for &(p, left, place) in &names {
        let parent = nodes.get(p).and_then(|node| node.parent);
        if parent != left {
            let msg = format!(
                "[ISO/IEC 9075-2:2023 §7.11 SR1k iii 6 B II] the path {} of the PLAN of JSON_TABLE is not the second operand of an OUTER or INNER whose first operand is the path it is nested in, which gives its context item",
                name_of(nodes, p)
            );
            return Err(rule(msg).or_at(place));
        }
    }
    Ok(plan)
}

/// [SR1j, SR1k] `written`, a `<JSON table plan>` or `<JSON table plan
/// primary>`, each path name its primitive: of the second operand of the
/// OUTER or INNER whose first operand is the primitive `left`, if any;
/// each path name pushed to `names`.
fn translated(written: &JsonTablePlan, (nodes, left): (&[Node], Option<usize>), names: &mut Vec<Named>) -> Result<Plan> {
    match written {
        JsonTablePlan::Name(name) => named(name, (nodes, left), names).map(Plan::Path),
        JsonTablePlan::Join(name, join, primary) => {
            let first = named(name, (nodes, left), names)?;
            Ok(Plan::Join(first, *join, Arc::new(translated(primary, (nodes, Some(first)), names)?)))
        }
        JsonTablePlan::Siblings(siblings, operands) => {
            let mut plans = Vec::with_capacity(operands.len());
            for operand in operands {
                plans.push(Arc::new(translated(operand, (nodes, left), names)?));
            }
            Ok(Plan::Siblings(*siblings, plans))
        }
    }
}

/// The primitive the path name `name` of the plan names, of the second
/// operand of the OUTER or INNER whose first operand is `left`, if any:
/// one that has it \[SR1d\], named once [SR1j i], pushed to `names`.
fn named(name: &JsonPathName, (nodes, left): (&[Node], Option<usize>), names: &mut Vec<Named>) -> Result<usize> {
    let Some(p) = nodes.iter().position(|node| node.name.as_ref().is_some_and(|n| n.name == name.name)) else {
        let msg =
            format!("[ISO/IEC 9075-2:2023 §7.11 SR1d] the PLAN of JSON_TABLE names {}, the <JSON table path name> of no path", name.name);
        return Err(rule(msg).or_at(name.at));
    };
    if names.iter().any(|&(n, _, _)| n == p) {
        return Err(
            rule(format!("[ISO/IEC 9075-2:2023 §7.11 SR1j i] the PLAN of JSON_TABLE names the path {} twice", name.name)).or_at(name.at)
        );
    }
    names.push((p, left, name.at));
    Ok(p)
}

/// Whether the columns clause `p` is contained in the columns clause
/// `outer`: nested in it, at any depth.
fn within(nodes: &[Node], p: usize, outer: usize) -> bool {
    let mut above = nodes.get(p).and_then(|node| node.parent);
    while let Some(a) = above {
        if a == outer {
            return true;
        }
        above = nodes.get(a).and_then(|node| node.parent);
    }
    false
}

/// The path name of the columns clause `c`, as written.
fn name_of(nodes: &[Node], c: usize) -> String {
    nodes.get(c).and_then(|node| node.name.as_ref()).map_or_else(String::new, |n| n.name.clone())
}
