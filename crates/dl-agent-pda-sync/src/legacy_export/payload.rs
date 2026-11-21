//! The payload of the `legacy.export` site command (`LegacyExportPayload.cs`, ADR 0136)
//! and the values the cloud contract names around it.

use crate::contracts::WireJson;
use crate::contracts::json;
use crate::util::stj::{Obj, ReadError, Writer};

/// Wire name of the command (`SiteCommandTypes.LegacyExport`).
pub const LEGACY_EXPORT: &str = "legacy.export";
/// The one aggregate of every legacy export command: the queue hands them out one at a time.
pub const AGGREGATE_KEY: &str = "legacy:export";

/// Values of `source` (`LegacyExportSources`).
pub mod sources {
    /// The agent decides like the migrate script: the PDA database when it exists, else a
    /// from-SQL export.
    pub const AUTO: &str = "auto";
    /// Export from the on-prem PDA order database.
    pub const PDA_DB: &str = "pda-db";
    /// Export straight from the SMILE SQL Server database.
    pub const FROM_SQL: &str = "from-sql";
    /// Every value.
    pub const ALL: &[&str] = &[AUTO, PDA_DB, FROM_SQL];
}

/// Site event codes and sources of the run (`LegacyExportEventCodes`).
pub mod event_codes {
    /// The agent accepted the command and began the run (source `legacy-export`).
    pub const STARTED: &str = "LEGACY_EXPORT_STARTED";
    /// A stage failed. Detail: run id, stage, table, sql_number, error code; never row data.
    pub const FAILED: &str = "LEGACY_EXPORT_FAILED";
    /// The final upload finished and the local bundle was deleted (source `legacy-upload`).
    pub const UPLOADED: &str = "LEGACY_EXPORT_UPLOADED";
    /// Source of the export stages.
    pub const SOURCE_EXPORT: &str = "legacy-export";
    /// Source of the upload stages.
    pub const SOURCE_UPLOAD: &str = "legacy-upload";
}

/// Longest optional text member.
pub const MAX_TEXT: usize = 260;
/// Longest reason.
pub const MAX_REASON: usize = 300;

/// The cloud's order to do what `migrate-legacy.ps1` does by hand.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LegacyExportPayload {
    /// One of [`sources`].
    pub source: String,
    /// Optional SQL Server instance override.
    pub sql_instance: Option<String>,
    /// Optional database name override.
    pub database: Option<String>,
    /// Optional IIS site root of the on-prem edition.
    pub site_root: Option<String>,
    /// Optional IIS application pool of the on-prem edition.
    pub app_pool: Option<String>,
    /// Run although the store is live; false keeps the maintenance gate.
    pub allow_live: bool,
    /// False: upload as a dry run only. True: dry run, commit, then the post-commit dry run.
    pub commit: bool,
    /// Passed to the upload as `preferLegacy`.
    pub prefer_legacy: bool,
    /// Export only the VietQR account (the only import an active tenant accepts, ADR 0099).
    pub vietqr_only: bool,
    /// Why the operator ordered it.
    pub reason: String,
}

fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

fn text_problem(value: &Option<String>) -> Option<&'static str> {
    let v = value.as_deref()?;
    let len = utf16_len(v);
    if len == 0 || len > MAX_TEXT {
        return Some("must be 1..260 characters when given");
    }
    v.chars()
        .any(char::is_control)
        .then_some("must not contain control characters")
}

impl LegacyExportPayload {
    /// The validation problem, `None` when valid (`Problem()`).
    pub fn problem(&self) -> Option<String> {
        if !sources::ALL.contains(&self.source.as_str()) {
            return Some("source must be auto, pda-db or from-sql".to_owned());
        }
        for (name, v) in [
            ("sql_instance", &self.sql_instance),
            ("database", &self.database),
            ("site_root", &self.site_root),
            ("app_pool", &self.app_pool),
        ] {
            if let Some(p) = text_problem(v) {
                return Some(format!("{name} {p}"));
            }
        }
        if self.reason.trim().is_empty() {
            return Some("reason is required".to_owned());
        }
        if utf16_len(&self.reason) > MAX_REASON {
            return Some("reason must be at most 300 characters".to_owned());
        }
        self.reason
            .chars()
            .any(char::is_control)
            .then(|| "reason must not contain control characters".to_owned())
    }
}

impl WireJson for LegacyExportPayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("source").string(&self.source);
        json::opt_str(w, "sql_instance", &self.sql_instance);
        json::opt_str(w, "database", &self.database);
        json::opt_str(w, "site_root", &self.site_root);
        json::opt_str(w, "app_pool", &self.app_pool);
        w.key("allow_live").bool(self.allow_live);
        w.key("commit").bool(self.commit);
        w.key("prefer_legacy").bool(self.prefer_legacy);
        w.key("viet_qr_only").bool(self.vietqr_only);
        w.key("reason").string(&self.reason);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(LegacyExportPayload {
            source: o.string("source")?,
            sql_instance: o.opt_string("sql_instance")?,
            database: o.opt_string("database")?,
            site_root: o.opt_string("site_root")?,
            app_pool: o.opt_string("app_pool")?,
            allow_live: o.bool("allow_live")?,
            commit: o.bool("commit")?,
            prefer_legacy: o.bool("prefer_legacy")?,
            vietqr_only: o.bool("viet_qr_only")?,
            reason: o.string("reason")?,
        })
    }
}
