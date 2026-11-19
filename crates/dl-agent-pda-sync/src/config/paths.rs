//! Well-known files under the agent data directory (port of `Config/AgentPaths.cs`) and
//! the write-then-rename helper (`AtomicFile`).

use std::io;
use std::path::{Path, PathBuf};

/// Well-known files under the agent data directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPaths {
    data_dir: PathBuf,
}

impl AgentPaths {
    /// Paths under `data_dir`.
    pub fn new(data_dir: impl Into<PathBuf>) -> AgentPaths {
        AgentPaths {
            data_dir: data_dir.into(),
        }
    }

    /// The data directory itself.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// `logs` (JSON-lines logs).
    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    /// `secrets` (one `.bin` per secret).
    pub fn secrets_dir(&self) -> PathBuf {
        self.data_dir.join("secrets")
    }

    /// `outbox.db`.
    pub fn outbox_db(&self) -> PathBuf {
        self.data_dir.join("outbox.db")
    }

    /// `blobs.db`.
    pub fn blob_queue_db(&self) -> PathBuf {
        self.data_dir.join("blobs.db")
    }

    /// `capture.db`: shadow hashes, watermarks and snapshot progress.
    pub fn capture_state_db(&self) -> PathBuf {
        self.data_dir.join("capture.db")
    }

    /// `agent.json` (the enrolled identity).
    pub fn identity_file(&self) -> PathBuf {
        self.data_dir.join("agent.json")
    }

    /// `status.json` (last known state for the installer and `status`).
    pub fn status_file(&self) -> PathBuf {
        self.data_dir.join("status.json")
    }

    /// `runtime-config.json` (cloud-pushed tunables).
    pub fn runtime_config_file(&self) -> PathBuf {
        self.data_dir.join("runtime-config.json")
    }

    /// `events.db`: site events waiting for the cloud.
    pub fn events_db(&self) -> PathBuf {
        self.data_dir.join("events.db")
    }

    /// Creates the data directory and `logs`.
    pub fn ensure_created(&self) -> io::Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(self.logs_dir())
    }
}

/// Writes `content` (UTF-8, no BOM) to `path + ".tmp"`, then renames it over `path`, so a
/// crash never leaves a half-written state file.
pub fn write_atomic(path: &Path, content: &str) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, content.as_bytes())?;
    if path.is_dir() {
        // File.Move(overwrite: true) refuses a directory target on every platform.
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "Cannot create '{}' because a directory with that name exists.",
                path.display()
            ),
        ));
    }
    std::fs::rename(&tmp, path)
}
