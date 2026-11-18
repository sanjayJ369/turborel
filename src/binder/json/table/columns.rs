//! [ISO/IEC 9075-2:2023 §7.11 SR1e to SR1h] The `<JSON table columns
//! clause>`s of a `<JSON table>`, walked in the order written: each a
//! primitive, CNODELIST's next [SR1i iv], each column definition checked
//! and placed among the table's columns \[SR1l\].
//!
//! The parser nests columns clauses at most `MAX_NESTING_DEPTH` deep, so
//! the walk's recursion is bounded (RULES.md R-CF-6).

use super::super::super::{Binder, Scope, rule};
use super::super::{returnable, starts_with_arguments};
use super::Node;
use crate::ast::{
    JsonFormattedColumn, JsonPathName, JsonPathText, JsonRegularColumn, JsonTable, JsonTableColumn, JsonValueBehavior, TypeName,
};
use crate::binder::composite::integer;
use crate::catalog::ColumnDesc;
use crate::error::{At, Result};
use crate::functions::binary_json;
use crate::json::{
    Accessor, Column, ColumnKind, Expr as PathExpr, Mode, Path, Primitive, QueryBehavior, Quotes, Slot, TableBehavior, Wrapper, parse_path,
};
use crate::logical::Expr;
use crate::params::ParameterType;
use crate::value::DataType;

/// What the columns clauses of a `<JSON table>` are bound to, as they are
/// walked.
pub(super) struct Walk<'w, 'a> {
    /// The binder of its operands.
    binder: &'w Binder<'a>,
    /// The `<JSON table>`.
    table: &'w JsonTable,
    /// JTEB, its explicit or implicit `<JSON table error behavior>`.
    on_error: TableBehavior,
    /// Its PASSING arguments' identifiers.
    names: &'w [&'w str],
    /// The declared types of its context item and PASSING arguments.
    types: &'w [DataType],
    /// What its operands are bound in.
    scope: Scope<'w>,
    /// Its primitives, CNODELIST, so far.
    pub(super) primitives: Vec<Primitive>,
    /// Of each primitive, its node of COLTREE.
    pub(super) nodes: Vec<Node>,
    /// The DEFAULTs' value expressions, bound, so far.
    pub(super) defaults: Vec<Expr>,
    /// Its columns so far, in the order written.
    pub(super) descs: Vec<ColumnDesc>,
    /// Each `<column name>` and `<JSON table path name>` written so far,
    /// and where \[SR1h\].
    written: Vec<(String, usize)>,
}

/// [§7.11 SR1e ii] The type of a regular column, `written`: a `<predefined
/// type>` of a character string, a number, a truth value or a datetime.
///
/// # Errors
///
/// SQLCODE -103 if it is not.
fn regular_type(written: &TypeName) -> Result<DataType> {
    let TypeName::Data(t) = written else { return Err(rule(REGULAR_RULE)) };
    if !returnable(t) {
        return Err(rule(format!("{REGULAR_RULE}, not {t}")));
    }
    Ok(t.clone())
}

/// The Syntax Rule a regular column's type breaks if it is none it may
/// be.
const REGULAR_RULE: &str = "[ISO/IEC 9075-2:2023 §7.11 SR1e ii] the type of a regular column of JSON_TABLE is a <predefined type> of a character string, a number, a truth value or a datetime";

/// [§7.11 SR1f iv] The type of a formatted column, `written`, FORMAT JSON:
/// a string type.
///
/// # Errors
///
/// SQLCODE -103 if it is none; -900 if it is a binary string type, whose
/// encodings turborel does not implement.
fn formatted_type(written: &TypeName) -> Result<DataType> {
    let TypeName::Data(t) = written else {
        return Err(rule(
            "[ISO/IEC 9075-2:2023 §7.11 SR1f iv] the type of a formatted column of JSON_TABLE, FORMAT JSON, is a string type",
        ));
    };
    if t.is_binary() {
        return Err(binary_json(&format!("a formatted column of JSON_TABLE of {t}")));
    }
    if !t.is_character() {
        return Err(rule(format!(
            "[ISO/IEC 9075-2:2023 §7.11 SR1f iv] the type of a formatted column of JSON_TABLE, FORMAT JSON, is a string type, not {t}"
        )));
    }
    Ok(t.clone())
}

/// [§7.11 SR1e v 2, SR1f xii 2] The implicit path of the column `name`:
/// `$.` and a JSON string whose value is its name, the `<column name>`'s
/// value as SQL defines it, upper case of a `<regular identifier>`
/// [ISO/IEC 9075:1992 §5.2 SR10], so `name` reads the member `NAME` and
/// `"name"` the member `name`. The rule writes no `<JSON path mode>`,
/// which a `<JSON path expression>` has [§9.46 F]; it is lax, as an
/// accessor of no member then gives no item, for ON EMPTY to judge.
fn implicit_path(name: &str) -> Path {
    Path { mode: Mode::Lax, expr: PathExpr::Access(Box::new(PathExpr::Context), Accessor::Member(name.to_owned())) }
}

/// The column `name` of type `data_type`, which may be null.
fn described(name: &str, data_type: DataType) -> ColumnDesc {
    ColumnDesc { name: name.to_owned(), data_type, not_null: false, default: None, domain: None, collation: None, generated: None }
}

impl<'w, 'a> Walk<'w, 'a> {
    /// The walk of `table`'s columns clauses, its operands bound by
    /// `binder` in `scope`, of JTEB and its PASSING arguments' identifiers
    /// and their types after its context item's.
    pub(super) fn new(
        binder: &'w Binder<'a>,
        table: &'w JsonTable,
        (on_error, names, types): (TableBehavior, &'w [&'w str], &'w [DataType]),
        scope: Scope<'w>,
    ) -> Walk<'w, 'a> {
        Walk {
            binder,
            table,
            on_error,
            names,
            types,
            scope,
            primitives: Vec::new(),
            nodes: Vec::new(),
            defaults: Vec::new(),
            descs: Vec::new(),
            written: Vec::new(),
        }
    }

    /// \[SR1h\] That `name`, written at `at`, is no other `<column name>` or
    /// `<JSON table path name>` of the table's, which it then is.
    fn named(&mut self, name: &str, at: usize) -> Result<()> {
        if self.written.iter().any(|(other, _)| other == name) {
            return Err(rule(format!(
                "[ISO/IEC 9075-2:2023 §7.11 SR1h] {name} is the name of two of the columns and paths of a JSON_TABLE, which are distinct"
            ))
            .or_at(at));
        }
        self.written.push((name.to_owned(), at));
        Ok(())
    }

    /// The `<JSON table columns clause>` `columns`, of the path `path`, and
    /// `named`, its path name, if written, and the primitive it is nested
    /// in, if any: its primitive's place among them.
    pub(super) fn clause(
        &mut self,
        path: Path,
        named: (Option<JsonPathName>, Option<usize>),
        columns: &[JsonTableColumn],
    ) -> Result<usize> {
        let (path_name, parent) = named;
        if let Some(n) = &path_name {
            self.named(&n.name, n.at)?;
        }
        let place = self.primitives.len();
        self.primitives.push(Primitive { path, columns: Vec::new() });
        self.nodes.push(Node { name: path_name, parent, children: Vec::new() });
        for definition in columns {
            let made = match definition {
                JsonTableColumn::Ordinality(name, at) => {
                    self.named(name, *at)?;
                    Some((name, integer(), ColumnKind::Ordinality))
                }
                JsonTableColumn::Regular(r) => Some(self.regular(r)?),
                JsonTableColumn::Formatted(f) => Some(self.formatted(f)?),
                JsonTableColumn::Nested(n) => {
                    let nested = self.nested_path(&n.path, n.name.as_ref(), n.at)?;
                    let child = self.clause(nested, (n.name.clone(), Some(place)), &n.columns)?;
                    if let Some(node) = self.nodes.get_mut(place) {
                        node.children.push(child);
                    }
                    None
                }
            };
            let Some((column_name, data_type, kind)) = made else { continue };
            let column = Column { place: self.descs.len(), kind };
            self.descs.push(described(column_name, data_type));
            if let Some(primitive) = self.primitives.get_mut(place) {
                primitive.columns.push(column);
            }
        }
        Ok(place)
    }

    /// \[SR1g\] The `<JSON table nested path specification>` `written` of
    /// `<JSON table nested columns>` written at `at`, of the path name
    /// `name`: parsed, of the PASSING clause of the `<JSON API common
    /// syntax>`, which the primitive it is the path of reads [SR1k iii 6
    /// D, §10.14 SR5]. SR1g v cites JTRCPATH, a regular column's path, and
    /// the zero-length PASSING clause, which is read as of this path and
    /// of the clause the transformation gives it.
    ///
    /// # Errors
    ///
    /// SQLCODE -103 if the table has a plan clause and `name` is none
    /// \[SR1g i\], or a named variable of the path no argument has [§9.46
    /// SR4, SR8]; else as `parse_path`.
    fn nested_path(&self, written: &JsonPathText, name: Option<&JsonPathName>, at: usize) -> Result<Path> {
        if self.table.plan.is_some() && name.is_none() {
            return Err(rule("[ISO/IEC 9075-2:2023 §7.11 SR1g i] NESTED of a JSON_TABLE of a PLAN clause has AS a <JSON table path name>")
                .or_at(at));
        }
        let path = parse_path(&written.text, self.binder.now).or_at(written.at)?;
        path.check_variables(self.names).or_at(written.at)?;
        starts_with_arguments(&self.table.common, &path, self.types)?;
        Ok(path)
    }

    /// [SR1e v, vi; SR1f xii, xiii] The path of the column `name`: the one
    /// written, parsed, of the zero-length PASSING clause; else the
    /// implicit one.
    ///
    /// # Errors
    ///
    /// SQLCODE -103 if the path written has a named variable [§9.46 SR4];
    /// else as `parse_path`.
    fn column_path(&self, written: Option<&JsonPathText>, name: &str) -> Result<Path> {
        let Some(written) = written else { return Ok(implicit_path(name)) };
        let path = parse_path(&written.text, self.binder.now).or_at(written.at)?;
        path.check_variables(&[]).or_at(written.at)?;
        Ok(path)
    }

    /// [SR1e iv 1, SR1f xi 1] Whether a column's implicit behavior ON
    /// ERROR is ERROR: of ERROR ON ERROR; else it is NULL [SR1e iv 2, SR1f
    /// xi 2].
    fn errs(&self) -> bool {
        self.on_error == TableBehavior::Error
    }

    /// \[SR1e\] The regular column `r`: its name, type and kind.
    fn regular<'c>(&mut self, r: &'c JsonRegularColumn) -> Result<(&'c String, DataType, ColumnKind)> {
        self.named(&r.name, r.at)?;
        let returning = regular_type(&r.data_type).or_at(r.at)?;
        let path = self.column_path(r.path.as_ref(), &r.name)?;
        // [SR1e iii] NULL ON EMPTY if none is written.
        let on_empty = self.slot(r.on_empty.as_ref(), Slot::Null, &returning)?;
        let on_error = self.slot(r.on_error.as_ref(), if self.errs() { Slot::Error } else { Slot::Null }, &returning)?;
        Ok((&r.name, returning.clone(), ColumnKind::Regular { path, returning, on_empty, on_error }))
    }

    /// A regular column's behavior `written`, of the type `to`, else
    /// `implicit`: a DEFAULT's value expression bound, a `?` of the type
    /// [ISO/IEC 9075:1992 §17.6 GR5], and cast to it, which CAST is to take
    /// [§9.48 GR4c iii, GR5d].
    fn slot(&mut self, written: Option<&JsonValueBehavior>, implicit: Slot, to: &DataType) -> Result<Slot> {
        match written {
            None => Ok(implicit),
            Some(JsonValueBehavior::Error) => Ok(Slot::Error),
            Some(JsonValueBehavior::Null) => Ok(Slot::Null),
            Some(JsonValueBehavior::Default(v)) => {
                let (e, t) = self.binder.typed(v, ParameterType::Of(to.clone()), self.scope)?;
                self.binder.cast_bound((e.clone(), t), to.clone())?;
                self.defaults.push(e);
                Ok(Slot::Default(self.defaults.len().saturating_sub(1)))
            }
        }
    }

    /// \[SR1f\] The formatted column `f`: its name, type and kind.
    fn formatted<'c>(&mut self, f: &'c JsonFormattedColumn) -> Result<(&'c String, DataType, ColumnKind)> {
        self.named(&f.name, f.at)?;
        // [SR1f ii 1] turborel has no JSON type, so FORMAT is written.
        if f.format.is_none() {
            return Err(rule(
                "[ISO/IEC 9075-2:2023 §7.11 SR1f ii 1] a formatted column of JSON_TABLE, of a wrapper, quotes or EMPTY behavior, of no JSON type, has FORMAT JSON",
            )
            .or_at(f.at));
        }
        let returning = formatted_type(&f.data_type).or_at(f.at)?;
        // [SR1f vii] no quotes behavior WITH a wrapper.
        if let (Some(Wrapper::Conditional | Wrapper::Unconditional), Some((_, at))) = (f.wrapper, f.quotes) {
            return Err(rule(
                "[ISO/IEC 9075-2:2023 §7.11 SR1f vii] a formatted column of JSON_TABLE WITH a wrapper has no quotes behavior",
            )
            .or_at(at));
        }
        let path = self.column_path(f.path.as_ref(), &f.name)?;
        let kind = ColumnKind::Formatted {
            path,
            returning: returning.clone(),
            // [SR1f v, SR1f vi] WITHOUT ARRAY if none is written, WITH
            // UNCONDITIONAL of WITH alone, as the parser reads it.
            wrapper: f.wrapper.unwrap_or(Wrapper::Without),
            // [SR1f viii] KEEP.
            quotes: f.quotes.map_or(Quotes::Keep, |(quotes, _)| quotes),
            // [SR1f x] NULL ON EMPTY.
            on_empty: f.on_empty.unwrap_or(QueryBehavior::Null),
            on_error: f.on_error.unwrap_or(if self.errs() { QueryBehavior::Error } else { QueryBehavior::Null }),
        };
        Ok((&f.name, returning, kind))
    }
}
