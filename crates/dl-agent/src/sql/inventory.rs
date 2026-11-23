//! Database inventory (PROTOCOL.md §9.8): per table the row count and a hash of its column
//! layout, per procedure, view, function and trigger a hash of its definition, so two copies
//! of a database (a site and its cloud twin) can be compared without moving any data.
//! Counts, hashes and object names only: no row ever leaves the server.
//!
//! An optional data checksum per table (`CHECKSUM_AGG(BINARY_CHECKSUM(*))` with an exact
//! `COUNT_BIG(*)`) reads the table, so it runs only for the tables asked for, or for every
//! table up to [`MAX_CHECKSUM_ROWS`] rows when `checksum: true`.

use std::collections::BTreeSet;

use dl_json::{Map, Value, json};

use super::pool::Connection;
use crate::Error;

/// Tables larger than this (by the catalog count) are skipped by `checksum: true`.
pub const MAX_CHECKSUM_ROWS: u64 = 2_000_000;
/// Most tables one `checksum` list may name.
pub const MAX_CHECKSUM_TABLES: usize = 500;

/// Row count from the catalog (no table scan) and a hash of name, type, length, precision,
/// scale and nullability of every column. SQL Server 2005 built-ins only, values as text.
pub const TABLES_SQL: &str = "SELECT s.name AS s, t.name AS n, \
CAST((SELECT SUM(p.rows) FROM sys.partitions p WHERE p.object_id = t.object_id AND p.index_id IN (0,1)) AS nvarchar(20)) AS r, \
CAST((SELECT COUNT(*) FROM sys.columns c WHERE c.object_id = t.object_id) AS nvarchar(10)) AS c, \
CAST((SELECT CHECKSUM_AGG(BINARY_CHECKSUM(c.name, TYPE_NAME(c.user_type_id), c.max_length, c.precision, c.scale, c.is_nullable, c.column_id)) \
FROM sys.columns c WHERE c.object_id = t.object_id) AS nvarchar(20)) AS h \
FROM sys.tables t JOIN sys.schemas s ON s.schema_id = t.schema_id WHERE t.is_ms_shipped = 0";

/// Programmable objects and a checksum of their definition (`NULL` without VIEW DEFINITION
/// or for an encrypted module).
pub const MODULES_SQL: &str = "SELECT s.name AS s, o.name AS n, RTRIM(o.type) AS t, \
CAST(CHECKSUM(OBJECT_DEFINITION(o.object_id)) AS nvarchar(20)) AS h \
FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id \
WHERE o.is_ms_shipped = 0 AND o.type IN ('P','V','FN','IF','TF','TR')";

/// What `sql.inventory` was asked for.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum Checksum {
    /// Catalog facts only (the default).
    #[default]
    None,
    /// Every table up to [`MAX_CHECKSUM_ROWS`] rows.
    All,
    /// Only these `schema.table` names (lower-cased).
    Tables(BTreeSet<String>),
}

impl Checksum {
    /// From the command's `checksum` argument: absent/false, `true`, or a list of names.
    ///
    /// # Errors
    /// [`Error::Config`] for another type or a list over [`MAX_CHECKSUM_TABLES`].
    pub fn from_arg(v: Option<&Value>) -> Result<Checksum, Error> {
        match v {
            None | Some(Value::Null) | Some(Value::Bool(false)) => Ok(Checksum::None),
            Some(Value::Bool(true)) => Ok(Checksum::All),
            Some(Value::Array(items)) => {
                if items.len() > MAX_CHECKSUM_TABLES {
                    return Err(Error::Config(format!(
                        "inventory: at most {MAX_CHECKSUM_TABLES} tables per checksum"
                    )));
                }
                let mut set = BTreeSet::new();
                for i in items {
                    let name = i.as_str().ok_or_else(|| {
                        Error::Config("inventory: checksum names must be text".into())
                    })?;
                    let n = super::probe::Name::parse(name)?;
                    set.insert(key(&n.schema, &n.name));
                }
                Ok(Checksum::Tables(set))
            }
            Some(_) => Err(Error::Config(
                "inventory: checksum must be true or a list of tables".into(),
            )),
        }
    }

    fn wants(&self, k: &str, rows: Option<u64>) -> Option<Result<(), &'static str>> {
        match self {
            Checksum::None => None,
            Checksum::All => Some(match rows {
                Some(r) if r > MAX_CHECKSUM_ROWS => Err("too_large"),
                _ => Ok(()),
            }),
            Checksum::Tables(set) => set.contains(k).then_some(Ok(())),
        }
    }
}

fn key(schema: &str, name: &str) -> String {
    format!("{}.{}", schema.to_lowercase(), name.to_lowercase())
}

fn text<'a>(row: &'a Value, k: &str) -> Option<&'a str> {
    row.get(k).and_then(Value::as_str).map(str::trim)
}

fn num(row: &Value, k: &str) -> Option<i64> {
    text(row, k).and_then(|s| s.parse().ok())
}

/// `[schema].[name]` with `]` doubled, for a name that came from the catalog.
pub fn quote(schema: &str, name: &str) -> String {
    format!(
        "[{}].[{}]",
        schema.replace(']', "]]"),
        name.replace(']', "]]")
    )
}

/// The inventory of the database behind `conn`:
/// `{"tables":{"dbo.x":{"rows","columns","layout","checksum"?,"exact_rows"?}},
///   "modules":{"dbo.p":{"type","definition"}}, "totals":{…}}`.
///
/// # Errors
/// The SQL error when a catalog query fails; a failed checksum only marks that table.
pub fn inventory(conn: &mut impl Connection, checksum: &Checksum) -> Result<Value, Error> {
    let mut tables = Map::new();
    let mut total_rows: i64 = 0;
    let mut asked: BTreeSet<String> = match checksum {
        Checksum::Tables(set) => set.clone(),
        _ => BTreeSet::new(),
    };
    for row in conn.query_json(TABLES_SQL, &[])? {
        let (Some(s), Some(n)) = (text(&row, "s"), text(&row, "n")) else {
            continue;
        };
        let k = key(s, n);
        let rows = num(&row, "r");
        total_rows += rows.unwrap_or(0);
        let mut t = Map::new();
        t.insert("rows", rows.map_or(Value::Null, Value::from));
        t.insert("columns", num(&row, "c").map_or(Value::Null, Value::from));
        t.insert("layout", num(&row, "h").map_or(Value::Null, Value::from));
        asked.remove(&k);
        match checksum.wants(&k, rows.and_then(|r| u64::try_from(r).ok())) {
            None => {}
            Some(Err(why)) => {
                t.insert("checksum_skipped", why);
            }
            Some(Ok(())) => {
                let sql = format!(
                    "SELECT CAST(CHECKSUM_AGG(BINARY_CHECKSUM(*)) AS nvarchar(20)) AS h, \
                     CAST(COUNT_BIG(*) AS nvarchar(20)) AS n FROM {}",
                    quote(s, n)
                );
                match conn.query_json(&sql, &[]) {
                    Ok(r) => {
                        let r = r.into_iter().next().unwrap_or(Value::Null);
                        t.insert("checksum", num(&r, "h").map_or(Value::Null, Value::from));
                        t.insert("exact_rows", num(&r, "n").map_or(Value::Null, Value::from));
                    }
                    Err(e) => {
                        t.insert("checksum_error", e.to_string());
                    }
                }
            }
        }
        tables.insert(format!("{s}.{n}"), Value::Object(t));
    }
    let mut modules = Map::new();
    for row in conn.query_json(MODULES_SQL, &[])? {
        let (Some(s), Some(n)) = (text(&row, "s"), text(&row, "n")) else {
            continue;
        };
        modules.insert(
            format!("{s}.{n}"),
            json!({
                "type": text(&row, "t").unwrap_or(""),
                "definition": num(&row, "h").map_or(Value::Null, Value::from),
            }),
        );
    }
    let missing: Vec<Value> = asked.into_iter().map(Value::from).collect();
    Ok(json!({
        "totals": {"tables": tables.len(), "rows": total_rows, "modules": modules.len()},
        "tables": Value::Object(tables),
        "modules": Value::Object(modules),
        "checksum_missing": Value::Array(missing),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Cat {
        queries: Vec<String>,
    }

    impl Connection for Cat {
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
        fn query_json(&mut self, sql: &str, _: &[(&str, &str)]) -> Result<Vec<Value>, Error> {
            self.queries.push(sql.to_string());
            if sql.starts_with("SELECT s.name AS s, t.name") {
                return Ok(vec![
                    json!({"s": "dbo", "n": "HoaDon", "r": "120", "c": "5", "h": "-77"}),
                    json!({"s": "dbo", "n": "Big]Log", "r": "9000000", "c": "3", "h": "4"}),
                ]);
            }
            if sql.contains("OBJECT_DEFINITION") {
                return Ok(vec![
                    json!({"s": "dbo", "n": "usp_GetBill", "t": "P", "h": "123"}),
                    json!({"s": "dbo", "n": "v_Secret", "t": "V", "h": null}),
                ]);
            }
            if sql.contains("FROM [dbo].[HoaDon]") {
                return Ok(vec![json!({"h": "42", "n": "120"})]);
            }
            Err(Error::Sql(format!("unexpected {sql}")))
        }
    }

    #[test]
    fn catalog_only_by_default() {
        let mut c = Cat { queries: vec![] };
        let v = inventory(&mut c, &Checksum::None).unwrap();
        assert_eq!(c.queries.len(), 2, "no table is read");
        assert_eq!(v["totals"]["rows"].as_i64(), Some(9_000_120));
        assert_eq!(v["tables"]["dbo.HoaDon"]["layout"].as_i64(), Some(-77));
        assert_eq!(
            v["modules"]["dbo.usp_GetBill"]["definition"].as_i64(),
            Some(123)
        );
        assert!(v["modules"]["dbo.v_Secret"]["definition"].is_null());
        assert!(v["tables"]["dbo.HoaDon"].get("checksum").is_none());
    }

    #[test]
    fn checksum_all_skips_large_tables_and_quotes_names() {
        let mut c = Cat { queries: vec![] };
        let v = inventory(&mut c, &Checksum::All).unwrap();
        assert_eq!(v["tables"]["dbo.HoaDon"]["checksum"].as_i64(), Some(42));
        assert_eq!(v["tables"]["dbo.HoaDon"]["exact_rows"].as_i64(), Some(120));
        assert_eq!(
            v["tables"]["dbo.Big]Log"]["checksum_skipped"].as_str(),
            Some("too_large")
        );
        assert!(!c.queries.iter().any(|q| q.contains("Big")));
        assert_eq!(quote("dbo", "Big]Log"), "[dbo].[Big]]Log]");
    }

    #[test]
    fn checksum_list_reads_only_named_tables_and_lists_unknown_names() {
        let arg = json!(["HoaDon", "dbo.Nope"]);
        let cs = Checksum::from_arg(Some(&arg)).unwrap();
        let mut c = Cat { queries: vec![] };
        let v = inventory(&mut c, &cs).unwrap();
        assert_eq!(v["tables"]["dbo.HoaDon"]["checksum"].as_i64(), Some(42));
        assert_eq!(v["checksum_missing"], json!(["dbo.nope"]));
        assert!(Checksum::from_arg(Some(&json!(7))).is_err());
        assert_eq!(Checksum::from_arg(None).unwrap(), Checksum::None);
    }
}
