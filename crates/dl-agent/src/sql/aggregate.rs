//! `sql.aggregate` (PROTOCOL.md §9.9): a fixed allow-list of aggregate queries an operator
//! (or an AI) can run on a site database without any SQL of its own, so figures such as the
//! distinct codes of a status column or the daily money total of a Z report can be measured
//! on site. The allow-list is compiled into the agent, so it is covered by the release
//! signature of the agent build: a request can only pick a query kind and name a table and
//! columns, which are resolved against the catalog and quoted from the catalog's spelling.
//! No row-level data leaves the server: code columns must be short (a status, a type), a
//! result has at most [`MAX_GROUPS`] groups, and money is only ever summed.

use dl_json::{Value, json};

use super::inventory::quote;
use super::pool::Connection;
use super::probe::Name;
use crate::Error;

/// Most groups (distinct values, days) one query may return; more is refused, never cut.
pub const MAX_GROUPS: usize = 400;
/// Most columns one `count_by` may group by.
pub const MAX_GROUP_COLUMNS: usize = 3;
/// Smallest group a row may represent: groups with fewer rows are left out of every answer,
/// so no single row (or its amount) can be read back, whatever column is asked.
pub const MIN_GROUP: u32 = 5;
/// Widest code column (bytes) `distinct` and `count_by` accept: a status, not a name.
pub const MAX_CODE_BYTES: i64 = 40;

/// The query kinds, the whole allow-list.
pub const KINDS: [&str; 4] = ["distinct", "count_by", "sign_sum", "daily_total"];

const COLUMNS_SQL: &str = "SELECT c.name AS n, TYPE_NAME(c.user_type_id) AS t, \
CAST(c.max_length AS nvarchar(10)) AS l, s.name AS s, o.name AS o \
FROM sys.columns c JOIN sys.tables o ON o.object_id = c.object_id \
JOIN sys.schemas s ON s.schema_id = o.schema_id \
WHERE LOWER(s.name) = LOWER(@s) AND LOWER(o.name) = LOWER(@o) AND o.is_ms_shipped = 0";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Code,
    Amount,
    Date,
}

struct Col {
    name: String,
}

struct Table {
    schema: String,
    name: String,
    cols: Vec<(String, String, i64)>,
}

fn bad(m: &str) -> Error {
    Error::Config(format!("aggregate: {m}"))
}

fn fits(role: Role, ty: &str, bytes: i64) -> bool {
    let t = ty.to_lowercase();
    match role {
        Role::Code => match t.as_str() {
            "tinyint" | "smallint" | "int" | "bigint" | "bit" => true,
            "char" | "varchar" | "nchar" | "nvarchar" => (1..=MAX_CODE_BYTES).contains(&bytes),
            _ => false,
        },
        Role::Amount => matches!(
            t.as_str(),
            "decimal" | "numeric" | "money" | "smallmoney" | "float" | "real"
        ),
        Role::Date => matches!(
            t.as_str(),
            "datetime" | "smalldatetime" | "date" | "datetime2"
        ),
    }
}

fn load(conn: &mut impl Connection, table: &str) -> Result<Table, Error> {
    let n = Name::parse(table)?;
    let rows = conn.query_json(COLUMNS_SQL, &[("s", &n.schema), ("o", &n.name)])?;
    let mut t = Table {
        schema: String::new(),
        name: String::new(),
        cols: Vec::new(),
    };
    for r in &rows {
        let g = |k: &str| {
            r.get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("")
        };
        t.schema = g("s").to_string();
        t.name = g("o").to_string();
        t.cols.push((
            g("n").to_string(),
            g("t").to_string(),
            g("l").parse().unwrap_or(0),
        ));
    }
    let mut tables: Vec<(&str, &str)> = rows
        .iter()
        .map(|r| {
            let g = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("");
            (g("s"), g("o"))
        })
        .collect();
    tables.sort_unstable();
    tables.dedup();
    if tables.len() > 1 {
        return Err(bad(&format!(
            "table {table:?} is ambiguous (case-only differences)"
        )));
    }
    if t.cols.is_empty() {
        return Err(bad(&format!("no table {table:?}")));
    }
    Ok(t)
}

impl Table {
    fn col(&self, args: &Value, key: &str, role: Role) -> Result<Col, Error> {
        let want = args
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| bad(&format!("{key} must be a column name")))?;
        self.pick(want, role)
    }

    fn pick(&self, want: &str, role: Role) -> Result<Col, Error> {
        let (name, ty, len) = self
            .cols
            .iter()
            .find(|(n, _, _)| n.eq_ignore_ascii_case(want.trim()))
            .ok_or_else(|| bad(&format!("no column {want:?}")))?;
        if !fits(role, ty, *len) {
            return Err(bad(&format!(
                "column {name} ({ty}) is not allowed here: only short codes, money and dates"
            )));
        }
        Ok(Col { name: name.clone() })
    }

    fn quoted(&self) -> String {
        quote(&self.schema, &self.name)
    }
}

fn q(c: &Col) -> String {
    format!("[{}]", c.name.replace(']', "]]"))
}

fn date_arg(args: &Value, key: &str) -> Result<Option<String>, Error> {
    let Some(v) = args.get(key).filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let s = v
        .as_str()
        .ok_or_else(|| bad(&format!("{key} must be YYYY-MM-DD")))?;
    let b = s.as_bytes();
    let ok = b.len() == 10
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            _ => c.is_ascii_digit(),
        });
    if ok {
        Ok(Some(s.to_string()))
    } else {
        Err(bad(&format!("{key} must be YYYY-MM-DD")))
    }
}

fn run(
    conn: &mut impl Connection,
    sql: &str,
    params: &[(&str, &str)],
) -> Result<Vec<Value>, Error> {
    let rows = conn.query_json(sql, params)?;
    if rows.len() > MAX_GROUPS {
        return Err(bad(&format!(
            "more than {MAX_GROUPS} groups: narrow the question (not a code column?)"
        )));
    }
    Ok(rows)
}

/// Runs the allow-listed query `args.query` and returns `{"query","table","rows":[…]}`.
/// `{"list": true}` returns the allow-list instead. Anything else is refused.
///
/// # Errors
/// [`Error::Config`] (`aggregate: …`) for a query outside the allow-list, an unknown table or
/// column, a column of a type the query kind does not accept, or too many groups; the SQL
/// error when the server refuses the query.
pub fn aggregate(conn: &mut impl Connection, args: &Value) -> Result<Value, Error> {
    if args.get("list").and_then(Value::as_bool) == Some(true) {
        return Ok(json!({"queries": KINDS.iter().map(|k| Value::from(*k)).collect::<Vec<_>>()}));
    }
    let kind = args.get("query").and_then(Value::as_str).unwrap_or("");
    if !KINDS.contains(&kind) {
        return Err(bad(&format!(
            "query {kind:?} is not in the allow-list ({})",
            KINDS.join(", ")
        )));
    }
    let table = args.get("table").and_then(Value::as_str).unwrap_or("");
    let t = load(conn, table)?;
    let from_t = t.quoted();
    let rows = match kind {
        "distinct" => {
            let c = t.col(args, "column", Role::Code)?;
            let sql = format!(
                "SELECT CAST({c0} AS nvarchar(60)) AS v, CAST(COUNT_BIG(*) AS nvarchar(20)) AS n \
                 FROM {from_t} GROUP BY {c0} HAVING COUNT_BIG(*) >= {MIN_GROUP} ORDER BY {c0}",
                c0 = q(&c)
            );
            run(conn, &sql, &[])?
        }
        "count_by" => {
            let names = args
                .get("columns")
                .and_then(Value::as_array)
                .filter(|a| (1..=MAX_GROUP_COLUMNS).contains(&a.len()))
                .ok_or_else(|| bad("columns must list 1 to 3 column names"))?;
            let mut cols = Vec::new();
            for n in names {
                let n = n.as_str().ok_or_else(|| bad("column names must be text"))?;
                cols.push(t.pick(n, Role::Code)?);
            }
            let sel: Vec<String> = cols
                .iter()
                .enumerate()
                .map(|(i, c)| format!("CAST({} AS nvarchar(60)) AS c{i}", q(c)))
                .collect();
            let grp: Vec<String> = cols.iter().map(q).collect();
            let sql = format!(
                "SELECT {}, CAST(COUNT_BIG(*) AS nvarchar(20)) AS n FROM {from_t} GROUP BY {} \
                 HAVING COUNT_BIG(*) >= {MIN_GROUP} ORDER BY {}",
                sel.join(", "),
                grp.join(", "),
                grp.join(", ")
            );
            run(conn, &sql, &[])?
        }
        "sign_sum" => {
            let k = t.col(args, "type_column", Role::Code)?;
            let a = t.col(args, "amount_column", Role::Amount)?;
            let sql = format!(
                "SELECT CAST({k0} AS nvarchar(60)) AS v, \
                 CAST(SIGN(SUM(CAST({a0} AS float))) AS nvarchar(5)) AS sign, \
                 CAST(SUM(CASE WHEN {a0} < 0 THEN 1 ELSE 0 END) AS nvarchar(20)) AS negative, \
                 CAST(SUM(CASE WHEN {a0} > 0 THEN 1 ELSE 0 END) AS nvarchar(20)) AS positive, \
                 CAST(COUNT_BIG(*) AS nvarchar(20)) AS n FROM {from_t} GROUP BY {k0} HAVING COUNT_BIG(*) >= {MIN_GROUP} ORDER BY {k0}",
                k0 = q(&k),
                a0 = q(&a)
            );
            run(conn, &sql, &[])?
        }
        _ => {
            let d = t.col(args, "date_column", Role::Date)?;
            let a = t.col(args, "amount_column", Role::Amount)?;
            let (from, to) = (date_arg(args, "from")?, date_arg(args, "to")?);
            let day = format!("CONVERT(char(10), {}, 120)", q(&d));
            let mut wh = Vec::new();
            let mut params: Vec<(&str, &str)> = Vec::new();
            if let Some(f) = &from {
                wh.push(format!("{day} >= @from"));
                params.push(("from", f));
            }
            if let Some(x) = &to {
                wh.push(format!("{day} <= @to"));
                params.push(("to", x));
            }
            let wh = if wh.is_empty() {
                String::new()
            } else {
                format!(" WHERE {}", wh.join(" AND "))
            };
            let sql = format!(
                "SELECT {day} AS day, CAST(SUM(CAST({a0} AS decimal(38,4))) AS nvarchar(40)) AS total, \
                 CAST(COUNT_BIG(*) AS nvarchar(20)) AS n FROM {from_t}{wh} GROUP BY {day} HAVING COUNT_BIG(*) >= {MIN_GROUP} ORDER BY {day}",
                a0 = q(&a)
            );
            run(conn, &sql, &params)?
        }
    };
    Ok(json!({"query": kind, "table": format!("{}.{}", t.schema, t.name), "rows": rows}))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Db {
        queries: Vec<String>,
        groups: usize,
    }

    impl Connection for Db {
        fn ping(&mut self) -> Result<(), Error> {
            Ok(())
        }
        fn is_broken(&self) -> bool {
            false
        }
        fn in_transaction(&self) -> bool {
            false
        }
        fn set_query_timeout(&mut self, _: Option<std::time::Duration>) -> Result<(), Error> {
            Ok(())
        }
        fn query_json(&mut self, sql: &str, p: &[(&str, &str)]) -> Result<Vec<Value>, Error> {
            self.queries.push(sql.to_string());
            if sql.contains("FROM sys.columns") {
                let t = p
                    .iter()
                    .find(|(k, _)| *k == "o")
                    .map(|(_, v)| v.to_lowercase());
                if t.as_deref() != Some("cashiershift") {
                    return Ok(vec![]);
                }
                let c = |n: &str, t: &str, l: &str| json!({"n": n, "t": t, "l": l, "s": "dbo", "o": "CashierShift"});
                return Ok(vec![
                    c("ShiftStatus", "tinyint", "1"),
                    c("Name", "nvarchar", "200"),
                    c("TrnAmount", "money", "8"),
                    c("TrnType", "char", "2"),
                    c("Closed", "datetime", "8"),
                ]);
            }
            Ok((0..self.groups)
                .map(|i| json!({"v": i.to_string(), "n": "1"}))
                .collect())
        }
    }

    fn db() -> Db {
        Db {
            queries: vec![],
            groups: 2,
        }
    }

    #[test]
    fn distinct_runs_on_a_short_code_column() {
        let mut d = db();
        let v = aggregate(
            &mut d,
            &json!({"query": "distinct", "table": "CashierShift", "column": "shiftstatus"}),
        )
        .unwrap();
        assert_eq!(v["rows"].as_array().map(Vec::len), Some(2));
        let sql = d.queries.last().unwrap();
        assert!(
            sql.contains("FROM [dbo].[CashierShift] GROUP BY [ShiftStatus]"),
            "{sql}"
        );
    }

    #[test]
    fn refuses_any_query_outside_the_allow_list_and_never_runs_it() {
        for args in [
            json!({"query": "SELECT * FROM CashierShift", "table": "CashierShift"}),
            json!({"query": "raw", "sql": "DROP TABLE x", "table": "CashierShift"}),
            json!({"table": "CashierShift"}),
        ] {
            let mut d = db();
            let e = aggregate(&mut d, &args).unwrap_err().to_string();
            assert!(e.contains("not in the allow-list"), "{e}");
            assert!(d.queries.is_empty(), "no SQL may run");
        }
    }

    #[test]
    fn injection_in_names_never_reaches_the_sql_text() {
        let mut d = db();
        let e = aggregate(
            &mut d,
            &json!({"query": "distinct", "table": "CashierShift", "column": "ShiftStatus]; DROP TABLE x;--"}),
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("no column"), "{e}");
        let e = aggregate(
            &mut d,
            &json!({"query": "distinct", "table": "Nope]; DROP", "column": "a"}),
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("no table"), "{e}");
        assert!(d.queries.iter().all(|s| !s.contains("DROP")));
    }

    #[test]
    fn wide_text_columns_and_wrong_types_are_refused() {
        let mut d = db();
        let e = aggregate(
            &mut d,
            &json!({"query": "distinct", "table": "CashierShift", "column": "Name"}),
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("not allowed"), "{e}");
        let e = aggregate(
            &mut d,
            &json!({"query": "sign_sum", "table": "CashierShift", "type_column": "TrnType", "amount_column": "TrnType"}),
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("not allowed"), "{e}");
    }

    #[test]
    fn sign_sum_and_daily_total_bind_dates_as_parameters() {
        let mut d = db();
        aggregate(
            &mut d,
            &json!({"query": "sign_sum", "table": "CashierShift", "type_column": "TrnType", "amount_column": "TrnAmount"}),
        )
        .unwrap();
        assert!(d.queries.last().unwrap().contains("SIGN(SUM("));
        aggregate(
            &mut d,
            &json!({"query": "daily_total", "table": "CashierShift", "date_column": "Closed",
                    "amount_column": "TrnAmount", "from": "2026-11-01", "to": "2026-11-23"}),
        )
        .unwrap();
        let sql = d.queries.last().unwrap();
        assert!(
            sql.contains(">= @from") && sql.contains("<= @to") && !sql.contains("2026"),
            "{sql}"
        );
        let e = aggregate(
            &mut d,
            &json!({"query": "daily_total", "table": "CashierShift", "date_column": "Closed",
                    "amount_column": "TrnAmount", "from": "x' OR 1=1"}),
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("YYYY-MM-DD"), "{e}");
    }

    #[test]
    fn too_many_groups_is_refused_not_cut() {
        let mut d = Db {
            queries: vec![],
            groups: MAX_GROUPS + 1,
        };
        let e = aggregate(
            &mut d,
            &json!({"query": "count_by", "table": "CashierShift", "columns": ["ShiftStatus", "TrnType"]}),
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("more than"), "{e}");
    }

    #[test]
    fn small_groups_are_left_out_and_only_money_is_summed() {
        let mut d = db();
        for a in [
            json!({"query": "distinct", "table": "CashierShift", "column": "ShiftStatus"}),
            json!({"query": "count_by", "table": "CashierShift", "columns": ["ShiftStatus"]}),
            json!({"query": "sign_sum", "table": "CashierShift", "type_column": "TrnType", "amount_column": "TrnAmount"}),
            json!({"query": "daily_total", "table": "CashierShift", "date_column": "Closed", "amount_column": "TrnAmount"}),
        ] {
            aggregate(&mut d, &a).unwrap();
            assert!(
                d.queries
                    .last()
                    .unwrap()
                    .contains("HAVING COUNT_BIG(*) >= 5")
            );
        }
        // an integer column is a code, never an amount
        let e = aggregate(
            &mut d,
            &json!({"query": "daily_total", "table": "CashierShift", "date_column": "Closed", "amount_column": "ShiftStatus"}),
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("not allowed"), "{e}");
    }

    #[test]
    fn list_returns_the_allow_list_without_touching_sql() {
        let mut d = db();
        let v = aggregate(&mut d, &json!({"list": true})).unwrap();
        assert_eq!(v["queries"].as_array().map(Vec::len), Some(4));
        assert!(d.queries.is_empty());
    }
}
