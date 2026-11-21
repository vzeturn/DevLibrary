//! Packs a verified bundle as one zip and sends it to the cloud's import endpoint
//! (`LegacyBundleZip.cs`, `LegacyUploadClient.cs`, `LegacyUploadCommand.cs`,
//! `LegacyUploadReport.cs`, ADR 0095): `PUT /sync/v1/legacy/import/{sha256}` signed like
//! every agent call, then the sequence of the `legacy.export` command: a dry run, and with
//! `commit` the committing upload and a second dry run of the same bundle as the count check.

use std::path::{Path, PathBuf};

use dl_hash::{Digest, Sha256};
use dl_json::Map;
use dl_zip::{Method, Modified, Options, ZipWriter};

use super::events::RunEvents;
use super::sources::{ExportError, Result, stages};
use crate::config::codes;
use crate::contracts::json::read_with;
use crate::contracts::legacy::bundle::MANIFEST_FILE;
use crate::contracts::legacy::{VerifiedLegacyBundle, bundle_files};
use crate::contracts::wire::{LegacyImportAck, legacy_import_wire, site_event_levels};
use crate::transport::retry::is_transient;
use crate::transport::{Error as TransportError, SyncHttpClient, SyncResponse, TokenProvider};
use crate::util::ctx::Ctx;
use crate::util::stj;

/// Longest problem text kept from a cloud error.
const MAX_PROBLEM: usize = 500;

/// A verified bundle packed as one zip; [`delete`](Self::delete) removes the file.
#[derive(Debug)]
pub struct BundleZip {
    /// The verified bundle.
    pub bundle: VerifiedLegacyBundle,
    /// The zip file.
    pub path: PathBuf,
    /// Lowercase hex SHA-256 of the zip: the path of the upload names it.
    pub sha256: String,
    /// Size of the zip.
    pub length: u64,
    /// Rows of every file.
    pub rows: i64,
}

fn zip_error(code: &str, message: impl Into<String>) -> ExportError {
    ExportError::new(stages::ZIP, code, message)
}

impl BundleZip {
    /// Verifies the bundle (manifest, every file hash, every row count), then packs exactly
    /// `manifest.json` and the files the manifest lists under their bundle-relative names.
    /// Each file is hashed again while it is packed, so a file changed after the check is
    /// refused rather than sent.
    pub fn create(bundle_dir: &Path, temp_dir: &Path, max_bytes: u64) -> Result<BundleZip> {
        let bundle = bundle_files::read_verified(bundle_dir).map_err(|e| {
            zip_error(
                codes::LEGACY_BUNDLE_INVALID,
                format!("{} Nothing was sent.", e.message),
            )
        })?;
        let mut rows: i64 = 0;
        for file in &bundle.manifest.files {
            let reader = bundle_files::read_rows(&bundle, file).map_err(|e| {
                zip_error(
                    codes::LEGACY_BUNDLE_INVALID,
                    format!("{} Nothing was sent.", e.message),
                )
                .at_table(&file.table)
            })?;
            for row in reader {
                row.map_err(|e| {
                    zip_error(
                        codes::LEGACY_BUNDLE_INVALID,
                        format!("{} Nothing was sent.", e.message),
                    )
                    .at_table(&file.table)
                })?;
                rows += 1;
            }
        }

        let created = bundle.manifest.created_at.unix_seconds();
        let mut zip = ZipWriter::new(Vec::new());
        add(
            &mut zip,
            bundle_dir,
            MANIFEST_FILE,
            &bundle.manifest_sha256,
            Method::Deflate(dl_flate::Level::Default),
            created,
        )?;
        for file in &bundle.manifest.files {
            // Already gzip: stored as it is.
            add(
                &mut zip,
                bundle_dir,
                &file.path,
                &file.sha256,
                Method::Stored,
                created,
            )?;
        }
        let bytes = zip.finish().map_err(|e| {
            zip_error(
                codes::LEGACY_UPLOAD_FAILED,
                format!("the zip could not be finished ({e})"),
            )
        })?;
        let length = bytes.len() as u64;
        if length > max_bytes {
            return Err(ExportError::refused(
                stages::ZIP,
                codes::LEGACY_UPLOAD_TOO_LARGE,
                format!(
                    "The bundle zip is {}, larger than the {} the cloud accepts. Nothing was sent. Import this bundle at the operator desk instead (TenantCli legacy-import).",
                    size(length),
                    size(max_bytes)
                ),
            ));
        }
        let sha256 = dl_encoding::hex::encode(Sha256::digest(&bytes));
        std::fs::create_dir_all(temp_dir)
            .map_err(|e| ExportError::io(stages::ZIP, "temp folder", &e))?;
        let path = temp_dir.join(format!(
            "pda-legacy-upload-{}.zip",
            dl_uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&path, &bytes).map_err(|e| {
            // The half-written zip is removed; a failure to remove it is told, not swallowed.
            let left = match std::fs::remove_file(&path) {
                Ok(()) => String::new(),
                Err(d) if d.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(d) => format!(
                    " The partial zip could not be deleted either ({:?}).",
                    d.kind()
                ),
            };
            let mut err = ExportError::io(stages::ZIP, "the zip could not be written", &e);
            err.message.push_str(&left);
            err
        })?;
        Ok(BundleZip {
            bundle,
            path,
            sha256,
            length,
            rows,
        })
    }

    /// Removes the zip; `None` when it is gone, else why it could not be removed.
    pub fn delete(&self) -> Option<String> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => None,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => Some(e.to_string()),
        }
    }
}

fn add(
    zip: &mut ZipWriter<Vec<u8>>,
    dir: &Path,
    entry: &str,
    expected_sha256: &str,
    method: Method,
    created: i64,
) -> Result<()> {
    let bytes = std::fs::read(dir.join(entry))
        .map_err(|e| ExportError::io(stages::ZIP, "a bundle file could not be read", &e))?;
    if !dl_encoding::hex::encode(Sha256::digest(&bytes)).eq_ignore_ascii_case(expected_sha256) {
        return Err(zip_error(
            codes::LEGACY_BUNDLE_INVALID,
            format!(
                "{entry} changed while it was packed (SHA-256 differs from the manifest). Nothing was sent."
            ),
        ));
    }
    zip.add_file(
        entry,
        &bytes,
        Options {
            method,
            modified: Modified::Unix(created),
        },
    )
    .map_err(|e| {
        zip_error(
            codes::LEGACY_UPLOAD_FAILED,
            format!("{entry} could not be packed ({e})"),
        )
    })
}

/// `1.5 MB` / `12.0 KB`.
pub fn size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / 1024.0 / 1024.0)
    } else {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    }
}

// ---- the cloud call

/// The cloud's answer: the import result on 2xx, else the HTTP status and the problem named.
#[derive(Debug, Clone, PartialEq)]
pub struct UploadAnswer {
    /// HTTP status.
    pub status: u16,
    /// The first answer was 401 and the upload was sent again with a fresh token.
    pub token_refreshed: bool,
    /// The import result (2xx).
    pub ack: Option<LegacyImportAck>,
    /// The `code` of an error answer.
    pub problem_code: Option<String>,
    /// The message of an error answer.
    pub problem: Option<String>,
}

/// The call itself failed (unreachable, not enrolled, token exchange refused).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportFailure {
    /// Stable code.
    pub code: String,
    /// Message.
    pub message: String,
}

impl TransportFailure {
    fn of(e: &TransportError) -> TransportFailure {
        match e {
            TransportError::Agent(a) => TransportFailure {
                code: a.code().to_owned(),
                message: a.message().to_owned(),
            },
            TransportError::Cancelled => TransportFailure {
                code: "CANCELLED".to_owned(),
                message: "The operation was canceled.".to_owned(),
            },
            TransportError::Unexpected { message, .. } => TransportFailure {
                code: codes::LEGACY_UPLOAD_FAILED.to_owned(),
                message: message.clone(),
            },
        }
    }
}

/// The query flags of an upload besides `dryRun`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UploadQuery {
    /// Sent as `preferLegacy`.
    pub prefer_legacy: bool,
    /// Sent as `keepTenant=true` when set, left out otherwise.
    pub keep_tenant: bool,
}

impl UploadQuery {
    /// The flags.
    pub const fn new(prefer_legacy: bool, keep_tenant: bool) -> UploadQuery {
        UploadQuery {
            prefer_legacy,
            keep_tenant,
        }
    }
}

/// Sends the zip to the import endpoint.
pub trait BundleUploader: Send + Sync {
    /// One upload; a dry run writes nothing in the cloud.
    fn send(
        &self,
        zip: &BundleZip,
        dry_run: bool,
        query: UploadQuery,
        ctx: &Ctx,
    ) -> std::result::Result<UploadAnswer, TransportFailure>;
}

/// [`BundleUploader`] over the agent's signed client: transient failures retry with the
/// agent's retry policy inside [`SyncHttpClient`] (safe, a rerun of the import adds
/// nothing), and a 401 gets one fresh token and one more try.
pub struct HttpBundleUploader {
    http: std::sync::Arc<SyncHttpClient>,
    tokens: std::sync::Arc<TokenProvider>,
}

impl HttpBundleUploader {
    /// An uploader over the host's client and token provider.
    pub fn new(
        http: std::sync::Arc<SyncHttpClient>,
        tokens: std::sync::Arc<TokenProvider>,
    ) -> Self {
        HttpBundleUploader { http, tokens }
    }

    fn put(
        &self,
        path: &str,
        zip: &BundleZip,
        ctx: &Ctx,
    ) -> std::result::Result<SyncResponse, TransportError> {
        let bearer = self.tokens.get_access_token(ctx)?;
        self.http.put_file(
            ctx,
            path,
            &zip.path,
            &zip.sha256,
            legacy_import_wire::CONTENT_TYPE,
            Some(&bearer),
        )
    }
}

impl BundleUploader for HttpBundleUploader {
    fn send(
        &self,
        zip: &BundleZip,
        dry_run: bool,
        query: UploadQuery,
        ctx: &Ctx,
    ) -> std::result::Result<UploadAnswer, TransportFailure> {
        let path = legacy_import_wire::path_and_query(
            &zip.sha256,
            dry_run,
            query.prefer_legacy,
            query.keep_tenant,
        );
        let mut resp = self
            .put(&path, zip, ctx)
            .map_err(|e| TransportFailure::of(&e))?;
        let refreshed = resp.status == 401;
        if refreshed {
            // The cached token expired or was revoked on the server: one fresh token, one
            // more try.
            self.tokens.invalidate();
            resp = self
                .put(&path, zip, ctx)
                .map_err(|e| TransportFailure::of(&e))?;
        }
        if resp.is_success() {
            let ack = parse_ack(&resp)?;
            return Ok(UploadAnswer {
                status: resp.status,
                token_refreshed: refreshed,
                ack: Some(ack),
                problem_code: None,
                problem: None,
            });
        }
        let (code, problem) = parse_problem(&resp.body);
        Ok(UploadAnswer {
            status: resp.status,
            token_refreshed: refreshed,
            ack: None,
            problem_code: code,
            problem,
        })
    }
}

/// The `data` of the API envelope as an ack; camelCase (the API default), snake_case
/// accepted too.
pub fn parse_ack(resp: &SyncResponse) -> std::result::Result<LegacyImportAck, TransportFailure> {
    let bad = |what: &str| TransportFailure {
        code: codes::LEGACY_UPLOAD_FAILED.to_owned(),
        message: format!("The cloud answered HTTP {} {what}.", resp.status),
    };
    let data = resp
        .data()
        .map_err(|_| bad("with an unreadable body"))?
        .ok_or_else(|| bad("without an import result"))?;
    if !data.is_object() {
        return Err(bad("with something other than an import result"));
    }
    let opts = if data.get("manifest_sha256").is_some() {
        stj::SNAKE
    } else {
        stj::WEB
    };
    let ack = read_with::<LegacyImportAck>(&data, opts)
        .map_err(|_| bad("with an unreadable import result"))?;
    if ack.manifest_sha256.is_empty() {
        return Err(bad("with an import result that names no manifest"));
    }
    Ok(ack)
}

/// The `code` and message of a sync problem response; `None`s when unreadable.
pub fn parse_problem(body: &[u8]) -> (Option<String>, Option<String>) {
    if body.is_empty() {
        return (None, None);
    }
    let Ok(text) = std::str::from_utf8(body) else {
        return (None, None);
    };
    // A proxy page (HTML) rather than an API answer: the HTTP status alone describes it.
    let Ok(dl_json::Value::Object(root)) = stj::parse(text) else {
        return (None, None);
    };
    let field = |name: &str| match root.get(name) {
        Some(dl_json::Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    };
    let message = field("message")
        .or_else(|| field("detail"))
        .or_else(|| field("title"));
    let message = message.map(|m| {
        if m.chars().count() > MAX_PROBLEM {
            format!("{}...", m.chars().take(MAX_PROBLEM).collect::<String>())
        } else {
            m
        }
    });
    (field("code"), message)
}

// ---- the sequence

/// What the sequence did.
#[derive(Debug, Clone, PartialEq)]
pub struct UploadOutcome {
    /// The first dry run.
    pub dry_run: AckSummary,
    /// The committing upload.
    pub commit: Option<AckSummary>,
    /// The dry run after the commit (the count check).
    pub post_commit: Option<AckSummary>,
    /// A VietQR-only bundle whose account the tenant already had with other values: kept as
    /// it is, nothing imported.
    pub vietqr_kept: bool,
}

/// Counts of one import answer (never row data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AckSummary {
    /// The cloud ran it as a dry run.
    pub dry_run: bool,
    /// The cloud wrote the import.
    pub committed: bool,
    /// Tables answered.
    pub tables: usize,
    /// Rows the cloud would add / added.
    pub inserted: i64,
    /// Rows already there and equal.
    pub same: i64,
    /// Rows already there and different.
    pub differing: i64,
    /// Host printers to convert.
    pub host_printers: usize,
    /// The manifest the cloud names.
    pub manifest_sha256: String,
}

impl AckSummary {
    fn of(a: &LegacyImportAck) -> AckSummary {
        AckSummary {
            dry_run: a.dry_run,
            committed: a.committed,
            tables: a.tables.len(),
            inserted: a.tables.iter().map(|t| t.inserted).sum(),
            same: a.tables.iter().map(|t| t.same).sum(),
            differing: a.tables.iter().map(|t| t.differing).sum(),
            host_printers: a.host_printers.len(),
            manifest_sha256: a.manifest_sha256.clone(),
        }
    }

    /// The counts as event detail.
    pub fn detail(&self) -> Map {
        let mut m = Map::new();
        m.insert("committed", self.committed);
        m.insert("tables", i64::try_from(self.tables).unwrap_or(i64::MAX));
        m.insert("inserted", self.inserted);
        m.insert("same", self.same);
        m.insert("differing", self.differing);
        m
    }
}

/// How to run the sequence.
#[derive(Debug, Clone, Copy)]
pub struct UploadPlan {
    /// Commit after a clean dry run.
    pub commit: bool,
    /// Passed as `preferLegacy`.
    pub prefer_legacy: bool,
    /// Passed as `keepTenant=true` on every upload of the run when set.
    pub keep_tenant: bool,
    /// A VietQR-only bundle: `ROWS_DIFFER` keeps the cloud's account, and the count check
    /// reads `differing` strictly.
    pub vietqr_only: bool,
}

/// The `HTTP_<status>` code of an error answer, or the problem's own code.
fn http_code(a: &UploadAnswer) -> String {
    a.problem_code
        .as_deref()
        .filter(|c| {
            !c.is_empty()
                && c.len() <= 64
                && c.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
        .map_or_else(|| format!("HTTP_{}", a.status), str::to_owned)
}

/// A cloud refusal code that is safe to carry in an event: the code, or a generic one.
fn refusal_code(code: &str) -> String {
    if !code.is_empty()
        && code.len() <= 64
        && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        code.to_owned()
    } else {
        codes::LEGACY_UPLOAD_REFUSED.to_owned()
    }
}

fn one_upload(
    up: &dyn BundleUploader,
    zip: &BundleZip,
    stage: &'static str,
    dry_run: bool,
    query: UploadQuery,
    ctx: &Ctx,
) -> Result<LegacyImportAck> {
    let answer = up
        .send(zip, dry_run, query, ctx)
        .map_err(|f| ExportError::new(stage, &f.code, f.message))?;
    match answer.ack.clone() {
        Some(ack) => Ok(ack),
        None if is_transient(answer.status) => Err(ExportError::new(
            stage,
            codes::LEGACY_UPLOAD_FAILED,
            format!(
                "The cloud is unavailable (HTTP {} after every retry). A rerun adds nothing twice.",
                answer.status
            ),
        )),
        None => Err(ExportError::refused(
            stage,
            &http_code(&answer),
            format!(
                "The cloud refused the upload with HTTP {}. Nothing was written.",
                answer.status
            ),
        )),
    }
}

fn require_clean(ack: &LegacyImportAck, stage: &'static str) -> Result<()> {
    match &ack.refusal_code {
        Some(code) => Err(ExportError::refused(
            stage,
            &refusal_code(code),
            format!("The cloud refused the bundle ({code}). Nothing was written."),
        )),
        None => Ok(()),
    }
}

/// The dry run, and with `plan.commit` the commit and the count check (the C# rule of
/// `legacy-upload` plus the migrate script's: commit only after a clean dry run; the count
/// check passes when no table still has rows to insert or rows that differ without having
/// been taken from legacy).
pub fn run_sequence(
    up: &dyn BundleUploader,
    zip: &BundleZip,
    plan: UploadPlan,
    events: &RunEvents,
    ctx: &Ctx,
) -> Result<UploadOutcome> {
    let sent = |stage: &str, a: &AckSummary| {
        let mut d = a.detail();
        d.insert("stage", stage);
        d.insert("zip_sha256", zip.sha256.as_str());
        d.insert("bytes", i64::try_from(zip.length).unwrap_or(i64::MAX));
        events.upload_progress(
            site_event_levels::INFO,
            "LEGACY_EXPORT_UPLOAD_SENT",
            &format!("Bundle sent ({stage})"),
            d,
        );
    };

    let query = UploadQuery::new(plan.prefer_legacy, plan.keep_tenant);
    let dry = one_upload(up, zip, stages::DRY_RUN, true, query, ctx)?;
    let dry_summary = AckSummary::of(&dry);
    sent(stages::DRY_RUN, &dry_summary);
    if let Some(code) = &dry.refusal_code {
        if plan.vietqr_only && code == "ROWS_DIFFER" {
            return Ok(UploadOutcome {
                dry_run: dry_summary,
                commit: None,
                post_commit: None,
                vietqr_kept: true,
            });
        }
        require_clean(&dry, stages::DRY_RUN)?;
    }
    if !plan.commit {
        return Ok(UploadOutcome {
            dry_run: dry_summary,
            commit: None,
            post_commit: None,
            vietqr_kept: false,
        });
    }

    let commit = one_upload(up, zip, stages::COMMIT, false, query, ctx)?;
    let commit_summary = AckSummary::of(&commit);
    sent(stages::COMMIT, &commit_summary);
    require_clean(&commit, stages::COMMIT)?;
    if !commit.committed {
        return Err(ExportError::new(
            stages::COMMIT,
            codes::LEGACY_UPLOAD_FAILED,
            "The cloud answered without committing the import and without a refusal code.",
        ));
    }

    let post = one_upload(up, zip, stages::POST_COMMIT, true, query, ctx)?;
    let post_summary = AckSummary::of(&post);
    sent(stages::POST_COMMIT, &post_summary);
    require_clean(&post, stages::POST_COMMIT)?;
    let gap = post
        .tables
        .iter()
        .find(|t| t.inserted != 0 || (t.differing != 0 && (plan.vietqr_only || !t.updated)));
    if let Some(t) = gap {
        let still = post
            .tables
            .iter()
            .filter(|t| t.inserted != 0 || (t.differing != 0 && (plan.vietqr_only || !t.updated)))
            .count();
        return Err(ExportError::new(
            stages::POST_COMMIT,
            "COUNT_CHECK_FAILED",
            format!(
                "{still} table(s) still differ after the commit; the first is {}.",
                t.target
            ),
        )
        .at_table(&t.target));
    }
    if !post
        .manifest_sha256
        .eq_ignore_ascii_case(&zip.bundle.manifest_sha256)
    {
        return Err(ExportError::new(
            stages::POST_COMMIT,
            "MANIFEST_MISMATCH",
            "The cloud's post-commit dry run names another manifest than the bundle that was committed.",
        ));
    }
    Ok(UploadOutcome {
        dry_run: dry_summary,
        commit: Some(commit_summary),
        post_commit: Some(post_summary),
        vietqr_kept: false,
    })
}
