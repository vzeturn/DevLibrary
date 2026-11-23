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

/// Which part of the inventory one call returns. A database with hundreds of tables and
/// modules does not fit one command result ([`crate::command::MAX_OUTPUT`]), so the caller
/// asks for one `section` at a time, `limit` entries after the key `after` (keys sorted).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Page {
    /// `None` lists both sections (and cannot be paged).
    pub section: Option<Section>,
    /// Only keys sorting after this one.
    pub after: Option<String>,
    /// At most this many entries ([`MAX_PAGE`]).
    pub limit: Option<usize>,
}

/// A part of the inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    /// User tables.
    Tables,
    /// Procedures, views, functions and triggers.
    Modules,
}

/// Largest `limit` one call accepts.
pub const MAX_PAGE: usize = 1000;

impl Page {
    /// Reads `section`, `after` and `limit` from the command arguments.
    ///
    /// # Errors
    /// A bad value, or `after`/`limit` without a `section`.
    pub fn from_args(args: &Value) -> Result<Page, Error> {
        let bad = |m: &str| Error::Config(format!("inventory: {m}"));
        let section = match args.get("section") {
            None | Some(Value::Null) => None,
            Some(v) => match v.as_str() {
                Some("tables") => Some(Section::Tables),
                Some("modules") => Some(Section::Modules),
                _ => return Err(bad("section must be \"tables\" or \"modules\"")),
            },
        };
        let after = match args.get("after") {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                v.as_str()
                    .ok_or_else(|| bad("after must be text"))?
                    .to_string(),
            ),
        };
        let limit = match args.get("limit") {
            None | Some(Value::Null) => None,
            Some(v) => match v.as_u64() {
                Some(n) if (1..=MAX_PAGE as u64).contains(&n) => Some(n as usize),
                _ => return Err(bad(&format!("limit must be 1..={MAX_PAGE}"))),
            },
        };
        if section.is_none() && (after.is_some() || limit.is_some()) {
            return Err(bad("after and limit need a section"));
        }
        Ok(Page {
            section,
            after,
            limit,
        })
    }

    fn wants(&self, s: Section) -> bool {
        self.section.is_none_or(|x| x == s)
    }

    /// Sorted entries of one section that fall in the page, and the key to continue from.
    fn cut<T>(&self, mut all: Vec<(String, T)>) -> (Vec<(String, T)>, Option<String>) {
        all.sort_by(|a, b| a.0.cmp(&b.0));
        if let Some(a) = &self.after {
            all.retain(|(k, _)| k.as_str() > a.as_str());
        }
        match self.limit {
            Some(n) if all.len() > n => {
                all.truncate(n);
                let next = all.last().map(|(k, _)| k.clone());
                (all, next)
            }
            _ => (all, None),
        }
    }
}

/// The inventory of the database behind `conn`:
/// `{"tables":{"dbo.x":{"rows","columns","layout","checksum"?,"exact_rows"?}},
///   "modules":{"dbo.p":{"type","definition"}}, "totals":{…}, "next"?}`. `totals` always
/// covers the whole database; with a [`Page`] only that section and range is listed (and
/// checksummed), and `next` is the key to pass as `after` while more remain.
///
/// # Errors
/// The SQL error when a catalog query fails; a failed checksum only marks that table.
pub fn inventory(
    conn: &mut impl Connection,
    checksum: &Checksum,
    page: &Page,
) -> Result<Value, Error> {
    let mut total_rows: i64 = 0;
    let mut asked: BTreeSet<String> = match checksum {
        Checksum::Tables(set) => set.clone(),
        _ => BTreeSet::new(),
    };
    let mut catalog = Vec::new();
    for row in conn.query_json(TABLES_SQL, &[])? {
        let (Some(s), Some(n)) = (text(&row, "s"), text(&row, "n")) else {
            continue;
        };
        let rows = num(&row, "r");
        total_rows += rows.unwrap_or(0);
        asked.remove(&key(s, n));
        let (s, n) = (s.to_string(), n.to_string());
        catalog.push((
            format!("{s}.{n}"),
            (s, n, rows, num(&row, "c"), num(&row, "h")),
        ));
    }
    let table_count = catalog.len();
    let mut out = Map::new();
    let mut next = None;
    if page.wants(Section::Tables) {
        let (rows_in_page, more) = page.cut(catalog);
        next = more;
        let mut tables = Map::new();
        for (k, (s, n, rows, columns, layout)) in rows_in_page {
            tables.insert(
                k,
                Value::Object(table_entry(conn, checksum, &s, &n, rows, columns, layout)),
            );
        }
        out.insert("tables", Value::Object(tables));
    }
    let mut defs = Vec::new();
    for row in conn.query_json(MODULES_SQL, &[])? {
        let (Some(s), Some(n)) = (text(&row, "s"), text(&row, "n")) else {
            continue;
        };
        defs.push((
            format!("{s}.{n}"),
            json!({
                "type": text(&row, "t").unwrap_or(""),
                "definition": num(&row, "h").map_or(Value::Null, Value::from),
            }),
        ));
    }
    let module_count = defs.len();
    if page.wants(Section::Modules) {
        let (in_page, more) = page.cut(defs);
        if page.section.is_some() {
            next = more;
        }
        out.insert("modules", Value::Object(in_page.into_iter().collect()));
    }
    let missing: Vec<Value> = asked.into_iter().map(Value::from).collect();
    out.insert(
        "totals",
        json!({"tables": table_count, "rows": total_rows, "modules": module_count}),
    );
    out.insert("checksum_missing", Value::Array(missing));
    if let Some(k) = next {
        out.insert("next", Value::from(k));
    }
    Ok(Value::Object(out))
}

fn table_entry(
    conn: &mut impl Connection,
    checksum: &Checksum,
    s: &str,
    n: &str,
    rows: Option<i64>,
    columns: Option<i64>,
    layout: Option<i64>,
) -> Map {
    let mut t = Map::new();
    t.insert("rows", rows.map_or(Value::Null, Value::from));
    t.insert("columns", columns.map_or(Value::Null, Value::from));
    t.insert("layout", layout.map_or(Value::Null, Value::from));
    match checksum.wants(&key(s, n), rows.and_then(|r| u64::try_from(r).ok())) {
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
    t
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
        let v = inventory(&mut c, &Checksum::None, &Page::default()).unwrap();
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
        let v = inventory(&mut c, &Checksum::All, &Page::default()).unwrap();
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
        let v = inventory(&mut c, &cs, &Page::default()).unwrap();
        assert_eq!(v["tables"]["dbo.HoaDon"]["checksum"].as_i64(), Some(42));
        assert_eq!(v["checksum_missing"], json!(["dbo.nope"]));
        assert!(Checksum::from_arg(Some(&json!(7))).is_err());
        assert_eq!(Checksum::from_arg(None).unwrap(), Checksum::None);
    }
    #[test]
    fn pages_one_section_in_key_order_and_reads_only_tables_in_the_page() {
        let mut c = Cat { queries: vec![] };
        let p = Page::from_args(&json!({"section": "tables", "limit": 1})).unwrap();
        let v = inventory(&mut c, &Checksum::All, &p).unwrap();
        assert_eq!(v["next"].as_str(), Some("dbo.Big]Log"));
        assert!(v["tables"].get("dbo.Big]Log").is_some());
        assert!(v["tables"].get("dbo.HoaDon").is_none());
        assert!(v.get("modules").is_none());
        assert_eq!(v["totals"]["tables"].as_i64(), Some(2));
        assert!(!c.queries.iter().any(|q| q.contains("HoaDon")));

        let p = Page::from_args(&json!({"section": "tables", "after": "dbo.Big]Log"})).unwrap();
        let v = inventory(&mut c, &Checksum::All, &p).unwrap();
        assert_eq!(v["tables"]["dbo.HoaDon"]["checksum"].as_i64(), Some(42));
        assert!(v.get("next").is_none());

        let p = Page::from_args(&json!({"section": "modules", "limit": 5})).unwrap();
        let v = inventory(&mut c, &Checksum::All, &p).unwrap();
        assert!(v.get("tables").is_none());
        assert_eq!(v["modules"].as_object().map(|m| m.len()), Some(2));
        assert!(v.get("next").is_none());
    }

    #[test]
    fn page_args_are_checked() {
        assert!(Page::from_args(&json!({"limit": 5})).is_err());
        assert!(Page::from_args(&json!({"section": "rows"})).is_err());
        assert!(Page::from_args(&json!({"section": "tables", "limit": 0})).is_err());
        assert_eq!(Page::from_args(&json!({})).unwrap(), Page::default());
    }
}
