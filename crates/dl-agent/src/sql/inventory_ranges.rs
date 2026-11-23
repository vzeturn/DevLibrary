//! Date ranges for `sql.inventory {ranges}` (PROTOCOL.md §9.8): for each named table, the
//! smallest and largest value of every date-typed column, the count of non-null values and,
//! with `by_year`, the count per calendar year. Aggregates of date columns only: no row and
//! no other column ever leaves the server, so the date span of a site's bookings or folios
//! can be checked without exporting guest data.

use dl_json::{Map, Value};

use super::inventory::quote;
use super::pool::Connection;
use super::probe::Name;
use crate::Error;

/// Most tables one `ranges` list may name.
pub const MAX_RANGE_TABLES: usize = 20;

/// Date-typed columns of one table (catalog only). `@t` is the quoted table name.
pub const DATE_COLUMNS_SQL: &str = "SELECT c.name AS n FROM sys.columns c \
WHERE c.object_id = OBJECT_ID(@t) AND TYPE_NAME(c.system_type_id) IN \
('date','datetime','datetime2','smalldatetime','datetimeoffset') ORDER BY c.column_id";

/// What `ranges` asked for.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Ranges {
    /// Tables, in the order given.
    pub tables: Vec<Name>,
    /// Add the count per calendar year.
    pub by_year: bool,
}

impl Ranges {
    /// Reads `ranges` (a list of `schema.table` names) and `by_year` from the arguments;
    /// `None` when `ranges` is absent.
    ///
    /// # Errors
    /// [`Error::Config`] for a value that is not a list of names, more than
    /// [`MAX_RANGE_TABLES`] names, or a `by_year` that is not a boolean.
    pub fn from_args(args: &Value) -> Result<Option<Ranges>, Error> {
        let bad = |m: &str| Error::Config(format!("inventory: {m}"));
        let items = match args.get("ranges") {
            None | Some(Value::Null) => return Ok(None),
            Some(Value::Array(items)) => items,
            Some(_) => return Err(bad("ranges must be a list of tables")),
        };
        if items.is_empty() || items.len() > MAX_RANGE_TABLES {
            return Err(bad(&format!("ranges takes 1..={MAX_RANGE_TABLES} tables")));
        }
        let mut tables = Vec::with_capacity(items.len());
        for i in items {
            let s = i.as_str().ok_or_else(|| bad("ranges names must be text"))?;
            tables.push(Name::parse(s)?);
        }
        let by_year = match args.get("by_year") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err(bad("by_year must be true or false")),
        };
        Ok(Some(Ranges { tables, by_year }))
    }
}

fn text(row: &Value, k: &str) -> Option<String> {
    row.get(k)
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
}

fn num(row: &Value, k: &str) -> Value {
    text(row, k)
        .and_then(|s| s.parse::<i64>().ok())
        .map_or(Value::Null, Value::from)
}

/// `{"ranges": {"dbo.Folio": {"ArrivalDate": {"min","max","count","years"?}}},
///   "ranges_missing": [...]}`: a table that does not exist (or has no date column) is listed
/// in `ranges_missing`; a failed read only marks that column (`error`).
///
/// # Errors
/// Never for a single table; the signature leaves room for a connection-level failure.
pub fn ranges(conn: &mut impl Connection, r: &Ranges) -> Result<Value, Error> {
    let mut out = Map::new();
    let mut missing = Vec::new();
    for t in &r.tables {
        let q = quote(&t.schema, &t.name);
        let key = format!("{}.{}", t.schema, t.name);
        let cols: Vec<String> = conn
            .query_json(DATE_COLUMNS_SQL, &[("t", q.as_str())])?
            .iter()
            .filter_map(|row| text(row, "n"))
            .collect();
        if cols.is_empty() {
            missing.push(Value::from(key));
            continue;
        }
        let mut entry = Map::new();
        for c in cols {
            entry.insert(c.clone(), Value::Object(column(conn, &q, &c, r.by_year)));
        }
        out.insert(key, Value::Object(entry));
    }
    let mut answer = Map::new();
    answer.insert("ranges", Value::Object(out));
    answer.insert("ranges_missing", Value::Array(missing));
    Ok(Value::Object(answer))
}

fn column(conn: &mut impl Connection, table: &str, col: &str, by_year: bool) -> Map {
    let c = format!("[{}]", col.replace(']', "]]"));
    let mut m = Map::new();
    let sql = format!(
        "SELECT CONVERT(nvarchar(10), MIN({c}), 120) AS lo, CONVERT(nvarchar(10), MAX({c}), 120) AS hi, \
         CAST(COUNT_BIG({c}) AS nvarchar(20)) AS n FROM {table}"
    );
    match conn.query_json(&sql, &[]) {
        Ok(rows) => {
            let row = rows.into_iter().next().unwrap_or(Value::Null);
            m.insert("min", text(&row, "lo").map_or(Value::Null, Value::from));
            m.insert("max", text(&row, "hi").map_or(Value::Null, Value::from));
            m.insert("count", num(&row, "n"));
        }
        Err(e) => {
            m.insert("error", e.to_string());
            return m;
        }
    }
    if by_year {
        let sql = format!(
            "SELECT CAST(YEAR({c}) AS nvarchar(4)) AS y, CAST(COUNT_BIG(*) AS nvarchar(20)) AS n \
             FROM {table} WHERE {c} IS NOT NULL GROUP BY YEAR({c}) ORDER BY YEAR({c})"
        );
        match conn.query_json(&sql, &[]) {
            Ok(rows) => {
                let mut years = Map::new();
                for row in rows {
                    if let Some(y) = text(&row, "y") {
                        years.insert(y, num(&row, "n"));
                    }
                }
                m.insert("years", Value::Object(years));
            }
            Err(e) => {
                m.insert("years_error", e.to_string());
            }
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use dl_json::json;

    struct Fake {
        queries: Vec<String>,
    }

    impl Connection for Fake {
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
            if sql == DATE_COLUMNS_SQL {
                return Ok(match p.first().map(|x| x.1) {
                    Some("[dbo].[Folio]") => {
                        vec![json!({"n": "ArrivalDate"}), json!({"n": "Odd]Date"})]
                    }
                    _ => vec![],
                });
            }
            if sql.contains("GROUP BY") {
                return Ok(vec![
                    json!({"y": "2026", "n": "10"}),
                    json!({"y": "2028", "n": "3"}),
                ]);
            }
            if sql.contains("[Odd]]Date]") {
                return Err(Error::Sql("boom".into()));
            }
            Ok(vec![
                json!({"lo": "2019-01-02", "hi": "2028-12-31", "n": "13"}),
            ])
        }
    }

    #[test]
    fn args_are_checked() {
        assert_eq!(Ranges::from_args(&json!({})).unwrap(), None);
        assert!(Ranges::from_args(&json!({"ranges": "dbo.Folio"})).is_err());
        assert!(Ranges::from_args(&json!({"ranges": []})).is_err());
        assert!(Ranges::from_args(&json!({"ranges": [1]})).is_err());
        assert!(Ranges::from_args(&json!({"ranges": ["Folio"], "by_year": "yes"})).is_err());
        let many: Vec<String> = (0..=MAX_RANGE_TABLES).map(|i| format!("t{i}")).collect();
        assert!(Ranges::from_args(&json!({ "ranges": many })).is_err());
        let r = Ranges::from_args(&json!({"ranges": ["Folio"], "by_year": true}))
            .unwrap()
            .unwrap();
        assert_eq!(r.tables[0].schema, "dbo");
        assert!(r.by_year);
    }

    #[test]
    fn date_columns_only_with_quoted_names_and_missing_tables_listed() {
        let mut c = Fake { queries: vec![] };
        let r = Ranges::from_args(&json!({"ranges": ["Folio", "dbo.Nope"], "by_year": true}))
            .unwrap()
            .unwrap();
        let v = ranges(&mut c, &r).unwrap();
        let a = &v["ranges"]["dbo.Folio"]["ArrivalDate"];
        assert_eq!(a["min"].as_str(), Some("2019-01-02"));
        assert_eq!(a["max"].as_str(), Some("2028-12-31"));
        assert_eq!(a["count"].as_i64(), Some(13));
        assert_eq!(a["years"]["2028"].as_i64(), Some(3));
        assert!(
            v["ranges"]["dbo.Folio"]["Odd]Date"]["error"]
                .as_str()
                .is_some()
        );
        assert!(v["ranges"]["dbo.Folio"]["Odd]Date"].get("min").is_none());
        assert_eq!(v["ranges_missing"], json!(["dbo.Nope"]));
        assert!(c.queries.iter().any(|q| q.contains("MIN([ArrivalDate])")));
        assert!(c.queries.iter().all(|q| !q.contains("SELECT *")));
    }
}
