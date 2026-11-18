//! JSON_TABLE [ISO/IEC 9075-2:2023 §7.11], `<JSON table>`: the rows an
//! SQL/JSON item gives as a table, of inputs already evaluated.
//!
//! \[DESIGN\] §7.11 defines a `<JSON table>` by a syntactic transformation
//! [SR1i to SR1m]: each `<JSON table columns clause>` becomes a `<JSON table
//! primitive>` whose rows are the items its path gives \[GR1\], each nested
//! one reading the item of its parent's row as its context item, through a
//! chaining column [GR1e ii 4], and the primitives are joined as the plan
//! says, OUTER a LEFT OUTER JOIN ON 0=0, INNER a cross join, UNION a FULL
//! OUTER JOIN ON 0=1, CROSS a cross join [SR1k iii]. The rules are followed
//! here directly, with the results the transformation's query has: a
//! `Primitive` is processed as §9.47 processes a `<JSON API common
//! syntax>`, each column of each row as JSON_VALUE's and JSON_QUERY's rules
//! cast and serialize a sequence [GR1e ii 2, 3], a nested primitive's
//! context item is its parent's row item serialized as JSON text, as the
//! chaining column has it [§9.43], and the plan is walked as the joins
//! combine rows (`json/table/run.rs`). The query is not built, since every
//! LATERAL derived table of it would be planned and run apart.
//!
//! A `Table` is what the SQL layer binds; `Table::rows` gives the rows of
//! one evaluation of its operands, lazily, a row at a time.

mod run;

use std::sync::{Arc, Mutex};

pub use run::Rows;

use super::{Path, QueryBehavior, Quotes, Wrapper};
use crate::error::{Code, SqlError, SqlState};
use crate::value::{DataType, Value};

/// [§7.11] A `<JSON table>`, bound: its PASSING clause, its primitives, its
/// plan and its `<JSON table error behavior>`.
#[derive(Debug)]
pub struct Table {
    /// Each `<JSON argument>`'s identifier, as the path's named variable
    /// writes it, and whether it is FORMAT JSON; every primitive's path
    /// reads them [SR1k iii 6 D].
    pub passing: Vec<(String, bool)>,
    /// The `<JSON table columns clause>`s, CNODELIST [SR1i iv]: in the
    /// order their COLUMNS is written, the outermost, JACSPATH's, first.
    pub primitives: Vec<Primitive>,
    /// The explicit or implicit `<JSON table specific plan>`, JTPLAN
    /// \[SR1j\], its `<JSON table path name>`s each the place of its
    /// primitive among `primitives`.
    pub plan: Arc<Plan>,
    /// The explicit or implicit `<JSON table error behavior>`, JTEB
    /// \[SR1b\].
    pub on_error: TableBehavior,
    /// How many columns it has: every `<column name>` it contains, at any
    /// depth of nesting \[SR1l\].
    pub width: usize,
}

/// [§7.11] A `<JSON table error behavior>`: what an exception condition of
/// a primitive's path or of a chaining column makes of it [GR1e i, GR1e ii
/// 4 A II]. \[SR1b\] Without one, EMPTY ON ERROR is implicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableBehavior {
    /// `ERROR ON ERROR`: the exception condition is raised.
    Error,
    /// `EMPTY ON ERROR`: the primitive is an empty table; the default.
    Empty,
}

/// [§7.11 SR1k iii 6 D] A `<JSON table primitive>`: of a `<JSON table
/// columns clause>`, its path and its own columns. Its chaining columns
/// [SR1k iii 6 C I] are the plan's, whose OUTER or INNER it is the first
/// operand of.
#[derive(Debug)]
pub struct Primitive {
    /// The `<JSON path specification>` associated with it [SR1d, SR1g vii]:
    /// JACSPATH of the outermost, a `<JSON table nested path
    /// specification>` of another; parsed.
    pub path: Path,
    /// Its `<JSON table ordinality column definition>`s, `<JSON table
    /// regular column definition>`s and `<JSON table formatted column
    /// definition>`s, in the order written.
    pub columns: Vec<Column>,
}

/// A column of a primitive, and its place among the table's columns.
#[derive(Debug)]
pub struct Column {
    /// Its place among the table's columns, from 0, in the order the
    /// `<column name>`s are written in the `<JSON table>` \[SR1l\].
    pub place: usize,
    /// What it is.
    pub kind: ColumnKind,
}

/// [§7.11] What a column of a primitive is, each clause explicit or
/// implicit, as SR1e and SR1f make them.
#[derive(Debug)]
pub enum ColumnKind {
    /// `<JSON table ordinality column definition>`: the row's place among
    /// the primitive's rows, from 1 [GR1e ii 1].
    Ordinality,
    /// `<JSON table regular column definition>`: the sequence its path
    /// gives cast to its type, as JSON_VALUE's is [GR1e ii 2].
    Regular {
        /// JTRCPATH, explicit or implicit [SR1e v], parsed.
        path: Path,
        /// DTi, its `<data type>`.
        returning: DataType,
        /// ZBi, its `<JSON table column empty behavior>` [SR1e iii].
        on_empty: Slot,
        /// EBi, its `<JSON table column error behavior>` [SR1e iv].
        on_error: Slot,
    },
    /// `<JSON table formatted column definition>`: the sequence its path
    /// gives serialized to its type, as JSON_QUERY's is [GR1e ii 3 C II].
    /// \[DESIGN\] turborel has no JSON type, so its type is a character
    /// string type, FORMAT JSON [SR1f ii 1, SR1f iv].
    Formatted {
        /// JTQCPATH, explicit or implicit [SR1f xii], parsed.
        path: Path,
        /// DTi, its `<data type>`.
        returning: DataType,
        /// WBi [SR1f v, SR1f vi].
        wrapper: Wrapper,
        /// QBi [SR1f viii].
        quotes: Quotes,
        /// ZBi [SR1f x].
        on_empty: QueryBehavior,
        /// EBi [SR1f xi].
        on_error: QueryBehavior,
    },
}

/// [§7.11] A `<JSON table column empty behavior>` or `<JSON table column
/// error behavior>`, bound: a DEFAULT's value is among the operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// ERROR.
    Error,
    /// NULL.
    Null,
    /// DEFAULT `<value expression>`: the place of its value among the
    /// operands' `defaults`.
    Default(usize),
}

/// [§7.11 SR1k] A node of the plan tree JTTREE: a `<JSON table path
/// name>`, a `<JSON table plan parent/child>` or a `<JSON table plan
/// sibling>`. A node of one child is that child [SR1k iii 1], so none is
/// kept.
#[derive(Debug, Clone)]
pub enum Plan {
    /// A `<JSON table path name>`: the rows of its primitive [SR1k iii 6].
    Path(usize),
    /// `<JSON table plan outer>` or `<JSON table plan inner>`: of each row
    /// of the first operand's primitive, the rows of the second operand
    /// over that row's item [SR1k iii 2, 3].
    Join(usize, Join, Arc<Plan>),
    /// `<JSON table plan union>` or `<JSON table plan cross>`, of two
    /// operands or more [SR1k iii 4, 5].
    Siblings(Siblings, Vec<Arc<Plan>>),
}

/// [§7.11] How a parent primitive's rows are joined to its child's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Join {
    /// OUTER: a row of the parent of no child's row is kept, the child's
    /// columns null [SR1k iii 2, NOTE 243].
    Outer,
    /// INNER: it is not [SR1k iii 3].
    Inner,
}

/// [§7.11] How the rows of siblings are combined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Siblings {
    /// UNION: each operand's rows, the others' columns null [SR1k iii 4,
    /// NOTE 244].
    Union,
    /// CROSS: their cross product [SR1k iii 5].
    Cross,
}

/// The values of a `<JSON table>`'s operands, evaluated for one row of
/// what precedes it.
#[derive(Debug, Clone)]
pub struct Operands {
    /// The value of the `<JSON context item>`'s `<value expression>`,
    /// JACSCI: a character string, its JSON text, or the null value.
    pub context: Value,
    /// Each `<JSON argument>`'s value, in the order of `Table::passing`.
    pub passing: Vec<Value>,
    /// Each DEFAULT `<value expression>`'s value, in the order the columns
    /// are written, a column's ON EMPTY before its ON ERROR.
    pub defaults: Vec<Value>,
}

impl Table {
    /// [§7.11 GR] Its rows of `operands`: of a null context item none
    /// \[GR1a\]; else those its plan gives of the context item \[SR1m\], each
    /// a value of each of its columns, lazily. A warning a cast of a column
    /// raises, string data, right truncation, is kept for the caller to
    /// take (`Rows::warnings`).
    ///
    /// # Errors
    ///
    /// Each row read may be an error: an exception condition of a
    /// primitive's path or of a chaining column, if ERROR ON ERROR
    /// [GR1e i 1, GR1e ii 4 A II 1]; of a column, if its behavior ON ERROR
    /// is ERROR [GR1e ii 2 D, GR1e ii 3 D]; or an error no exception
    /// condition, whatever the behaviors: a Syntax Rule's or a limit's of
    /// turborel's (`query::is_exception`).
    pub fn rows(self: &Arc<Self>, operands: Operands) -> Rows {
        let Operands { context, passing, defaults } = operands;
        let run = Arc::new(run::Run { table: Arc::clone(self), passing, defaults, warned: Mutex::new(Vec::new()) });
        Rows::of(run, context)
    }
}

/// The warnings `Rows` hold: of each cast that raised one, its SQLSTATE.
type Warned = Vec<SqlState>;

/// \[DESIGN\] The error of a `Table` that `what`, which the binder never
/// makes: a broken invariant (SQLCODE -910).
fn broken(what: &str) -> SqlError {
    SqlError::new(Code::Api, format!("[DESIGN] a JSON_TABLE {what}"))
}

#[cfg(test)]
mod tests;
