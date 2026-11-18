//! The rows of a `<JSON table>` [ISO/IEC 9075-2:2023 §7.11], made by
//! walking its plan as the joins of the query its Syntax Rules write
//! combine rows [SR1k iii], a row at a time.
//!
//! A primitive's items are those its path gives of its context item
//! \[GR1d\], so they are held while its rows are made; each row's columns
//! are found as the row is reached [GR1e ii]. A row is a value of each
//! column of the table, null where no primitive of it gave one, so the
//! rows of two primitives combine by taking each column's value from the
//! one that has it: the columns of two primitives are never the same.
//!
//! \[DESIGN\] Of a CROSS, the rows of every operand but the first are held,
//! each operand read once, and the first's streamed against them. Every
//! operand is read whole even if another has no row, so an error of any
//! is raised as the cross join of the transformation, which evaluates each
//! table it joins, raises it. The plan is as deep as the parser nests
//! `<JSON table columns clause>`s and plans, at most `MAX_NESTING_DEPTH`
//! levels, so the walk's recursion is bounded (RULES.md R-CF-6).

use std::iter;
use std::sync::{Arc, Mutex};

use super::{ColumnKind, Join, Plan, Siblings, Slot, Table, TableBehavior, Warned, broken};
use crate::error::{Result, SqlError, SqlState};
use crate::json::{
    CommonSyntax, Item, Passing, QueryOptions, ValueBehavior, cast_sequence, is_exception, process, serialize, serialize_sequence,
};
use crate::sync::lock;
use crate::value::{Decimal, Value};

/// A row of a JSON table, or the error making it raised.
type Made = Result<Vec<Value>>;

/// Rows being made.
type Stream = Box<dyn Iterator<Item = Made> + Send>;

/// What the rows of one evaluation of a table's operands are made with.
pub(super) struct Run {
    /// The table.
    pub(super) table: Arc<Table>,
    /// The values of its PASSING clause's arguments.
    pub(super) passing: Vec<Value>,
    /// The values of its columns' DEFAULTs.
    pub(super) defaults: Vec<Value>,
    /// The warnings its columns' casts raised and its caller has not
    /// taken.
    pub(super) warned: Mutex<Warned>,
}

/// [§7.11 GR] The rows of a `<JSON table>` of one evaluation of its
/// operands (`Table::rows`), made as each is read.
pub struct Rows {
    /// What they are made with.
    run: Arc<Run>,
    /// The rows to come.
    rows: Stream,
}

impl Rows {
    /// The rows of `run` of the context item `context`: those of its plan.
    pub(super) fn of(run: Arc<Run>, context: Value) -> Rows {
        let rows = if run.passing.len() == run.table.passing.len() {
            let plan = Arc::clone(&run.table.plan);
            plan_rows(&run, &plan, context)
        } else {
            failed(broken("is given other values of its PASSING clause than it has arguments"))
        };
        Rows { run, rows }
    }

    /// [ISO/IEC 9075:1992 §3.3.4.1] The warnings the casts of its columns
    /// raised since they were last taken, each of the SQLSTATE a cast
    /// raised [§9.48 GR4b iii 2, §9.49 GR5b i].
    ///
    /// # Errors
    ///
    /// SQLCODE -910 if the lock that holds them is poisoned.
    pub fn warnings(&self) -> Result<Vec<SqlState>> {
        Ok(std::mem::take(&mut *lock(&self.run.warned)?))
    }
}

impl Iterator for Rows {
    type Item = Made;

    fn next(&mut self) -> Option<Made> {
        self.rows.next()
    }
}

impl TableBehavior {
    /// [GR1e i, GR1e ii 4 A II] Nothing, of an exception condition EMPTY
    /// ON ERROR replaces; `error` itself, of one ERROR ON ERROR raises and
    /// of an error that is none (`query::is_exception`).
    pub(super) fn caught(self, error: SqlError) -> Result<()> {
        if self == TableBehavior::Empty && is_exception(&error) { Ok(()) } else { Err(error) }
    }
}

/// The rows of `error` alone.
fn failed(error: SqlError) -> Stream {
    Box::new(iter::once(Err(error)))
}

/// [SR1k iii] The rows `plan` gives of `context`, the context item of the
/// primitives it reads first.
fn plan_rows(run: &Arc<Run>, plan: &Arc<Plan>, context: Value) -> Stream {
    match &**plan {
        Plan::Path(p) => leaf(run, *p, &context),
        Plan::Join(p, join, child) => joined(run, (*p, *join, child), &context),
        Plan::Siblings(Siblings::Union, operands) => union(run, operands, context),
        Plan::Siblings(Siblings::Cross, operands) => cross(run, operands, context),
    }
}

/// [GR1a to GR1e i] The items the path of primitive `p` gives of
/// `context`, as §9.47 processes a `<JSON API common syntax>` \[GR1d\]: none
/// of the null value \[GR1a\], nor of an exception condition EMPTY ON ERROR
/// replaces [GR1e i 2].
fn items(run: &Run, p: usize, context: &Value) -> Result<Vec<Item>> {
    let primitive = run.table.primitives.get(p).ok_or_else(|| broken("names a primitive it has not"))?;
    let passing: Vec<Passing<'_>> = run
        .table
        .passing
        .iter()
        .zip(&run.passing)
        .map(|((name, format_json), value)| Passing { name, value, format_json: *format_json })
        .collect();
    let common = CommonSyntax { context, path: &primitive.path, passing: &passing };
    match process(&common) {
        Ok(items) => Ok(items.unwrap_or_default()),
        Err(error) => run.table.on_error.caught(error).map(|()| Vec::new()),
    }
}

/// [SR1k iii 6] The rows of primitive `p` of `context`, each of its own
/// columns alone.
fn leaf(run: &Arc<Run>, p: usize, context: &Value) -> Stream {
    let items = match items(run, p, context) {
        Ok(items) => items,
        Err(error) => return failed(error),
    };
    let run = Arc::clone(run);
    Box::new(items.into_iter().zip(1_usize..).map(move |(item, j)| own(&run, p, &item, j)))
}

/// [GR1e ii] The row of primitive `p` of its `j`-th item `item`: the value
/// of each of its columns, null in every other column.
fn own(run: &Run, p: usize, item: &Item, j: usize) -> Made {
    let primitive = run.table.primitives.get(p).ok_or_else(|| broken("names a primitive it has not"))?;
    let mut row = vec![Value::Null; run.table.width];
    for column in &primitive.columns {
        let value = run.column(&column.kind, item, j)?;
        let place = row.get_mut(column.place).ok_or_else(|| broken("places a column past its columns"))?;
        *place = value;
    }
    Ok(row)
}

impl Run {
    /// [GR1e ii 1 to 3] The value of the column `kind` of the `j`-th row,
    /// of the item `item`; a warning its cast raised kept.
    fn column(&self, kind: &ColumnKind, item: &Item, j: usize) -> Result<Value> {
        let (value, warning) = match kind {
            // [GR1e ii 1] j.
            ColumnKind::Ordinality => return Ok(Value::Exact(Decimal::new(i128::try_from(j).unwrap_or(i128::MAX), 0))),
            // [GR1e ii 2 B to D] The path of Ij, already parsed, of no
            // PASSING clause, cast [§9.48]; its exception raised.
            ColumnKind::Regular { path, returning, on_empty, on_error } => {
                let status = path.evaluate(item, &[]);
                cast_sequence(status, &self.behavior(*on_empty)?, &self.behavior(*on_error)?, returning)?
            }
            // [GR1e ii 3 B to D] Serialized to a character string type
            // [§9.49], turborel having no JSON type.
            ColumnKind::Formatted { path, returning, wrapper, quotes, on_empty, on_error } => {
                let options = QueryOptions { returning, wrapper: *wrapper, quotes: *quotes, on_empty: *on_empty, on_error: *on_error };
                serialize_sequence(path.evaluate(item, &[]), &options)?
            }
        };
        if let Some(state) = warning {
            lock(&self.warned)?.push(state);
        }
        Ok(value)
    }

    /// [§9.48 GR4c, GR5] The behavior `slot` is, of a DEFAULT its value.
    fn behavior(&self, slot: Slot) -> Result<ValueBehavior> {
        match slot {
            Slot::Error => Ok(ValueBehavior::Error),
            Slot::Null => Ok(ValueBehavior::Null),
            Slot::Default(d) => {
                self.defaults.get(d).cloned().map(ValueBehavior::Default).ok_or_else(|| broken("is given fewer DEFAULT values than it has"))
            }
        }
    }

    /// [GR1e ii 4] The value of the chaining column of the row item `item`,
    /// the context item of the primitives it is the parent of: the item
    /// serialized as JSON text [§9.43], of a character string type as long
    /// as the text (UV085, UV123); of an exception condition, as the `<JSON
    /// table error behavior>` says [GR1e ii 4 A II], EMPTY the null value,
    /// of which those primitives give no row [NOTE 259].
    fn chained(&self, item: &Item) -> Result<Value> {
        match serialize(item) {
            Ok(text) => Ok(Value::Char(text)),
            Err(error) => self.table.on_error.caught(error).map(|()| Value::Null),
        }
    }
}

/// [SR1k iii 2, 3] Of each row of primitive `p` of `context`, the rows of
/// `child` of that row's item, each with the row's columns; of a row
/// whose item gives none, `join` OUTER the row alone [NOTE 243], INNER
/// none.
fn joined(run: &Arc<Run>, (p, join, child): (usize, Join, &Arc<Plan>), context: &Value) -> Stream {
    let items = match items(run, p, context) {
        Ok(items) => items,
        Err(error) => return failed(error),
    };
    let (run, child) = (Arc::clone(run), Arc::clone(child));
    Box::new(items.into_iter().zip(1_usize..).flat_map(move |(item, j)| parented(&run, (p, join, &child), &item, j)))
}

/// `joined`'s rows of the `j`-th item `item` of primitive `p`.
fn parented(run: &Arc<Run>, (p, join, child): (usize, Join, &Arc<Plan>), item: &Item, j: usize) -> Stream {
    let (row, chained) = match own(run, p, item, j).and_then(|row| Ok((row, run.chained(item)?))) {
        Ok(made) => made,
        Err(error) => return failed(error),
    };
    let mut children = plan_rows(run, child, chained);
    let Some(first) = children.next() else {
        return match join {
            Join::Outer => Box::new(iter::once(Ok(row))),
            Join::Inner => Box::new(iter::empty()),
        };
    };
    Box::new(iter::once(first).chain(children).map(move |made| made.map(|other| merged(&row, other))))
}

/// The row of the columns of `row` and of `other`, rows of primitives of
/// no column in common: each column's value of the one that has it.
fn merged(row: &[Value], other: Vec<Value>) -> Vec<Value> {
    row.iter().zip(other).map(|(mine, theirs)| if theirs.is_null() { mine.clone() } else { theirs }).collect()
}

/// [SR1k iii 4] A `<JSON table plan union>`, a FULL OUTER JOIN ON 0=1: each
/// operand's rows of `context` in turn, the others' columns null.
fn union(run: &Arc<Run>, operands: &[Arc<Plan>], context: Value) -> Stream {
    let (run, operands) = (Arc::clone(run), operands.to_vec());
    Box::new(operands.into_iter().flat_map(move |plan| plan_rows(&run, &plan, context.clone())))
}

/// [SR1k iii 5] A `<JSON table plan cross>`: of the operands' rows of
/// `context`, every combination of a row of each. The operands after the
/// first are read and held; if one has no row, the first is read for its
/// errors alone.
fn cross(run: &Arc<Run>, operands: &[Arc<Plan>], context: Value) -> Stream {
    let Some((first, rest)) = operands.split_first() else { return Box::new(iter::empty()) };
    if rest.is_empty() {
        return plan_rows(run, first, context);
    }
    let mut held = Vec::with_capacity(rest.len());
    for plan in rest {
        match plan_rows(run, plan, context.clone()).collect::<Result<Vec<_>>>() {
            Ok(rows) => held.push(rows),
            Err(error) => return failed(error),
        }
    }
    let firsts = plan_rows(run, first, context);
    if held.iter().any(Vec::is_empty) {
        return Box::new(firsts.filter_map(|made| made.err().map(Err)));
    }
    Box::new(Product { firsts, held, current: None, at: Vec::new() })
}

/// The rows of a CROSS of operands none of which, after the first, has no
/// row: of each row of the first, one of each combination of the held
/// rows of the rest, the last operand's varying fastest.
struct Product {
    /// The first operand's rows to come.
    firsts: Stream,
    /// The rows of each operand after it.
    held: Vec<Vec<Vec<Value>>>,
    /// The first operand's row being combined.
    current: Option<Vec<Value>>,
    /// Of each operand after the first, the place of its row in the next
    /// combination; the first's past its last when none is left.
    at: Vec<usize>,
}

impl Iterator for Product {
    type Item = Made;

    fn next(&mut self) -> Option<Made> {
        loop {
            if let Some(made) = self.combination() {
                return Some(Ok(made));
            }
            match self.firsts.next()? {
                Ok(row) => {
                    self.current = Some(row);
                    self.at = vec![0; self.held.len()];
                }
                Err(error) => return Some(Err(error)),
            }
        }
    }
}

impl Product {
    /// The current row with the held rows `at` names, `at` then moved to
    /// the next combination; none if no combination is left.
    fn combination(&mut self) -> Option<Vec<Value>> {
        let mut made = self.current.clone()?;
        for (rows, &k) in self.held.iter().zip(&self.at) {
            made = merged(&made, rows.get(k)?.clone());
        }
        self.advance();
        Some(made)
    }

    /// `at` moved to the next combination: the last place on by one, and
    /// a place past its operand's last row back to 0 and the one before it
    /// on, but the first's.
    fn advance(&mut self) {
        let mut i = self.at.len();
        while let Some(before) = i.checked_sub(1) {
            i = before;
            let rows = self.held.get(i).map_or(0, Vec::len);
            let Some(k) = self.at.get_mut(i) else { return };
            *k = k.saturating_add(1);
            if *k < rows {
                return;
            }
            if i == 0 {
                return;
            }
            *k = 0;
        }
    }
}
