//! `collect_diagnostics` (PROTOCOL.md §4.1): a zip of the diagnostics, the newest logs, the
//! state files and the files the modules named, every text scrubbed of secrets, uploaded to
//! the hub.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use dl_json::{Value, json};
use dl_zip::{Level, Method, Modified, Options, ZipWriter};

use super::Agent;
use crate::Error;

/// Largest bundle the hub accepts (agent-hub `MAX_DIAGNOSTICS_BYTES`).
pub const MAX_BUNDLE_BYTES: usize = 8 * 1024 * 1024;

/// Log bytes taken, newest file first and the end of each file: even stored uncompressed the
/// zip stays under [`MAX_BUNDLE_BYTES`].
pub const LOG_BUDGET: usize = 6 * 1024 * 1024;

/// State files copied into `state/` (scrubbed). `identity.key` and `sql.json` never are.
pub const STATE_FILES: [&str; 9] = [
    "update.json",
    "config.json",
    "heartbeat.json",
    "watchdog.json",
    "watchdog-reports.json",
    "reports.json",
    "sql-installs.jsonl",
    "sql-setup.last.json",
    crate::selftest::LAST_RUN_FILE,
];

/// Most bytes taken from one module's rolling log folder
/// (`ModuleContext::add_diagnostic_folder`); a quarter of the log budget when that is less.
pub const MODULE_FOLDER_MAX: usize = 2 * 1024 * 1024;

/// Most bytes taken from one module file (its end).
pub const MODULE_FILE_MAX: usize = 512 * 1024;

/// Whether a module may name `path` for the bundle (`ModuleContext::add_diagnostic_file`):
/// a text state file whose name does not suggest a secret.
pub fn diagnostic_file_allowed(path: &Path) -> bool {
    let name = file_name(path).to_ascii_lowercase();
    let text = [".json", ".jsonl", ".log", ".txt"]
        .iter()
        .any(|e| name.ends_with(e));
    let secret = ["key", "secret", "identity", "password", "appsettings"]
        .iter()
        .any(|w| name.contains(w));
    text && !secret
}

/// Whether a module may name `path` as a rolling log folder
/// (`ModuleContext::add_diagnostic_folder`): its name does not suggest a secret.
pub fn diagnostic_folder_allowed(path: &Path) -> bool {
    let name = file_name(path).to_ascii_lowercase();
    !name.is_empty()
        && !["key", "secret", "identity", "password", "cert"]
            .iter()
            .any(|w| name.contains(w))
}

/// The files the modules named, as `(entry name, path)`.
pub(crate) type ModuleFiles = Vec<(String, PathBuf)>;

/// What goes into a bundle besides the data directory.
#[derive(Default)]
pub(crate) struct Inputs {
    /// `diagnostics.json`: identity, version, update state, module statuses.
    pub diagnostics: Value,
    /// Further top-level JSON documents (`survey.json`).
    pub documents: Vec<(&'static str, Value)>,
    /// Single files the modules named, as `(entry name, path)`.
    pub module_files: ModuleFiles,
    /// Rolling log folders the modules named, as `(entry prefix, folder)`.
    pub module_folders: ModuleFiles,
}

fn zip_error(e: impl std::fmt::Display) -> Error {
    Error::Io(
        "build the diagnostics zip",
        std::io::Error::other(e.to_string()),
    )
}

/// The last `max` bytes of `path`, from the first whole line when cut; `(text, file size)`.
fn tail(path: &Path, max: usize) -> std::io::Result<(String, u64)> {
    let mut f = File::open(path)?;
    let size = f.metadata()?.len();
    let start = size.saturating_sub(max as u64);
    f.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity(max.min(size as usize));
    f.take(max as u64).read_to_end(&mut bytes)?;
    if start > 0
        && let Some(nl) = bytes.iter().position(|&b| b == b'\n')
    {
        bytes.drain(..=nl);
    }
    Ok((String::from_utf8_lossy(&bytes).into_owned(), size))
}

/// Log files, newest first.
fn logs_newest_first(dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<_> = rd
        .filter_map(Result::ok)
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .filter(|(_, p)| p.is_file())
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    files.into_iter().map(|(_, p)| p).collect()
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Adds the newest files of `dir` under `prefix/`, the end of each, until `budget` bytes are
/// taken; files `keep` refuses are left out. Every file gets a line in `index`.
fn add_folder(
    add: &mut dyn FnMut(&str, &str) -> Result<(), Error>,
    index: &mut Vec<Value>,
    dir: &Path,
    prefix: &str,
    budget: usize,
    keep: fn(&Path) -> bool,
) -> Result<(), Error> {
    let mut left = budget;
    for path in logs_newest_first(dir).into_iter().filter(|p| keep(p)) {
        let file = format!("{prefix}/{}", file_name(&path));
        if left == 0 {
            index.push(json!({"file": file, "skipped": "log budget used up"}));
            continue;
        }
        match tail(&path, left) {
            Ok((text, size)) if text.is_empty() && size > 0 => {
                left = 0;
                index.push(json!({"file": file, "size": size, "skipped": "log budget used up"}));
            }
            Ok((text, size)) => {
                left = left.saturating_sub(text.len());
                add(&file, &text)?;
                index.push(json!({"file": file, "size": size, "taken": text.len() as u64}));
            }
            Err(e) => index.push(json!({"file": file, "error": e.to_string()})),
        }
    }
    Ok(())
}

/// Builds the zip with at most `log_budget` log bytes: `diagnostics.json`, the extra
/// documents (`survey.json`), `logs/<file>`, `state/<file>`, `modules/<module>/<file>`,
/// `modules/<module>/<folder>/<file>` and `bundle.json` (what was taken from each file and
/// what was left out). Every entry is scrubbed of secrets.
pub(crate) fn bundle_with(
    inputs: &Inputs,
    data_dir: &Path,
    now: u64,
    log_budget: usize,
) -> Result<Vec<u8>, Error> {
    let options = Options {
        method: Method::Deflate(Level::Default),
        modified: Modified::Unix(i64::try_from(now).unwrap_or(0)),
    };
    let mut zip = ZipWriter::new(Vec::new());
    let mut add = |name: &str, text: &str| {
        zip.add_file(name, dl_log::scrub(text).as_bytes(), options)
            .map_err(zip_error)
    };
    add(
        "diagnostics.json",
        &dl_json::to_string_pretty(&inputs.diagnostics),
    )?;
    for (name, doc) in &inputs.documents {
        add(name, &dl_json::to_string_pretty(doc))?;
    }
    let mut index = Vec::new();
    let any_file: fn(&Path) -> bool = |_| true;
    add_folder(
        &mut add,
        &mut index,
        &data_dir.join("logs"),
        "logs",
        log_budget,
        any_file,
    )?;
    for name in STATE_FILES {
        let path = data_dir.join(name);
        match std::fs::read(&path) {
            Ok(bytes) => {
                add(&format!("state/{name}"), &String::from_utf8_lossy(&bytes))?;
                index.push(json!({"file": format!("state/{name}"), "size": bytes.len() as u64}));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => index.push(json!({"file": format!("state/{name}"), "error": e.to_string()})),
        }
    }
    for (entry, path) in &inputs.module_files {
        match tail(path, MODULE_FILE_MAX) {
            Ok((text, size)) => {
                add(entry, &text)?;
                index.push(json!({"file": entry, "size": size, "taken": text.len() as u64}));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                index.push(json!({"file": entry, "skipped": "not there"}));
            }
            Err(e) => index.push(json!({"file": entry, "error": e.to_string()})),
        }
    }
    let folder_budget = MODULE_FOLDER_MAX.min(log_budget / 4);
    for (prefix, dir) in &inputs.module_folders {
        if !dir.is_dir() {
            index.push(json!({"file": prefix, "skipped": "not there"}));
            continue;
        }
        add_folder(
            &mut add,
            &mut index,
            dir,
            prefix,
            folder_budget,
            diagnostic_file_allowed,
        )?;
    }
    let no_update = data_dir.join("no-update").exists();
    add(
        "bundle.json",
        &dl_json::to_string_pretty(&json!({
            "created_at": now, "log_budget": log_budget as u64, "no_update_file": no_update, "files": index,
        })),
    )?;
    zip.finish().map_err(zip_error)
}

/// [`bundle_with`] at [`LOG_BUDGET`], halving the log share until the zip fits
/// [`MAX_BUNDLE_BYTES`].
pub(crate) fn bundle(inputs: &Inputs, data_dir: &Path, now: u64) -> Result<Vec<u8>, Error> {
    let mut budget = LOG_BUDGET;
    loop {
        let zip = bundle_with(inputs, data_dir, now, budget)?;
        if zip.len() <= MAX_BUNDLE_BYTES {
            return Ok(zip);
        }
        if budget < 64 * 1024 {
            return Err(Error::Config(format!(
                "diagnostics bundle is {} bytes even without logs, over the {MAX_BUNDLE_BYTES} limit",
                zip.len()
            )));
        }
        budget /= 2;
    }
}

impl Agent {
    /// What the bundle takes besides the data directory: `diagnostics`, the site survey, and the files and folders the modules named
    /// (`modules/<module>/<file name>`, `modules/<module>/<folder name>/<file name>`).
    fn bundle_inputs(&self, diagnostics: &Value) -> Inputs {
        let mut inputs = Inputs {
            diagnostics: diagnostics.clone(),
            documents: vec![(
                "survey.json",
                json!({"host": self.host, "databases": self.sql.pools.surveys()}),
            )],
            ..Inputs::default()
        };
        for s in self.supervisor.statuses() {
            for p in s
                .diagnostic_files
                .into_iter()
                .filter(|p| diagnostic_file_allowed(p))
            {
                inputs
                    .module_files
                    .push((format!("modules/{}/{}", s.name, file_name(&p)), p));
            }
            for p in s
                .diagnostic_folders
                .into_iter()
                .filter(|p| diagnostic_folder_allowed(p))
            {
                inputs
                    .module_folders
                    .push((format!("modules/{}/{}", s.name, file_name(&p)), p));
            }
        }
        inputs
    }

    /// Runs `collect_diagnostics`: builds the bundle, uploads it and answers with where it
    /// went plus the diagnostics inline. A bundle that cannot be built or uploaded is a failed
    /// command whose output still carries the diagnostics and the error.
    pub(super) fn collect_diagnostics(&self, command_id: &str, now: u64) -> (bool, String) {
        let mut d = self.diagnostics(now);
        let stored = bundle(&self.bundle_inputs(&d), &self.opts.data_dir, now)
            .and_then(|zip| self.hub.upload_diagnostics(&zip, command_id));
        let (ok, out) = match stored {
            Ok(stored) => {
                // The R2 object key goes out as `r2_key`: a bare `key` field is scrubbed.
                let r2_key = stored.get("key").and_then(Value::as_str).unwrap_or("");
                dl_log::info!("diagnostics bundle uploaded"; r2_key = r2_key);
                if let Some(o) = d.as_object_mut() {
                    o.remove("log_tail");
                }
                let uploaded = json!({"id": stored.get("id").cloned().unwrap_or(Value::Null),
                    "r2_key": r2_key, "size": stored.get("size").cloned().unwrap_or(Value::Null),
                    "sha256": stored.get("sha256").cloned().unwrap_or(Value::Null)});
                (true, json!({"uploaded": uploaded, "diagnostics": d}))
            }
            Err(e) => {
                dl_log::warn!("diagnostics bundle not uploaded"; error = %e);
                (
                    false,
                    json!({"upload_error": e.to_string(), "diagnostics": d}),
                )
            }
        };
        (
            ok,
            dl_log::scrub(&dl_json::to_string_pretty(&out)).into_owned(),
        )
    }
}

#[cfg(test)]
#[path = "agent_diag_tests.rs"]
mod tests;

/// The last `lines` lines of the newest file in `dir`.
pub fn log_tail(dir: &Path, lines: usize) -> Vec<String> {
    let newest = std::fs::read_dir(dir).ok().and_then(|rd| {
        rd.filter_map(Result::ok)
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .filter(|(_, p)| p.is_file())
            .max()
            .map(|(_, p)| p)
    });
    let Some(path) = newest else {
        return Vec::new();
    };
    let text = std::fs::read(&path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..]
        .iter()
        .map(|s| s.to_string())
        .collect()
}
