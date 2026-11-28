//! Ports of `tests/MM.PdaOrder.Sync.Tests/*.cs` (same cases, same expected values) and the
//! byte-for-byte cross-checks against System.Text.Json output captured from .NET 10.

mod cashier;
mod catalog;
mod dotnet;
mod hashing;
mod hops;
mod legacy;
mod mapper;
mod wire;
mod wire_po_number;
mod wire_pos_order_residue;
mod wire_pos_reads;
mod wire_pos_stock;
mod wire_txn;
mod wire_txn_counters;
mod wire_txn_room_charge;
mod wire_txn_shift;
mod wire_txn_stock;
mod wire_txn_stock_issue;

use super::catalog::SyncCatalog;
use super::map::{OrderedMap, StrMap};
use super::mapping::{CatalogRowMapper, MapperOptions, RawValue, VendorRow};
use super::time::{DateTime, TimeZone};

/// `Fixtures.Catalog`: the embedded catalog with the overlay.
pub(super) fn catalog() -> &'static SyncCatalog {
    SyncCatalog::load().expect("embedded catalog loads")
}

/// `Fixtures.Vietnam`.
pub(super) fn vietnam() -> TimeZone {
    TimeZone::find_system_time_zone_by_id("Asia/Ho_Chi_Minh").expect("zone")
}

/// `Fixtures.Options(hasExtra)`.
pub(super) fn options(has_extra: Option<fn(&str) -> bool>) -> MapperOptions {
    let mut o = MapperOptions::new(vietnam());
    if let Some(f) = has_extra {
        o.target_has_extra = Box::new(f);
    }
    o
}

/// `Fixtures.Mapper(hasExtra)`.
pub(super) fn mapper() -> CatalogRowMapper {
    CatalogRowMapper::new(catalog().clone(), options(None))
}

/// TCVN3 bytes of "Phở bò": P h 0xEB(ở) space b 0xDF(ò).
pub(super) const PHO_BO_TCVN3: [u8; 6] = [0x50, 0x68, 0xEB, 0x20, 0x62, 0xDF];

/// A vendor row from `(column, value)` pairs, in order.
pub(super) fn row<const N: usize>(pairs: [(&str, RawValue); N]) -> VendorRow {
    pairs.into_iter().collect()
}

/// `new DateTime(y, M, d, h, m, s)` as a raw value.
pub(super) fn dt(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> RawValue {
    RawValue::DateTime(DateTime::new(y, mo, d, h, mi, s).expect("valid date"))
}

/// A decimal from its invariant text (scale kept).
pub(super) fn dec(s: &str) -> RawValue {
    RawValue::Decimal(s.parse().expect("decimal"))
}

/// A string map from pairs.
pub(super) fn smap<const N: usize>(pairs: [(&str, Option<&str>); N]) -> StrMap {
    super::map::str_map(pairs)
}

/// The map as sorted `(key, value)` pairs, to compare maps like `BeEquivalentTo`.
pub(super) fn sorted<V: Clone + Ord>(m: &OrderedMap<V>) -> Vec<(String, V)> {
    let mut v: Vec<(String, V)> = m.iter().map(|(k, v)| (k.to_owned(), v.clone())).collect();
    v.sort();
    v
}
