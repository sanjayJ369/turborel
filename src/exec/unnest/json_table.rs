//! [ISO/IEC 9075-2:2023 §7.11] A `<JSON table>`, run: of each row of its
//! input, its operands evaluated and the rows the table makes of them
//! (`json::Table::rows`), each after the input row's columns \[SR1m\]. Of
//! the second operand of a LEFT join, an input row none of whose rows its
//! condition makes true is kept, its own columns null [§7.10 GR1d].
//!
//! The rows of one input row are made as they are read, none held but as
//! the table holds them, and each is taken only while the statement is not
//! cancelled (RULES.md D44). A warning a column's cast raises is the
//! statement's [ISO/IEC 9075:1992 §3.3.4.1], as a CAST's is.

use std::sync::Arc;

use super::super::eval::warn;
use super::super::{RowIter, Runner, eval, eval_cond};
use crate::error::Result;
use crate::json::{Operands, Rows, Table};
use crate::logical::{Truth, Unnest};
use crate::storage::Row;
use crate::value::Value;

/// The rows of the JSON table `table`, `u`, over the rows `input`, its
/// operands and condition evaluated by `runner`.
pub(super) fn joined(u: Unnest<()>, table: Arc<Table>, input: RowIter, runner: Runner) -> impl Iterator<Item = Result<Row>> + Send {
    JsonRows { u, table, input, runner, current: None }
}

/// The rows of a JSON table over its input's.
struct JsonRows {
    /// The node, apart from its input.
    u: Unnest<()>,
    /// The table.
    table: Arc<Table>,
    /// The input's rows to come.
    input: RowIter,
    /// What evaluates the operands and the condition.
    runner: Runner,
    /// The input row whose rows are being made.
    current: Option<Current>,
}

/// An input row and the rows the table makes of it.
struct Current {
    /// The input row.
    row: Row,
    /// The table's rows of it to come.
    rows: Rows,
    /// Whether one of them was given, of a LEFT join: whose condition it
    /// made true.
    kept: bool,
}

impl Iterator for JsonRows {
    type Item = Result<Row>;

    fn next(&mut self) -> Option<Result<Row>> {
        loop {
            let Some(current) = &mut self.current else {
                let started = self.input.next()?.and_then(|row| self.start(row));
                match started {
                    Ok(current) => self.current = Some(current),
                    Err(error) => return Some(Err(error)),
                }
                continue;
            };
            let made = current.rows.next();
            if let Err(error) = self.warned() {
                return Some(Err(error));
            }
            let found = match made {
                Some(made) => self.given(made).transpose(),
                None => self.current.take().and_then(|done| self.padded(done)).map(Ok),
            };
            if found.is_some() {
                return found;
            }
        }
    }
}

impl JsonRows {
    /// The table's rows of the input row `row`, its operands' values
    /// evaluated of it: the context item's, the PASSING arguments', then
    /// the DEFAULTs'.
    fn start(&self, row: Row) -> Result<Current> {
        let values = self.u.collections.iter().map(|e| eval(e, &row, &self.runner)).collect::<Result<Vec<_>>>()?;
        let mut values = values.into_iter();
        let context = values.next().unwrap_or(Value::Null);
        let passing = values.by_ref().take(self.table.passing.len()).collect();
        let rows = self.table.rows(Operands { context, passing, defaults: values.collect() });
        Ok(Current { row, rows, kept: false })
    }

    /// The row of the input row and the table's row `made`, if it is
    /// given: of a LEFT join only if its condition makes it true.
    fn given(&mut self, made: Result<Vec<Value>>) -> Result<Option<Row>> {
        let Some(current) = &mut self.current else { return Ok(None) };
        let own = made?;
        let mut row = Vec::with_capacity(current.row.len().saturating_add(own.len()));
        row.extend(current.row.iter().cloned());
        row.extend(own);
        let Some(cond) = &self.u.outer else { return Ok(Some(row)) };
        if eval_cond(cond, &row, &self.runner)? != Truth::True {
            return Ok(None);
        }
        current.kept = true;
        Ok(Some(row))
    }

    /// The warnings the casts of the current input row's columns raised,
    /// raised.
    fn warned(&self) -> Result<()> {
        let Some(current) = &self.current else { return Ok(()) };
        for state in current.rows.warnings()? {
            warn(&self.runner, state);
        }
        Ok(())
    }

    /// [§7.10 GR1d] Of a LEFT join, the input row of `done`, its own
    /// columns null, if none of the table's rows was given; else none.
    fn padded(&self, done: Current) -> Option<Row> {
        if self.u.outer.is_none() || done.kept {
            return None;
        }
        let mut padded = done.row;
        padded.resize(padded.len().saturating_add(self.table.width), Value::Null);
        Some(padded)
    }
}

#[cfg(test)]
mod tests;
