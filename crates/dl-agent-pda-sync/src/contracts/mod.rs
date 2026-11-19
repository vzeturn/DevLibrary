//! Wire DTOs, sync catalog, row hashing, legacy bundle (port of `src/MM.PdaOrder.Sync.Contracts`)
//! and the catalog-driven row mapper (port of `src/MM.PdaOrder.Sync.Mapping`).
//!
//! - [`wire`]: every DTO of the snake_case JSON contexts, with [`WireJson`] for
//!   `to_json` / `from_json` byte-compatible with System.Text.Json.
//! - [`catalog`]: the embedded `sync-catalog.json` + `logical-keys.json`
//!   ([`SyncCatalog::load`]).
//! - [`hashing`]: canonical values, row hash, row key.
//! - [`time`]: .NET `DateTimeOffset` / `DateTime` / `TimeZoneInfo` subset.
//! - [`map`]: the insertion-ordered map behind every `Dictionary<string, string?>`.
//! - [`legacy`]: the legacy bundle contract (`manifest.json`, builds, SQL store map).
//! - [`staff`]: PIN lookup v2.
//! - [`mapping`]: vendor row to canonical row, row hash and PG row.

pub mod catalog;
pub mod hashing;
pub mod json;
pub mod legacy;
pub mod map;
pub mod mapping;
pub mod staff;
pub mod time;
pub mod wire;

#[cfg(test)]
mod tests;

pub use catalog::{CatalogError, CatalogTable, SyncCatalog};
pub use json::{ContractError, WireJson};
pub use map::{OrderedMap, StrMap};
pub use time::{DateTime, DateTimeKind, DateTimeOffset, TimeZone};

/// Length in UTF-16 code units (`string.Length` in C#), which the C# validators compare
/// against their limits.
pub fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}
