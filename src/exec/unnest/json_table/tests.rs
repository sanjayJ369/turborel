//! Tests of whether a plan of a `<JSON table>` can raise an error, which
//! decides whether a rewrite or FETCH FIRST may leave rows of it unread
//! (RULES.md D15).

use super::*;
use crate::json::{Plan as JsonPlan, Primitive, TableBehavior, parse_path};
use crate::logical::{Expr, LogicalPlan, Subquery};
use crate::physical::Plan;
use crate::value::datetime::Clocked;

/// A table of one primitive, of the path `lax $` and no column.
fn table() -> Arc<Table> {
    let primitive = Primitive { path: parse_path("lax $", Clocked::default()).unwrap(), columns: Vec::new() };
    Arc::new(Table {
        passing: Vec::new(),
        primitives: vec![primitive],
        plan: Arc::new(JsonPlan::Path(0)),
        on_error: TableBehavior::Empty,
        width: 0,
    })
}

/// The node over `input` of one constant operand, of `json` if given.
fn node<P>(input: P, json: Option<Arc<Table>>) -> Unnest<P> {
    Unnest {
        collections: vec![Expr::Const(Value::Char("[]".to_owned()))],
        spread: vec![None],
        ordinality: false,
        outer: None,
        names: Vec::new(),
        input,
        json,
    }
}

/// Verifies: [DESIGN] RULES.md D15: a JSON table can raise an error
/// whatever its operands, as its columns' casts can, and a collection
/// derived table of constants cannot. MC/DC of `json || collections can
/// raise || ...` of `Plan::can_raise` and of a subquery's plan: of
/// constant collections, no condition and an input that cannot raise,
/// the JSON table alone makes it true.
#[test]
fn json_tables_can_raise() {
    let values = || Plan::Values { rows: vec![Vec::new()], detail: String::new() };
    assert!(Plan::Unnest(Box::new(node(values(), Some(table())))).can_raise());
    assert!(!Plan::Unnest(Box::new(node(values(), None))).can_raise());
    let one = || LogicalPlan::Values { rows: vec![Vec::new()], types: Vec::new() };
    assert!(Subquery::new(LogicalPlan::Unnest(Box::new(node(one(), Some(table())))), Vec::new()).can_raise());
    assert!(!Subquery::new(LogicalPlan::Unnest(Box::new(node(one(), None))), Vec::new()).can_raise());
}
