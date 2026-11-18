//! Tests of the rows of a `<JSON table>` of its operands' values: its
//! primitives' rows, each kind of column, the joins of the plan, the
//! `<JSON table error behavior>`, the chaining of a row's item, and the
//! warnings of casts.

use super::*;
use crate::error::{Code, Result};
use crate::json::parse_path;
use crate::value::datetime::Clocked;

/// A character string.
fn text(s: &str) -> Value {
    Value::Char(s.to_owned())
}

/// The path `p`, parsed.
fn path(p: &str) -> Path {
    parse_path(p, Clocked::default()).unwrap()
}

/// A regular column at `place` of the path `p`, of CHARACTER VARYING(10),
/// NULL ON EMPTY and `on_error` ON ERROR.
fn regular(place: usize, p: &str, on_error: Slot) -> Column {
    Column { place, kind: ColumnKind::Regular { path: path(p), returning: DataType::varying(10), on_empty: Slot::Null, on_error } }
}

/// The primitive of the path `p` and of `columns`.
fn primitive(p: &str, columns: Vec<Column>) -> Primitive {
    Primitive { path: path(p), columns }
}

/// The table of `primitives`, `plan`, `on_error`, `width` columns and no
/// PASSING clause.
fn table(primitives: Vec<Primitive>, plan: Plan, on_error: TableBehavior, width: usize) -> Arc<Table> {
    Arc::new(Table { passing: Vec::new(), primitives, plan: Arc::new(plan), on_error, width })
}

/// The rows of `t` of the context item `context`, each its values shown
/// and joined by commas.
fn rows_of(t: &Arc<Table>, context: Value) -> Result<Vec<String>> {
    let rows = t.rows(Operands { context, passing: Vec::new(), defaults: Vec::new() });
    rows.map(|row| row.map(|values| values.iter().map(ToString::to_string).collect::<Vec<_>>().join(","))).collect()
}

/// A parent of the path `lax $[*]`, its ordinality and `lax $.a` as its
/// columns 0 and 1, and a child of `lax $.b[*]`, its ordinality and `lax
/// $` as columns 2 and 3, joined by `join`.
fn parented(join: Join) -> Arc<Table> {
    let parent = primitive("lax $[*]", vec![Column { place: 0, kind: ColumnKind::Ordinality }, regular(1, "lax $.a", Slot::Null)]);
    let child = primitive("lax $.b[*]", vec![Column { place: 2, kind: ColumnKind::Ordinality }, regular(3, "lax $", Slot::Null)]);
    table(vec![parent, child], Plan::Join(0, join, Arc::new(Plan::Path(1))), TableBehavior::Empty, 4)
}

/// The document `parented` reads: a parent of two children, one of none.
const FAMILY: &str = r#"[{"a":"p","b":["x","y"]},{"a":"q","b":[]}]"#;

/// Verifies: [ISO/IEC 9075-2:2023 §7.11 GR1a, GR1d, GR1e ii 1, GR1e ii 2]
/// A primitive's rows are the items its path gives, each a row: the
/// ordinality column its place from 1, a regular column its path of the
/// item cast; a null context item gives none.
#[test]
fn primitive_rows_gr1() {
    let t = table(
        vec![primitive("lax $[*]", vec![Column { place: 0, kind: ColumnKind::Ordinality }, regular(1, "lax $.a", Slot::Null)])],
        Plan::Path(0),
        TableBehavior::Empty,
        2,
    );
    assert_eq!(rows_of(&t, text(r#"[{"a":"x"},{"b":1},{"a":7}]"#)).unwrap(), ["1,x", "2,NULL", "3,7"]);
    assert_eq!(rows_of(&t, Value::Null).unwrap(), Vec::<String>::new());
}

/// Verifies: [ISO/IEC 9075-2:2023 §7.11 SR1k iii 2, SR1k iii 3, NOTE 243]
/// OUTER keeps a parent's row of no child's row, the child's columns null;
/// INNER does not; the child's ordinality counts from 1 under each parent.
#[test]
fn outer_and_inner_sr1k() {
    assert_eq!(rows_of(&parented(Join::Outer), text(FAMILY)).unwrap(), ["1,p,1,x", "1,p,2,y", "2,q,NULL,NULL"]);
    assert_eq!(rows_of(&parented(Join::Inner), text(FAMILY)).unwrap(), ["1,p,1,x", "1,p,2,y"]);
}

/// Two siblings of `lax $.b[*]` and `lax $.c[*]`, of the columns 1 and 2,
/// under a parent of `lax $` and the column 0, combined by `siblings`.
fn siblings(siblings: Siblings) -> Arc<Table> {
    let parent = primitive("lax $", vec![regular(0, "lax $.a", Slot::Null)]);
    let (b, c) =
        (primitive("lax $.b[*]", vec![regular(1, "lax $", Slot::Null)]), primitive("lax $.c[*]", vec![regular(2, "lax $", Slot::Null)]));
    let operands = vec![Arc::new(Plan::Path(1)), Arc::new(Plan::Path(2))];
    table(vec![parent, b, c], Plan::Join(0, Join::Outer, Arc::new(Plan::Siblings(siblings, operands))), TableBehavior::Empty, 3)
}

/// Verifies: [ISO/IEC 9075-2:2023 §7.11 SR1k iii 4, SR1k iii 5, NOTE 244]
/// UNION gives each sibling's rows, the other's columns null; CROSS their
/// cross product, the last varying fastest; and none if one has no row,
/// so OUTER keeps the parent's row alone.
#[test]
fn union_and_cross_sr1k() {
    let doc = text(r#"{"a":"p","b":[1,2],"c":["x","y"]}"#);
    assert_eq!(rows_of(&siblings(Siblings::Union), doc.clone()).unwrap(), ["p,1,NULL", "p,2,NULL", "p,NULL,x", "p,NULL,y"]);
    assert_eq!(rows_of(&siblings(Siblings::Cross), doc).unwrap(), ["p,1,x", "p,1,y", "p,2,x", "p,2,y"]);
    let none = text(r#"{"a":"p","b":[1,2],"c":[]}"#);
    assert_eq!(rows_of(&siblings(Siblings::Cross), none.clone()).unwrap(), ["p,NULL,NULL"]);
    assert_eq!(rows_of(&siblings(Siblings::Union), none).unwrap(), ["p,1,NULL", "p,2,NULL"]);
}

/// Verifies: [ISO/IEC 9075-2:2023 §7.11 SR1k iii 5] A CROSS of three
/// operands combines every row of each, the odometer carrying from the
/// last operand to the one before it; an operand's error is raised though
/// another has no row.
#[test]
fn cross_of_three_sr1k() {
    let parent = primitive("lax $", Vec::new());
    let ops = ["lax $.a[*]", "lax $.b[*]", "lax $.c[*]"];
    let mut primitives = vec![parent];
    primitives.extend(ops.iter().zip(0_usize..).map(|(p, k)| primitive(p, vec![regular(k, "lax $", Slot::Error)])));
    let operands = (1_usize..=3).map(|p| Arc::new(Plan::Path(p))).collect();
    let t = table(primitives, Plan::Join(0, Join::Inner, Arc::new(Plan::Siblings(Siblings::Cross, operands))), TableBehavior::Empty, 3);
    let got = rows_of(&t, text(r#"{"a":[1,2],"b":[3],"c":[4,5]}"#)).unwrap();
    assert_eq!(got, ["1,3,4", "1,3,5", "2,3,4", "2,3,5"]);
    let raised = rows_of(&t, text(r#"{"a":[[1]],"b":[],"c":[4]}"#)).unwrap_err();
    assert_eq!(raised.sqlstate(), SqlState::SQL_JSON_SCALAR_REQUIRED, "the first operand's error, though the second has no row");
    let later = rows_of(&t, text(r#"{"a":[],"b":[3],"c":[[4]]}"#)).unwrap_err();
    assert_eq!(later.sqlstate(), SqlState::SQL_JSON_SCALAR_REQUIRED, "a later operand's error, though the first has no row");
}

/// Verifies: [ISO/IEC 9075-2:2023 §7.11 GR1e i] An exception condition of
/// a primitive's path, of JSON text that is none, gives no row EMPTY ON
/// ERROR and is raised ERROR ON ERROR; an error no exception condition, a
/// limit of turborel's, is raised either way. MC/DC of `Empty &&
/// is_exception` in `TableBehavior::caught`: EMPTY of an exception
/// caught, ERROR of one raised, EMPTY of no exception raised.
#[test]
fn table_error_behavior_gr1e() {
    let of = |on_error| table(vec![primitive("lax $[*]", vec![regular(0, "lax $", Slot::Null)])], Plan::Path(0), on_error, 1);
    assert_eq!(rows_of(&of(TableBehavior::Empty), text("{")).unwrap(), Vec::<String>::new());
    assert_eq!(rows_of(&of(TableBehavior::Error), text("{")).unwrap_err().sqlstate(), SqlState::INVALID_JSON_TEXT);
    let deep = text(&format!("{}1{}", "[".repeat(200), "]".repeat(200)));
    assert_eq!(rows_of(&of(TableBehavior::Empty), deep).unwrap_err().code, Code::Unsupported);
}

/// Verifies: [ISO/IEC 9075-2:2023 §7.11 GR1e ii 4, NOTE 259; §9.43 GR2a
/// ii] A row's item is its child's context item as JSON text, so one of no
/// JSON text, a datetime, gives the child no row EMPTY ON ERROR, and its
/// exception ERROR ON ERROR.
#[test]
fn chaining_gr1e_ii_4() {
    let of = |on_error| {
        let parent = primitive("lax $[*].datetime()", vec![regular(0, "lax $", Slot::Null)]);
        let child = primitive("lax $", vec![regular(1, "lax $", Slot::Null)]);
        table(vec![parent, child], Plan::Join(0, Join::Outer, Arc::new(Plan::Path(1))), on_error, 2)
    };
    assert_eq!(rows_of(&of(TableBehavior::Empty), text(r#"["2024-02-29"]"#)).unwrap(), ["2024-02-29,NULL"]);
    assert_eq!(rows_of(&of(TableBehavior::Error), text(r#"["2024-02-29"]"#)).unwrap_err().sqlstate(), SqlState::INVALID_JSON_TEXT);
}

/// Verifies: [ISO/IEC 9075-2:2023 §7.11 GR1e ii 2 D, GR1e ii 3; §9.48
/// GR4c iii, GR5; §9.49 GR4c] A column's exception is raised of ERROR ON
/// ERROR, replaced of NULL or DEFAULT ON ERROR; DEFAULT ON EMPTY gives its
/// value cast; a formatted column serializes its item, EMPTY ARRAY ON
/// EMPTY of none.
#[test]
fn column_behaviors_gr1e_ii() {
    let formatted = Column {
        place: 3,
        kind: ColumnKind::Formatted {
            path: path("lax $.b"),
            returning: DataType::varying(20),
            wrapper: Wrapper::Without,
            quotes: Quotes::Keep,
            on_empty: QueryBehavior::EmptyArray,
            on_error: QueryBehavior::Null,
        },
    };
    let columns =
        vec![regular(0, "lax $.a", Slot::Default(1)), regular(1, "lax $.c", Slot::Null), regular(2, "lax $.d", Slot::Null), formatted];
    let mut columns = columns;
    if let Some(Column { kind: ColumnKind::Regular { on_empty, .. }, .. }) = columns.get_mut(2) {
        *on_empty = Slot::Default(0);
    }
    let t = table(vec![primitive("lax $[*]", columns)], Plan::Path(0), TableBehavior::Empty, 4);
    let made = |doc: &str| {
        let rows = t.rows(Operands { context: text(doc), passing: Vec::new(), defaults: vec![text("none"), text("bad")] });
        rows.map(|row| row.map(|values| values.iter().map(ToString::to_string).collect::<Vec<_>>().join(","))).collect::<Result<Vec<_>>>()
    };
    assert_eq!(made(r#"[{"a":[1],"b":{"x":1},"c":[2]}]"#).unwrap(), [r#"bad,NULL,none,{"x":1}"#]);
    assert_eq!(made(r#"[{"a":"v"}]"#).unwrap(), ["v,NULL,none,[]"]);
    let raises = table(vec![primitive("lax $[*]", vec![regular(0, "lax $", Slot::Error)])], Plan::Path(0), TableBehavior::Empty, 1);
    assert_eq!(rows_of(&raises, text("[[1]]")).unwrap_err().sqlstate(), SqlState::SQL_JSON_SCALAR_REQUIRED);
}

/// Verifies: [ISO/IEC 9075-2:2023 §9.48 GR4b iii 2; ISO/IEC 9075:1992
/// §3.3.4.1] A cast that cuts a string to a column's type keeps its
/// warning, 01004, for the caller to take once.
#[test]
fn warnings_are_kept() {
    let t = table(vec![primitive("lax $[*]", vec![regular(0, "lax $", Slot::Null)])], Plan::Path(0), TableBehavior::Empty, 1);
    let mut rows = t.rows(Operands { context: text(r#"["abcdefghijkl"]"#), passing: Vec::new(), defaults: Vec::new() });
    let first = rows.next().map(|row| row.unwrap().iter().map(ToString::to_string).collect::<Vec<_>>());
    assert_eq!(first, Some(vec!["abcdefghij".to_owned()]));
    assert_eq!(rows.warnings().unwrap(), [SqlState::STRING_TRUNCATED]);
    assert!(rows.warnings().unwrap().is_empty(), "taken once");
}

/// Verifies: [DESIGN] Operands other than the table was bound with are a
/// broken invariant, SQLCODE -910, and so is a plan naming a primitive the
/// table has not.
#[test]
fn broken_operands() {
    let t = table(vec![primitive("lax $", Vec::new())], Plan::Path(0), TableBehavior::Empty, 0);
    let rows = t.rows(Operands { context: text("1"), passing: vec![text("x")], defaults: Vec::new() });
    assert_eq!(rows.collect::<Result<Vec<_>>>().unwrap_err().code, Code::Api);
    let gone = table(vec![primitive("lax $", Vec::new())], Plan::Path(3), TableBehavior::Empty, 0);
    assert_eq!(rows_of(&gone, text("1")).unwrap_err().code, Code::Api);
}
