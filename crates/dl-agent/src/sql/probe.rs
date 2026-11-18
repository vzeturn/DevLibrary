//! Schema probe (PROTOCOL.md §9.6): which tables, columns, views, procedures, functions and
//! configuration rows a product needs are missing from a database, before a sync fails on
//! "Invalid column name" at 3 a.m. Also classifies the SQL errors that mean "the schema is
//! not what the code expects" so modules report them as such.

use std::collections::{BTreeMap, BTreeSet};

use dl_json::{Value, json};

use super::pool::Connection;
use crate::Error;

/// Largest requirement accepted (objects + rows), well under SQL Server's 2100 parameters.
pub const MAX_ITEMS: usize = 1000;

const OBJECTS_SQL: &str = "SELECT s.name AS s, o.name AS n, RTRIM(o.type) AS t \
FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id \
WHERE o.type IN ('U','V','P','PC','X','FN','IF','TF','FS','FT','SN')";

/// A schema-qualified object name (`dbo` when none is given).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Name {
    /// Schema.
    pub schema: String,
    /// Object.
    pub name: String,
}

impl Name {
    /// Parses `name` or `schema.name`; brackets are accepted (`[dbo].[HoaDon]`).
    ///
    /// # Errors
    /// [`Error::Config`] for an empty part, a part over 128 characters or more than two parts.
    pub fn parse(s: &str) -> Result<Name, Error> {
        let parts: Vec<String> = s
            .split('.')
            .map(|p| {
                p.trim()
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .to_string()
            })
            .collect();
        let bad = |p: &String| p.is_empty() || p.chars().count() > 128;
        match parts.as_slice() {
            [n] if !bad(n) => Ok(Name {
                schema: "dbo".into(),
                name: n.clone(),
            }),
            [s, n] if !bad(s) && !bad(n) => Ok(Name {
                schema: s.clone(),
                name: n.clone(),
            }),
            _ => Err(Error::Config(format!("probe: bad object name {s:?}"))),
        }
    }

    fn key(&self) -> (String, String) {
        (self.schema.to_lowercase(), self.name.to_lowercase())
    }

    /// `[schema].[name]` with `]` doubled: safe to splice into SQL text.
    pub fn quoted(&self) -> String {
        format!("{}.{}", quote(&self.schema), quote(&self.name))
    }
}

impl std::fmt::Display for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.schema, self.name)
    }
}

fn quote(ident: &str) -> String {
    format!("[{}]", ident.replace(']', "]]"))
}

/// Configuration rows that must exist: `column` of `table` must hold every value.
#[derive(Debug, Clone, PartialEq)]
pub struct Rows {
    /// Table holding the rows.
    pub table: Name,
    /// Key column.
    pub column: String,
    /// Required key values.
    pub values: Vec<String>,
}

/// What a product needs from one database.
///
/// ```json
/// {"tables": {"dbo.HoaDon": ["Id", "SoHD"], "dbo.Config": []},
///  "views": ["dbo.vw_Bill"], "procedures": ["dbo.usp_GetBill"], "functions": ["dbo.fn_Tcvn3"],
///  "rows": [{"table": "dbo.Config", "column": "Key", "values": ["EINV_URL"]}]}
/// ```
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Requirement {
    /// Tables (or views) and the columns each must have.
    pub tables: BTreeMap<Name, Vec<String>>,
    /// Views.
    pub views: Vec<Name>,
    /// Stored procedures.
    pub procedures: Vec<Name>,
    /// Scalar or table-valued functions.
    pub functions: Vec<Name>,
    /// Configuration rows.
    pub rows: Vec<Rows>,
}

fn names(v: &Value, key: &str) -> Result<Vec<Name>, Error> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| {
                x.as_str()
                    .ok_or_else(|| Error::Config(format!("probe: {key} must hold strings")))
                    .and_then(Name::parse)
            })
            .collect(),
        Some(_) => Err(Error::Config(format!("probe: {key} must be an array"))),
    }
}

fn strings(v: Option<&Value>, what: &str) -> Result<Vec<String>, Error> {
    match v {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| {
                x.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| Error::Config(format!("probe: {what} must hold strings")))
            })
            .collect(),
        Some(_) => Err(Error::Config(format!("probe: {what} must be an array"))),
    }
}

impl Requirement {
    /// Parses the JSON form.
    ///
    /// # Errors
    /// [`Error::Config`] for a wrong shape, a bad name or more than [`MAX_ITEMS`] items.
    pub fn from_json(v: &Value) -> Result<Requirement, Error> {
        let mut req = Requirement::default();
        match v.get("tables") {
            None | Some(Value::Null) => {}
            Some(Value::Object(m)) => {
                for (k, cols) in m.iter() {
                    req.tables
                        .insert(Name::parse(k)?, strings(Some(cols), "table columns")?);
                }
            }
            Some(Value::Array(_)) => {
                for n in names(v, "tables")? {
                    req.tables.insert(n, Vec::new());
                }
            }
            Some(_) => return Err(Error::Config("probe: tables must be an object".into())),
        }
        req.views = names(v, "views")?;
        req.procedures = names(v, "procedures")?;
        req.functions = names(v, "functions")?;
        if let Some(rows) = v.get("rows").and_then(Value::as_array) {
            for r in rows {
                let table = r
                    .get("table")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::Config("probe: rows need a table".into()))
                    .and_then(Name::parse)?;
                let column = r
                    .get("column")
                    .and_then(Value::as_str)
                    .filter(|c| !c.is_empty() && c.chars().count() <= 128)
                    .ok_or_else(|| Error::Config("probe: rows need a column".into()))?
                    .to_string();
                let values = strings(r.get("values"), "row values")?;
                req.rows.push(Rows {
                    table,
                    column,
                    values,
                });
            }
        }
        let size = req.tables.values().map(|c| 1 + c.len()).sum::<usize>()
            + req.views.len()
            + req.procedures.len()
            + req.functions.len()
            + req.rows.iter().map(|r| r.values.len()).sum::<usize>();
        if size > MAX_ITEMS {
            return Err(Error::Config(format!(
                "probe: {size} items, at most {MAX_ITEMS}"
            )));
        }
        Ok(req)
    }
}

/// An object found in the database.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// Where it lives.
    pub name: Name,
    /// `sys.objects.type`, trimmed (`U`, `V`, `P`, `FN`, …).
    pub kind: String,
}

fn kind_name(t: &str) -> &'static str {
    match t {
        "U" => "table",
        "V" => "view",
        "P" | "PC" | "X" => "procedure",
        "FN" | "IF" | "TF" | "FS" | "FT" => "function",
        "SN" => "synonym",
        _ => "object",
    }
}

/// What is missing, given the objects, the columns of the required tables and the
/// configuration values present. Names compare case-insensitively; a synonym satisfies any
/// kind. An object of the wrong kind is reported with what was found instead.
pub fn compare(
    req: &Requirement,
    objects: &[Found],
    columns: &BTreeMap<(String, String), BTreeSet<String>>,
    present_rows: &[BTreeSet<String>],
) -> Vec<Value> {
    let index: BTreeMap<(String, String), &str> = objects
        .iter()
        .map(|o| (o.name.key(), o.kind.as_str()))
        .collect();
    let mut missing = Vec::new();
    let mut need = |kind: &str, n: &Name, ok: &dyn Fn(&str) -> bool| -> bool {
        match index.get(&n.key()) {
            Some(t) if *t == "SN" || ok(t) => true,
            Some(t) => {
                missing.push(json!({"kind": kind, "name": n.to_string(), "found": kind_name(t)}));
                false
            }
            None => {
                missing.push(json!({"kind": kind, "name": n.to_string()}));
                false
            }
        }
    };
    let mut tables_ok = BTreeSet::new();
    for n in req.tables.keys() {
        if need("table", n, &|t| t == "U" || t == "V") {
            tables_ok.insert(n.key());
        }
    }
    for n in &req.views {
        need("view", n, &|t| t == "V");
    }
    for n in &req.procedures {
        need("procedure", n, &|t| kind_name(t) == "procedure");
    }
    for n in &req.functions {
        need("function", n, &|t| kind_name(t) == "function");
    }
    let empty = BTreeSet::new();
    for (n, cols) in &req.tables {
        if !tables_ok.contains(&n.key()) || index.get(&n.key()) == Some(&"SN") {
            continue;
        }
        let have = columns.get(&n.key()).unwrap_or(&empty);
        for c in cols {
            if !have.contains(&c.to_lowercase()) {
                missing.push(json!({"kind": "column", "name": format!("{n}.{c}")}));
            }
        }
    }
    for (i, r) in req.rows.iter().enumerate() {
        let Some(have) = present_rows.get(i) else {
            continue; // table or column missing: already reported
        };
        for v in &r.values {
            if !have.contains(&v.to_lowercase()) {
                missing.push(json!({
                    "kind": "row", "name": format!("{}.{}", r.table, r.column), "value": v
                }));
            }
        }
    }
    missing
}

/// Probes the database behind `conn`: `{"ok", "missing": [...], "checked": n}`.
///
/// # Errors
/// The SQL error when a catalog query fails.
pub fn probe(conn: &mut impl Connection, req: &Requirement) -> Result<Value, Error> {
    let objects: Vec<Found> = conn
        .query_json(OBJECTS_SQL, &[])?
        .iter()
        .filter_map(|r| {
            Some(Found {
                name: Name {
                    schema: r.get("s")?.as_str()?.to_string(),
                    name: r.get("n")?.as_str()?.to_string(),
                },
                kind: r.get("t")?.as_str()?.trim().to_string(),
            })
        })
        .collect();
    let index: BTreeMap<(String, String), &Found> =
        objects.iter().map(|o| (o.name.key(), o)).collect();

    let mut columns: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    let with_cols: Vec<&Name> = req
        .tables
        .iter()
        .filter(|(n, c)| !c.is_empty() && index.contains_key(&n.key()))
        .map(|(n, _)| n)
        .collect();
    for chunk in with_cols.chunks(200) {
        let mut params = Vec::new();
        let mut list = Vec::new();
        for (i, n) in chunk.iter().enumerate() {
            list.push(format!("OBJECT_ID(@p{i})"));
            params.push((format!("p{i}"), n.quoted()));
        }
        let sql = format!(
            "SELECT s.name AS s, o.name AS n, c.name AS c FROM sys.columns c \
             JOIN sys.objects o ON o.object_id = c.object_id \
             JOIN sys.schemas s ON s.schema_id = o.schema_id \
             WHERE o.object_id IN ({})",
            list.join(", ")
        );
        let p: Vec<(&str, &str)> = params
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        for r in conn.query_json(&sql, &p)? {
            let (Some(s), Some(n), Some(c)) = (
                r.get("s").and_then(Value::as_str),
                r.get("n").and_then(Value::as_str),
                r.get("c").and_then(Value::as_str),
            ) else {
                continue;
            };
            columns
                .entry((s.to_lowercase(), n.to_lowercase()))
                .or_default()
                .insert(c.to_lowercase());
        }
    }

    let mut present_rows = Vec::new();
    for r in &req.rows {
        let Some(found) = index.get(&r.table.key()) else {
            break; // keep positions aligned: rows after a missing table are skipped
        };
        if !["U", "V", "SN"].contains(&found.kind.as_str()) || r.values.is_empty() {
            present_rows.push(BTreeSet::new());
            continue;
        }
        let col_ok = columns
            .get(&r.table.key())
            .is_none_or(|c| c.contains(&r.column.to_lowercase()));
        if !col_ok {
            break;
        }
        let mut params = Vec::new();
        let mut list = Vec::new();
        for (i, v) in r.values.iter().enumerate() {
            list.push(format!("@v{i}"));
            params.push((format!("v{i}"), v.clone()));
        }
        let col = quote(&r.column);
        let sql = format!(
            "SELECT DISTINCT CAST({col} AS nvarchar(4000)) AS v FROM {} \
             WHERE CAST({col} AS nvarchar(4000)) IN ({})",
            found.name.quoted(),
            list.join(", ")
        );
        let p: Vec<(&str, &str)> = params
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let have = conn
            .query_json(&sql, &p)?
            .iter()
            .filter_map(|r| r.get("v").and_then(Value::as_str).map(str::to_lowercase))
            .collect();
        present_rows.push(have);
    }

    let missing = compare(req, &objects, &columns, &present_rows);
    let checked = req.tables.values().map(|c| 1 + c.len()).sum::<usize>()
        + req.views.len()
        + req.procedures.len()
        + req.functions.len()
        + req.rows.iter().map(|r| r.values.len()).sum::<usize>();
    Ok(json!({"ok": missing.is_empty(), "missing": missing, "checked": checked as u64}))
}

/// What a SQL Server error means for the agent and its modules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// 207 invalid column, 208 invalid object, 2812 missing procedure, 4121 missing function:
    /// the schema is not what the code expects. `name` is the quoted name in the message.
    MissingObject {
        /// SQL error number.
        number: i32,
        /// The object or column named in the message, if any.
        name: Option<String>,
    },
    /// 1222: a row stayed locked past `LOCK_TIMEOUT`; retry later.
    Locked,
    /// 1205: chosen as deadlock victim; retry.
    Deadlock,
    /// 18456 login failed, 4060 cannot open database, 229/230 permission denied.
    Access {
        /// SQL error number.
        number: i32,
    },
    /// The request hit the query timeout.
    Timeout,
    /// Anything else.
    Other,
}

impl Problem {
    /// The problem-report code for it (`sql_missing_object`, `sql_locked`, …).
    pub fn code(&self) -> &'static str {
        match self {
            Problem::MissingObject { .. } => "sql_missing_object",
            Problem::Locked => "sql_locked",
            Problem::Deadlock => "sql_deadlock",
            Problem::Access { .. } => "sql_access_denied",
            Problem::Timeout => "sql_timeout",
            Problem::Other => "sql_error",
        }
    }

    /// `true` when trying again later can succeed without anyone changing anything.
    pub fn retryable(&self) -> bool {
        matches!(self, Problem::Locked | Problem::Deadlock | Problem::Timeout)
    }
}

/// Classifies an error from a session.
pub fn classify(e: &dl_tds::Error) -> Problem {
    match e {
        dl_tds::Error::Server {
            number, message, ..
        } => match number {
            207 | 208 | 2812 | 4121 => Problem::MissingObject {
                number: *number,
                name: message.split('\'').nth(1).map(str::to_string),
            },
            1222 => Problem::Locked,
            1205 => Problem::Deadlock,
            18456 | 4060 | 229 | 230 => Problem::Access { number: *number },
            _ => Problem::Other,
        },
        dl_tds::Error::Timeout => Problem::Timeout,
        _ => Problem::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(n: &str, t: &str) -> Found {
        Found {
            name: Name::parse(n).unwrap(),
            kind: t.into(),
        }
    }

    fn req() -> Requirement {
        Requirement::from_json(&json!({
            "tables": {"dbo.HoaDon": ["Id", "SoHD", "MaCQT"], "Config": [], "dbo.Gone": ["X"]},
            "views": ["dbo.vw_Bill"],
            "procedures": ["dbo.usp_GetBill", "dbo.usp_New"],
            "functions": ["[dbo].[fn_Tcvn3]"],
            "rows": [{"table": "dbo.Config", "column": "Key", "values": ["EINV_URL", "EINV_KEY"]}]
        }))
        .unwrap()
    }

    #[test]
    fn names_parse_with_default_schema_and_brackets() {
        assert_eq!(Name::parse("HoaDon").unwrap().to_string(), "dbo.HoaDon");
        assert_eq!(
            Name::parse("[sale].[Bill]").unwrap().to_string(),
            "sale.Bill"
        );
        assert_eq!(Name::parse("a]b").unwrap().quoted(), "[dbo].[a]]b]");
        assert!(Name::parse("a.b.c").is_err());
        assert!(Name::parse("").is_err());
        assert!(Requirement::from_json(&json!({"tables": 3})).is_err());
        let many: Vec<String> = (0..=MAX_ITEMS).map(|i| format!("p{i}")).collect();
        assert!(Requirement::from_json(&json!({"procedures": many})).is_err());
    }

    #[test]
    fn reports_missing_objects_columns_wrong_kinds_and_rows() {
        let objects = [
            found("dbo.hoadon", "U"),
            found("dbo.Config", "U"),
            found("dbo.vw_Bill", "U"), // a table where a view is expected
            found("dbo.usp_GetBill", "P"),
            found("dbo.fn_Tcvn3", "FN"),
        ];
        let mut cols = BTreeMap::new();
        cols.insert(
            ("dbo".to_string(), "hoadon".to_string()),
            ["id", "sohd"].iter().map(|s| s.to_string()).collect(),
        );
        let rows = [["einv_url".to_string()].into_iter().collect()];
        let m = compare(&req(), &objects, &cols, &rows);
        let flat: Vec<String> = m
            .iter()
            .map(|v| {
                format!(
                    "{}:{}{}",
                    v["kind"].as_str().unwrap(),
                    v["name"].as_str().unwrap(),
                    v.get("found")
                        .or(v.get("value"))
                        .and_then(Value::as_str)
                        .map(|s| format!("={s}"))
                        .unwrap_or_default()
                )
            })
            .collect();
        assert_eq!(
            flat,
            [
                "table:dbo.Gone",
                "view:dbo.vw_Bill=table",
                "procedure:dbo.usp_New",
                "column:dbo.HoaDon.MaCQT",
                "row:dbo.Config.Key=EINV_KEY",
            ]
        );
    }

    #[test]
    fn a_synonym_satisfies_any_kind_and_a_full_schema_is_ok() {
        let objects = [
            found("dbo.HoaDon", "SN"),
            found("dbo.Config", "U"),
            found("dbo.Gone", "U"),
            found("dbo.vw_Bill", "V"),
            found("dbo.usp_GetBill", "P"),
            found("dbo.usp_New", "SN"),
            found("dbo.fn_Tcvn3", "IF"),
        ];
        let mut cols = BTreeMap::new();
        cols.insert(
            ("dbo".to_string(), "gone".to_string()),
            ["x".to_string()].into_iter().collect(),
        );
        let rows = [["einv_url".to_string(), "einv_key".to_string()]
            .into_iter()
            .collect()];
        assert!(compare(&req(), &objects, &cols, &rows).is_empty());
    }

    #[test]
    fn classifies_schema_lock_and_access_errors() {
        let server = |number: i32, message: &str| dl_tds::Error::Server {
            number,
            state: 1,
            class: 16,
            message: message.into(),
            server: String::new(),
            procedure: String::new(),
            line: 1,
        };
        assert_eq!(
            classify(&server(207, "Invalid column name 'MaCQT'.")),
            Problem::MissingObject {
                number: 207,
                name: Some("MaCQT".into())
            }
        );
        assert_eq!(
            classify(&server(
                2812,
                "Could not find stored procedure 'dbo.usp_New'."
            ))
            .code(),
            "sql_missing_object"
        );
        let locked = classify(&server(1222, "Lock request time out period exceeded."));
        assert_eq!(locked, Problem::Locked);
        assert!(locked.retryable());
        assert_eq!(
            classify(&server(18456, "Login failed")).code(),
            "sql_access_denied"
        );
        assert_eq!(classify(&dl_tds::Error::Timeout), Problem::Timeout);
        assert!(!classify(&server(102, "Incorrect syntax")).retryable());
    }
}
