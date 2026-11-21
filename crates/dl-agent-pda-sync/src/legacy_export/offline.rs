//! `take_offline` of `legacy.export` (RT-0390): the agent puts the legacy on-prem PDA web
//! app offline itself, by writing `app_offline.htm` into its site root, so the maintenance
//! gate passes without the operator knowing the site root or the app pool.
//!
//! - The site root is the command's `site_root`, else the one IIS application that looks
//!   like the legacy PDA web app ([`discover`], read-only `appcmd list vdir`).
//! - The file is written only when it is not there yet; the agent remembers whether it
//!   created it ([`Placement`]) and never removes a file it did not create.
//! - Before the cloud commit succeeded (a refusal, a failure, a dry run only) the file the
//!   agent created is removed again; after a successful commit it stays: the site is retired.
//!
//! Heuristic of the legacy app: the physical path holds a `web.config` and an assembly named
//! `MM.PdaOrder*.dll` or `MM.PDA*.dll` (case-insensitive) in the folder itself (the ASP.NET
//! Core publish of `MM.PdaOrder.Api` puts its assemblies beside `web.config`) or in `bin`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dl_json::Map;

use super::events::RunEvents;
use super::sources::{ExportError, OFFLINE_FILE, Result, stages};
use crate::config::codes;
use crate::contracts::wire::site_event_levels;
use crate::reporting::{Logger, Value};

/// How long the agent waits after writing `app_offline.htm` so IIS unloads the app.
pub const OFFLINE_DELAY: Duration = Duration::from_secs(5);

/// The maintenance notice the agent writes (static, Vietnamese and English).
pub const OFFLINE_PAGE: &str = "<!DOCTYPE html>\n<html lang=\"vi\"><head><meta charset=\"utf-8\"><title>Bảo trì / Maintenance</title></head>\n<body><h1>Hệ thống đang bảo trì</h1><p>Ứng dụng đặt món đang được chuyển lên phiên bản mới. Vui lòng quay lại sau.</p>\n<h1>Under maintenance</h1><p>The ordering app is moving to its new version. Please come back later.</p></body></html>\n";

/// Event: the agent wrote `app_offline.htm`.
pub const EVENT_PLACED: &str = "LEGACY_SITE_OFFLINE_PLACED";
/// Event: the agent removed the file it wrote (the site is back online).
pub const EVENT_REMOVED: &str = "LEGACY_SITE_OFFLINE_REMOVED";
/// Event: the commit succeeded, the file stays (the site is retired).
pub const EVENT_KEPT: &str = "LEGACY_SITE_OFFLINE_KEPT";
/// Event: the commit's outcome is unknown, the file stays until the operator checks.
pub const EVENT_KEPT_UNKNOWN: &str = "LEGACY_SITE_OFFLINE_KEPT_UNKNOWN";
/// Event: `take_offline` was ignored (`allow_live` or `vietqr_only`).
pub const EVENT_IGNORED: &str = "LEGACY_SITE_OFFLINE_IGNORED";

/// What is known of the cloud commit when a run ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitState {
    /// The cloud answered: nothing was committed (refusal, failure before the commit, dry run).
    NotCommitted,
    /// The commit succeeded.
    Committed,
    /// The commit was sent and no definite answer came back; the cloud may have committed.
    Unknown,
}

impl CommitState {
    /// The state after a run: committed when it says so or failed after the commit, unknown
    /// when the commit stage failed without a definite cloud answer.
    pub fn of(outcome: std::result::Result<bool, &ExportError>) -> CommitState {
        match outcome {
            Ok(true) => CommitState::Committed,
            Ok(false) => CommitState::NotCommitted,
            Err(e) if e.stage == stages::POST_COMMIT => CommitState::Committed,
            Err(e) if super::upload::commit_outcome_unknown(e) => CommitState::Unknown,
            Err(_) => CommitState::NotCommitted,
        }
    }
}

/// Says that `take_offline` was ignored, when it was ordered with `allow_live` or
/// `vietqr_only` (neither needs the site offline).
pub fn report_ignored(p: &super::payload::LegacyExportPayload, events: &RunEvents, log: &Logger) {
    if !p.take_offline || !(p.allow_live || p.vietqr_only) {
        return;
    }
    let why = if p.vietqr_only {
        "viet_qr_only"
    } else {
        "allow_live"
    };
    let mut d = Map::new();
    d.insert("reason", why);
    events.export(
        site_event_levels::INFO,
        EVENT_IGNORED,
        &format!("take_offline ignored with {why}: the legacy site is left as it is"),
        d,
    );
    log.info(
        "legacy.export: take_offline ignored with {Why}",
        &[Value::from(why)],
    );
}

/// IIS as the agent sees it; tests replace it.
pub trait LegacySite: Send + Sync {
    /// The physical paths of the IIS virtual directories, environment variables expanded;
    /// empty where IIS is not installed.
    fn physical_paths(&self) -> std::result::Result<Vec<String>, String>;
    /// Waits for IIS to unload the app after `app_offline.htm` appeared.
    fn settle(&self);
}

/// The machine's IIS: `appcmd list vdir /text:physicalPath` (read-only), nothing off Windows.
#[derive(Debug, Clone, Copy)]
pub struct IisSite {
    /// The wait of [`LegacySite::settle`].
    pub delay: Duration,
}

impl Default for IisSite {
    fn default() -> Self {
        IisSite {
            delay: OFFLINE_DELAY,
        }
    }
}

impl LegacySite for IisSite {
    fn physical_paths(&self) -> std::result::Result<Vec<String>, String> {
        if !cfg!(windows) {
            return Ok(Vec::new());
        }
        let windir = std::env::var_os("windir").unwrap_or_else(|| r"C:\Windows".into());
        let appcmd = Path::new(&windir).join(r"system32\inetsrv\appcmd.exe");
        if !appcmd.is_file() {
            return Ok(Vec::new());
        }
        let out = std::process::Command::new(&appcmd)
            .args(["list", "vdir", "/text:physicalPath"])
            .output()
            .map_err(|e| format!("appcmd.exe did not start ({:?})", e.kind()))?;
        if !out.status.success() {
            return Err("appcmd could not list the IIS virtual directories".to_owned());
        }
        Ok(parse_paths(&String::from_utf8_lossy(&out.stdout), &|n| {
            std::env::var(n).ok()
        }))
    }

    fn settle(&self) {
        std::thread::sleep(self.delay);
    }
}

/// Expands `%NAME%` with `lookup`; an unknown name stays as written.
pub fn expand_env(s: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => {
                let name = &after[..end];
                match lookup(name) {
                    Some(v) => out.push_str(&v),
                    None => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// One physical path per non-empty line, expanded, trailing separators trimmed, without
/// duplicates (compared case-insensitively, like Windows paths).
pub fn parse_paths(text: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim().trim_matches('"');
        if line.is_empty() {
            continue;
        }
        let expanded = expand_env(line, lookup);
        let trimmed = expanded.trim_end_matches(['\\', '/']);
        let path = if trimmed.ends_with(':') || trimmed.is_empty() {
            expanded.clone()
        } else {
            trimmed.to_owned()
        };
        let key = path.to_lowercase();
        if !seen.contains(&key) {
            seen.push(key);
            out.push(path);
        }
    }
    out
}

fn holds_pda_assembly(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        let name = e.file_name().to_string_lossy().to_lowercase();
        name.starts_with("mm.pda") && name.ends_with(".dll") && e.path().is_file()
    })
}

/// Whether `root` looks like the legacy PDA web app (see the module documentation).
pub fn looks_like_pda(root: &Path) -> bool {
    root.join("web.config").is_file()
        && (holds_pda_assembly(root) || holds_pda_assembly(&root.join("bin")))
}

/// The one IIS application that looks like the legacy PDA web app; none or several refuse
/// the command (the message counts them and names no path).
pub fn discover(site: &dyn LegacySite) -> Result<String> {
    let paths = site.physical_paths().map_err(|e| {
        ExportError::refused(
            stages::PRECHECK,
            codes::LEGACY_SITE_UNRESOLVED,
            format!("take_offline: the IIS applications could not be listed ({e}); name site_root in the command."),
        )
    })?;
    let mut found: Vec<String> = paths
        .into_iter()
        .filter(|p| looks_like_pda(Path::new(p)))
        .collect();
    if found.len() == 1 {
        return Ok(found.remove(0));
    }
    Err(ExportError::refused(
        stages::PRECHECK,
        codes::LEGACY_SITE_UNRESOLVED,
        format!(
            "take_offline: found {} IIS applications that look like the legacy PDA web app (web.config plus an MM.PdaOrder*.dll or MM.PDA*.dll); exactly one is needed. Name site_root in the command.",
            found.len()
        ),
    ))
}

/// The `app_offline.htm` of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// The site root, as given or found.
    pub root: String,
    /// True when the agent wrote the file; false when it was already there.
    pub created: bool,
}

impl Placement {
    /// The file.
    pub fn file(&self) -> PathBuf {
        Path::new(&self.root).join(OFFLINE_FILE)
    }

    /// Removes the file when the agent created it; `Ok(false)` when there was nothing of the
    /// agent's to remove.
    pub fn remove(&self) -> std::io::Result<bool> {
        if !self.created {
            return Ok(false);
        }
        match std::fs::remove_file(self.file()) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// The maintenance evidence with the agent's part named.
    pub fn evidence(&self, gate_evidence: &str) -> String {
        if !self.created {
            return gate_evidence.to_owned();
        }
        gate_evidence.replace(
            &format!("{OFFLINE_FILE} present in {}", self.root),
            &format!("{OFFLINE_FILE} placed by the agent in {}", self.root),
        )
    }
}

/// Writes `app_offline.htm` into `root` unless it is there, then waits for IIS.
pub fn place(root: &str, site: &dyn LegacySite) -> Result<Placement> {
    if !Path::new(root).is_dir() {
        return Err(ExportError::refused(
            stages::PRECHECK,
            codes::BAD_ARGUMENTS,
            format!("site_root '{root}' does not exist on this machine."),
        ));
    }
    let file = Path::new(root).join(OFFLINE_FILE);
    let opened = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file);
    let mut f = match opened {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Ok(Placement {
                root: root.to_owned(),
                created: false,
            });
        }
        Err(e) => return Err(offline_failed(root, "could not be written", &e)),
    };
    let placement = Placement {
        root: root.to_owned(),
        created: true,
    };
    if let Err(e) = f
        .write_all(OFFLINE_PAGE.as_bytes())
        .and_then(|()| f.sync_all())
    {
        drop(f);
        let mut err = offline_failed(root, "could not be written", &e);
        if let Err(rm) = std::fs::remove_file(&file) {
            err.message.push_str(&format!(
                "; the partial file could not be removed ({:?})",
                rm.kind()
            ));
        }
        return Err(err);
    }
    drop(f);
    site.settle();
    Ok(placement)
}

fn offline_failed(root: &str, what: &str, e: &std::io::Error) -> ExportError {
    ExportError::new(
        stages::PRECHECK,
        codes::LEGACY_SITE_OFFLINE_FAILED,
        format!("{OFFLINE_FILE} in '{root}' {what} ({:?})", e.kind()),
    )
}

fn detail(p: &Placement) -> Map {
    let mut d = Map::new();
    d.insert("site_root", p.root.as_str());
    d.insert("created_by_agent", p.created);
    d
}

/// Reports a placement as a site event.
pub fn report_placed(events: &RunEvents, p: &Placement) {
    let message = if p.created {
        format!("{OFFLINE_FILE} placed by the agent in {}", p.root)
    } else {
        format!("{OFFLINE_FILE} was already in {}; left as it is", p.root)
    };
    events.export(site_event_levels::INFO, EVENT_PLACED, &message, detail(p));
}

/// Ends the placement of a run: after a successful commit the file stays (the site is
/// retired); when the commit's outcome is unknown it stays too and the operator is told to
/// check the import; otherwise the file the agent created is removed. A file that was already
/// there is never touched. A removal failure is logged and returned (stage `stage`), for the
/// caller to report.
pub fn finish(
    p: &Placement,
    state: CommitState,
    stage: &'static str,
    events: &RunEvents,
    log: &Logger,
) -> std::result::Result<(), ExportError> {
    if !p.created {
        return Ok(());
    }
    if state == CommitState::Unknown {
        events.export(
            site_event_levels::WARN,
            EVENT_KEPT_UNKNOWN,
            &format!(
                "The commit was sent but no definite answer came back; {OFFLINE_FILE} stays in {}. Check the import result (tenant readiness) in the cloud: if nothing was imported, delete the file to bring the legacy site back, or rerun the export",
                p.root
            ),
            detail(p),
        );
        log.warn(
            "legacy.export: commit outcome unknown; app_offline.htm left in place",
            &[],
        );
        return Ok(());
    }
    if state == CommitState::Committed {
        events.export(
            site_event_levels::INFO,
            EVENT_KEPT,
            &format!(
                "The cloud commit succeeded; {OFFLINE_FILE} stays in {}: the legacy site is retired",
                p.root
            ),
            detail(p),
        );
        return Ok(());
    }
    match p.remove() {
        Ok(_) => {
            events.export(
                site_event_levels::INFO,
                EVENT_REMOVED,
                &format!(
                    "The run did not commit; {OFFLINE_FILE} removed from {}: the legacy site is back online",
                    p.root
                ),
                detail(p),
            );
            Ok(())
        }
        Err(e) => {
            log.error(
                "app_offline.htm placed by legacy.export could not be removed ({Kind}); remove it by hand",
                &[Value::from(format!("{:?}", e.kind()))],
            );
            Err(ExportError {
                stage,
                ..offline_failed(
                    &p.root,
                    "could not be removed; remove it by hand to bring the legacy site back",
                    &e,
                )
            })
        }
    }
}
