//! Agent-driven legacy import (`Wire/LegacyImport.cs`, ADR 0095/0096). The ack has no
//! C# serializer context of its own: the reader takes the options (camelCase API
//! envelope or snake_case), the writer the naming policy.

use crate::contracts::json::{self, WireJson};
use crate::util::stj::{Naming, Obj, ReadError, Writer};

/// Paths and limits of the import upload.
pub mod legacy_import_wire {
    /// Agent path prefix (`PUT {prefix}{sha256}?dryRun=..&preferLegacy=..`).
    pub const PATH_PREFIX: &str = "/sync/v1/legacy/import/";
    /// API-key path prefix (ADR 0096).
    pub const KEY_PATH_PREFIX: &str = "/api/v1/legacy/import/";
    /// Body content type.
    pub const CONTENT_TYPE: &str = "application/zip";
    /// Largest zip the cloud accepts.
    pub const MAX_BYTES: i64 = 95 * 1024 * 1024;

    /// Path and query the agent signs and sends.
    pub fn path_and_query(sha256: &str, dry_run: bool, prefer_legacy: bool) -> String {
        query(PATH_PREFIX, sha256, dry_run, prefer_legacy)
    }

    /// Path and query of the API-key upload.
    pub fn key_path_and_query(sha256: &str, dry_run: bool, prefer_legacy: bool) -> String {
        query(KEY_PATH_PREFIX, sha256, dry_run, prefer_legacy)
    }

    fn query(prefix: &str, sha256: &str, dry_run: bool, prefer_legacy: bool) -> String {
        format!("{prefix}{sha256}?dryRun={dry_run}&preferLegacy={prefer_legacy}")
    }
}

fn key<'w>(w: &'w mut Writer, naming: Naming, snake: &str) -> &'w mut Writer {
    w.key(&naming.name(snake))
}

/// One target table of the import.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LegacyImportTableAck {
    /// Bundle table.
    pub source: String,
    /// Cloud table.
    pub target: String,
    /// Rows in the bundle.
    pub source_rows: i64,
    /// Rows inserted.
    pub inserted: i64,
    /// Rows already equal.
    pub same: i64,
    /// Rows the tenant has with other values.
    pub differing: i64,
    /// Whether differing rows were taken from the bundle.
    pub updated: bool,
    /// Keys of differing rows.
    pub differing_keys: Vec<String>,
}

impl LegacyImportTableAck {
    /// Writes with a naming policy.
    pub fn write_with(&self, w: &mut Writer, n: Naming) {
        w.begin_object();
        key(w, n, "source").string(&self.source);
        key(w, n, "target").string(&self.target);
        key(w, n, "source_rows").int(self.source_rows);
        key(w, n, "inserted").int(self.inserted);
        key(w, n, "same").int(self.same);
        key(w, n, "differing").int(self.differing);
        key(w, n, "updated").bool(self.updated);
        key(w, n, "differing_keys");
        json::write_strings(w, &self.differing_keys);
        w.end_object();
    }
}

impl WireJson for LegacyImportTableAck {
    fn write_json(&self, w: &mut Writer) {
        self.write_with(w, Naming::SnakeLower);
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(LegacyImportTableAck {
            source: o.string("source")?,
            target: o.string("target")?,
            source_rows: o.int("source_rows")?,
            inserted: o.int("inserted")?,
            same: o.int("same")?,
            differing: o.int("differing")?,
            updated: o.bool("updated")?,
            differing_keys: o.list("differing_keys", json::string_item)?,
        })
    }
}

/// A branch the import created, renamed or closed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LegacyImportBranchAck {
    /// RVC.
    pub rvc_no: i32,
    /// Name.
    pub name: String,
    /// What happened.
    pub action: String,
}

impl LegacyImportBranchAck {
    /// Writes with a naming policy.
    pub fn write_with(&self, w: &mut Writer, n: Naming) {
        w.begin_object();
        key(w, n, "rvc_no").int(self.rvc_no.into());
        key(w, n, "name").string(&self.name);
        key(w, n, "action").string(&self.action);
        w.end_object();
    }
}

impl WireJson for LegacyImportBranchAck {
    fn write_json(&self, w: &mut Writer) {
        self.write_with(w, Naming::SnakeLower);
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(LegacyImportBranchAck {
            rvc_no: o.int("rvc_no")?,
            name: o.string("name")?,
            action: o.string("action")?,
        })
    }
}

/// A printer that printed through the on-prem server.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LegacyImportHostPrinterAck {
    /// RVC.
    pub rvc_no: Option<i32>,
    /// Name.
    pub name: String,
}

impl LegacyImportHostPrinterAck {
    /// Writes with a naming policy; `None` members are written as null when
    /// `write_nulls` (options without `WhenWritingNull`).
    pub fn write_with(&self, w: &mut Writer, n: Naming, write_nulls: bool) {
        w.begin_object();
        match self.rvc_no {
            Some(r) => {
                key(w, n, "rvc_no").int(r.into());
            }
            None if write_nulls => {
                key(w, n, "rvc_no").null();
            }
            None => {}
        }
        key(w, n, "name").string(&self.name);
        w.end_object();
    }
}

impl WireJson for LegacyImportHostPrinterAck {
    fn write_json(&self, w: &mut Writer) {
        self.write_with(w, Naming::SnakeLower, false);
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(LegacyImportHostPrinterAck {
            rvc_no: o.opt_int("rvc_no")?,
            name: o.string("name")?,
        })
    }
}

/// Answer of the import endpoint, also for a refusal the importer reached.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LegacyImportAck {
    /// Tenant slug.
    pub slug: String,
    /// Dry run.
    pub dry_run: bool,
    /// True only for a real import that was written.
    pub committed: bool,
    /// A `LifecycleRefusals` code, else `None`.
    pub refusal_code: Option<String>,
    /// Refusal text.
    pub refusal: Option<String>,
    /// Hash of `manifest.json`.
    pub manifest_sha256: String,
    /// Tenant status.
    pub tenant_status: String,
    /// Data mode.
    pub data_mode: String,
    /// Branches.
    pub branches: Vec<LegacyImportBranchAck>,
    /// Tables.
    pub tables: Vec<LegacyImportTableAck>,
    /// Archived tables.
    pub archived: Vec<String>,
    /// Left-out tables.
    pub left_out: Vec<String>,
    /// Host printers to convert.
    pub host_printers: Vec<LegacyImportHostPrinterAck>,
}

impl LegacyImportAck {
    /// Writes with a naming policy; `None` members written as null when `write_nulls`.
    pub fn write_with(&self, w: &mut Writer, n: Naming, write_nulls: bool) {
        w.begin_object();
        key(w, n, "slug").string(&self.slug);
        key(w, n, "dry_run").bool(self.dry_run);
        key(w, n, "committed").bool(self.committed);
        for (name, v) in [
            ("refusal_code", &self.refusal_code),
            ("refusal", &self.refusal),
        ] {
            match v {
                Some(s) => {
                    key(w, n, name).string(s);
                }
                None if write_nulls => {
                    key(w, n, name).null();
                }
                None => {}
            }
        }
        key(w, n, "manifest_sha256").string(&self.manifest_sha256);
        key(w, n, "tenant_status").string(&self.tenant_status);
        key(w, n, "data_mode").string(&self.data_mode);
        key(w, n, "branches").begin_array();
        for b in &self.branches {
            b.write_with(w, n);
        }
        w.end_array();
        key(w, n, "tables").begin_array();
        for t in &self.tables {
            t.write_with(w, n);
        }
        w.end_array();
        key(w, n, "archived");
        json::write_strings(w, &self.archived);
        key(w, n, "left_out");
        json::write_strings(w, &self.left_out);
        key(w, n, "host_printers").begin_array();
        for p in &self.host_printers {
            p.write_with(w, n, write_nulls);
        }
        w.end_array();
        w.end_object();
    }
}

impl WireJson for LegacyImportAck {
    fn write_json(&self, w: &mut Writer) {
        self.write_with(w, Naming::SnakeLower, false);
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(LegacyImportAck {
            slug: o.string("slug")?,
            dry_run: o.bool("dry_run")?,
            committed: o.bool("committed")?,
            refusal_code: o.opt_string("refusal_code")?,
            refusal: o.opt_string("refusal")?,
            manifest_sha256: o.string("manifest_sha256")?,
            tenant_status: o.string("tenant_status")?,
            data_mode: o.string("data_mode")?,
            branches: o.list("branches", json::item)?,
            tables: o.list("tables", json::item)?,
            archived: o.list("archived", json::string_item)?,
            left_out: o.list("left_out", json::string_item)?,
            host_printers: o.list("host_printers", json::item)?,
        })
    }
}
