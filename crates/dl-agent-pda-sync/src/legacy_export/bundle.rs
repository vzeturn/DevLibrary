//! Builds a legacy bundle (ADR 0085) from the on-prem edition (`LegacyExporter.cs`,
//! `LegacyBundleWriter.cs`): the tables of its store classified by the contract, the RVC
//! list of `SMILE_POS`, the plain registry brand values and the VietQR account (ADR 0099),
//! as gzip JSON lines per table and a `manifest.json`. SELECT-only everywhere; no other
//! secret is ever read or written.

use std::io::Write;
use std::path::Path;

use dl_decimal::Decimal;
use dl_hash::{Digest, Sha256};

use super::sources::{Cell, ExportError, LegacySqlSource, LegacyStore, Result, Row, cell, stages};
use crate::config::codes;
use crate::contracts::legacy::bundle::{
    self, FORMAT, FORMAT_VERSION, LIVE_REHEARSAL_MODE, REGISTRY_BRANDING_TABLE,
    REGISTRY_VIETQR_TABLE, RVC_LIST_TABLE, VIETQR_ONLY_MODE,
};
use crate::contracts::legacy::{
    LegacyBundleFile, LegacyBundleMaintenance, LegacyBundleManifest, LegacyBundleSource,
    LegacyDisposition, LegacyLeftOut,
};
use crate::contracts::time::DateTimeOffset;
use crate::util::stj::{Style, Writer};

/// The credential-bearing config table; its secret keys never leave.
pub const CONFIG_TABLE: &str = "pda_config";

/// Called after each bundle file is written, with its size in bytes (progress events;
/// never row values).
pub type FileWritten<'a> = &'a mut dyn FnMut(&LegacyBundleFile, u64);

/// What one export reads and how it names itself.
pub struct ExportParts<'a> {
    /// The store (`pda.db` or SQL Server tables of an older build).
    pub store: &'a dyn LegacyStore,
    /// The POS database (the RVC list, the old tables found there).
    pub sql: &'a dyn LegacySqlSource,
    /// The plain registry brand values.
    pub branding: &'a dyn BrandingSource,
    /// The VietQR account as found.
    pub vietqr: &'a VietQrRead,
    /// The legacy API's version, when known.
    pub app_version: Option<String>,
    /// Why the exporter judged the site quiet.
    pub maintenance: LegacyBundleMaintenance,
    /// `PdaSyncAgent <version>`.
    pub exporter: String,
    /// When the bundle was made.
    pub created_at: DateTimeOffset,
}

fn export_error(e: &std::io::Error, what: &str) -> ExportError {
    ExportError::io(stages::EXPORT, what, e)
}

/// Empties nothing: refuses a file or a non-empty directory, then creates the directory.
pub fn prepare_out_dir(dir: &Path) -> Result<()> {
    if dir.is_file() {
        return Err(ExportError::new(
            stages::EXPORT,
            codes::BAD_ARGUMENTS,
            format!("the bundle directory '{}' is a file.", dir.display()),
        ));
    }
    if dir.is_dir() {
        let mut entries =
            std::fs::read_dir(dir).map_err(|e| export_error(&e, "bundle directory"))?;
        if entries.next().is_some() {
            return Err(ExportError::new(
                stages::EXPORT,
                codes::BAD_ARGUMENTS,
                format!("the bundle directory '{}' is not empty.", dir.display()),
            ));
        }
    }
    std::fs::create_dir_all(dir).map_err(|e| export_error(&e, "bundle directory"))
}

/// One row as a JSON object. A BLOB becomes a PostgreSQL `bytea` hex literal, so the
/// importer needs no decoding.
fn row_json(row: &Row) -> String {
    let mut w = Writer::new(Style::Compact);
    w.begin_object();
    for (name, value) in row {
        w.key(name);
        match value {
            Cell::Null => {
                w.null();
            }
            Cell::Int(n) => {
                w.int(*n);
            }
            Cell::Real(f) if f.is_finite() => {
                w.f64(*f);
            }
            Cell::Real(f) => {
                w.string(&non_finite(*f));
            }
            Cell::Decimal(d) => {
                write_decimal(&mut w, d);
            }
            Cell::Text(s) => {
                w.string(s);
            }
            Cell::Blob(b) => {
                w.string(&format!("\\x{}", dl_encoding::hex::encode(b)));
            }
        }
    }
    w.end_object();
    w.finish()
}

fn write_decimal(w: &mut Writer, d: &Decimal) {
    w.decimal(d);
}

fn non_finite(f: f64) -> String {
    if f.is_nan() {
        "NaN".to_owned()
    } else if f > 0.0 {
        "Infinity".to_owned()
    } else {
        "-Infinity".to_owned()
    }
}

/// Writes one table: gzip JSON lines, never over an existing file. `fill` gets the row
/// sink and feeds it every row.
pub fn write_table(
    bundle_dir: &Path,
    table: &str,
    disposition: LegacyDisposition,
    fill: impl FnOnce(&mut dyn FnMut(Row) -> Result<()>) -> Result<()>,
) -> Result<LegacyBundleFile> {
    let relative = bundle::path_for(table).ok_or_else(|| {
        ExportError::new(
            stages::EXPORT,
            codes::LEGACY_EXPORT_FAILED,
            "a table name is not a valid bundle table name",
        )
        .at_table(table)
    })?;
    let full = bundle_dir.join(&relative);
    let mut text = Vec::new();
    let mut rows: i64 = 0;
    fill(&mut |row: Row| {
        text.extend_from_slice(row_json(&row).as_bytes());
        text.push(b'\n');
        rows += 1;
        Ok(())
    })
    .map_err(|e| {
        if e.table.is_none() {
            e.at_table(table)
        } else {
            e
        }
    })?;
    let gz = dl_flate::gzip::compress(&text, dl_flate::Level::Default);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| export_error(&e, "bundle directory").at_table(table))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&full)
        .map_err(|e| export_error(&e, "bundle file").at_table(table))?;
    file.write_all(&gz)
        .and_then(|()| file.flush())
        .map_err(|e| export_error(&e, "bundle file").at_table(table))?;
    Ok(LegacyBundleFile {
        table: table.to_owned(),
        path: relative,
        rows,
        sha256: dl_encoding::hex::encode(Sha256::digest(&gz)),
        disposition,
    })
}

/// Writes `manifest.json`.
pub fn write_manifest(bundle_dir: &Path, manifest: &LegacyBundleManifest) -> Result<()> {
    let path = bundle_dir.join(bundle::MANIFEST_FILE);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| export_error(&e, "manifest"))?;
    file.write_all(manifest.to_json().as_bytes())
        .and_then(|()| file.flush())
        .map_err(|e| export_error(&e, "manifest"))
}

fn written(
    out: &Path,
    file: LegacyBundleFile,
    notify: &mut dyn FnMut(&LegacyBundleFile, u64),
) -> LegacyBundleFile {
    let bytes = std::fs::metadata(out.join(&file.path)).map_or(0, |m| m.len());
    notify(&file, bytes);
    file
}

fn single_row(cells: impl IntoIterator<Item = (String, String)>) -> Row {
    cells.into_iter().map(|(k, v)| (k, Cell::Text(v))).collect()
}

fn write_vietqr(out: &Path, vietqr: &VietQrRead) -> Result<LegacyBundleFile> {
    let row = match vietqr {
        VietQrRead::Read(values) => single_row(values.iter().cloned()),
        _ => Vec::new(),
    };
    write_table(
        out,
        REGISTRY_VIETQR_TABLE,
        LegacyDisposition::Import,
        |sink| sink(row),
    )
}

/// The protected registry values the bundle does not carry, and what became of the VietQR
/// account.
pub fn dpapi_left_out(vietqr: &VietQrRead) -> LegacyLeftOut {
    let fate = match vietqr {
        VietQrRead::Read(_) => format!("the VietQR account moved as {REGISTRY_VIETQR_TABLE}"),
        VietQrRead::NotSet => "no VietQR account was set".to_owned(),
        VietQrRead::Unreadable(p) => {
            format!("the VietQR account could not be read ({p}): enter it again in the cloud")
        }
    };
    LegacyLeftOut {
        table: "registry.dpapi".to_owned(),
        rows: 0,
        reason: format!(
            "secret: SQL connection string, JWT, MFA and licence keys (DPAPI, never read); {fate}"
        ),
    }
}

// ---- registry: branding and VietQR

/// The legacy edition's registry key.
pub const REGISTRY_SUB_KEY: &str = r"SOFTWARE\MM-PDA";

/// Registry value name to cloud `BrandingSettings` property.
pub const BRAND_NAMES: &[(&str, &str)] = &[
    ("BrandName", "Name"),
    ("BrandSubName", "SubName"),
    ("BrandTagline", "Tagline"),
    ("BrandPromoFooter", "PromoFooter"),
    ("BrandAccentHex", "AccentHex"),
    ("BrandLogoSvg", "LogoSvg"),
    ("BrandThemePack", "ThemePack"),
];

/// Registry value name to cloud `VietQrSettings` field: the allow-list, nothing else of
/// the protected values is ever read.
pub const VIETQR_NAMES: &[(&str, &str)] = &[
    ("VietQrBankBin", "BankBin"),
    ("VietQrAccountNo", "AccountNo"),
    ("VietQrAccountName", "AccountName"),
    ("VietQrAmountLock", "AmountLock"),
    ("VietQrFixedAmount", "FixedAmount"),
    ("VietQrAmountMode", "AmountMode"),
    ("VietQrContentTemplate", "ContentTemplate"),
];

/// The plain brand values of the legacy edition, keyed by the cloud property names.
pub trait BrandingSource: Send + Sync {
    /// The values found; empty when there are none or the registry is not available.
    fn read(&self) -> Vec<(String, String)>;
}

/// What the export found of the legacy VietQR account.
#[derive(Debug, Clone, PartialEq)]
pub enum VietQrRead {
    /// No bank BIN or no account number: VietQR was never set up.
    NotSet,
    /// Read and decrypted: cloud field name to value.
    Read(Vec<(String, String)>),
    /// The values exist but could not be read; the problem never holds a value.
    Unreadable(String),
}

impl VietQrRead {
    /// A value of the account.
    pub fn value(&self, field: &str) -> Option<&str> {
        match self {
            VietQrRead::Read(v) => v.iter().find(|(k, _)| k == field).map(|(_, v)| v.as_str()),
            _ => None,
        }
    }

    /// The account number as it may be logged: its last four digits only.
    pub fn masked_account(&self) -> String {
        match self.value("AccountNo") {
            None | Some("") => String::new(),
            Some(a) if a.chars().count() <= 4 => "****".to_owned(),
            Some(a) => {
                let chars: Vec<char> = a.chars().collect();
                let tail: String = chars[chars.len() - 4..].iter().collect();
                format!("****{tail}")
            }
        }
    }
}

/// The legacy VietQR account (ADR 0099).
pub trait VietQrSource: Send + Sync {
    /// Reads the account.
    fn read(&self) -> VietQrRead;
}

/// The DPAPI-protected registry values of the legacy key (the seam under
/// [`RegistryVietQr`]): one value, decrypted; `Ok(None)` when absent or empty.
pub trait ProtectedRegistry: Send + Sync {
    /// Reads and decrypts `value_name` (a name of [`VIETQR_NAMES`] only).
    fn read_protected(&self, value_name: &str) -> std::result::Result<Option<String>, String>;
}

/// Reads the VietQR account of the legacy edition through `registry`; only the names of
/// [`VIETQR_NAMES`] are ever asked for.
pub struct RegistryVietQr<R: ProtectedRegistry>(pub R);

impl<R: ProtectedRegistry> VietQrSource for RegistryVietQr<R> {
    fn read(&self) -> VietQrRead {
        let mut values: Vec<(String, String)> = Vec::new();
        for (registry_name, cloud_name) in VIETQR_NAMES {
            match self.0.read_protected(registry_name) {
                Ok(Some(v)) => values.push(((*cloud_name).to_owned(), v)),
                Ok(None) => {}
                // Reported, never silent: the problem text holds no value.
                Err(problem) => return VietQrRead::Unreadable(problem),
            }
        }
        let has = |f: &str| values.iter().any(|(k, v)| k == f && !v.is_empty());
        if has("BankBin") && has("AccountNo") {
            VietQrRead::Read(values)
        } else {
            VietQrRead::NotSet
        }
    }
}

/// The registry of this machine: DPAPI blobs and brand values on Windows, nothing else.
#[derive(Debug, Default, Clone, Copy)]
pub struct MachineRegistry;

#[cfg(windows)]
mod machine {
    use super::*;
    use dl_winos::registry::{Hive, Key, Value, View};

    /// The fixed DPAPI entropy of the legacy edition (kept through the MM rebrand).
    const ENTROPY: &[u8] = b"SMILE-PDA/v1";

    fn open() -> std::result::Result<Option<Key>, dl_winos::Error> {
        match Key::open(Hive::LocalMachine, REGISTRY_SUB_KEY, View::Default) {
            Ok(k) => Ok(Some(k)),
            Err(e) if e.code() == Some(dl_winos::codes::FILE_NOT_FOUND) => Ok(None),
            Err(e) => Err(e),
        }
    }

    impl ProtectedRegistry for MachineRegistry {
        fn read_protected(&self, name: &str) -> std::result::Result<Option<String>, String> {
            if !VIETQR_NAMES.iter().any(|(n, _)| *n == name) {
                return Err(format!(
                    "'{name}' is not a VietQR value; no other protected value is ever read."
                ));
            }
            let key = match open() {
                Ok(Some(k)) => k,
                Ok(None) => return Ok(None),
                Err(e) if e.code() == Some(dl_winos::codes::ACCESS_DENIED) => {
                    return Err(format!(
                        "no access to HKLM\\{REGISTRY_SUB_KEY}; run as administrator"
                    ));
                }
                Err(_) => return Err(format!("I/O error reading HKLM\\{REGISTRY_SUB_KEY}")),
            };
            let blob = match key.value(name) {
                Ok(Some(Value::Binary(b))) if !b.is_empty() => b,
                Ok(_) => return Ok(None),
                Err(_) => return Err(format!("I/O error reading HKLM\\{REGISTRY_SUB_KEY}")),
            };
            let plain = dl_winsvc::dpapi::unprotect_with_entropy(
                &blob,
                dl_winsvc::dpapi::Scope::LocalMachine,
                ENTROPY,
            )
            .map_err(|_| {
                "the DPAPI blobs do not decrypt on this machine (copied from another machine, or corrupt)"
                    .to_owned()
            })?;
            String::from_utf8(plain)
                .map(Some)
                .map_err(|_| "a VietQR value is not UTF-8 text".to_owned())
        }
    }

    impl BrandingSource for MachineRegistry {
        fn read(&self) -> Vec<(String, String)> {
            let Ok(Some(key)) = open() else {
                return Vec::new();
            };
            BRAND_NAMES
                .iter()
                .filter_map(|(registry_name, cloud_name)| {
                    // A REG_SZ reads as text; a DPAPI blob (REG_BINARY) never does.
                    match key.value(registry_name) {
                        Ok(Some(Value::String(s))) if !s.is_empty() => {
                            Some(((*cloud_name).to_owned(), s))
                        }
                        _ => None,
                    }
                })
                .collect()
        }
    }
}

#[cfg(not(windows))]
mod machine {
    use super::*;

    impl ProtectedRegistry for MachineRegistry {
        fn read_protected(&self, _name: &str) -> std::result::Result<Option<String>, String> {
            Ok(None)
        }
    }

    impl BrandingSource for MachineRegistry {
        fn read(&self) -> Vec<(String, String)> {
            Vec::new()
        }
    }
}

/// The VietQR source of this machine (`LegacyRegistryVietQr.ForMachine`).
pub fn machine_vietqr() -> RegistryVietQr<MachineRegistry> {
    RegistryVietQr(MachineRegistry)
}

/// Exports the store into `out` and writes the manifest (`LegacyExporter.ExportAsync`).
pub fn export(
    out: &Path,
    p: &ExportParts<'_>,
    notify: FileWritten<'_>,
) -> Result<LegacyBundleManifest> {
    prepare_out_dir(out)?;
    let mut files: Vec<LegacyBundleFile> = Vec::new();
    let mut left_out: Vec<LegacyLeftOut> = Vec::new();

    for table in p.store.tables()? {
        let (disposition, reason) = bundle::classify(&table);
        if disposition == LegacyDisposition::Exclude {
            let rows = p.store.count(&table)?;
            left_out.push(LegacyLeftOut {
                table,
                rows,
                reason,
            });
            continue;
        }
        let file = write_table(out, &table, disposition, |sink| {
            p.store.for_each_row(&table, &mut |row| {
                if table == CONFIG_TABLE
                    && let Some(Cell::Text(key)) = cell(&row, "key")
                    && bundle::is_secret_config_key(key)
                {
                    let rvc = super::sql_source::cell_text(cell(&row, "rvc_no"));
                    left_out.push(LegacyLeftOut {
                        table: format!("{CONFIG_TABLE}[{rvc}:{key}]"),
                        rows: 1,
                        reason: "secret: enter it again in the cloud settings".to_owned(),
                    });
                    return Ok(());
                }
                sink(row)
            })
        })?;
        files.push(written(out, file, notify));
    }

    let rvcs = p.sql.rvc_list()?;
    let rvc_file = write_table(out, RVC_LIST_TABLE, LegacyDisposition::Import, |sink| {
        for (no, name) in &rvcs {
            sink(vec![
                ("RVCNo".to_owned(), Cell::Int(i64::from(*no))),
                ("RVCName".to_owned(), Cell::Text(name.clone())),
            ])?;
        }
        Ok(())
    })?;
    files.push(written(out, rvc_file, notify));
    for (table, rows) in p.sql.pda_tables()? {
        if let Some(reason) = p.store.sql_table_left_out(&table) {
            left_out.push(LegacyLeftOut {
                table,
                rows,
                reason,
            });
        }
    }

    let brand = p.branding.read();
    if !brand.is_empty() {
        let file = write_table(
            out,
            REGISTRY_BRANDING_TABLE,
            LegacyDisposition::Import,
            |sink| sink(single_row(brand.iter().cloned())),
        )?;
        files.push(written(out, file, notify));
    }
    if matches!(p.vietqr, VietQrRead::Read(_)) {
        let file = write_vietqr(out, p.vietqr)?;
        files.push(written(out, file, notify));
    }
    left_out.push(dpapi_left_out(p.vietqr));

    let manifest = LegacyBundleManifest {
        format: FORMAT.to_owned(),
        format_version: FORMAT_VERSION,
        created_at: p.created_at,
        source: LegacyBundleSource {
            app_version: p.app_version.clone(),
            sqlite_schema_version: p.store.schema_version()?,
            sql_database: p.sql.database(),
            exporter: p.exporter.clone(),
            store: p.store.kind(),
        },
        maintenance: p.maintenance.clone(),
        files,
        left_out,
    };
    write_manifest(out, &manifest)?;
    Ok(manifest)
}

/// `vietqr_only` (ADR 0099): a bundle of the one `registry.vietqr` row, for a site that has
/// already cut over or that moves VietQR on its own. No store, no SQL Server, no gate.
pub fn export_vietqr_only(
    out: &Path,
    vietqr: &VietQrRead,
    app_version: Option<String>,
    database: &str,
    exporter: &str,
    created_at: DateTimeOffset,
    notify: FileWritten<'_>,
) -> Result<LegacyBundleManifest> {
    match vietqr {
        VietQrRead::NotSet => {
            return Err(ExportError::refused(
                stages::EXPORT,
                codes::LEGACY_VIETQR_NOT_SET,
                format!(
                    "HKLM\\{} holds no VietQR account (bank BIN and account number). Run this on the machine of the legacy IIS site, or enter VietQR in the cloud payment settings. Nothing was written.",
                    REGISTRY_SUB_KEY
                ),
            ));
        }
        VietQrRead::Unreadable(problem) => {
            return Err(ExportError::refused(
                stages::EXPORT,
                codes::LEGACY_VIETQR_UNREADABLE,
                format!(
                    "The legacy VietQR account could not be read: {problem}. Enter it in the cloud payment settings. Nothing was written."
                ),
            ));
        }
        VietQrRead::Read(_) => {}
    }
    prepare_out_dir(out)?;
    let file = written(out, write_vietqr(out, vietqr)?, notify);
    let manifest = LegacyBundleManifest {
        format: FORMAT.to_owned(),
        format_version: FORMAT_VERSION,
        created_at,
        source: LegacyBundleSource {
            app_version,
            sqlite_schema_version: None,
            sql_database: database.to_owned(),
            exporter: exporter.to_owned(),
            store: Some("registry".to_owned()),
        },
        maintenance: LegacyBundleMaintenance {
            mode: VIETQR_ONLY_MODE.to_owned(),
            evidence: "VietQR registry values only; no store rows".to_owned(),
        },
        files: vec![file],
        left_out: vec![dpapi_left_out(vietqr)],
    };
    write_manifest(out, &manifest)?;
    Ok(manifest)
}

/// The left-out tables whose rows nobody accounted for (the operator must look at them).
pub fn leftovers<'m>(
    m: &'m LegacyBundleManifest,
    store: &dyn LegacyStore,
) -> Vec<&'m LegacyLeftOut> {
    m.left_out
        .iter()
        .filter(|l| l.rows > 0 && store.sql_table_needs_review(&l.table))
        .collect()
}

/// Whether the maintenance of a manifest is a live rehearsal.
pub fn is_live(m: &LegacyBundleMaintenance) -> bool {
    m.mode == LIVE_REHEARSAL_MODE
}
