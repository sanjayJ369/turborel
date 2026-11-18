//! [ISO/IEC 9075-2:2023 §7.11] JSON_TABLE, a `<JSON table>`, read as a
//! `<table primary>` [§7.6] with its `<correlation or recognition>`.
//!
//! JSON_TABLE is no key word of SQL-92, so it is the `<JSON table>` only
//! before "(", as the query operators' names are; COLUMNS, NESTED, PATH,
//! ORDINALITY and PLAN are words only where the Format has them, so a
//! table or a column of any of those names stays one. NESTED begins
//! `<JSON table nested columns>` only before a `<character string
//! literal>`, or PATH and one; else it is a column's name.
//!
//! A regular and a formatted column definition begin alike [§7.11 F]: a
//! column with FORMAT, a wrapper or a quotes behavior, or an ON EMPTY or
//! ON ERROR of EMPTY ARRAY or EMPTY OBJECT, is formatted; any other is
//! regular, as a formatted one of none of those would break SR1f ii 1.

use super::super::Parser;
use super::super::composite::ORDINALITY;
use super::super::modules::PATH;
use super::{EMPTY, ERROR, PASSING};
use crate::ast::{
    JsonCommon, JsonFormattedColumn, JsonNestedColumns, JsonPathName, JsonPathText, JsonPlanClause, JsonRegularColumn, JsonTable,
    JsonTableColumn, JsonTablePlan, JsonValueBehavior, TableReference, ValueExpression,
};
use crate::error::Result;
use crate::functions::FORMAT;
use crate::json::{Join, QueryBehavior, Siblings, TableBehavior};
use crate::lexer::{Tok, delim, kw};
use crate::value::types::array::ARRAY_NAME;

// [ISO/IEC 9075-2:2023 §7.11 F] The words of a <JSON table> that are no
// key words of SQL-92.
/// JSON_TABLE.
const JSON_TABLE: &str = "JSON_TABLE";
/// COLUMNS, of a `<JSON table columns clause>`.
const COLUMNS: &str = "COLUMNS";
/// NESTED, of `<JSON table nested columns>`.
const NESTED: &str = "NESTED";
/// PLAN, of a `<JSON table plan clause>`.
const PLAN: &str = "PLAN";
/// OBJECT, of EMPTY OBJECT.
const OBJECT: &str = "OBJECT";

/// [§7.11] A column's behavior ON EMPTY or ON ERROR, read before it is
/// known whether the column is regular or formatted.
enum Read {
    /// ERROR or NULL, of either.
    Simple(Simple),
    /// DEFAULT `<value expression>`, of a regular column.
    Default(Box<ValueExpression>),
    /// EMPTY ARRAY or EMPTY OBJECT, of a formatted column.
    Empty(QueryBehavior),
}

/// ERROR or NULL, a behavior of a regular and of a formatted column.
#[derive(Clone, Copy)]
enum Simple {
    /// ERROR.
    Error,
    /// NULL.
    Null,
}

impl Simple {
    /// As a regular column's behavior.
    fn value(self) -> JsonValueBehavior {
        match self {
            Simple::Error => JsonValueBehavior::Error,
            Simple::Null => JsonValueBehavior::Null,
        }
    }

    /// As a formatted column's behavior.
    fn query(self) -> QueryBehavior {
        match self {
            Simple::Error => QueryBehavior::Error,
            Simple::Null => QueryBehavior::Null,
        }
    }
}

/// The behaviors ON EMPTY and ON ERROR of a column, and so its kind.
enum Behaviors {
    /// Of a regular column.
    Regular(Option<JsonValueBehavior>, Option<JsonValueBehavior>),
    /// Of a formatted column.
    Formatted(Option<QueryBehavior>, Option<QueryBehavior>),
}

/// The behaviors of a column of the one behavior `first`, ON ERROR.
fn on_error_alone(first: Read) -> Behaviors {
    match first {
        Read::Simple(s) => Behaviors::Regular(None, Some(s.value())),
        Read::Default(v) => Behaviors::Regular(None, Some(JsonValueBehavior::Default(v))),
        Read::Empty(q) => Behaviors::Formatted(None, Some(q)),
    }
}

impl Parser {
    /// Whether a `<JSON table>` begins here: JSON_TABLE before "(".
    pub(in crate::parser) fn at_json_table(&self) -> bool {
        self.at_json(JSON_TABLE)
    }

    /// [§7.11] `<JSON table>`, its name next, then [§7.6] its
    /// `<correlation or recognition>`: \[AS\] `<correlation name>` \[(
    /// `<derived column list>` )\], the correlation name not optional.
    /// A `<row pattern recognition clause and name>` is read and refused,
    /// SQLCODE -900, as the row pattern input table is bound apart from the
    /// table references before it, which a `<JSON table>` may read.
    pub(in crate::parser) fn json_table(&mut self) -> Result<TableReference> {
        let at = self.here();
        self.advance();
        self.expect_delim(delim::LEFT_PAREN)?;
        let (common, name) = self.json_table_common()?;
        let columns = self.json_table_columns()?;
        let plan = self.json_table_plan_clause()?;
        let on_error = self.json_table_error()?;
        self.expect_delim(delim::RIGHT_PAREN)?;
        let correlation = self.correlation()?;
        let recognized = self.at_match_recognize();
        let Some(correlation) = correlation.or_else(|| recognized.then(String::new)) else {
            return Err(self.syntax("[ISO/IEC 9075-2:2023 §7.6 F] a <JSON table> has a correlation name"));
        };
        let derived = if correlation.is_empty() { None } else { self.derived_columns()? };
        let table = TableReference::JsonTable(Box::new(JsonTable { common, name, columns, plan, on_error, correlation, derived, at }));
        if !self.at_match_recognize() {
            return Ok(table);
        }
        let refused = self.unsupported("a <row pattern recognition clause> of a <JSON table>");
        self.refuse_later(refused);
        self.recognized(table)
    }

    /// [§10.14] `<JSON API common syntax>` ::= `<JSON context item>` ,
    /// `<JSON path specification>` \[AS `<JSON table path name>`\]
    /// \[`<JSON passing clause>`\], and its path name apart.
    fn json_table_common(&mut self) -> Result<(JsonCommon, Option<JsonPathName>)> {
        let context = self.json_input()?;
        self.expect_delim(delim::COMMA)?;
        let path = self.json_path_text()?;
        let name = if self.accept_kw(kw::AS) { Some(self.json_path_name()?) } else { None };
        let mut passing = Vec::new();
        if self.accept_word(PASSING) {
            passing.push(self.json_argument()?);
            while self.accept_delim(delim::COMMA) {
                passing.push(self.json_argument()?);
            }
        }
        Ok((JsonCommon { context, path: path.text, path_at: path.at, passing }, name))
    }

    /// [§10.14] A `<JSON path specification>`, a `<character string
    /// literal>`.
    fn json_path_text(&mut self) -> Result<JsonPathText> {
        let at = self.here();
        let Tok::CharacterString(text) = self.peek().clone() else {
            return Err(self.syntax("expected a <JSON path specification>, a <character string literal>"));
        };
        self.advance();
        Ok(JsonPathText { text, at })
    }

    /// [§7.11] `<JSON table path name>` ::= `<identifier>`.
    fn json_path_name(&mut self) -> Result<JsonPathName> {
        let at = self.here();
        self.identifier().map(|name| JsonPathName { name, at })
    }

    /// [§7.11] `<JSON table columns clause>` ::= COLUMNS ( `<JSON table
    /// column definition>` \[, ...\] ).
    fn json_table_columns(&mut self) -> Result<Vec<JsonTableColumn>> {
        self.expect_word(COLUMNS)?;
        self.expect_delim(delim::LEFT_PAREN)?;
        let mut columns = vec![self.json_table_column()?];
        while self.accept_delim(delim::COMMA) {
            columns.push(self.json_table_column()?);
        }
        self.expect_delim(delim::RIGHT_PAREN)?;
        Ok(columns)
    }

    /// Whether `<JSON table nested columns>` begin here: NESTED before a
    /// `<character string literal>`, or before PATH and one.
    fn at_nested(&self) -> bool {
        let path = Tok::Identifier(PATH.to_owned());
        let literal = |n: usize| matches!(self.peek_at(n), Tok::CharacterString(_));
        self.is_word(NESTED) && (literal(1) || (*self.peek_at(1) == path && literal(2)))
    }

    /// [§7.11] `<JSON table column definition>`.
    fn json_table_column(&mut self) -> Result<JsonTableColumn> {
        if self.at_nested() {
            return self.json_nested_columns();
        }
        let at = self.here();
        let name = self.identifier()?;
        // <JSON table ordinality column definition> ::= <column name> FOR
        // ORDINALITY
        if self.accept_kw(kw::FOR) {
            self.expect_word(ORDINALITY)?;
            return Ok(JsonTableColumn::Ordinality(name, at));
        }
        self.json_typed_column(name, at)
    }

    /// [§7.11] `<JSON table nested columns>` ::= NESTED \[PATH\] `<JSON
    /// table nested path specification>` \[AS `<JSON table nested path
    /// name>`\] `<JSON table columns clause>`, NESTED next.
    fn json_nested_columns(&mut self) -> Result<JsonTableColumn> {
        let at = self.here();
        self.advance();
        self.accept_word(PATH);
        let path = self.json_path_text()?;
        let name = if self.accept_kw(kw::AS) { Some(self.json_path_name()?) } else { None };
        let columns = self.nested(Self::json_table_columns)?;
        Ok(JsonTableColumn::Nested(Box::new(JsonNestedColumns { path, name, columns, at })))
    }

    /// [§7.11] A `<JSON table regular column definition>` or a `<JSON table
    /// formatted column definition>`, after its `<column name>` `name`,
    /// written at `at`.
    fn json_typed_column(&mut self, name: String, at: usize) -> Result<JsonTableColumn> {
        let data_type = self.type_name()?;
        let format = if self.accept_word(FORMAT) { Some(self.json_representation()?) } else { None };
        let path = if self.accept_word(PATH) { Some(self.json_path_text()?) } else { None };
        let wrapper = self.wrapper_behavior()?;
        let quotes_at = self.here();
        let quotes = self.quotes_behavior()?.map(|q| (q, quotes_at));
        let behaviors = if format.is_some() || wrapper.is_some() || quotes.is_some() {
            let (on_empty, on_error) = self.behaviors(Self::query_behavior)?;
            Behaviors::Formatted(on_empty, on_error)
        } else {
            self.column_behaviors()?
        };
        Ok(match behaviors {
            Behaviors::Regular(on_empty, on_error) => {
                JsonTableColumn::Regular(Box::new(JsonRegularColumn { name, at, data_type, path, on_empty, on_error }))
            }
            Behaviors::Formatted(on_empty, on_error) => JsonTableColumn::Formatted(Box::new(JsonFormattedColumn {
                name,
                at,
                data_type,
                format,
                path,
                wrapper,
                quotes,
                on_empty,
                on_error,
            })),
        })
    }

    /// \[`<behavior>` ON EMPTY\] \[`<behavior>` ON ERROR\] of a column of
    /// no FORMAT, wrapper or quotes behavior: of a regular column, unless
    /// EMPTY ARRAY or EMPTY OBJECT is written, the first behavior deciding
    /// what the second may be.
    fn column_behaviors(&mut self) -> Result<Behaviors> {
        let Some(first) = self.read_behavior()? else { return Ok(Behaviors::Regular(None, None)) };
        self.expect_kw(kw::ON)?;
        if self.accept_word(ERROR) {
            return Ok(on_error_alone(first));
        }
        if !self.accept_word(EMPTY) {
            return Err(self.syntax("expected EMPTY or ERROR"));
        }
        Ok(match first {
            Read::Default(v) => Behaviors::Regular(Some(JsonValueBehavior::Default(v)), self.on_error_after(Self::value_behavior)?),
            Read::Empty(q) => Behaviors::Formatted(Some(q), self.on_error_after(Self::query_behavior)?),
            Read::Simple(s) => match self.on_error_after(Self::read_behavior)? {
                None => Behaviors::Regular(Some(s.value()), None),
                Some(Read::Simple(t)) => Behaviors::Regular(Some(s.value()), Some(t.value())),
                Some(Read::Default(v)) => Behaviors::Regular(Some(s.value()), Some(JsonValueBehavior::Default(v))),
                Some(Read::Empty(q)) => Behaviors::Formatted(Some(s.query()), Some(q)),
            },
        })
    }

    /// \[`<behavior>` ON ERROR\] after ON EMPTY, the behavior as `read`
    /// reads one.
    fn on_error_after<T>(&mut self, read: fn(&mut Parser) -> Result<Option<T>>) -> Result<Option<T>> {
        let Some(behavior) = read(self)? else { return Ok(None) };
        self.expect_kw(kw::ON)?;
        self.expect_word(ERROR)?;
        Ok(Some(behavior))
    }

    /// A column's behavior, ERROR | NULL | DEFAULT `<value expression>` |
    /// EMPTY ARRAY | EMPTY OBJECT, if one is next.
    fn read_behavior(&mut self) -> Result<Option<Read>> {
        if self.accept_word(ERROR) {
            return Ok(Some(Read::Simple(Simple::Error)));
        }
        if self.accept_kw(kw::NULL) {
            return Ok(Some(Read::Simple(Simple::Null)));
        }
        if self.accept_kw(kw::DEFAULT) {
            return Ok(Some(Read::Default(Box::new(self.value_expression()?))));
        }
        if !self.accept_word(EMPTY) {
            return Ok(None);
        }
        if self.accept_word(ARRAY_NAME) {
            return Ok(Some(Read::Empty(QueryBehavior::EmptyArray)));
        }
        self.expect_word(OBJECT)?;
        Ok(Some(Read::Empty(QueryBehavior::EmptyObject)))
    }

    /// [§7.11] \[`<JSON table plan clause>`\].
    fn json_table_plan_clause(&mut self) -> Result<Option<JsonPlanClause>> {
        let at = self.here();
        if !self.accept_word(PLAN) {
            return Ok(None);
        }
        if self.accept_kw(kw::DEFAULT) {
            return self.json_default_plan().map(Some);
        }
        self.expect_delim(delim::LEFT_PAREN)?;
        let plan = self.nested(Self::json_table_plan)?;
        self.expect_delim(delim::RIGHT_PAREN)?;
        Ok(Some(JsonPlanClause::Specific(plan, at)))
    }

    /// [§7.11] `<JSON table default plan>` after PLAN DEFAULT: ( `<JSON
    /// table default plan choices>` ).
    fn json_default_plan(&mut self) -> Result<JsonPlanClause> {
        self.expect_delim(delim::LEFT_PAREN)?;
        let choices = if let Some(join) = self.join_choice() {
            let siblings = if self.accept_delim(delim::COMMA) {
                self.siblings_choice().ok_or_else(|| self.syntax("expected UNION or CROSS"))?
            } else {
                Siblings::Union
            };
            (join, siblings)
        } else if let Some(siblings) = self.siblings_choice() {
            let join = if self.accept_delim(delim::COMMA) {
                self.join_choice().ok_or_else(|| self.syntax("expected INNER or OUTER"))?
            } else {
                Join::Outer
            };
            (join, siblings)
        } else {
            return Err(self.syntax("expected INNER, OUTER, UNION or CROSS"));
        };
        self.expect_delim(delim::RIGHT_PAREN)?;
        Ok(JsonPlanClause::Default(choices.0, choices.1))
    }

    /// `<JSON table default plan inner/outer>`, INNER or OUTER, if next.
    fn join_choice(&mut self) -> Option<Join> {
        if self.accept_kw(kw::INNER) {
            Some(Join::Inner)
        } else if self.accept_kw(kw::OUTER) {
            Some(Join::Outer)
        } else {
            None
        }
    }

    /// `<JSON table default plan union/cross>`, UNION or CROSS, if next.
    fn siblings_choice(&mut self) -> Option<Siblings> {
        if self.accept_kw(kw::UNION) {
            Some(Siblings::Union)
        } else if self.accept_kw(kw::CROSS) {
            Some(Siblings::Cross)
        } else {
            None
        }
    }

    /// [§7.11] `<JSON table plan>`: a `<JSON table path name>`, a `<JSON
    /// table plan parent/child>`, which begins with one, or a `<JSON table
    /// plan sibling>`, which begins with a `<JSON table plan primary>`.
    fn json_table_plan(&mut self) -> Result<JsonTablePlan> {
        if self.is_delim(delim::LEFT_PAREN) {
            let first = self.json_plan_primary()?;
            return self.json_plan_siblings(first);
        }
        let name = self.json_path_name()?;
        if let Some(join) = self.join_choice() {
            let primary = self.json_plan_primary()?;
            return Ok(JsonTablePlan::Join(name, join, Box::new(primary)));
        }
        if self.is_kw(kw::UNION) || self.is_kw(kw::CROSS) {
            return self.json_plan_siblings(JsonTablePlan::Name(name));
        }
        Ok(JsonTablePlan::Name(name))
    }

    /// [§7.11] `<JSON table plan union>` or `<JSON table plan cross>`, after
    /// its first `<JSON table plan primary>`, `first`: UNION or CROSS and
    /// a primary, then as many more of the same word as are written.
    fn json_plan_siblings(&mut self, first: JsonTablePlan) -> Result<JsonTablePlan> {
        let Some(siblings) = self.siblings_choice() else { return Err(self.syntax("expected UNION or CROSS")) };
        let word = match siblings {
            Siblings::Union => kw::UNION,
            Siblings::Cross => kw::CROSS,
        };
        let mut operands = vec![first, self.json_plan_primary()?];
        while self.accept_kw(word) {
            operands.push(self.json_plan_primary()?);
        }
        Ok(JsonTablePlan::Siblings(siblings, operands))
    }

    /// [§7.11] `<JSON table plan primary>` ::= `<JSON table path name>` | (
    /// `<JSON table plan>` ).
    fn json_plan_primary(&mut self) -> Result<JsonTablePlan> {
        if !self.accept_delim(delim::LEFT_PAREN) {
            return self.json_path_name().map(JsonTablePlan::Name);
        }
        let plan = self.nested(Self::json_table_plan)?;
        self.expect_delim(delim::RIGHT_PAREN)?;
        Ok(plan)
    }

    /// [§7.11] \[`<JSON table error behavior>` ON ERROR\], ERROR or EMPTY.
    fn json_table_error(&mut self) -> Result<Option<TableBehavior>> {
        let behavior = if self.accept_word(ERROR) {
            TableBehavior::Error
        } else if self.accept_word(EMPTY) {
            TableBehavior::Empty
        } else {
            return Ok(None);
        };
        self.expect_kw(kw::ON)?;
        self.expect_word(ERROR)?;
        Ok(Some(behavior))
    }
}
