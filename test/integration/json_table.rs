//! JSON_TABLE [ISO/IEC 9075-2:2023 §7.11; RULES.md D84] in statements:
//! every kind of column, nested paths, the plan clauses, ON EMPTY and ON
//! ERROR of the columns and of the table, PASSING, its lateral reference
//! to the table references before it [§7.6 SR20a], and the Syntax Rules
//! refused as a statement is prepared.

use turborel::db::Session;
use turborel::error::{SqlError, SqlState};
use turborel::value::Value;

use crate::common::{exec, expected, rows, setup};

/// The error `sql` raises as it is prepared, opened or read.
fn failed(s: &Session, sql: &str) -> SqlError {
    let read = s.open_cursor(sql).and_then(|mut c| {
        while c.fetch()?.is_some() {}
        Ok(())
    });
    read.expect_err(sql)
}

/// The SQLCODE, the SQLSTATE and the rule of the error `sql` raises.
fn refused(s: &Session, sql: &str) -> (i32, String, Option<String>) {
    let e = failed(s, sql);
    (e.sqlcode(), e.sqlstate().as_str().to_owned(), e.rule)
}

/// `refused`, of a Syntax Rule of ISO/IEC 9075-2:2023 `cited`.
fn broke(cited: &str) -> (i32, String, Option<String>) {
    (-103, "2A000".to_owned(), Some(format!("ISO/IEC 9075-2:2023 {cited}")))
}

/// The SQLSTATE of the error `sql` raises.
fn state(s: &Session, sql: &str) -> String {
    failed(s, sql).sqlstate().as_str().to_owned()
}

/// Table DOCS of `setup`'s database: three documents, of people, their
/// pets and their pets' toys, and their phones, the third null.
fn docs(s: &Session) {
    exec(s, "CREATE TABLE DOCS (ID INTEGER, J VARCHAR(400))").unwrap();
    let ann = r#"{"name":"Ann","pets":[{"kind":"cat","toys":["ball","box"]},{"kind":"dog","toys":[]}],"phones":["111","222"]}"#;
    let bob = r#"{"name":"Bob","pets":[],"phones":["333"]}"#;
    exec(s, &format!("INSERT INTO DOCS VALUES (1, '{ann}'), (2, '{bob}'), (3, NULL)")).unwrap();
}

/// Verifies: [ISO/IEC 9075-2:2023 §7.11 SR1e, SR1f, SR2b, GR1e ii 1 to 3;
/// Feature T821] Each kind of column: FOR ORDINALITY the row's place from
/// 1, of INTEGER (IV158); a regular column its path's one scalar cast to
/// its type, as JSON_VALUE's; a formatted column its path's item
/// serialized, FORMAT JSON, with a wrapper and quotes behavior, as
/// JSON_QUERY's; the columns in the order written, of their declared
/// types.
#[test]
fn columns_7_11() {
    let db = setup();
    let s = db.session("APP");
    let q = r#"SELECT * FROM JSON_TABLE('[{"A":1,"b":"x","c":[1,2]},{"A":2,"b":"yy"}]', 'lax $[*]'
             COLUMNS (N FOR ORDINALITY, A INTEGER, "b" VARCHAR(5), B2 VARCHAR(3) PATH 'lax $.b',
                      C VARCHAR(20) FORMAT JSON PATH 'lax $.c', D VARCHAR(20) FORMAT JSON PATH 'lax $.c[*]' WITH WRAPPER,
                      E VARCHAR(5) FORMAT JSON PATH 'lax $.b' OMIT QUOTES, F VARCHAR(5) FORMAT JSON PATH 'lax $.b' KEEP QUOTES ON SCALAR STRING)) AS J"#;
    assert_eq!(rows(&s, q), expected(&[r#"1,1,x,x,[1,2],[1,2],x,"x""#, r#"2,2,yy,yy,NULL,NULL,yy,"yy""#]));
    let view = "CREATE VIEW JT AS SELECT * FROM JSON_TABLE('[]', 'lax $' COLUMNS (N FOR ORDINALITY, A SMALLINT, B DATE, \
                C CHAR(4) FORMAT JSON, D BOOLEAN)) AS J";
    exec(&s, view).unwrap();
    let q = "SELECT COLUMN_NAME, DATA_TYPE FROM INFORMATION_SCHEMA.COLUMNS WHERE TABLE_NAME = 'JT' ORDER BY ORDINAL_POSITION";
    assert_eq!(rows(&s, q), expected(&["N,INTEGER", "A,SMALLINT", "B,DATE", "C,CHARACTER", "D,BOOLEAN"]));
}

/// Verifies: [ISO/IEC 9075-2:2023 §7.11 SR1e v 2, SR1f xii 2] A column of
/// no PATH reads `$.` and its name as a member: its `<column name>`'s
/// value, so a regular identifier the member in upper case, a delimited
/// one its body, a quotation mark in it too; read in lax mode, so a member
/// not there is empty and NULL ON EMPTY.
#[test]
fn default_path_sr1e_v() {
    let db = setup();
    let s = db.session("APP");
    let q = r#"SELECT * FROM JSON_TABLE('{"A":1,"a":2,"x\"y":3,"F":[4]}', 'lax $' COLUMNS (a INTEGER, "a" INTEGER, "x""y" INTEGER,
             F VARCHAR(9) FORMAT JSON, G INTEGER)) AS J"#;
    assert_eq!(rows(&s, q), expected(&["1,2,3,[4],NULL"]));
}

/// Verifies: [ISO/IEC 9075-2:2023 §7.11 SR1i, SR1k iii 2, SR1k iii 6 B
/// II, GR1e ii 4; §7.6 SR20a; Feature T821] Nested columns, at any depth,
/// read the item of their parent's row; the implicit plan joins a parent
/// and its child OUTER, so a parent's row of no child's row is kept, its
/// child's columns null; a child's ordinality counts from 1 under each
/// parent's row; the context item, a column of the table before it, is
/// read row by row, and a null one gives no row.
#[test]
fn nested_columns_7_11() {
    let db = setup();
    let s = db.session("APP");
    docs(&s);
    let q = "SELECT D.ID, J.* FROM DOCS D, JSON_TABLE(D.J, 'lax $' COLUMNS (NAME VARCHAR(5) PATH 'lax $.name', \
             NESTED PATH 'lax $.pets[*]' COLUMNS (PN FOR ORDINALITY, KIND VARCHAR(5) PATH 'lax $.kind', \
             NESTED 'lax $.toys[*]' COLUMNS (TN FOR ORDINALITY, TOY VARCHAR(5) PATH 'lax $')))) AS J ORDER BY D.ID, J.PN, J.TN";
    assert_eq!(rows(&s, q), expected(&["1,Ann,1,cat,1,ball", "1,Ann,1,cat,2,box", "1,Ann,2,dog,NULL,NULL", "2,Bob,NULL,NULL,NULL,NULL"]));
}

/// The query of `ID`, `NAME`, `KIND` and `PHONE` of DOCS, its pets and
/// phones sibling nested columns, of the plan clause `plan`, ordered.
fn siblings(plan: &str) -> String {
    format!(
        "SELECT D.ID, J.NAME, J.KIND, J.PHONE FROM DOCS D, JSON_TABLE(D.J, 'lax $' AS R COLUMNS (NAME VARCHAR(5) PATH 'lax $.name', \
         NESTED PATH 'lax $.pets[*]' AS P COLUMNS (KIND VARCHAR(5) PATH 'lax $.kind'), \
         NESTED PATH 'lax $.phones[*]' AS F COLUMNS (PHONE VARCHAR(5) PATH 'lax $')) {plan}) AS J \
         ORDER BY D.ID, J.KIND, J.PHONE"
    )
}

/// Verifies: [ISO/IEC 9075-2:2023 §7.11 SR1i, SR1k iii 4, SR1k iii 5,
/// NOTE 244, NOTE 250; Features T827, T838] Sibling nested columns: the
/// implicit plan, OUTER and UNION, gives each sibling's rows, the other's
/// columns null; PLAN DEFAULT (CROSS) their cross product, of a sibling of
/// no row none, so OUTER keeps the parent's row; PLAN DEFAULT (INNER,
/// CROSS) drops it, and PLAN DEFAULT (UNION, INNER) is UNION's of a row of
/// one sibling.
#[test]
fn sibling_nested_columns_t827() {
    let db = setup();
    let s = db.session("APP");
    docs(&s);
    let union = ["1,Ann,cat,NULL", "1,Ann,dog,NULL", "1,Ann,NULL,111", "1,Ann,NULL,222", "2,Bob,NULL,333"];
    assert_eq!(rows(&s, &siblings("")), expected(&union));
    assert_eq!(rows(&s, &siblings("PLAN DEFAULT (OUTER, UNION)")), expected(&union));
    assert_eq!(rows(&s, &siblings("PLAN DEFAULT (UNION, INNER)")), expected(&union));
    let cross = ["1,Ann,cat,111", "1,Ann,cat,222", "1,Ann,dog,111", "1,Ann,dog,222"];
    let outer_cross: Vec<&str> = cross.iter().copied().chain(["2,Bob,NULL,NULL"]).collect();
    assert_eq!(rows(&s, &siblings("PLAN DEFAULT (CROSS)")), expected(&outer_cross));
    assert_eq!(rows(&s, &siblings("PLAN DEFAULT (INNER, CROSS)")), expected(&cross));
    assert_eq!(rows(&s, &siblings("PLAN DEFAULT (CROSS, INNER)")), expected(&cross));
}

/// Verifies: [ISO/IEC 9075-2:2023 §7.11 SR1j, SR1k; Features T824, T827] A
/// specific plan joins the paths it names: OUTER keeps a parent's row of
/// no child's, INNER does not; UNION and CROSS combine siblings, in
/// parentheses to any depth; the plan writes each path once, the row path
/// by its `AS` name. MC/DC of `UNION || CROSS` after a path name of
/// `json_table_plan`: UNION alone and CROSS alone a sibling, neither the
/// name alone (`name_refusals_sr1h`).
#[test]
fn specific_plan_t824() {
    let db = setup();
    let s = db.session("APP");
    docs(&s);
    let outer_cross = ["1,Ann,cat,111", "1,Ann,cat,222", "1,Ann,dog,111", "1,Ann,dog,222", "2,Bob,NULL,NULL"];
    assert_eq!(rows(&s, &siblings("PLAN (R OUTER (P CROSS F))")), expected(&outer_cross));
    assert_eq!(
        rows(&s, &siblings("PLAN (R INNER (F UNION P))")),
        expected(&["1,Ann,cat,NULL", "1,Ann,dog,NULL", "1,Ann,NULL,111", "1,Ann,NULL,222", "2,Bob,NULL,333"])
    );
    assert_eq!(rows(&s, &siblings("PLAN (R INNER ((P) CROSS (F)))")), expected(&outer_cross[..4]));
    let q = "SELECT D.ID, J.KIND, J.TOY FROM DOCS D, JSON_TABLE(D.J, 'lax $' AS R COLUMNS ( \
             NESTED PATH 'lax $.pets[*]' AS P COLUMNS (KIND VARCHAR(5) PATH 'lax $.kind', \
             NESTED PATH 'lax $.toys[*]' AS T COLUMNS (TOY VARCHAR(5) PATH 'lax $'))) PLAN (R OUTER (P INNER T))) AS J ORDER BY D.ID, J.TOY";
    assert_eq!(rows(&s, q), expected(&["1,cat,ball", "1,cat,box", "2,NULL,NULL"]));
    let q = "SELECT J.* FROM JSON_TABLE('{\"a\":[1,2],\"b\":[3],\"c\":[4,5]}', 'lax $' AS R COLUMNS ( \
             NESTED 'lax $.a[*]' AS A COLUMNS (X INTEGER PATH 'lax $'), NESTED 'lax $.b[*]' AS B COLUMNS (Y INTEGER PATH 'lax $'), \
             NESTED 'lax $.c[*]' AS C COLUMNS (Z INTEGER PATH 'lax $')) PLAN (R INNER ((A CROSS B) UNION C))) AS J ORDER BY J.X, J.Z";
    assert_eq!(rows(&s, q), expected(&["1,3,NULL", "2,3,NULL", "NULL,NULL,4", "NULL,NULL,5"]));
}

/// Verifies: [ISO/IEC 9075-2:2023 §7.11 SR1b, GR1e i; Feature T825] The
/// table's error behavior: an exception of a primitive's path, of a
/// context item that is no JSON text or of strict mode, gives no row EMPTY
/// ON ERROR, explicit or implicit, and is raised ERROR ON ERROR; a limit
/// of turborel's, SQLCODE -900, is raised either way.
#[test]
fn table_error_behavior_t825() {
    let db = setup();
    let s = db.session("APP");
    let of = |doc: &str, path: &str, behavior: &str| {
        format!("SELECT * FROM JSON_TABLE('{doc}', '{path}' COLUMNS (N FOR ORDINALITY) {behavior}) AS J")
    };
    assert_eq!(rows(&s, &of("{", "lax $", "")), expected(&[]));
    assert_eq!(rows(&s, &of("{", "lax $", "EMPTY ON ERROR")), expected(&[]));
    assert_eq!(state(&s, &of("{", "lax $", "ERROR ON ERROR")), "22032");
    assert_eq!(rows(&s, &of("{}", "strict $.a", "EMPTY ON ERROR")), expected(&[]));
    assert_eq!(state(&s, &of("{}", "strict $.a", "ERROR ON ERROR")), "2203A");
    assert_eq!(rows(&s, &of("[1,2]", "strict $[*]", "ERROR ON ERROR")), expected(&["1", "2"]));
    let deep = of(&format!("{}1{}", "[".repeat(200), "]".repeat(200)), "lax $", "EMPTY ON ERROR");
    assert_eq!(failed(&s, &deep).sqlcode(), -900, "a limit of turborel's is no exception EMPTY ON ERROR replaces");
}

/// Verifies: [ISO/IEC 9075-2:2023 §7.11 SR1e iii, SR1e iv, SR1f x, SR1f
/// xi, GR1e ii 2, GR1e ii 3; §9.48 GR4, GR5; §9.49 GR4c, GR6; Features
/// T825, T826] A column's behaviors: NULL ON EMPTY implicit; ON ERROR
/// implicit NULL, or ERROR of the table's ERROR ON ERROR, which a column's
/// own NULL ON ERROR overrides; ERROR ON EMPTY 22035, replaced by NULL ON
/// ERROR; DEFAULT any value expression, a column of the table before it
/// too, cast to the column's type; a formatted column's EMPTY ARRAY ON
/// EMPTY and EMPTY OBJECT ON ERROR.
#[test]
fn column_behaviors_t826() {
    let db = setup();
    let s = db.session("APP");
    let q = r#"SELECT * FROM JSON_TABLE('{"a":"x","b":[1,2]}', 'lax $' COLUMNS (A INTEGER PATH 'lax $.a', Z INTEGER PATH 'lax $.z',
             E INTEGER PATH 'lax $.z' ERROR ON EMPTY, F INTEGER PATH 'lax $.a' DEFAULT -1 ON ERROR,
             G INTEGER PATH 'lax $.z' DEFAULT 7 ON EMPTY, H VARCHAR(9) FORMAT JSON PATH 'lax $.z' EMPTY ARRAY ON EMPTY,
             I VARCHAR(9) FORMAT JSON PATH 'lax $.b[*]' EMPTY OBJECT ON ERROR, K VARCHAR(9) FORMAT JSON PATH 'lax $.z' EMPTY OBJECT ON EMPTY)) AS J"#;
    assert_eq!(rows(&s, q), expected(&["NULL,NULL,NULL,-1,7,[],{},{}"]));
    let of = |column: &str, behavior: &str| {
        format!(r#"SELECT * FROM JSON_TABLE('{{"a":"x","b":[1,2]}}', 'lax $' COLUMNS ({column})) AS J {behavior}"#)
    };
    assert_eq!(state(&s, &of("A INTEGER PATH 'lax $.a' ERROR ON ERROR", "")), "22018");
    let table_error =
        |column: &str| format!(r#"SELECT * FROM JSON_TABLE('{{"a":"x","b":[1,2]}}', 'lax $' COLUMNS ({column}) ERROR ON ERROR) AS J"#);
    assert_eq!(state(&s, &table_error("A INTEGER PATH 'lax $.a'")), "22018");
    assert_eq!(state(&s, &table_error("B VARCHAR(9) FORMAT JSON PATH 'lax $.b[*]'")), "22034");
    assert_eq!(rows(&s, &table_error("A INTEGER PATH 'lax $.a' NULL ON ERROR")), expected(&["NULL"]));
    assert_eq!(rows(&s, &table_error("B VARCHAR(9) FORMAT JSON PATH 'lax $.b[*]' NULL ON ERROR")), expected(&["NULL"]));
    assert_eq!(state(&s, &table_error("Z INTEGER PATH 'lax $.z' ERROR ON EMPTY")), "22035");
    assert_eq!(state(&s, &of("Z INTEGER PATH 'lax $.z' ERROR ON EMPTY ERROR ON ERROR", "")), "22035");
    assert_eq!(state(&s, &of("Z VARCHAR(9) FORMAT JSON PATH 'lax $.z' ERROR ON EMPTY ERROR ON ERROR", "")), "22035");
    assert_eq!(state(&s, &of("Z INTEGER PATH 'lax $.z' DEFAULT 'q' ON EMPTY ERROR ON ERROR", "")), "22018");
    let q = "SELECT T.ID, J.A FROM T, JSON_TABLE('{}', 'lax $' COLUMNS (A INTEGER PATH 'lax $.a' DEFAULT T.ID * 10 ON EMPTY)) AS J \
             ORDER BY T.ID";
    assert_eq!(rows(&s, q), expected(&["1,10", "2,20", "3,30"]));
}

/// Verifies: [ISO/IEC 9075-2:2023 §7.11 SR1e vi, SR1k iii 6 D; §10.14
/// SR5; §10.12 SR4a; §9.46 SR4, SR8, GR11b; Feature T823] PASSING gives
/// the row path and the nested paths their variables, FORMAT JSON, or a
/// JSON-returning function, an argument's JSON text; a column's path is of no PASSING clause, so a variable in it is
/// refused, as one no argument names is in any path.
#[test]
fn passing_t823() {
    let db = setup();
    let s = db.session("APP");
    let q = "SELECT * FROM JSON_TABLE('[1,5,9]', 'lax $[*] ? (@ > $LO)' PASSING 2 AS LO COLUMNS (V INTEGER PATH 'lax $')) AS J";
    assert_eq!(rows(&s, q), expected(&["5", "9"]));
    let q = r#"SELECT * FROM JSON_TABLE('{"a":[1,2,3]}', 'lax $' PASSING 2 AS "x", '[7,8]' FORMAT JSON AS Y
             COLUMNS (NESTED 'lax $.a[*] ? (@ >= $x)' COLUMNS (A INTEGER PATH 'lax $'), NESTED 'lax $Y[*]' COLUMNS (B INTEGER PATH 'lax $'))) AS J
             ORDER BY J.A, J.B"#;
    assert_eq!(rows(&s, q), expected(&["2,NULL", "3,NULL", "NULL,7", "NULL,8"]));
    let q = "SELECT * FROM JSON_TABLE(JSON_ARRAY(1, 2), 'lax $[*] ? (@ < $A[1])' PASSING JSON_ARRAY(0, 2) AS A \
             COLUMNS (V INTEGER PATH 'lax $')) AS J";
    assert_eq!(rows(&s, q), expected(&["1"]));
    let sr4 = broke("§9.46 SR4");
    let column = "SELECT * FROM JSON_TABLE('[1]', 'lax $' PASSING 1 AS X COLUMNS (V INTEGER PATH 'lax $X')) AS J";
    assert_eq!(refused(&s, column), sr4);
    assert!(s.prepare(column).is_err(), "refused as the statement is prepared");
    assert_eq!(refused(&s, "SELECT * FROM JSON_TABLE('[1]', 'lax $Q' COLUMNS (V FOR ORDINALITY)) AS J"), sr4);
    let nested = "SELECT * FROM JSON_TABLE('[1]', 'lax $' COLUMNS (NESTED 'lax $Q' COLUMNS (V FOR ORDINALITY))) AS J";
    assert_eq!(refused(&s, nested), sr4);
    let sr8 = broke("§9.46 SR8");
    let starts = "SELECT * FROM JSON_TABLE('[\"a\"]', 'lax $' PASSING 1 AS P COLUMNS (NESTED 'lax $[*] ? (@ starts with $P)' \
                  COLUMNS (V FOR ORDINALITY))) AS J";
    assert_eq!(refused(&s, starts), sr8);
}

/// Verifies: [ISO/IEC 9075-2:2023 §7.11 GR1a; §7.6 SR20a; §7.10 GR1d] A
/// null context item gives no row: a lateral reference row by row drops
/// its row, a LEFT join keeps it, its columns null, and so does a LEFT
/// join whose condition no row makes true. MC/DC of `outer.is_none() ||
/// kept` in `padded`: no LEFT join pads no row; a LEFT join whose row was
/// given none; one of no row given pads it.
#[test]
fn null_context_and_left_join_gr1a() {
    let db = setup();
    let s = db.session("APP");
    docs(&s);
    let phones = "JSON_TABLE(D.J, 'lax $.phones[*]' COLUMNS (N VARCHAR(5) PATH 'lax $')) AS P";
    assert_eq!(rows(&s, &format!("SELECT D.ID, P.N FROM DOCS D, {phones} ORDER BY D.ID, P.N")), expected(&["1,111", "1,222", "2,333"]));
    let left = format!("SELECT D.ID, P.N FROM DOCS D LEFT JOIN {phones} ON TRUE ORDER BY D.ID, P.N");
    assert_eq!(rows(&s, &left), expected(&["1,111", "1,222", "2,333", "3,NULL"]));
    let on = format!("SELECT D.ID, P.N FROM DOCS D LEFT JOIN {phones} ON P.N <> '333' ORDER BY D.ID, P.N");
    assert_eq!(rows(&s, &on), expected(&["1,111", "1,222", "2,NULL", "3,NULL"]));
    let q = "SELECT * FROM JSON_TABLE(CAST(NULL AS VARCHAR(5)), 'lax $' COLUMNS (N FOR ORDINALITY) ERROR ON ERROR) AS J";
    assert_eq!(rows(&s, q), expected(&[]));
}

/// Verifies: [ISO/IEC 9075-2:2023 §7.6 SR12b ii, SR15b, SR20a; §7.11 SR1m;
/// §7.10 SR4] A JSON_TABLE is a table reference among others: joined to a
/// table by ON, of its correlation name and its derived column list, in a
/// view, in a subquery reading the outer query's row, in a scalar
/// subquery and in a derived table; the second operand of a RIGHT join,
/// it reads nothing of the first.
#[test]
fn joins_views_and_subqueries_7_6() {
    let db = setup();
    let s = db.session("APP");
    docs(&s);
    let q = r#"SELECT T.NAME, J.V FROM T JOIN JSON_TABLE('[{"id":1,"v":"a"},{"id":2,"v":"b"}]', 'lax $[*]'
             COLUMNS (ID INTEGER PATH 'lax $.id', V VARCHAR(2) PATH 'lax $.v')) AS J (K, V) ON J.K = T.ID ORDER BY J.V"#;
    assert_eq!(rows(&s, q), expected(&["ANN   ,a", "BOB   ,b"]));
    exec(
        &s,
        "CREATE VIEW PHONES AS SELECT D.ID, P.N FROM DOCS D, JSON_TABLE(D.J, 'lax $.phones[*]' COLUMNS (N VARCHAR(5) PATH 'lax $')) AS P",
    )
    .unwrap();
    assert_eq!(rows(&s, "SELECT ID, N FROM PHONES WHERE N > '111' ORDER BY N"), expected(&["1,222", "2,333"]));
    let q = "SELECT D.ID FROM DOCS D WHERE EXISTS (SELECT 1 FROM JSON_TABLE(D.J, 'lax $.phones[*]' COLUMNS (N VARCHAR(5) PATH 'lax $')) AS P \
             WHERE P.N = '333')";
    assert_eq!(rows(&s, q), expected(&["2"]));
    let q =
        "SELECT D.ID, (SELECT COUNT(*) FROM JSON_TABLE(D.J, 'lax $.pets[*]' COLUMNS (K FOR ORDINALITY)) AS P) FROM DOCS D ORDER BY D.ID";
    assert_eq!(rows(&s, q), expected(&["1,2", "2,0", "3,0"]));
    let q = "SELECT X.* FROM (SELECT * FROM JSON_TABLE('[3,4]', 'lax $[*]' COLUMNS (V INTEGER PATH 'lax $')) AS J) AS X WHERE X.V > 3";
    assert_eq!(rows(&s, q), expected(&["4"]));
    let right = "SELECT * FROM DOCS D RIGHT JOIN JSON_TABLE(D.J, 'lax $' COLUMNS (N FOR ORDINALITY)) AS P ON TRUE";
    assert_eq!(refused(&s, right), broke("§7.10 SR4"), "a RIGHT join's second operand reads none of its first");
}

/// Verifies: [ISO/IEC 9075-2:2023 §9.16 SR1u, §11.9 CR2] A `<JSON table>`
/// is a potential source of non-determinism, so a CHECK constraint with
/// one in a subquery is SQLCODE -103.
#[test]
fn in_a_constraint_9_16() {
    let db = setup();
    let s = db.session("APP");
    let sql = "CREATE TABLE C1 (J VARCHAR(9) CHECK (EXISTS (SELECT * FROM JSON_TABLE(J, 'lax $' COLUMNS (V FOR ORDINALITY)) AS T)))";
    let e = s.run(sql).unwrap_err();
    assert_eq!((e.sqlcode(), e.sqlstate().as_str().to_owned(), e.rule), broke("§9.16 SR1u, §11.9 CR2"));
}

/// Verifies: [ISO/IEC 9075-2:2023 §7.11 GR1e ii 4, NOTE 259; §9.43 GR2a
/// ii] A nested path reads its parent row's item serialized as JSON text;
/// an item of none, a datetime, gives it no row EMPTY ON ERROR, and is
/// 22032 ERROR ON ERROR; the parent's own columns read the item as it is.
#[test]
fn chaining_gr1e_ii_4() {
    let db = setup();
    let s = db.session("APP");
    let of = |behavior: &str| {
        format!(
            r#"SELECT * FROM JSON_TABLE('["2024-02-29"]', 'lax $[*].datetime()' COLUMNS (D DATE PATH 'lax $',
               NESTED 'lax $' COLUMNS (T VARCHAR(30) PATH 'lax $.type()')) {behavior}) AS J"#
        )
    };
    assert_eq!(rows(&s, &of("")), expected(&["2024-02-29,NULL"]));
    assert_eq!(state(&s, &of("ERROR ON ERROR")), "22032");
    let q = r#"SELECT * FROM JSON_TABLE('[{"n":1.50,"e":2E0}]', 'lax $[*]' COLUMNS (NESTED 'lax $' COLUMNS (
             N VARCHAR(9) FORMAT JSON PATH 'lax $.n', E VARCHAR(9) PATH 'lax $.e.type()'))) AS J"#;
    assert_eq!(rows(&s, q), expected(&["1.50,number"]));
}

/// Verifies: [ISO/IEC 9075-2:2023 §9.48 GR4b iii 2; ISO/IEC 9075:1992
/// §3.3.4.1] A column's cast that cuts a string completes with the warning
/// 01004, as JSON_VALUE's does.
#[test]
fn warnings_9_48() {
    let db = setup();
    let s = db.session("APP");
    assert_eq!(
        rows(&s, r#"SELECT * FROM JSON_TABLE('["abcdef"]', 'lax $[*]' COLUMNS (A VARCHAR(3) PATH 'lax $')) AS J"#),
        expected(&["abc"])
    );
    assert_eq!(s.warning(), Some(SqlState::STRING_TRUNCATED));
    assert_eq!(rows(&s, r#"SELECT * FROM JSON_TABLE('["ab"]', 'lax $[*]' COLUMNS (A VARCHAR(3) PATH 'lax $')) AS J"#), expected(&["ab"]));
    assert_eq!(s.warning(), None);
}

/// Verifies: [EXTENSION] [ISO/IEC 9075-2:2023 §20.7 GR5a] A `?` context
/// item or PASSING argument FORMAT JSON is CHARACTER VARYING, a DEFAULT
/// its column's type; an argument without FORMAT JSON has no type and is
/// refused.
#[test]
fn dynamic_parameters_20_7() {
    let db = setup();
    let s = db.session("APP");
    let p = s
        .prepare(
            "SELECT * FROM JSON_TABLE(?, 'lax $[*]' PASSING ? FORMAT JSON AS B COLUMNS (A INTEGER PATH 'lax $.a' DEFAULT ? ON EMPTY)) AS J",
        )
        .unwrap();
    let text = |t: &str| Value::Char(t.to_owned());
    let mut c = s.open_prepared(&p, &[text(r#"[{"a":3},{}]"#), text("1"), text("9")]).unwrap();
    let mut got = Vec::new();
    while let Some(row) = c.fetch().unwrap() {
        got.push(row[0].to_string());
    }
    assert_eq!(got, ["3", "9"]);
    assert!(s.prepare("SELECT * FROM JSON_TABLE('[]', 'lax $B' PASSING ? AS B COLUMNS (A FOR ORDINALITY)) AS J").is_err());
}

/// Verifies: [ISO/IEC 9075-2:2023 §7.11 GR1e ii 2 D; RULES.md D15] A
/// JSON_TABLE's columns may raise an error whatever its operands are, so
/// FETCH FIRST reads its rows to their end and raises one past the rows
/// kept, as a raising input's; and a filter of a subquery over one is not
/// merged with one that removes the rows it would raise of; each pair of
/// the decision shown in `exec::unnest::json_table::tests`.
#[test]
fn raising_d15() {
    let db = setup();
    let s = db.session("APP");
    docs(&s);
    exec(&s, "INSERT INTO DOCS VALUES (4, '{')").unwrap();
    let q = "SELECT * FROM JSON_TABLE('[1,[2]]', 'lax $[*]' COLUMNS (A INTEGER PATH 'lax $' ERROR ON ERROR)) AS J FETCH FIRST 1 ROW ONLY";
    assert_eq!(state(&s, q), "2203F");
    let q = "SELECT D.ID FROM DOCS D WHERE D.ID = 1 AND EXISTS (SELECT 1 FROM JSON_TABLE(D.J, 'lax $' \
             COLUMNS (N FOR ORDINALITY) ERROR ON ERROR) AS Q)";
    assert_eq!(state(&s, q), "22032", "the conjunct is evaluated of every row, the fourth's too");
}

/// `SELECT * FROM JSON_TABLE('{}', 'lax $' <rest>) AS J`.
fn of_columns(rest: &str) -> String {
    format!("SELECT * FROM JSON_TABLE('{{}}', 'lax $' {rest}) AS J")
}

/// Verifies: [ISO/IEC 9075-2:2023 §7.11 SR1e ii, SR1f ii 1, SR1f iv, SR1f
/// vii; §10.14 SR1; §10.12 SR3, SR7] The binder's refusals of a column's
/// type and clauses, each as the statement is prepared: a context item of
/// no string type; a regular column of a type JSON_VALUE does not return;
/// a formatted one, of a wrapper, a quotes behavior or an EMPTY behavior,
/// without FORMAT JSON, as there is no JSON type; FORMAT JSON of no string
/// type, of a binary string (SQLCODE -900); a quotes behavior WITH a
/// wrapper; a PASSING argument of a binary string or an array; a DEFAULT
/// CAST does not take to the column's type. MC/DC of `WITH a wrapper &&
/// quotes` of SR1f vii: both refused, WITH alone and quotes alone, or of
/// WITHOUT, taken; of `FORMAT || wrapper || quotes` in `json_typed_column`,
/// each alone makes a column formatted, of none it is regular
/// (`columns_7_11`).
#[test]
fn column_refusals_sr1e() {
    let db = setup();
    let s = db.session("APP");
    let cast_rule = (-103, "2A000".to_owned(), Some("ISO/IEC 9075:1992 §6.10 SR4".to_owned()));
    let cases = [
        ("SELECT * FROM JSON_TABLE(1, 'lax $' COLUMNS (N FOR ORDINALITY)) AS J".to_owned(), broke("§10.14 SR1")),
        (of_columns("COLUMNS (A INTERVAL DAY)"), broke("§7.11 SR1e ii")),
        (of_columns("COLUMNS (A BINARY(2))"), broke("§7.11 SR1e ii")),
        (of_columns("COLUMNS (A INTEGER ARRAY)"), broke("§7.11 SR1e ii")),
        (of_columns("COLUMNS (A VARCHAR(5) WITH WRAPPER)"), broke("§7.11 SR1f ii 1")),
        (of_columns("COLUMNS (A VARCHAR(5) OMIT QUOTES)"), broke("§7.11 SR1f ii 1")),
        (of_columns("COLUMNS (A VARCHAR(5) EMPTY ARRAY ON EMPTY)"), broke("§7.11 SR1f ii 1")),
        (of_columns("COLUMNS (A VARCHAR(5) NULL ON EMPTY EMPTY OBJECT ON ERROR)"), broke("§7.11 SR1f ii 1")),
        (of_columns("COLUMNS (A INTEGER FORMAT JSON)"), broke("§7.11 SR1f iv")),
        (of_columns("COLUMNS (A BLOB FORMAT JSON)"), (-900, "0A000".to_owned(), None)),
        (of_columns("COLUMNS (A VARCHAR(5) FORMAT JSON WITH WRAPPER KEEP QUOTES)"), broke("§7.11 SR1f vii")),
        (
            of_columns("COLUMNS (A VARCHAR(5) FORMAT JSON WITH CONDITIONAL ARRAY WRAPPER OMIT QUOTES ON SCALAR STRING)"),
            broke("§7.11 SR1f vii"),
        ),
        (of_columns("COLUMNS (A INTEGER DEFAULT DATE '2024-01-01' ON EMPTY)"), cast_rule),
        ("SELECT * FROM JSON_TABLE('1', 'lax $A' PASSING X'00' AS A COLUMNS (N FOR ORDINALITY)) AS J".to_owned(), broke("§10.12 SR7")),
        ("SELECT * FROM JSON_TABLE('1', 'lax $A' PASSING ARRAY[1] AS A COLUMNS (N FOR ORDINALITY)) AS J".to_owned(), broke("§10.12 SR3")),
    ];
    for (sql, expected) in cases {
        assert_eq!(refused(&s, &sql), expected, "{sql}");
        assert!(s.prepare(&sql).is_err(), "refused as prepared: {sql}");
    }
    let taken = r#"SELECT * FROM JSON_TABLE('{"a":["x"]}', 'lax $' COLUMNS (A VARCHAR(9) FORMAT JSON PATH 'lax $.a' WITH WRAPPER,
                 B VARCHAR(9) FORMAT JSON PATH 'lax $.a[0]' OMIT QUOTES, C VARCHAR(9) FORMAT JSON PATH 'lax $.a[0]' WITHOUT WRAPPER KEEP QUOTES,
                 D VARCHAR(9) FORMAT JSON ENCODING UTF8 PATH 'lax $.a')) AS J"#;
    assert_eq!(rows(&s, taken), expected(&[r#"[["x"]],x,"x",["x"]"#]));
}

/// Verifies: [ISO/IEC 9075-2:2023 §7.11 SR1g i, SR1h; §7.6 SR12b ii 1]
/// The refusals of names: of a JSON_TABLE of a plan clause, specific or
/// default, NESTED of no path name; two columns of one name, a column of
/// a path's name, two paths of one name, the row path's of a column's; a
/// derived column list of other than its columns' number, or of a name
/// twice. MC/DC of `plan written && no name` of SR1g i: both refused, a
/// plan and a name taken (`specific_plan_t824`), no plan and no name
/// taken (`nested_columns_7_11`).
#[test]
fn name_refusals_sr1h() {
    let db = setup();
    let s = db.session("APP");
    let nested = "COLUMNS (NESTED 'lax $' COLUMNS (B FOR ORDINALITY))";
    let cases = [
        (of_columns(&format!("{nested} PLAN DEFAULT (INNER)")), broke("§7.11 SR1g i")),
        (of_columns(&format!("AS R {nested} PLAN (R)")), broke("§7.11 SR1g i")),
        (of_columns("COLUMNS (A FOR ORDINALITY, A INTEGER)"), broke("§7.11 SR1h")),
        (of_columns("COLUMNS (A FOR ORDINALITY, NESTED 'lax $' AS A COLUMNS (B FOR ORDINALITY))"), broke("§7.11 SR1h")),
        (
            of_columns("COLUMNS (NESTED 'lax $' AS P COLUMNS (B FOR ORDINALITY), NESTED 'lax $' AS P COLUMNS (C FOR ORDINALITY))"),
            broke("§7.11 SR1h"),
        ),
        (of_columns("AS A COLUMNS (A FOR ORDINALITY)"), broke("§7.11 SR1h")),
        (of_columns("COLUMNS (A FOR ORDINALITY, NESTED 'lax $' COLUMNS (A INTEGER))"), broke("§7.11 SR1h")),
        ("SELECT * FROM JSON_TABLE('{}', 'lax $' COLUMNS (A FOR ORDINALITY, B INTEGER)) AS J (X)".to_owned(), broke("§7.6 SR12b ii 1 B")),
    ];
    for (sql, expected) in cases {
        assert_eq!(refused(&s, &sql), expected, "{sql}");
    }
    let twice = "SELECT * FROM JSON_TABLE('{}', 'lax $' COLUMNS (A FOR ORDINALITY, B INTEGER)) AS J (X, X)";
    assert_eq!(refused(&s, twice), (-103, "2A000".to_owned(), Some("ISO/IEC 9075:1992 §6.3 SR6".to_owned())));
}

/// The table of DOCS's paths R, P, T and F, of the plan `plan`.
fn planned(plan: &str) -> String {
    format!(
        "SELECT * FROM DOCS D, JSON_TABLE(D.J, 'lax $' AS R COLUMNS (NESTED 'lax $.pets[*]' AS P COLUMNS ( \
         NESTED 'lax $.toys[*]' AS T COLUMNS (TOY VARCHAR(5) PATH 'lax $')), NESTED 'lax $.phones[*]' AS F COLUMNS (N FOR ORDINALITY)) \
         PLAN ({plan})) AS J"
    )
}

/// Verifies: [ISO/IEC 9075-2:2023 §7.11 SR1d, SR1j i, SR1j ii, SR1k iii 6
/// B II] The refusals of a specific plan: of a path it does not name, the
/// row path's of no name too, or names twice; of a name no path has; of a
/// path not within the columns of the first operand of the OUTER or INNER
/// it is in, or within them but not simply, or in no OUTER or INNER's
/// second operand, which gives no context item. MC/DC of `an OUTER or
/// INNER && not within it` of SR1j ii: the second operand of one, not
/// within it, refused; within it taken; of none, not checked, refused by
/// SR1k iii 6 B II instead.
#[test]
fn plan_refusals_sr1j() {
    let db = setup();
    let s = db.session("APP");
    docs(&s);
    let cases = [
        (planned("R OUTER (P OUTER T)"), "§7.11 SR1j i"),
        (planned("R OUTER ((P OUTER T) UNION (F UNION P))"), "§7.11 SR1j i"),
        (planned("R OUTER (P OUTER (T UNION F))"), "§7.11 SR1j ii"),
        (planned("R OUTER ((P OUTER Q) UNION F)"), "§7.11 SR1d"),
        (planned("R OUTER ((T OUTER P) UNION F)"), "§7.11 SR1j ii"),
        (planned("R OUTER (P UNION T UNION F)"), "§7.11 SR1k iii 6 B II"),
        (planned("(R OUTER (P OUTER T)) UNION F"), "§7.11 SR1k iii 6 B II"),
        (planned("P OUTER (R OUTER (T UNION F))"), "§7.11 SR1j ii"),
    ];
    for (sql, cited) in cases {
        assert_eq!(refused(&s, &sql), broke(cited), "{sql}");
    }
    let unnamed = "SELECT * FROM JSON_TABLE('{}', 'lax $' COLUMNS (NESTED 'lax $' AS P COLUMNS (A FOR ORDINALITY)) PLAN (P)) AS J";
    assert_eq!(refused(&s, unnamed), broke("§7.11 SR1j i"));
    assert_eq!(rows(&s, &format!("{} ORDER BY D.ID, J.TOY, J.N", planned("R OUTER ((P OUTER T) UNION F)"))).map(|r| r.len()), Ok(6));
}

/// Verifies: [ISO/IEC 9075-2:2023 §7.11 F; §7.6 F; RULES.md D18, D24] The
/// Format's refusals, each "expected", SQLCODE -102: no correlation name,
/// a plan of a parenthesized plan alone, UNION and CROSS together, PLAN
/// DEFAULT of neither choice, a column's DEFAULT with EMPTY ARRAY, a
/// table's error behavior of NULL; and row pattern recognition of a
/// JSON_TABLE, valid SQL not implemented, SQLCODE -900.
#[test]
fn format_refusals_7_11() {
    let db = setup();
    let s = db.session("APP");
    let code = |sql: &str| failed(&s, sql).sqlcode();
    for sql in [
        "SELECT * FROM JSON_TABLE('{}', 'lax $' COLUMNS (A FOR ORDINALITY))".to_owned(),
        of_columns("AS R COLUMNS (NESTED 'lax $' AS P COLUMNS (A FOR ORDINALITY)) PLAN ((R OUTER P))"),
        of_columns(
            "AS R COLUMNS (NESTED 'lax $' AS P COLUMNS (A FOR ORDINALITY), NESTED 'lax $' AS Q COLUMNS (B FOR ORDINALITY)) PLAN (R OUTER (P UNION Q CROSS P))",
        ),
        of_columns("COLUMNS (A FOR ORDINALITY) PLAN DEFAULT ()"),
        of_columns("COLUMNS (A FOR ORDINALITY) PLAN DEFAULT (INNER, OUTER)"),
        of_columns("COLUMNS (A INTEGER DEFAULT 1 ON EMPTY EMPTY ARRAY ON ERROR)"),
        of_columns("COLUMNS (A FOR ORDINALITY) NULL ON ERROR"),
        of_columns("COLUMNS ()"),
    ] {
        assert_eq!(code(&sql), -102, "{sql}");
    }
    let recognized = "SELECT * FROM JSON_TABLE('[1]', 'lax $[*]' COLUMNS (V INTEGER PATH 'lax $')) AS J \
                      MATCH_RECOGNIZE (ORDER BY V MEASURES FIRST(V) AS F PATTERN (A) DEFINE A AS TRUE) AS M";
    assert_eq!(code(recognized), -900);
}

/// Verifies: [ISO/IEC 9075-2:2023 §5.2, §7.11 F] JSON_TABLE is the `<JSON
/// table>` only before "(", and COLUMNS, NESTED, PATH, ORDINALITY and PLAN
/// are words only where its Format has them: a table and columns of those
/// names stay theirs, inside a JSON_TABLE too, a path named PLAN as well.
/// MC/DC of `NESTED && (literal || (PATH && literal))` of `at_nested`:
/// NESTED and a literal nested columns, NESTED PATH and a literal too, a
/// column NESTED of a type, NESTED PATH of no literal after a column of
/// the domain type PATH, refused as of no predefined type, and a column of
/// another name.
#[test]
fn words_stay_identifiers_5_2() {
    let db = setup();
    let s = db.session("APP");
    exec(&s, "CREATE TABLE JSON_TABLE (NESTED INTEGER, PATH INTEGER, COLUMNS INTEGER, PLAN INTEGER, ORDINALITY INTEGER)").unwrap();
    exec(&s, "INSERT INTO JSON_TABLE VALUES (1, 2, 3, 4, 5)").unwrap();
    assert_eq!(rows(&s, "SELECT NESTED + PATH + COLUMNS + PLAN + ORDINALITY FROM JSON_TABLE"), expected(&["15"]));
    let q = r#"SELECT * FROM JSON_TABLE, JSON_TABLE('{"NESTED":6,"PATH":[7]}', 'lax $' AS PLAN COLUMNS (NESTED INTEGER,
             NESTED PATH 'lax $.PATH[*]' AS JSON_TABLE COLUMNS (PATH INTEGER PATH 'lax $', ORDINALITY FOR ORDINALITY),
             NESTED 'lax $' AS X COLUMNS (COLUMNS INTEGER PATH 'lax $.NESTED')) PLAN (PLAN OUTER (JSON_TABLE UNION X))) AS PLAN
             ORDER BY PLAN.ORDINALITY"#;
    assert_eq!(rows(&s, q), expected(&["1,2,3,4,5,6,7,1,NULL", "1,2,3,4,5,6,NULL,NULL,6"]));
    exec(&s, "CREATE DOMAIN PATH AS INTEGER").unwrap();
    assert_eq!(refused(&s, &of_columns("COLUMNS (NESTED PATH PATH 'lax $')")), broke("§7.11 SR1e ii"));
}

/// Verifies: [DESIGN] RULES.md R-CF-6, D18: nested columns nest to the
/// parser's limit, the deepest a statement may, and each level is read,
/// joined and run within the stack; one more is SQLCODE -900.
#[test]
fn nesting_beyond_the_limit_is_refused() {
    let db = setup();
    let s = db.session("APP");
    let nested = |depth: usize| {
        let mut columns = "Z FOR ORDINALITY".to_owned();
        for k in 0..depth {
            columns = format!("N{k} FOR ORDINALITY, NESTED 'lax $' COLUMNS ({columns})");
        }
        format!("SELECT COUNT(*) FROM JSON_TABLE('1', 'lax $' COLUMNS ({columns})) AS J")
    };
    let mut depth = 1_usize;
    while rows(&s, &nested(depth)) == expected(&["1"]) {
        depth = depth.saturating_add(1);
    }
    assert!(depth > 50, "nested columns nest {depth} deep at most");
    assert_eq!(failed(&s, &nested(depth)).sqlcode(), -900);
}
