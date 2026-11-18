//! `Agent/ResultOutbox.cs`: results the agent owes the API, one JSON file per command
//! (`{commandId}.json`) under `DataDirectory/outbox`. A result is written before its first
//! send and removed once the API has answered for good, so a stop, crash or API outage
//! after the device acted never loses it.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use dl_uuid::Uuid;

use super::log::{Log, Logger};
use crate::config::DotnetError;
use crate::contracts::{PendingResult, parse_json};

/// The outbox directory.
#[derive(Debug, Clone)]
pub struct ResultOutbox {
    directory: PathBuf,
    log: Log,
}

impl ResultOutbox {
    /// .NET category of the outbox log.
    pub const CATEGORY: &'static str = "SmilePms.DeviceAgent.Agent.ResultOutbox";

    /// An outbox in `directory` (created on the first save).
    pub fn new(directory: impl Into<PathBuf>, log: Arc<dyn Logger>) -> ResultOutbox {
        ResultOutbox {
            directory: directory.into(),
            log: Log::new(log, Self::CATEGORY),
        }
    }

    /// The directory.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Writes `result` to `{id}.json` through `{id}.json.tmp` and an atomic replace, so a
    /// reader never sees half a file.
    pub fn save(&self, result: &PendingResult) -> io::Result<()> {
        std::fs::create_dir_all(&self.directory)?;
        let path = self.path_for(result.command_id);
        let mut temp = path.clone().into_os_string();
        temp.push(".tmp");
        std::fs::write(&temp, result.to_outbox_json())?;
        std::fs::rename(&temp, &path)
    }

    /// Deletes the entry of `command_id`; a missing file is not an error (`File.Delete`).
    pub fn remove(&self, command_id: Uuid) -> io::Result<()> {
        match std::fs::remove_file(self.path_for(command_id)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            r => r,
        }
    }

    /// Pending results, oldest write first. An unreadable entry is moved aside to
    /// `*.json.corrupt` and logged. I/O errors (other than JSON) are returned.
    pub fn list(&self) -> io::Result<Vec<PendingResult>> {
        let entries = match std::fs::read_dir(&self.directory) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut files: Vec<(SystemTime, PathBuf)> = Vec::new();
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let is_json = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(has_json_extension);
            if is_json && entry.file_type().is_ok_and(|t| t.is_file()) {
                let modified = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                files.push((modified, path));
            }
        }
        files.sort();
        let mut pending = Vec::with_capacity(files.len());
        for (_, file) in files {
            let bytes = std::fs::read(&file)?;
            let parsed = parse_json(&bytes)
                .map_err(|e| e.to_string())
                .and_then(|v| PendingResult::from_json(&v).map_err(|e| e.message().to_owned()));
            match parsed {
                Ok(p) => pending.push(p),
                Err(message) => {
                    let mut aside = file.clone().into_os_string();
                    aside.push(".corrupt");
                    let aside = PathBuf::from(aside);
                    std::fs::rename(&file, &aside)?;
                    self.log.error(
                        &format!(
                            "Outbox entry {} is unreadable; moved to {} for manual recovery",
                            file.display(),
                            aside.display()
                        ),
                        Some(&DotnetError::new("System.Text.Json.JsonException", message)),
                    );
                }
            }
        }
        Ok(pending)
    }

    /// How many entries wait in the outbox, without reading or moving them (0 when the
    /// directory does not exist yet).
    pub fn pending_count(&self) -> io::Result<usize> {
        let entries = match std::fs::read_dir(&self.directory) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };
        let mut n = 0;
        for entry in entries {
            let entry = entry?;
            let json = entry.file_name().to_str().is_some_and(has_json_extension);
            if json && entry.file_type().is_ok_and(|t| t.is_file()) {
                n += 1;
            }
        }
        Ok(n)
    }

    fn path_for(&self, command_id: Uuid) -> PathBuf {
        self.directory.join(format!("{command_id}.json"))
    }
}

/// `Directory.GetFiles(dir, "*.json")`: the name ends in `.json` (any case on Windows).
fn has_json_extension(name: &str) -> bool {
    let Some(ext) = name.len().checked_sub(5).and_then(|i| name.get(i..)) else {
        return false;
    };
    if cfg!(windows) {
        ext.eq_ignore_ascii_case(".json")
    } else {
        ext == ".json"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_json_files_are_entries() {
        assert!(has_json_extension(
            "6f1c2b0e-8d1a-4c55-9a51-3f7d2b9e0a11.json"
        ));
        assert!(!has_json_extension("a.json.tmp"));
        assert!(!has_json_extension("a.json.corrupt"));
        assert!(!has_json_extension("json"));
        assert!(has_json_extension(".json"));
        assert_eq!(has_json_extension("A.JSON"), cfg!(windows));
        assert!(!has_json_extension("ééé"));
    }
}
