//! Blob contract v1 (`Wire/Blobs.cs`, ADR 0030).

use crate::contracts::catalog::SyncCatalog;
use crate::contracts::json::{self, WireJson};
use crate::contracts::map::StrMap;
use crate::contracts::utf16_len;
use crate::util::stj::{Obj, ReadError, Writer, eq_ignore_case};

/// Cloud address of one blob column value; content travels separately by its hash.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncBlobRef {
    /// Vendor table as `sync-catalog.json` keys it.
    pub table: String,
    /// The row key exactly as [`super::SyncRow::key`] carries it.
    pub key: StrMap,
    /// Vendor blob column, listed in the table's `hash_exclude`.
    pub column: String,
    /// Lowercase hex SHA-256 of the content.
    pub sha256: String,
}

impl WireJson for SyncBlobRef {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("table").string(&self.table);
        w.key("key");
        json::write_str_map(w, &self.key);
        w.key("column").string(&self.column);
        w.key("sha256").string(&self.sha256);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(SyncBlobRef {
            table: o.string("table")?,
            key: json::str_map(&o, "key")?,
            column: o.string("column")?,
            sha256: o.string("sha256")?,
        })
    }
}

/// Points rows at already uploaded content (`POST /sync/v1/blobs/refs`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncBlobRefBatch {
    /// Agent id.
    pub agent_id: String,
    /// [`blob_contract::VERSION`].
    pub contract_version: i32,
    /// Refs.
    pub refs: Vec<SyncBlobRef>,
}

impl WireJson for SyncBlobRefBatch {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("agent_id").string(&self.agent_id);
        w.key("contract_version").int(self.contract_version.into());
        w.key("refs");
        json::write_items(w, &self.refs);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(SyncBlobRefBatch {
            agent_id: o.string("agent_id")?,
            contract_version: o.int("contract_version")?,
            refs: o.list("refs", json::item)?,
        })
    }
}

/// Cloud reply to [`SyncBlobRefBatch`]: refs whose content the cloud lacks are listed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncBlobRefsAck {
    /// Refs applied.
    pub applied: i32,
    /// Hashes the cloud does not hold yet.
    pub missing: Vec<String>,
}

impl WireJson for SyncBlobRefsAck {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("applied").int(self.applied.into());
        w.key("missing");
        json::write_strings(w, &self.missing);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(SyncBlobRefsAck {
            applied: o.int("applied")?,
            missing: o.list("missing", json::string_item)?,
        })
    }
}

/// Versioned blob transport contract (`BlobContract`).
pub mod blob_contract {
    use super::*;

    /// Current contract version written by agents.
    pub const VERSION: i32 = 1;
    /// Oldest version the cloud still accepts.
    pub const MIN_SUPPORTED_VERSION: i32 = 1;
    /// Largest content accepted for one blob (same ceiling as a batch body).
    pub const MAX_BYTES: i64 = super::super::wire_version::MAX_BODY_BYTES as i64;
    /// Most refs in one [`SyncBlobRefBatch`].
    pub const MAX_REFS_PER_BATCH: i32 = 500;
    /// Content upload path prefix.
    pub const CONTENT_PATH_PREFIX: &str = "/sync/v1/blobs/";
    /// Refs path.
    pub const REFS_PATH: &str = "/sync/v1/blobs/refs";
    /// Content type of an upload.
    pub const CONTENT_TYPE: &str = "application/octet-stream";

    /// Content upload path for a hash (`PUT`, raw bytes as the body).
    pub fn content_path(sha256: &str) -> String {
        format!("{CONTENT_PATH_PREFIX}{sha256}")
    }

    /// True for 64 lowercase hex characters.
    pub fn is_sha256(value: Option<&str>) -> bool {
        value.is_some_and(|v| {
            v.len() == 64
                && v.bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        })
    }

    /// Validates a ref against the catalog: `None` when acceptable, else a reason safe to
    /// log and return. Never-sync and excluded columns are refused even if listed as blobs.
    pub fn check(r: Option<&SyncBlobRef>, catalog: &SyncCatalog) -> Option<String> {
        let Some(r) = r else {
            return Some("ref is required".into());
        };
        if !is_sha256(Some(&r.sha256)) {
            return Some("sha256 must be 64 lowercase hex characters".into());
        }
        let t = if r.table.trim().is_empty() {
            None
        } else {
            catalog.find(&r.table)
        };
        let Some(t) = t.filter(|t| t.is_classified() && t.has_pg_target()) else {
            return Some(format!("table '{}' is not a synced table", r.table));
        };
        if r.column.trim().is_empty() || utf16_len(&r.column) > 128 {
            return Some("column is required".into());
        }
        let in_list = |l: &Option<Vec<String>>| {
            l.as_ref()
                .is_some_and(|l| l.iter().any(|c| eq_ignore_case(c, &r.column)))
        };
        if t.hash_exclude.as_ref().is_none_or(|h| h.is_empty()) || !in_list(&t.hash_exclude) {
            return Some(format!(
                "column '{}' is not a blob column of '{}'",
                r.column, t.table
            ));
        }
        if catalog
            .never_sync_columns(&t.table)
            .iter()
            .any(|c| eq_ignore_case(c, &r.column))
            || in_list(&t.exclude_columns)
        {
            return Some(format!("column '{}' must not leave the site", r.column));
        }
        if r.key.is_empty() {
            return Some("key is required".into());
        }
        if r.key.keys().any(|k| k.trim().is_empty()) {
            Some("key column name is required".into())
        } else {
            None
        }
    }
}
