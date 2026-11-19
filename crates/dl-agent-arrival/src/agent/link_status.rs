//! `LinkStatusFile`: `link-status.json` next to the identity file records the last change of
//! the link's state (Install.ps1 and support read it), and `link-heartbeat.txt` beside it
//! the time of the last authenticated poll. Both are written via a temporary file; a failure
//! is logged and never stops the link.

use std::path::{Path, PathBuf};

use dl_time::OffsetDateTime;

use crate::agent::vault::tmp_path;
use crate::stj;

/// File name of the status.
pub const FILE_NAME: &str = "link-status.json";
/// File name of the heartbeat.
pub const HEARTBEAT_FILE_NAME: &str = "link-heartbeat.txt";
/// `LinkStatus.Connected`.
pub const CONNECTED: &str = "connected";
/// `LinkStatus.Failed`.
pub const FAILED: &str = "failed";

/// `LinkStatus`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkStatus {
    /// [`CONNECTED`] or [`FAILED`].
    pub state: &'static str,
    /// Agent id.
    pub agent_id: String,
    /// Key id.
    pub kid: String,
    /// When the state changed.
    pub at_utc: OffsetDateTime,
    /// Why the link failed.
    pub problem: Option<String>,
}

impl LinkStatus {
    /// The JSON `JsonSerializerDefaults.Web` with `WriteIndented` writes.
    pub fn to_json(&self) -> String {
        let mut out = String::from("{\n  \"state\": ");
        stj::escape_into(&mut out, self.state);
        out.push_str(",\n  \"agentId\": ");
        stj::escape_into(&mut out, &self.agent_id);
        out.push_str(",\n  \"kid\": ");
        stj::escape_into(&mut out, &self.kid);
        out.push_str(",\n  \"atUtc\": ");
        // A typed DateTimeOffset is written raw by System.Text.Json (no `\u002B` for `+`).
        quoted_raw(&mut out, &stj::num::format_dto(&self.at_utc));
        out.push_str(",\n  \"problem\": ");
        match &self.problem {
            Some(p) => stj::escape_into(&mut out, p),
            None => out.push_str("null"),
        }
        out.push_str("\n}");
        out
    }
}

fn quoted_raw(out: &mut String, text: &str) {
    out.push('"');
    out.push_str(text);
    out.push('"');
}

/// The heartbeat line: `"<UTC time, O format> <agent version>"`.
pub fn heartbeat_line(at: &OffsetDateTime, version: &str) -> String {
    format!("{} {version}", stj::num::format_utc_o(at))
}

/// The two files next to the identity file.
#[derive(Debug, Clone)]
pub struct LinkStatusFile {
    path: PathBuf,
}

impl LinkStatusFile {
    /// `NextTo(identityFile)`.
    pub fn next_to(identity_file: &Path) -> LinkStatusFile {
        let dir = identity_file
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        LinkStatusFile {
            path: dir.join(FILE_NAME),
        }
    }

    /// Path of `link-status.json`.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Path of `link-heartbeat.txt`.
    pub fn heartbeat_path(&self) -> PathBuf {
        self.path.with_file_name(HEARTBEAT_FILE_NAME)
    }

    /// Writes the status.
    pub fn write(&self, status: &LinkStatus) {
        if let Err(e) = replace(&self.path, status.to_json().as_bytes()) {
            dl_log::warn!("could not write the link status"; path = %self.path.display(), error = %e);
        }
    }

    /// Records that the cloud answered just now.
    pub fn beat(&self, at: &OffsetDateTime, version: &str) {
        let path = self.heartbeat_path();
        if let Err(e) = replace(&path, heartbeat_line(at, version).as_bytes()) {
            dl_log::warn!("could not write the link heartbeat"; path = %path.display(), error = %e);
        }
    }
}

fn replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = tmp_path(path);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_land_next_to_the_identity() {
        let dir = std::env::temp_dir().join(format!("dl-arrival-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = LinkStatusFile::next_to(&dir.join("agent-identity.bin"));
        let at = OffsetDateTime::from_unix_millis(1_000).unwrap();
        f.write(&LinkStatus {
            state: CONNECTED,
            agent_id: "a".into(),
            kid: "k".into(),
            at_utc: at,
            problem: None,
        });
        f.beat(&at, "v1");
        assert!(
            std::fs::read_to_string(dir.join(FILE_NAME))
                .unwrap()
                .contains("\"state\": \"connected\"")
        );
        assert!(
            std::fs::read_to_string(dir.join(HEARTBEAT_FILE_NAME))
                .unwrap()
                .ends_with(" v1")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
