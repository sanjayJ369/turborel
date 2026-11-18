//! Abstract syntax of JSON_TABLE [ISO/IEC 9075-2:2023 §7.11], a `<JSON
//! table>` and its `<correlation or recognition>` [§7.6], a
//! `TableReference::JsonTable`: its `<JSON API common syntax>` [§10.14],
//! its `<JSON table columns clause>`, at any depth of nesting, its `<JSON
//! table plan clause>` and its `<JSON table error behavior>`.
//!
//! A clause not written is none here; the binder applies the Syntax Rules
//! that make one implicit.

use super::{JsonCommon, JsonFormat, JsonValueBehavior, Subquery, TypeName, ValueExpression};
use crate::json::{Join, QueryBehavior, Quotes, Siblings, TableBehavior, Wrapper};

/// [§7.11] `<JSON table>` ::= JSON_TABLE ( `<JSON API common syntax>`
/// `<JSON table columns clause>` \[`<JSON table plan clause>`\] \[`<JSON
/// table error behavior>` ON ERROR\] ), and the `<correlation name>` and
/// `<derived column list>` of its `<table primary>` [§7.6].
#[derive(Debug, Clone)]
pub struct JsonTable {
    /// The `<JSON API common syntax>` but its `<JSON table path name>`.
    pub common: JsonCommon,
    /// AS `<JSON table path name>` of the `<JSON API common syntax>`,
    /// JACSPN, if written [§10.14 SR2b].
    pub name: Option<JsonPathName>,
    /// The `<JSON table column definition>`s of its `<JSON table columns
    /// clause>`, in order.
    pub columns: Vec<JsonTableColumn>,
    /// The `<JSON table plan clause>`, if written.
    pub plan: Option<JsonPlanClause>,
    /// The `<JSON table error behavior>`, if written.
    pub on_error: Option<TableBehavior>,
    /// The `<correlation name>`.
    pub correlation: String,
    /// The `<derived column list>`, if written.
    pub derived: Option<Vec<String>>,
    /// Where JSON_TABLE is written.
    pub at: usize,
}

/// [§7.11] A `<JSON table path name>`, an `<identifier>`: its value, and
/// where it is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonPathName {
    /// A `<regular identifier>` in upper case, a `<delimited
    /// identifier>`'s body.
    pub name: String,
    /// Where it is written.
    pub at: usize,
}

/// [§7.11] A `<JSON path specification>` of a column or of `<JSON table
/// nested columns>`, a `<character string literal>`: its value, the path
/// expression's text, and where it is written.
#[derive(Debug, Clone)]
pub struct JsonPathText {
    /// The text of the path expression.
    pub text: String,
    /// Where it is written.
    pub at: usize,
}

/// [§7.11] `<JSON table column definition>`.
#[derive(Debug, Clone)]
pub enum JsonTableColumn {
    /// `<JSON table ordinality column definition>` ::= `<column name>` FOR
    /// ORDINALITY: its name and where it is written.
    Ordinality(String, usize),
    /// `<JSON table regular column definition>`.
    Regular(Box<JsonRegularColumn>),
    /// `<JSON table formatted column definition>`.
    Formatted(Box<JsonFormattedColumn>),
    /// `<JSON table nested columns>`.
    Nested(Box<JsonNestedColumns>),
}

/// [§7.11] `<JSON table regular column definition>` ::= `<column name>`
/// `<data type>` \[PATH `<JSON table column path specification>`\]
/// \[`<JSON table column empty behavior>` ON EMPTY\] \[`<JSON table column
/// error behavior>` ON ERROR\].
#[derive(Debug, Clone)]
pub struct JsonRegularColumn {
    /// The `<column name>`.
    pub name: String,
    /// Where the `<column name>` is written.
    pub at: usize,
    /// The `<data type>`.
    pub data_type: TypeName,
    /// The path specification after PATH, if written.
    pub path: Option<JsonPathText>,
    /// The behavior ON EMPTY.
    pub on_empty: Option<JsonValueBehavior>,
    /// The behavior ON ERROR.
    pub on_error: Option<JsonValueBehavior>,
}

/// [§7.11] `<JSON table formatted column definition>` ::= `<column name>`
/// `<data type>` \[FORMAT `<JSON representation>`\] \[PATH `<JSON table
/// column path specification>`\] \[`<JSON table formatted column wrapper
/// behavior>` WRAPPER\] \[`<JSON table formatted column quotes behavior>`
/// QUOTES \[ON SCALAR STRING\]\] \[`<JSON table formatted column empty
/// behavior>` ON EMPTY\] \[`<JSON table formatted column error behavior>`
/// ON ERROR\].
#[derive(Debug, Clone)]
pub struct JsonFormattedColumn {
    /// The `<column name>`.
    pub name: String,
    /// Where the `<column name>` is written.
    pub at: usize,
    /// The `<data type>`.
    pub data_type: TypeName,
    /// The `<JSON representation>` after FORMAT, if written.
    pub format: Option<JsonFormat>,
    /// The path specification after PATH, if written.
    pub path: Option<JsonPathText>,
    /// The wrapper behavior, WITH without CONDITIONAL or UNCONDITIONAL
    /// read as UNCONDITIONAL [SR1f vi].
    pub wrapper: Option<Wrapper>,
    /// The quotes behavior, and where it is written.
    pub quotes: Option<(Quotes, usize)>,
    /// The behavior ON EMPTY.
    pub on_empty: Option<QueryBehavior>,
    /// The behavior ON ERROR.
    pub on_error: Option<QueryBehavior>,
}

/// [§7.11] `<JSON table nested columns>` ::= NESTED \[PATH\] `<JSON table
/// nested path specification>` \[AS `<JSON table nested path name>`\]
/// `<JSON table columns clause>`.
#[derive(Debug, Clone)]
pub struct JsonNestedColumns {
    /// The `<JSON table nested path specification>`.
    pub path: JsonPathText,
    /// The `<JSON table nested path name>`, if written.
    pub name: Option<JsonPathName>,
    /// The `<JSON table column definition>`s of its `<JSON table columns
    /// clause>`, in order.
    pub columns: Vec<JsonTableColumn>,
    /// Where NESTED is written.
    pub at: usize,
}

/// [§7.11] `<JSON table plan clause>`.
#[derive(Debug, Clone)]
pub enum JsonPlanClause {
    /// `<JSON table specific plan>` ::= PLAN ( `<JSON table plan>` ), and
    /// where PLAN is written.
    Specific(JsonTablePlan, usize),
    /// `<JSON table default plan>` ::= PLAN DEFAULT ( `<JSON table default
    /// plan choices>` ): INNER if written, else OUTER; CROSS if written,
    /// else UNION [SR1i i, SR1i ii].
    Default(Join, Siblings),
}

/// [§7.11] `<JSON table plan>`, and the `<JSON table plan primary>`s within
/// it, a parenthesized plan read as the plan it holds.
#[derive(Debug, Clone)]
pub enum JsonTablePlan {
    /// `<JSON table path name>`.
    Name(JsonPathName),
    /// `<JSON table plan outer>` or `<JSON table plan inner>`: the `<JSON
    /// table path name>`, OUTER or INNER, and the `<JSON table plan
    /// primary>`.
    Join(JsonPathName, Join, Box<JsonTablePlan>),
    /// `<JSON table plan union>` or `<JSON table plan cross>`: the `<JSON
    /// table plan primary>`s, two or more.
    Siblings(Siblings, Vec<JsonTablePlan>),
}

impl JsonTable {
    /// Calls `f` with each value expression of it: the context item's, the
    /// PASSING arguments' in order, then each DEFAULT's in the order the
    /// columns are written, at any depth, ON EMPTY's before ON ERROR's.
    pub fn for_each_value<'a>(&'a self, f: &mut dyn FnMut(&'a ValueExpression)) {
        f(&self.common.context.value);
        self.common.passing.iter().for_each(|argument| f(&argument.input.value));
        for_each_default(&self.columns, f);
    }

    /// The subqueries of its value expressions, in the order
    /// `for_each_value` gives the expressions.
    pub fn subqueries(&self) -> Vec<&Subquery> {
        let mut subqueries = Vec::new();
        self.for_each_value(&mut |v| v.for_each_subquery(&mut |s| subqueries.push(s)));
        subqueries
    }
}

/// `JsonTable::for_each_value` of the DEFAULTs of `columns`. The parser
/// nests `<JSON table columns clause>`s at most `MAX_NESTING_DEPTH` deep,
/// so the recursion is bounded (RULES.md R-CF-6).
fn for_each_default<'a>(columns: &'a [JsonTableColumn], f: &mut dyn FnMut(&'a ValueExpression)) {
    for column in columns {
        match column {
            JsonTableColumn::Regular(regular) => {
                for behavior in [&regular.on_empty, &regular.on_error].into_iter().flatten() {
                    if let JsonValueBehavior::Default(value) = behavior {
                        f(value);
                    }
                }
            }
            JsonTableColumn::Nested(nested) => for_each_default(&nested.columns, f),
            JsonTableColumn::Ordinality(..) | JsonTableColumn::Formatted(_) => {}
        }
    }
}
