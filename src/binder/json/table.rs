//! [ISO/IEC 9075-2:2023 §7.11] A `<JSON table>`, bound: its operands, its
//! paths parsed once, its Syntax Rules checked, and the `json::Table` that
//! makes its rows (RULES.md D84).
//!
//! It supports lateral join [§7.6 SR5]: its operands, the context item,
//! the PASSING arguments and the DEFAULTs of its columns, may reference
//! every table reference before it in its FROM clause [§7.6 SR20a]. So it
//! is a FROM item planned as a collection derived table is
//! (`binder/unnest.rs`), its operands the node's collections and the
//! `json::Table` what makes its rows of their values. Its columns are
//! every `<column name>` it contains, in the order written \[SR1l\], named
//! by its derived column list if it has one [§7.6 SR12b ii].
//!
//! Its columns clauses are walked in `json/table/columns.rs` [SR1e to
//! SR1h], and its plan made in `json/table/plan.rs` [SR1i, SR1j].

mod columns;
mod plan;

use std::sync::Arc;

use super::super::expression::nondeterministic_source;
use super::super::from::{Bounds, From, FromItem, renamed};
use super::super::unnest::{Unnested, lateral_binder, lateral_item};
use super::super::{Binder, Scope, rule};
use super::{CONTEXT_RULE, PASSED, starts_with_arguments};
use crate::ast::{JsonPathName, JsonTable, ValueExpression};
use crate::catalog::ColumnDesc;
use crate::error::{At, Result};
use crate::functions::{input_type, json_text};
use crate::json::{Table, TableBehavior, parse_path};
use crate::logical::Expr;
use crate::params::ParameterType;
use crate::value::DataType;

/// What a constraint is told it contains.
const JSON_TABLE: &str = "JSON_TABLE";

/// The rule a set function in a `<JSON table>`'s operand breaks.
const SET_FUNCTION_RULE: &str = "[ISO/IEC 9075-2:2023 §6.9 SR7] a set function in a <JSON table> is over an outer reference";

/// A `<JSON table columns clause>` walked: of the primitive it is, its
/// `<JSON table path name>`, if written, the primitive it is nested in,
/// and those nested in it, COLTREE [SR1i v].
pub(super) struct Node {
    /// The explicit `<JSON table path name>`; none of an implicit one
    /// (UV084), which no plan can name.
    name: Option<JsonPathName>,
    /// The place of the primitive it is simply contained in, CNODELIST's
    /// [SR1i iv]; none of the outermost.
    parent: Option<usize>,
    /// The places of those it simply contains, in order [NOTE 245].
    children: Vec<usize>,
}

/// [§7.11 SR1, §7.6 SR12b ii] The `<JSON table>` `table`, its first column
/// at `offset` of the FROM row: its operands bound over every item of
/// `from` [§7.6 SR20a], its columns named by its derived column list, else
/// by their `<column name>`s.
///
/// # Errors
///
/// A Syntax Rule of §7.11, §10.14, §10.12 or §9.46 broken, or the table
/// in a constraint [§9.16 SR1u], SQLCODE -103;
/// a path specification that is no path expression, -101, -102 or -103;
/// -900 of a binary string's JSON text or a value or a type turborel does
/// not implement here.
pub(in crate::binder) fn json_table_item(bounds: Bounds<'_>, table: &JsonTable, offset: usize, from: &mut From) -> Result<FromItem> {
    // [§9.16 SR1u] a potential source of non-determinism, which a
    // constraint does not contain.
    if bounds.cx.purpose.constrains() {
        return Err(nondeterministic_source("u", JSON_TABLE).or_at(table.at));
    }
    let binder = lateral_binder(bounds, from);
    let (operands, bound, mut columns) = binder.json_table(table)?;
    let reads = binder.used.into_inner();
    from.used.extend(reads.iter().copied());
    if let Some(names) = &table.derived {
        if names.len() != columns.len() {
            let msg = format!("[ISO/IEC 9075-2:2023 §7.6 SR12b ii 1 B] {} column names for {} columns", names.len(), columns.len());
            return Err(rule(msg));
        }
        renamed(&mut columns, names)?;
    }
    let unnested = Unnested::json(operands, Arc::new(bound), reads);
    Ok(lateral_item((&table.correlation, table.at), columns, offset, unnested))
}

impl Binder<'_> {
    /// [§7.11 SR1] The operands of `t`, the table they make rows of, and
    /// its columns.
    fn json_table(&self, t: &JsonTable) -> Result<(Vec<Expr>, Table, Vec<ColumnDesc>)> {
        let scope = Scope::Rows(SET_FUNCTION_RULE);
        let common = &t.common;
        // [§10.14 SR1] The context item, of a string type.
        let (context, context_type) = self.json_operand(&common.context.value, Some(ParameterType::Varying), scope)?;
        json_text(&context_type, common.context.format, CONTEXT_RULE)?;
        let (mut operands, mut types) = (vec![context], vec![context_type]);
        for argument in &common.passing {
            let taken = argument.input.format.is_some().then_some(ParameterType::Varying);
            let (e, ty) = self.json_operand(&argument.input.value, taken, scope)?;
            input_type(&ty, argument.input.format, PASSED).or_at(argument.at)?;
            operands.push(e);
            types.push(ty);
        }
        // [§10.14 SR5] JACSPATH, of the PASSING clause.
        let names: Vec<&str> = common.passing.iter().map(|argument| argument.name.as_str()).collect();
        let path = parse_path(&common.path, self.now).or_at(common.path_at)?;
        path.check_variables(&names).or_at(common.path_at)?;
        starts_with_arguments(common, &path, &types)?;
        // [SR1b] EMPTY ON ERROR if none is written.
        let on_error = t.on_error.unwrap_or(TableBehavior::Empty);
        let mut walk = columns::Walk::new(self, t, (on_error, &names, &types), scope);
        walk.clause(path, (t.name.clone(), None), &t.columns)?;
        let plan = plan::plan(t.plan.as_ref(), &walk.nodes)?;
        let passing = common.passing.iter().map(|argument| (argument.name.clone(), argument.input.format.is_some())).collect();
        let columns::Walk { primitives, defaults, descs, .. } = walk;
        operands.extend(defaults);
        let table = Table { passing, primitives, plan: Arc::new(plan), on_error, width: descs.len() };
        Ok((operands, table, descs))
    }

    /// An operand of a `<JSON table>`, bound, a `<dynamic parameter
    /// specification>` of the type `taken`, if one is given [ISO/IEC
    /// 9075:1992 §17.6 GR5].
    fn json_operand(&self, v: &ValueExpression, taken: Option<ParameterType>, scope: Scope<'_>) -> Result<(Expr, DataType)> {
        match taken {
            Some(taken) => self.typed(v, taken, scope),
            None => self.value_expression(v, scope),
        }
    }
}
