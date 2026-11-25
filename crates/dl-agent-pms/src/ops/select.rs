//! `select` calls of the ops manifest (ADR 0007, RT-0614): a read op may read a table
//! directly instead of calling a procedure, but only a table and columns the manifest
//! declares in its top-level `tables` list, with every value bound as a parameter. The SQL
//! text is built here from the validated manifest identifiers only, never from op args.

use std::collections::{HashMap, HashSet};

use dl_json::{Map, Value};

use super::{Param, SQL_TYPES, arr, e, fields, i, obj_of, parse_param, s};
use crate::exec::DEFAULT_MAX_ROWS_PER_SET;
use crate::json;
use crate::minire::Regex;
use crate::pat;

/// Table names refused whatever the manifest says: system objects and temp tables.
const TABLE_FLOOR: [&str; 3] = ["^sys", "^#", "^INFORMATION_SCHEMA$"];

/// The comparison operators a `where` entry may use.
pub const SELECT_OPS: [&str; 6] = ["=", "<>", "<", "<=", ">", ">="];

/// One table a select call may read (always in schema `dbo`).
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    /// Table name (a bare identifier).
    pub name: String,
    /// The columns that may be read or filtered on.
    pub columns: Vec<String>,
}

/// One `where` condition: `[column] op @param`.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    /// The column compared.
    pub column: String,
    /// One of [`SELECT_OPS`].
    pub op: String,
    /// The bound value (an arg or a constant, never an output).
    pub param: Param,
}

/// The aggregate functions a select may compute (ADR 0007 addendum, RT-1129).
pub const SELECT_AGGREGATES: [&str; 2] = ["sum", "count"];

/// One aggregate output: `SUM([column]) AS [alias]` or `COUNT([column]|*) AS [alias]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Aggregate {
    /// Lower-case function name, one of [`SELECT_AGGREGATES`].
    pub func: String,
    /// The declared column; empty only for `count` (meaning `COUNT(*)`).
    pub column: String,
    /// The output column name (a bare identifier).
    pub alias: String,
}

/// A direct read of one declared table.
#[derive(Debug, Clone, PartialEq)]
pub struct Select {
    /// The table (in `tables`).
    pub table: String,
    /// The columns returned, in order.
    pub columns: Vec<String>,
    /// Conditions joined with AND.
    pub filters: Vec<Filter>,
    /// Sort columns, ascending.
    pub order_by: Vec<String>,
    /// `TOP (max_rows)`, 1..=[`DEFAULT_MAX_ROWS_PER_SET`].
    pub max_rows: i64,
    /// Aggregate outputs, after `columns`; non-empty or `group_by` non-empty makes the
    /// select grouped.
    pub aggregates: Vec<Aggregate>,
    /// `GROUP BY` columns; in a grouped select every plain output column and sort column
    /// must be one of them.
    pub group_by: Vec<String>,
    /// Columns of `group_by` taken as `CAST([col] AS date)` (a day bucket of a datetime).
    pub date_bucket: Vec<String>,
}

impl Select {
    /// The statement text: identifiers bracket-quoted from the validated manifest, values as
    /// `@name` placeholders only.
    pub fn sql(&self) -> String {
        let q = |c: &String| format!("[{c}]");
        // A grouped column is a day bucket when listed in `date_bucket`.
        let key = |c: &String| {
            if self.date_bucket.iter().any(|d| d.eq_ignore_ascii_case(c)) {
                format!("CAST([{c}] AS date)")
            } else {
                q(c)
            }
        };
        let mut outs: Vec<String> = self
            .columns
            .iter()
            .map(|c| match key(c) {
                k if k == q(c) => k,
                k => format!("{k} AS {}", q(c)),
            })
            .collect();
        for a in &self.aggregates {
            let arg = if a.column.is_empty() {
                "*".to_owned()
            } else {
                q(&a.column)
            };
            outs.push(format!("{}({arg}) AS [{}]", a.func.to_uppercase(), a.alias));
        }
        let mut sql = format!(
            "SELECT TOP ({}) {} FROM [dbo].[{}]",
            self.max_rows,
            outs.join(", "),
            self.table
        );
        if !self.filters.is_empty() {
            let conds: Vec<String> = self
                .filters
                .iter()
                .map(|f| format!("[{}] {} @{}", f.column, f.op, f.param.name))
                .collect();
            sql.push_str(" WHERE ");
            sql.push_str(&conds.join(" AND "));
        }
        if !self.group_by.is_empty() {
            sql.push_str(" GROUP BY ");
            sql.push_str(&self.group_by.iter().map(key).collect::<Vec<_>>().join(", "));
        }
        if !self.order_by.is_empty() {
            sql.push_str(" ORDER BY ");
            sql.push_str(&self.order_by.iter().map(key).collect::<Vec<_>>().join(", "));
        }
        sql
    }
}

/// Declared tables by lower-case name, each with its lower-case columns.
pub(super) type Tables = HashMap<String, HashSet<String>>;

fn strings(o: &Map, k: &str, what: &str) -> Result<Vec<String>, String> {
    arr(o, k)?
        .iter()
        .map(|v| match v {
            Value::String(s) => Ok(s.clone()),
            _ => Err(e(format!("{what}.{k}: not a string"))),
        })
        .collect()
}

/// Decodes the manifest's `tables` and refuses non-bare, floor-matching or duplicate
/// names and duplicate or empty column lists.
pub(super) fn parse_tables(o: &Map) -> Result<(Vec<Table>, Tables), String> {
    let floor = TABLE_FLOOR
        .iter()
        .map(|p| Regex::new(&format!("(?i){p}")).map_err(|x| e(format!("table floor {p}: {x}"))))
        .collect::<Result<Vec<_>, _>>()?;
    let mut list = Vec::new();
    let mut by_name = Tables::new();
    for v in arr(o, "tables")? {
        let to = fields(obj_of(v, "table")?, &["name", "columns"], "table")?;
        let t = Table {
            name: s(to, "name")?,
            columns: strings(to, "columns", "table")?,
        };
        let q = json::quote(&t.name);
        if !pat::is_bare_name(&t.name) || floor.iter().any(|re| re.is_match(&t.name)) {
            return Err(e(format!("table {q} is not an allowed bare name")));
        }
        if t.columns.is_empty() {
            return Err(e(format!("table {q} declares no columns")));
        }
        let cols = unique_bare(&t.columns).map_err(|c| e(format!("table {q}: column {c}")))?;
        if by_name.insert(t.name.to_lowercase(), cols).is_some() {
            return Err(e(format!("table {q} is listed twice")));
        }
        list.push(t);
    }
    Ok((list, by_name))
}

/// The lower-case set of `cols`, or the quoted first one that is not a unique bare name.
fn unique_bare(cols: &[String]) -> Result<HashSet<String>, String> {
    let mut set = HashSet::new();
    for c in cols {
        if !pat::is_bare_name(c) || !set.insert(c.to_lowercase()) {
            return Err(format!("{} is not a unique bare name", json::quote(c)));
        }
    }
    Ok(set)
}

/// Decodes one call's `select` object.
pub(super) fn parse_select(v: &Value) -> Result<Select, String> {
    let o = fields(
        obj_of(v, "select")?,
        &[
            "table",
            "columns",
            "where",
            "order_by",
            "max_rows",
            "aggregates",
            "group_by",
            "date_bucket",
        ],
        "select",
    )?;
    let mut aggregates = Vec::new();
    for a in arr(o, "aggregates")? {
        let ao = fields(
            obj_of(a, "aggregate")?,
            &["fn", "column", "as"],
            "aggregate",
        )?;
        aggregates.push(Aggregate {
            func: s(ao, "fn")?.to_lowercase(),
            column: s(ao, "column")?,
            alias: s(ao, "as")?,
        });
    }
    let mut filters = Vec::new();
    for w in arr(o, "where")? {
        let wo = fields(obj_of(w, "where")?, &["column", "op", "param"], "where")?;
        let param = match json::field_ci(wo, "param") {
            Some(p) => parse_param(p)?,
            None => return Err(e("where: param is required")),
        };
        filters.push(Filter {
            column: s(wo, "column")?,
            op: s(wo, "op")?,
            param,
        });
    }
    Ok(Select {
        table: s(o, "table")?,
        columns: strings(o, "columns", "select")?,
        filters,
        order_by: strings(o, "order_by", "select")?,
        max_rows: i(o, "max_rows")?,
        aggregates,
        group_by: strings(o, "group_by", "select")?,
        date_bucket: strings(o, "date_bucket", "select")?,
    })
}

/// Checks a select call against the declared tables (the op is known to be a read).
pub(super) fn validate_select(sel: &Select, tables: &Tables) -> Result<(), String> {
    let tq = json::quote(&sel.table);
    let Some(cols) = pat::is_bare_name(&sel.table)
        .then(|| tables.get(&sel.table.to_lowercase()))
        .flatten()
    else {
        return Err(format!("select table {tq} is not declared in tables"));
    };
    let known = |c: &str| pat::is_bare_name(c) && cols.contains(&c.to_lowercase());
    if sel.columns.is_empty() && sel.aggregates.is_empty() {
        return Err(format!("select on {tq} names no columns"));
    }
    unique_bare(&sel.columns).map_err(|c| format!("select on {tq}: column {c}"))?;
    validate_grouping(sel, &tq, &known)?;
    let filtered = sel.filters.iter().map(|f| &f.column);
    for c in sel.columns.iter().chain(filtered).chain(&sel.order_by) {
        if !known(c) {
            return Err(format!(
                "select on {tq}: column {} is not declared",
                json::quote(c)
            ));
        }
    }
    let cap = DEFAULT_MAX_ROWS_PER_SET as i64;
    if !(1..=cap).contains(&sel.max_rows) {
        return Err(format!("select on {tq}: max_rows must be 1..={cap}"));
    }
    let mut seen = HashSet::new();
    for f in &sel.filters {
        let p = &f.param;
        if !SELECT_OPS.contains(&f.op.as_str()) {
            return Err(format!(
                "select on {tq}: op {} is not one of {}",
                json::quote(&f.op),
                SELECT_OPS.join(" ")
            ));
        }
        if !pat::is_bare_name(&p.name) || !seen.insert(p.name.to_lowercase()) {
            return Err(format!(
                "select on {tq}: parameter {} is not a unique bare name",
                json::quote(&p.name)
            ));
        }
        if !SQL_TYPES.contains(&p.typ.as_str()) {
            return Err(format!(
                "select on {tq}: {}: type {} is not bindable",
                p.name,
                json::quote(&p.typ)
            ));
        }
        if p.output || p.arg.is_empty() != p.has_value() {
            return Err(format!(
                "select on {tq}: {}: exactly one of arg or value (no output)",
                p.name
            ));
        }
    }
    Ok(())
}

/// Checks the aggregate, `group_by` and `date_bucket` parts of a select (ADR 0007 addendum).
fn validate_grouping(sel: &Select, tq: &str, known: &dyn Fn(&str) -> bool) -> Result<(), String> {
    let grouped = !sel.aggregates.is_empty() || !sel.group_by.is_empty();
    let has = |list: &[String], c: &str| list.iter().any(|g| g.eq_ignore_ascii_case(c));
    let mut names: HashSet<String> = sel
        .columns
        .iter()
        .chain(&sel.group_by)
        .map(|c| c.to_lowercase())
        .collect();
    for a in &sel.aggregates {
        if !SELECT_AGGREGATES.contains(&a.func.as_str()) {
            return Err(format!(
                "select on {tq}: aggregate fn {} is not one of {}",
                json::quote(&a.func),
                SELECT_AGGREGATES.join(" ")
            ));
        }
        let column_ok = if a.column.is_empty() {
            a.func == "count"
        } else {
            known(&a.column)
        };
        if !column_ok {
            return Err(format!(
                "select on {tq}: aggregate {} column {} is not declared",
                a.func,
                json::quote(&a.column)
            ));
        }
        if !pat::is_bare_name(&a.alias) || !names.insert(a.alias.to_lowercase()) {
            return Err(format!(
                "select on {tq}: aggregate alias {} is not a unique bare name",
                json::quote(&a.alias)
            ));
        }
    }
    unique_bare(&sel.group_by).map_err(|c| format!("select on {tq}: group_by {c}"))?;
    if sel.group_by.iter().any(|c| !known(c)) {
        return Err(format!("select on {tq}: a group_by column is not declared"));
    }
    if !grouped && !sel.date_bucket.is_empty() {
        return Err(format!("select on {tq}: date_bucket needs group_by"));
    }
    for c in &sel.date_bucket {
        if !has(&sel.group_by, c) {
            return Err(format!(
                "select on {tq}: date_bucket column {} is not in group_by",
                json::quote(c)
            ));
        }
    }
    if grouped {
        for c in sel.columns.iter().chain(&sel.order_by) {
            if !has(&sel.group_by, c) {
                return Err(format!(
                    "select on {tq}: column {} must be in group_by",
                    json::quote(c)
                ));
            }
        }
    }
    Ok(())
}
