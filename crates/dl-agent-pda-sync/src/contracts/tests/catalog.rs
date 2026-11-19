//! `CatalogCoverageTests.cs`, `ServiceTimingRenameCatalogTests.cs`.

use super::*;
use crate::contracts::catalog::CatalogTable;
use crate::util::stj::eq_ignore_case;

#[test]
fn embedded_catalog_loads_with_expected_shape() {
    let c = catalog();
    assert_eq!(c.catalog_version, 1);
    assert_eq!(c.tables.len(), 426);
    assert_eq!(c.classified().count(), 132);
    assert_eq!(c.sqlite_tables.len(), 79);
}

#[test]
fn every_classified_table_has_a_key_and_a_known_capture_strategy() {
    let c = catalog();
    let bad: Vec<String> = c
        .classified()
        .filter(|t| {
            t.keys().is_empty()
                || t.capture.as_deref().is_none_or(str::is_empty)
                || !c
                    .conventions
                    .capture_strategies
                    .iter()
                    .any(|s| Some(s.as_str()) == t.capture.as_deref())
        })
        .map(|t| {
            format!(
                "{} keys=[{}] capture={:?}",
                t.table,
                t.keys().join(","),
                t.capture
            )
        })
        .collect();
    assert!(bad.is_empty(), "{bad:?}");
}

#[test]
fn every_classified_table_has_a_valid_class() {
    let c = catalog();
    assert!(
        c.classified()
            .all(|t| c.conventions.classes.contains_key(&t.class))
    );
}

#[test]
fn tables_with_a_pg_target_declare_pg_keys() {
    let bad: Vec<&str> = catalog()
        .classified()
        .filter(|t| t.has_pg_target() && !t.pg_raw && t.pg_keys.as_ref().is_none_or(Vec::is_empty))
        .map(|t| t.table.as_str())
        .collect();
    assert!(bad.is_empty(), "{bad:?}");
}

#[test]
fn watermark_tables_have_a_monotonic_key_or_candidate() {
    let bad: Vec<&str> = catalog()
        .classified()
        .filter(|t| {
            t.capture.as_deref() == Some("watermark+rescan")
                && t.monotonic_key.is_none()
                && t.watermark_candidates.as_ref().is_none_or(Vec::is_empty)
        })
        .map(|t| t.table.as_str())
        .collect();
    assert!(bad.is_empty(), "{bad:?}");
}

#[test]
fn overlay_keys_are_only_used_where_the_vendor_schema_has_none() {
    let overlay: Vec<&CatalogTable> = catalog()
        .classified()
        .filter(|t| t.keys_from_overlay)
        .collect();
    assert!(!overlay.is_empty());
    assert!(
        overlay
            .iter()
            .all(|t| t.source_keys.as_ref().is_some_and(|k| !k.is_empty()))
    );
    let mut dup: Vec<&str> = catalog()
        .classified()
        .filter(|t| t.duplicate_rows_tolerated)
        .map(|t| t.table.as_str())
        .collect();
    dup.sort();
    assert_eq!(dup, ["CashierRVC", "tmpInvoiceInfo"]);
}

#[test]
fn unclassified_tables_are_allowed_but_listed() {
    // The S0 probe found 118; a new one means the catalog was regenerated.
    assert_eq!(catalog().unclassified().count(), 118);
    assert!(catalog().unclassified().all(|t| t.rows > 0));
}

#[test]
fn never_sync_list_is_enforced_by_exclude_columns_of_every_classified_table() {
    let c = catalog();
    let mut parsed = 0;
    for entry in &c.conventions.never_sync {
        let dot = entry.rfind('.').unwrap();
        let cols: Vec<&str> = entry[dot + 1..].split('/').collect();
        for table in entry[..dot].split('/') {
            let t = c
                .find(table)
                .unwrap_or_else(|| panic!("never_sync names table {table}"));
            let listed = c.never_sync_columns(table);
            assert!(
                cols.iter().all(|x| listed.iter().any(|l| l == x)),
                "{table}"
            );
            parsed += 1;
            if !t.is_classified() {
                continue; // tbAccount is unclassified; the mapper still refuses it
            }
            for x in &cols {
                assert!(
                    t.exclude_columns.iter().flatten().any(|e| e == x),
                    "{table}.{x} must be excluded from sync"
                );
            }
        }
    }
    assert!(parsed > 8);
}

#[test]
fn class_e_sqlite_tables_have_pg_target_or_reason() {
    let c = catalog();
    let empty = |s: &Option<String>| s.as_deref().is_none_or(str::is_empty);
    assert!(
        !c.sqlite_tables
            .iter()
            .any(|t| t.class == "E" && empty(&t.pg_target) && empty(&t.note))
    );
    assert!(
        !c.sqlite_tables
            .iter()
            .any(|t| t.class == "skip" && empty(&t.reason))
    );
}

fn listed(l: &Option<Vec<String>>, c: &str) -> bool {
    l.iter().flatten().any(|x| eq_ignore_case(x, c))
}

fn sample_for(t: &CatalogTable, column: &str) -> RawValue {
    let tr = t.transforms.clone().unwrap_or_default();
    if listed(&tr.money_decimal_string, column) {
        return dec("2.5");
    }
    if listed(&tr.local_time_to_utc, column) {
        return dt(2026, 1, 1, 0, 0, 0);
    }
    "x".into()
}

/// Inserts into a case-insensitive row (`new Dictionary(StringComparer.OrdinalIgnoreCase)`):
/// an existing key keeps its spelling and takes the new value.
fn set_ci(r: &mut VendorRow, k: &str, v: RawValue) {
    let existing = r.get_ignore_case(k).map(|(name, _)| name.to_owned());
    r.insert(existing.unwrap_or_else(|| k.to_owned()), v);
}

#[test]
fn every_mappable_table_maps_a_synthetic_row_without_leaking_excluded_columns() {
    let m = mapper();
    let mut count = 0;
    for t in m.mappable_tables() {
        let mut r = VendorRow::new();
        for k in t.keys() {
            set_ci(&mut r, k, RawValue::I32(1));
        }
        let pg_keys = t.pg_keys.clone().unwrap_or_default();
        for (v, p) in t.pg_columns.iter().flat_map(|c| c.iter()) {
            if pg_keys.iter().any(|k| k == p) {
                set_ci(&mut r, v, RawValue::I32(1));
            }
        }
        for c in t.unmapped_vendor_columns.iter().flatten() {
            if r.get_ignore_case(c).is_none() {
                r.insert(c.clone(), sample_for(t, c));
            }
        }
        let tr = t.transforms.clone().unwrap_or_default();
        for c in tr.money_decimal_string.iter().flatten() {
            set_ci(&mut r, c, dec("2.5"));
        }
        for c in tr.local_time_to_utc.iter().flatten() {
            set_ci(&mut r, c, dt(2026, 1, 1, 0, 0, 0));
        }
        for c in t.exclude_columns.iter().flatten() {
            set_ci(&mut r, c, "SECRET".into());
        }
        let mapped = m
            .map(&t.table, &r)
            .unwrap_or_else(|e| panic!("{}: {e}", t.table));
        count += 1;
        assert_eq!(Some(&mapped.pg_target), t.pg_target.as_ref());
        let want = if t.is_typed() {
            pg_keys.len()
        } else {
            t.keys().len()
        };
        assert_eq!(mapped.key.len(), want, "{}", t.table);
        let mut ex = mapped.excluded.clone();
        ex.sort();
        let mut want_ex = t.exclude_columns.clone().unwrap_or_default();
        want_ex.sort();
        assert_eq!(ex, want_ex, "{}", t.table);
        assert!(
            !mapped
                .columns
                .values()
                .any(|v| v.as_deref() == Some("SECRET")),
            "{}",
            t.table
        );
        assert!(
            mapped.hash.len() == 64
                && mapped
                    .hash
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
    }
    assert_eq!(
        count,
        catalog().classified().filter(|t| t.has_pg_target()).count()
    );
}

// ---- ServiceTimingRenameCatalogTests ---------------------------------------------------

const STS_TABLES: &[(&str, &str)] = &[
    ("app.check_sts", "app.check_service_timings"),
    ("app.sts_slot", "app.service_timing_slots"),
    ("app.sts_value", "app.service_timing_values"),
    ("app.ird_config", "app.in_room_dining_settings"),
    ("app.ird_delivery", "app.in_room_dining_deliveries"),
];
const STS_COLUMNS: &[&str] = &["sts_code", "sts_name"];

#[test]
fn a_pda_owned_table_targets_the_renamed_table() {
    for (sqlite, expected) in [
        ("pda_ird_config", "app.in_room_dining_settings"),
        ("pda_ird_delivery", "app.in_room_dining_deliveries"),
        ("pda_sts_slot", "app.service_timing_slots"),
    ] {
        let e: Vec<_> = catalog()
            .sqlite_tables
            .iter()
            .filter(|t| t.table == sqlite)
            .collect();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].pg_target.as_deref(), Some(expected));
        assert!(STS_TABLES.iter().any(|(_, new)| *new == expected));
    }
}

#[test]
fn the_catalog_targets_no_table_or_column_renamed_by_0229() {
    let c = catalog();
    let old = |t: &Option<String>| STS_TABLES.iter().any(|(o, _)| Some(*o) == t.as_deref());
    for t in &c.sqlite_tables {
        assert!(!old(&t.pg_target), "{}", t.table);
    }
    for t in &c.tables {
        assert!(!old(&t.pg_target), "{}", t.table);
        assert!(
            !t.pg_keys
                .iter()
                .flatten()
                .any(|k| STS_COLUMNS.contains(&k.as_str()))
        );
        assert!(
            !t.pg_columns
                .iter()
                .flat_map(|m| m.values())
                .any(|v| STS_COLUMNS.contains(&v.as_str()))
        );
    }
}
