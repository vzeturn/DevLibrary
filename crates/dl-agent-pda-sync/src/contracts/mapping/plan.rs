//! Precomputed per-table lookups (`TablePlan.cs`), case-insensitive like SQL Server
//! identifiers.

use super::MapperOptions;
use super::transforms::snake_case;
use crate::contracts::catalog::{CatalogTable, SyncCatalog};
use crate::contracts::map::OrderedMap;
use crate::util::stj::eq_ignore_case;

/// A case-insensitive string set.
#[derive(Debug, Clone, Default)]
pub(crate) struct CiSet(Vec<String>);

impl CiSet {
    fn of<'a>(items: impl IntoIterator<Item = &'a String>) -> CiSet {
        CiSet(items.into_iter().cloned().collect())
    }

    pub(crate) fn contains(&self, s: &str) -> bool {
        self.0.iter().any(|x| eq_ignore_case(x, s))
    }
}

/// A case-insensitive dictionary (later inserts replace the value, keep the first key).
#[derive(Debug, Clone, Default)]
pub(crate) struct CiMap(Vec<(String, String)>);

impl CiMap {
    fn insert(&mut self, k: &str, v: &str) {
        if let Some(slot) = self.0.iter_mut().find(|(x, _)| eq_ignore_case(x, k)) {
            slot.1 = v.to_owned();
        } else {
            self.0.push((k.to_owned(), v.to_owned()));
        }
    }

    pub(crate) fn get(&self, k: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(x, _)| eq_ignore_case(x, k))
            .map(|(_, v)| v.as_str())
    }

    pub(crate) fn contains_key(&self, k: &str) -> bool {
        self.get(k).is_some()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

/// Renames that hold across the PG money schema (migration 0040).
const DEFAULT_RENAMES: &[(&str, &str)] = &[
    ("RVC", "rvc_no"),
    ("OpenTime", "opened_at"),
    ("CloseTime", "closed_at"),
];

const HASH_EXCEPT_BLOB: &str = "all-columns-except-blob";

#[derive(Debug, Clone)]
pub(crate) struct TablePlan {
    pub(crate) table: CatalogTable,
    rename: CiMap,
    pub(crate) key_aligned: bool,
    pub(crate) excluded: CiSet,
    pub(crate) tcvn3: CiSet,
    pub(crate) money: CiSet,
    pub(crate) local_time: CiSet,
    pub(crate) unmapped: CiSet,
    /// Vendor column to PG column for typed tables.
    pub(crate) typed: Option<CiMap>,
    pub(crate) constants: OrderedMap<String>,
    pub(crate) key_columns: CiSet,
}

impl TablePlan {
    pub(crate) fn pg_name(&self, vendor_column: &str) -> String {
        self.rename
            .get(vendor_column)
            .map_or_else(|| snake_case(vendor_column), str::to_owned)
    }

    pub(crate) fn is_raw(&self) -> bool {
        self.table.pg_raw
    }

    pub(crate) fn hashes_blobs_apart(&self) -> bool {
        self.table.hash_columns.as_deref() == Some(HASH_EXCEPT_BLOB)
    }

    pub(crate) fn build(
        t: &CatalogTable,
        catalog: &SyncCatalog,
        options: &MapperOptions,
    ) -> TablePlan {
        let mut rename = CiMap::default();
        for (k, v) in DEFAULT_RENAMES {
            rename.insert(k, v);
        }
        let vendor_keys = t.keys();
        let pg_keys: &[String] = t.pg_keys.as_deref().unwrap_or(&[]);
        let mut aligned = !vendor_keys.is_empty() && vendor_keys.len() == pg_keys.len();
        if aligned {
            for (v, p) in vendor_keys.iter().zip(pg_keys) {
                rename.insert(v, p);
            }
        }
        let overrides = options
            .column_overrides
            .iter()
            .find(|(table, _)| eq_ignore_case(table, &t.table))
            .map(|(_, o)| o);
        if let Some(o) = overrides {
            for (v, p) in o {
                rename.insert(v, p);
            }
        }
        let mut typed = None;
        if t.is_typed() {
            let mut m = CiMap::default();
            for (v, p) in t.pg_columns.iter().flat_map(|c| c.iter()) {
                m.insert(v, p);
            }
            if let Some(o) = overrides {
                for (v, p) in o {
                    m.insert(v, p);
                }
            }
            rename = m.clone();
            typed = Some(m);
            aligned = true;
        }
        let list = |l: &Option<Vec<String>>| CiSet::of(l.iter().flatten());
        let never = catalog.never_sync_columns(&t.table);
        let excluded = CiSet::of(t.exclude_columns.iter().flatten().chain(never.iter()));
        let transforms = t.transforms.clone().unwrap_or_default();
        TablePlan {
            table: t.clone(),
            rename,
            key_aligned: aligned,
            excluded,
            tcvn3: list(&transforms.tcvn3_decode),
            money: list(&transforms.money_decimal_string),
            local_time: list(&transforms.local_time_to_utc),
            unmapped: list(&t.unmapped_vendor_columns),
            typed,
            constants: t.pg_constants.clone().unwrap_or_default(),
            key_columns: CiSet::of(vendor_keys.iter()),
        }
    }
}
