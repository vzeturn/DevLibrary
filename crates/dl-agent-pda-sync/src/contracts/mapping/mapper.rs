//! The catalog-driven row mapper (`CatalogRowMapper.cs`, `MappedRow.cs`).

use dl_hash::{Digest, Sha256};

use super::plan::TablePlan;
use super::{EXTRA_COLUMN, MapperOptions, MappingError, RawValue, VendorRow, transforms};
use crate::contracts::catalog::{CatalogTable, SyncCatalog};
use crate::contracts::hashing::{ordinal_cmp, row_hasher};
use crate::contracts::map::{OrderedMap, StrMap};
use crate::contracts::wire::SyncRow;
use crate::util::stj::{Style, Writer, eq_ignore_case};

/// Vendor row after transforms, before any renaming: the input of the row hash.
#[derive(Debug, Clone, PartialEq)]
pub struct CanonicalRow {
    /// Vendor table as the catalog spells it.
    pub table: String,
    /// Vendor column to canonical string (ordinal order), never-sync and blob columns
    /// removed.
    pub columns: StrMap,
    /// Canonical row hash over `columns`.
    pub hash: String,
    /// Vendor columns removed because they must never leave the site (row order).
    pub excluded: Vec<String>,
    /// Blob column to lower-case hex SHA-256 of its content (ordinal order).
    pub blob_hashes: OrderedMap<String>,
}

/// Result of mapping one vendor row onto its Postgres target.
#[derive(Debug, Clone, PartialEq)]
pub struct MappedRow {
    /// Vendor table as the catalog spells it.
    pub table: String,
    /// Postgres target table.
    pub pg_target: String,
    /// PG key columns and values (ordinal order).
    pub key: StrMap,
    /// PG columns (ordinal order); includes `src_extra` (JSON) when unmapped columns were
    /// kept.
    pub columns: StrMap,
    /// Canonical row hash.
    pub hash: String,
    /// Unmapped vendor columns discarded because the target has no extra column.
    pub dropped: Vec<String>,
    /// Never-sync vendor columns removed.
    pub excluded: Vec<String>,
    /// Blob column to SHA-256 hex.
    pub blob_hashes: OrderedMap<String>,
    /// True when the vendor key and the PG key have the same arity (positional rename).
    pub key_aligned: bool,
    /// PG key columns whose vendor value was NULL and took the catalog's
    /// `null_key_defaults` constant (ADR 0106).
    pub defaulted_keys: Vec<String>,
}

impl MappedRow {
    /// The wire row (`ToSyncRow`).
    pub fn to_sync_row(&self, src_ver: i64) -> SyncRow {
        SyncRow {
            key: self.key.clone(),
            columns: self.columns.clone(),
            hash: self.hash.clone(),
            src_ver,
        }
    }
}

/// An ordinal-sorted dictionary under construction (`SortedDictionary` with
/// `StringComparer.Ordinal`): assignments replace, [`Sorted::finish`] orders the keys.
struct Sorted<V>(OrderedMap<V>);

impl<V> Sorted<V> {
    fn new() -> Self {
        Sorted(OrderedMap::new())
    }

    fn set(&mut self, k: &str, v: V) {
        self.0.insert(k, v);
    }

    fn finish(self) -> OrderedMap<V> {
        let mut entries: Vec<(String, V)> = self.0.into_iter().collect();
        entries.sort_by(|a, b| ordinal_cmp(&a.0, &b.0));
        entries.into_iter().collect()
    }
}

/// Catalog-driven mapper: vendor row (column name to raw value) to canonical row, hash and
/// PG row. Stateless after construction.
pub struct CatalogRowMapper {
    catalog: SyncCatalog,
    options: MapperOptions,
    /// One plan per classified table (looked up ignoring case).
    plans: Vec<TablePlan>,
}

impl std::fmt::Debug for CatalogRowMapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogRowMapper")
            .field("tables", &self.plans.len())
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl CatalogRowMapper {
    /// Builds a plan for every classified table.
    pub fn new(catalog: SyncCatalog, options: MapperOptions) -> CatalogRowMapper {
        let mut plans: Vec<TablePlan> = Vec::new();
        for t in catalog.classified() {
            let plan = TablePlan::build(t, &catalog, &options);
            // `_plans[t.Table] = ...` on a case-insensitive dictionary: the last wins.
            if let Some(slot) = plans
                .iter_mut()
                .find(|p| eq_ignore_case(&p.table.table, &t.table))
            {
                *slot = plan;
            } else {
                plans.push(plan);
            }
        }
        CatalogRowMapper {
            catalog,
            options,
            plans,
        }
    }

    /// The catalog the mapper was built from.
    pub fn catalog(&self) -> &SyncCatalog {
        &self.catalog
    }

    /// The options the mapper was built with.
    pub fn options(&self) -> &MapperOptions {
        &self.options
    }

    /// Tables of the catalog that can be mapped (classified with a PG target).
    pub fn mappable_tables(&self) -> impl Iterator<Item = &CatalogTable> {
        self.catalog.classified().filter(|t| t.has_pg_target())
    }

    fn plan_for(&self, vendor_table: &str) -> Result<&TablePlan, MappingError> {
        if let Some(p) = self
            .plans
            .iter()
            .find(|p| eq_ignore_case(&p.table.table, vendor_table))
        {
            return Ok(p);
        }
        match self.catalog.find(vendor_table) {
            None => Err(MappingError::UnknownTable {
                table: vendor_table.to_owned(),
            }),
            Some(t) => Err(MappingError::NotMappable {
                table: t.table.clone(),
                reason: format!("class '{}' is not synced", t.class),
            }),
        }
    }

    fn transform(
        &self,
        plan: &TablePlan,
        name: &str,
        raw: &RawValue,
    ) -> Result<Option<String>, MappingError> {
        if plan.tcvn3.contains(name) {
            return transforms::tcvn3(raw, name);
        }
        if plan.money.contains(name) {
            return transforms::money(raw, name);
        }
        if plan.local_time.contains(name) {
            return transforms::local_time(raw, name, &self.options.site_time_zone);
        }
        transforms::generic(raw, name, &self.options.site_time_zone)
    }

    /// Transforms every column, removes never-sync columns and hashes the result.
    pub fn canonicalize(
        &self,
        vendor_table: &str,
        row: &VendorRow,
    ) -> Result<CanonicalRow, MappingError> {
        let plan = self.plan_for(vendor_table)?;
        let mut columns = Sorted::new();
        let mut excluded = Vec::new();
        let mut blobs = Sorted::new();
        for (name, raw) in row.iter() {
            if plan.excluded.contains(name) {
                excluded.push(name.to_owned());
                continue;
            }
            if let RawValue::Bytes(blob) = raw
                && !plan.tcvn3.contains(name)
                && plan.hashes_blobs_apart()
            {
                blobs.set(name, dl_encoding::hex::encode(Sha256::digest(blob)));
                continue;
            }
            columns.set(name, self.transform(plan, name, raw)?);
        }
        let columns = columns.finish();
        let hash = row_hasher::hash(&columns);
        Ok(CanonicalRow {
            table: plan.table.table.clone(),
            columns,
            hash,
            excluded,
            blob_hashes: blobs.finish(),
        })
    }

    /// Maps a vendor row onto its Postgres target table.
    pub fn map(&self, vendor_table: &str, row: &VendorRow) -> Result<MappedRow, MappingError> {
        let plan = self.plan_for(vendor_table)?;
        let table = &plan.table;
        let Some(pg_target) = table.pg_target.clone().filter(|t| !t.is_empty()) else {
            return Err(MappingError::NotMappable {
                table: table.table.clone(),
                reason: "no pg_target in catalog".into(),
            });
        };
        let canon = self.canonicalize(vendor_table, row)?;
        let missing = |column: &str| MappingError::MissingKey {
            table: table.table.clone(),
            column: column.to_owned(),
        };
        let mut key: Sorted<Option<String>> = Sorted::new();
        let mut columns: Sorted<Option<String>> = Sorted::new();
        let mut extra: Sorted<Option<String>> = Sorted::new();
        let mut dropped = Vec::new();
        let mut defaulted: Vec<String> = Vec::new();

        if let Some(typed) = &plan.typed {
            // PG key (minus tenant) is authoritative; each key column is fed by the vendor
            // column mapped onto it, or by a catalog constant.
            // C# builds the inverse with ToDictionary (ordinal): a PG column fed twice
            // would throw there; here the first vendor column wins.
            let inverse = |pg: &str| typed.iter().find(|(_, p)| *p == pg).map(|(v, _)| v);
            let null_defaults = table.null_key_defaults_or_empty();
            for pg_key in table.pg_keys.iter().flatten() {
                if let Some(constant) = plan.constants.get(pg_key) {
                    key.set(pg_key, Some(constant.clone()));
                    continue;
                }
                let Some(vendor_key) = inverse(pg_key) else {
                    return Err(missing(pg_key));
                };
                let Some(value) = try_get_canonical(&canon.columns, vendor_key) else {
                    return Err(missing(vendor_key));
                };
                let value = match value {
                    Some(v) => v.clone(),
                    None => {
                        // A store-wide master row (no outlet) lands on the catalog's default
                        // (ADR 0106); a column the build lacks is still a reject.
                        let Some(fallback) = null_defaults.get(pg_key) else {
                            return Err(missing(vendor_key));
                        };
                        defaulted.push(pg_key.clone());
                        fallback.clone()
                    }
                };
                key.set(pg_key, Some(value));
            }
            for (k, v) in plan.constants.iter() {
                columns.set(k, Some(v.clone()));
            }
            for (name, value) in canon.columns.iter() {
                if typed.contains_key(name) {
                    columns.set(&plan.pg_name(name), value.clone());
                } else {
                    extra.set(name, value.clone());
                }
            }
            for pg_key in &defaulted {
                let v = key.0.get(pg_key).cloned().flatten();
                columns.set(pg_key, v);
            }
        } else if plan.is_raw() {
            // Raw landing: vendor names kept; the leading logical key column is required, a
            // NULL in a later overlay key column is kept as a null key value.
            let mut leading = true;
            for vendor_key in table.keys() {
                let value = match try_get_canonical(&canon.columns, vendor_key) {
                    Some(v) if v.is_some() || (!leading && table.keys_from_overlay) => v.clone(),
                    _ => return Err(missing(vendor_key)),
                };
                key.set(vendor_key, value);
                leading = false;
            }
            for (name, value) in canon.columns.iter() {
                columns.set(name, value.clone());
            }
        } else {
            for vendor_key in table.keys() {
                let Some(Some(value)) = try_get_canonical(&canon.columns, vendor_key) else {
                    return Err(missing(vendor_key));
                };
                key.set(&plan.pg_name(vendor_key), Some(value.clone()));
            }
            for (name, value) in canon.columns.iter() {
                if plan.unmapped.contains(name) && !plan.key_columns.contains(name) {
                    extra.set(name, value.clone());
                } else {
                    columns.set(&plan.pg_name(name), value.clone());
                }
            }
        }

        let extra = extra.finish();
        if !extra.is_empty() {
            if (self.options.target_has_extra)(&pg_target) {
                columns.set(EXTRA_COLUMN, Some(extra_json(&extra)));
            } else {
                dropped.extend(extra.keys().map(str::to_owned));
            }
        }

        Ok(MappedRow {
            table: table.table.clone(),
            pg_target,
            key: key.finish(),
            columns: columns.finish(),
            hash: canon.hash,
            dropped,
            excluded: canon.excluded,
            blob_hashes: canon.blob_hashes,
            key_aligned: plan.key_aligned,
            defaulted_keys: defaulted,
        })
    }
}

/// The first canonical column whose name equals `name` ignoring case.
fn try_get_canonical<'a>(columns: &'a StrMap, name: &str) -> Option<&'a Option<String>> {
    columns.get_ignore_case(name).map(|(_, v)| v)
}

/// `Utf8JsonWriter` (default encoder, compact) over the ordinal-sorted extra columns.
fn extra_json(extra: &StrMap) -> String {
    let mut w = Writer::new(Style::Compact);
    w.begin_object();
    for (k, v) in extra.iter() {
        w.key(k);
        match v {
            Some(s) => w.string(s),
            None => w.null(),
        };
    }
    w.end_object();
    w.finish()
}
