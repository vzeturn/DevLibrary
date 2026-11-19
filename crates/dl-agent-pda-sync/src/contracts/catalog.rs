//! The sync catalog (`catalog/CatalogModel.cs`, `catalog/CatalogLoader.cs`): the
//! embedded `sync-catalog.json` merged with the `logical-keys.json` overlay.

use std::sync::OnceLock;

use super::json;
use super::map::OrderedMap;
use crate::util::stj::{self, Obj, ReadError, ReadOptions, eq_ignore_case};

/// `sync-catalog.json`, embedded like the C# `EmbeddedResource`.
pub const CATALOG_JSON: &str = include_str!("../../assets/catalog/sync-catalog.json");

/// `logical-keys.json`, embedded like the C# `EmbeddedResource`.
pub const LOGICAL_KEYS_JSON: &str = include_str!("../../assets/catalog/logical-keys.json");

/// Sync class of a vendor table (the JSON strings of the catalog).
pub mod catalog_class {
    /// Master/config.
    pub const A: &str = "A";
    /// Open checks.
    pub const B: &str = "B";
    /// Closed ledger/payments/logs.
    pub const C: &str = "C";
    /// Counters/business date/shifts.
    pub const D: &str = "D";
    /// PDA-owned.
    pub const E: &str = "E";
    /// Mixed table split by row predicate.
    pub const AD: &str = "A+D";
    /// Not synced.
    pub const SKIP: &str = "skip";
    /// Probed, owner decision needed.
    pub const UNCLASSIFIED: &str = "unclassified";
}

/// A catalog that cannot be read or breaks a rule (`InvalidDataException`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogError {
    /// Not JSON or not the catalog shape.
    Json(ReadError),
    /// A rule of the loader failed; the message is the C# message.
    Invalid(String),
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CatalogError::Json(e) => e.fmt(f),
            CatalogError::Invalid(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for CatalogError {}

impl From<ReadError> for CatalogError {
    fn from(e: ReadError) -> Self {
        CatalogError::Json(e)
    }
}

/// `conventions` of the catalog.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CatalogConventions {
    /// Note on the tenant key.
    pub tenant_id: String,
    /// Class name to description.
    pub classes: OrderedMap<String>,
    /// Known capture strategies.
    pub capture_strategies: Vec<String>,
    /// Note on times.
    pub time: String,
    /// Note on money.
    pub money: String,
    /// `A/B.Col` or `T.Col1/Col2` entries that must never leave the site.
    pub never_sync: Vec<String>,
    /// Note on PDA-owned storage.
    pub pda_owned_storage: String,
}

/// Per-column transform lists driven by the catalog.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CatalogTransforms {
    /// Columns holding TCVN3 bytes.
    pub tcvn3_decode: Option<Vec<String>>,
    /// Policy note.
    pub tcvn3_policy: Option<String>,
    /// Money columns.
    pub money_decimal_string: Option<Vec<String>>,
    /// Site-local datetimes to convert to UTC.
    pub local_time_to_utc: Option<Vec<String>>,
}

/// One vendor table of the catalog.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CatalogTable {
    /// Vendor table name.
    pub table: String,
    /// Sync class ([`catalog_class`]).
    pub class: String,
    /// Rows seen on dev.
    pub rows: i64,
    /// Whether the PDA reads it.
    pub used_by_pda: bool,
    /// Postgres target table.
    pub pg_target: Option<String>,
    /// Note.
    pub note: Option<String>,
    /// Logical/schema source key.
    pub source_keys: Option<Vec<String>>,
    /// PG key columns.
    pub pg_keys: Option<Vec<String>>,
    /// Capture strategy.
    pub capture: Option<String>,
    /// Watermark candidates.
    pub watermark_candidates: Option<Vec<String>>,
    /// Monotonic key.
    pub monotonic_key: Option<String>,
    /// Datetime columns.
    pub datetime_columns: Option<Vec<String>>,
    /// Hash column policy (`all-columns-except-blob`, ...).
    pub hash_columns: Option<String>,
    /// Transforms.
    pub transforms: Option<CatalogTransforms>,
    /// Columns never selected.
    pub exclude_columns: Option<Vec<String>>,
    /// Why.
    pub exclude_reason: Option<String>,
    /// Vendor columns without PG home.
    pub unmapped_vendor_columns: Option<Vec<String>>,
    /// Vendor primary key.
    pub pk: Option<Vec<String>>,
    /// Vendor column to PG column (typed tables), in file order.
    pub pg_columns: Option<OrderedMap<String>>,
    /// PG column to constant.
    pub pg_constants: Option<OrderedMap<String>>,
    /// Raw landing (vendor names, jsonb).
    pub pg_raw: bool,
    /// Why raw.
    pub pg_raw_reason: Option<String>,
    /// Blob columns excluded from the hash.
    pub hash_exclude: Option<Vec<String>>,
    /// PG key column to the constant a NULL vendor value maps to (ADR 0106).
    pub null_key_defaults: Option<OrderedMap<String>>,
    /// Keys came from `logical-keys.json` (set by the loader).
    pub keys_from_overlay: bool,
    /// Exact duplicate rows were seen on dev (set by the loader).
    pub duplicate_rows_tolerated: bool,
}

impl CatalogTable {
    /// A typed PG home with an explicit vendor-to-PG column map.
    pub fn is_typed(&self) -> bool {
        self.has_pg_target()
            && !self.pg_raw
            && self.pg_columns.as_ref().is_some_and(|m| !m.is_empty())
    }

    /// `NullKeyDefaults` or an empty map.
    pub fn null_key_defaults_or_empty(&self) -> OrderedMap<String> {
        self.null_key_defaults.clone().unwrap_or_default()
    }

    /// True for A, B, C, D, E and A+D.
    pub fn is_classified(&self) -> bool {
        self.class != catalog_class::SKIP && self.class != catalog_class::UNCLASSIFIED
    }

    /// True when the table has a Postgres home.
    pub fn has_pg_target(&self) -> bool {
        self.pg_target.as_deref().is_some_and(|t| !t.is_empty())
    }

    /// Vendor-side row key: schema/logical key, else the PK; empty if none.
    pub fn keys(&self) -> &[String] {
        match (&self.source_keys, &self.pk) {
            (Some(k), _) if !k.is_empty() => k,
            (_, Some(pk)) if !pk.is_empty() => pk,
            _ => &[],
        }
    }
}

/// A PDA-owned SQLite table of the catalog.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CatalogSqliteTable {
    /// Table.
    pub table: String,
    /// Source.
    pub source: String,
    /// Class.
    pub class: String,
    /// Postgres target.
    pub pg_target: Option<String>,
    /// Capture.
    pub capture: Option<String>,
    /// Conflict policy.
    pub conflict: Option<String>,
    /// Reason.
    pub reason: Option<String>,
    /// Note.
    pub note: Option<String>,
}

/// Root of `sync-catalog.json`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncCatalog {
    /// Version.
    pub catalog_version: i32,
    /// Generation date.
    pub generated: String,
    /// Probed database.
    pub source_db: String,
    /// Conventions.
    pub conventions: CatalogConventions,
    /// Vendor tables.
    pub tables: Vec<CatalogTable>,
    /// PDA-owned SQLite tables.
    pub sqlite_tables: Vec<CatalogSqliteTable>,
}

static EMBEDDED: OnceLock<Result<SyncCatalog, CatalogError>> = OnceLock::new();

impl SyncCatalog {
    /// The embedded catalog with the overlay (`CatalogLoader.LoadEmbedded`), parsed once.
    pub fn load() -> Result<&'static SyncCatalog, CatalogError> {
        EMBEDDED
            .get_or_init(|| SyncCatalog::parse(CATALOG_JSON, Some(LOGICAL_KEYS_JSON)))
            .as_ref()
            .map_err(Clone::clone)
    }

    /// `CatalogLoader.Parse`: catalog and optional overlay JSON.
    pub fn parse(
        catalog_json: &str,
        logical_keys_json: Option<&str>,
    ) -> Result<SyncCatalog, CatalogError> {
        let v = stj::parse(catalog_json)?;
        let o = Obj::of(&v, stj::SNAKE, "$")?
            .ok_or_else(|| CatalogError::Invalid("sync-catalog.json is empty.".into()))?;
        let mut catalog = read_catalog(o)?;
        for t in &catalog.tables {
            validate_null_key_defaults(t)?;
        }
        let Some(keys_json) = logical_keys_json else {
            return Ok(catalog);
        };
        let kv = stj::parse(keys_json)?;
        let ko = Obj::of(&kv, stj::SNAKE, "$")?
            .ok_or_else(|| CatalogError::Invalid("logical-keys.json is empty.".into()))?;
        let tolerated = ko.list("duplicate_rows_tolerated", json::string_item)?;
        let mut logical: Vec<(String, Vec<String>)> = Vec::new();
        if let Some(m) = ko.get_non_null("logical_keys") {
            let Some(map) = m.as_object() else {
                return Err(
                    ReadError::new("$.logical_keys", "The JSON value is not an object.").into(),
                );
            };
            for (k, v) in map.iter() {
                let path = format!("$.logical_keys.{k}");
                let Some(items) = v.as_array() else {
                    return Err(ReadError::new(&path, "The JSON value is not an array.").into());
                };
                let cols = items
                    .iter()
                    .map(|x| json::string_item(x, stj::SNAKE, &path))
                    .collect::<Result<Vec<_>, _>>()?;
                logical.push((k.clone(), cols));
            }
        }
        for t in &mut catalog.tables {
            let dup = tolerated.iter().any(|x| eq_ignore_case(x, &t.table));
            let overlay = if t.keys().is_empty() {
                logical.iter().find(|(k, _)| eq_ignore_case(k, &t.table))
            } else {
                None
            };
            if let Some((_, cols)) = overlay {
                t.source_keys = Some(cols.clone());
                t.keys_from_overlay = true;
            } else {
                t.keys_from_overlay = false;
            }
            t.duplicate_rows_tolerated = dup;
        }
        Ok(catalog)
    }

    /// Case-insensitive lookup (SQL Server identifiers are case-insensitive).
    pub fn find(&self, vendor_table: &str) -> Option<&CatalogTable> {
        self.tables
            .iter()
            .find(|t| eq_ignore_case(&t.table, vendor_table))
    }

    /// Columns of `vendor_table` that must never leave the site, from
    /// `conventions.never_sync` (`A/B.Col`, `T.Col1/Col2`).
    pub fn never_sync_columns(&self, vendor_table: &str) -> Vec<String> {
        let mut result = Vec::new();
        for entry in &self.conventions.never_sync {
            let Some(dot) = entry.rfind('.') else {
                continue;
            };
            if dot == 0 {
                continue;
            }
            if !entry[..dot]
                .split('/')
                .any(|t| eq_ignore_case(t, vendor_table))
            {
                continue;
            }
            result.extend(entry[dot + 1..].split('/').map(str::to_owned));
        }
        result
    }

    /// Tables that take part in sync.
    pub fn classified(&self) -> impl Iterator<Item = &CatalogTable> {
        self.tables.iter().filter(|t| t.is_classified())
    }

    /// Tables nobody classified yet.
    pub fn unclassified(&self) -> impl Iterator<Item = &CatalogTable> {
        self.tables
            .iter()
            .filter(|t| t.class == catalog_class::UNCLASSIFIED)
    }
}

fn strings(o: &Obj<'_>, snake: &str) -> Result<Option<Vec<String>>, ReadError> {
    o.opt_list(snake, json::string_item)
}

fn string_map(o: &Obj<'_>, snake: &str) -> Result<Option<OrderedMap<String>>, ReadError> {
    if o.get_non_null(snake).is_none() {
        return Ok(None);
    }
    Ok(Some(
        o.str_dict(snake)?
            .into_iter()
            .map(|(k, v)| (k, v.unwrap_or_default()))
            .collect(),
    ))
}

fn read_catalog(o: Obj<'_>) -> Result<SyncCatalog, ReadError> {
    let conventions = o
        .opt_obj("conventions", |c| {
            Ok(CatalogConventions {
                tenant_id: c.string("tenant_id")?,
                classes: string_map(&c, "classes")?.unwrap_or_default(),
                capture_strategies: c.list("capture_strategies", json::string_item)?,
                time: c.string("time")?,
                money: c.string("money")?,
                never_sync: c.list("never_sync", json::string_item)?,
                pda_owned_storage: c.string("pda_owned_storage")?,
            })
        })?
        .unwrap_or_default();
    Ok(SyncCatalog {
        catalog_version: o.int("catalog_version")?,
        generated: o.string("generated")?,
        source_db: o.string("source_db")?,
        conventions,
        tables: o.list("tables", read_table)?,
        sqlite_tables: o.list("sqlite_tables", read_sqlite)?,
    })
}

fn read_table(
    v: &dl_json::Value,
    opts: ReadOptions,
    path: &str,
) -> Result<CatalogTable, ReadError> {
    let o = Obj::require(v, opts, path)?;
    Ok(CatalogTable {
        table: o.string("table")?,
        class: o.string("class")?,
        rows: o.int("rows")?,
        used_by_pda: o.bool("used_by_pda")?,
        pg_target: o.opt_string("pg_target")?,
        note: o.opt_string("note")?,
        source_keys: strings(&o, "source_keys")?,
        pg_keys: strings(&o, "pg_keys")?,
        capture: o.opt_string("capture")?,
        watermark_candidates: strings(&o, "watermark_candidates")?,
        monotonic_key: o.opt_string("monotonic_key")?,
        datetime_columns: strings(&o, "datetime_columns")?,
        hash_columns: o.opt_string("hash_columns")?,
        transforms: o.opt_obj("transforms", |t| {
            Ok(CatalogTransforms {
                tcvn3_decode: strings(&t, "tcvn3_decode")?,
                tcvn3_policy: t.opt_string("tcvn3_policy")?,
                money_decimal_string: strings(&t, "money_decimal_string")?,
                local_time_to_utc: strings(&t, "local_time_to_utc")?,
            })
        })?,
        exclude_columns: strings(&o, "exclude_columns")?,
        exclude_reason: o.opt_string("exclude_reason")?,
        unmapped_vendor_columns: strings(&o, "unmapped_vendor_columns")?,
        pk: strings(&o, "pk")?,
        pg_columns: string_map(&o, "pg_columns")?,
        pg_constants: string_map(&o, "pg_constants")?,
        pg_raw: o.bool("pg_raw")?,
        pg_raw_reason: o.opt_string("pg_raw_reason")?,
        hash_exclude: strings(&o, "hash_exclude")?,
        null_key_defaults: string_map(&o, "null_key_defaults")?,
        keys_from_overlay: false,
        duplicate_rows_tolerated: false,
    })
}

fn read_sqlite(
    v: &dl_json::Value,
    opts: ReadOptions,
    path: &str,
) -> Result<CatalogSqliteTable, ReadError> {
    let o = Obj::require(v, opts, path)?;
    Ok(CatalogSqliteTable {
        table: o.string("table")?,
        source: o.string("source")?,
        class: o.string("class")?,
        pg_target: o.opt_string("pg_target")?,
        capture: o.opt_string("capture")?,
        conflict: o.opt_string("conflict")?,
        reason: o.opt_string("reason")?,
        note: o.opt_string("note")?,
    })
}

/// `null_key_defaults` (ADR 0106) may only name a vendor-fed PG key column of a typed
/// table, with a non-empty constant.
fn validate_null_key_defaults(t: &CatalogTable) -> Result<(), CatalogError> {
    let Some(defaults) = t.null_key_defaults.as_ref().filter(|d| !d.is_empty()) else {
        return Ok(());
    };
    if !t.is_typed() {
        return Err(CatalogError::Invalid(format!(
            "{}: null_key_defaults needs a typed table (pg_columns).",
            t.table
        )));
    }
    for (pg_key, value) in defaults.iter() {
        let fed = t
            .pg_columns
            .as_ref()
            .is_some_and(|m| m.values().any(|v| v == pg_key));
        let constant = t
            .pg_constants
            .as_ref()
            .is_some_and(|m| m.contains_key(pg_key));
        let is_key = t
            .pg_keys
            .as_ref()
            .is_some_and(|k| k.iter().any(|x| x == pg_key));
        if !is_key || !fed || constant {
            return Err(CatalogError::Invalid(format!(
                "{}: null_key_defaults '{pg_key}' is not a vendor-fed pg_keys column.",
                t.table
            )));
        }
        if value.trim().is_empty() {
            return Err(CatalogError::Invalid(format!(
                "{}: null_key_defaults '{pg_key}' needs a non-empty value.",
                t.table
            )));
        }
    }
    Ok(())
}
