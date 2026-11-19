//! `status.json`: the last known agent state (port of `Config/AgentStatus.cs`).

use super::paths::{AgentPaths, write_atomic};
use super::stj_read::{self, JsonError};
use super::time::Timestamp;
use crate::util::stj::{Style, Writer};
use std::fmt;
use std::io;

/// Last known agent state, so the console/installer can show a clear error and hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStatus {
    /// `healthy`, `degraded`, `reenroll_required`, `enrolled`, `not_enrolled`, ... (empty
    /// when a hand-edited file has null or no `state`).
    pub state: String,
    /// The stable error code, if any.
    pub error_code: Option<String>,
    /// The operator hint, if any.
    pub hint: Option<String>,
    /// When the state was recorded (written with its offset, `atUtc`).
    pub at_utc: Timestamp,
}

/// Why `status.json` (or `agent.json`, `runtime-config.json`) could not be read.
#[derive(Debug)]
pub enum ReadError {
    /// The file could not be read (`IOException`, `UnauthorizedAccessException`).
    Io(io::Error),
    /// The content is not what the serializer accepts (`JsonException`).
    Json(JsonError),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Io(e) => write!(f, "{e}"),
            ReadError::Json(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ReadError::Io(e) => Some(e),
            ReadError::Json(e) => Some(e),
        }
    }
}

impl From<io::Error> for ReadError {
    fn from(e: io::Error) -> ReadError {
        ReadError::Io(e)
    }
}

impl From<JsonError> for ReadError {
    fn from(e: JsonError) -> ReadError {
        ReadError::Json(e)
    }
}

/// Reads a whole text file like `File.ReadAllText` (BOM skipped, invalid UTF-8 replaced).
pub(crate) fn read_text(path: &std::path::Path) -> io::Result<String> {
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Ok(match text.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_owned(),
        None => text,
    })
}

impl AgentStatus {
    /// A status record.
    pub fn new(
        state: impl Into<String>,
        error_code: Option<&str>,
        hint: Option<&str>,
        at_utc: Timestamp,
    ) -> AgentStatus {
        AgentStatus {
            state: state.into(),
            error_code: error_code.map(str::to_owned),
            hint: hint.map(str::to_owned),
            at_utc,
        }
    }

    /// The JSON System.Text.Json writes (Web defaults, indented, nulls written).
    pub fn to_json(&self) -> String {
        let mut w = Writer::new(Style::Indented);
        w.begin_object();
        w.key("state").string(&self.state);
        w.key("errorCode");
        match &self.error_code {
            Some(c) => w.string(c),
            None => w.null(),
        };
        w.key("hint");
        match &self.hint {
            Some(h) => w.string(h),
            None => w.null(),
        };
        // Utf8JsonWriter formats a DateTimeOffset straight to UTF-8: the `+` is not escaped.
        w.key("atUtc").raw(&format!("\"{}\"", self.at_utc.to_stj()));
        w.end_object();
        w.finish()
    }

    /// Parses `status.json` content; `Ok(None)` for `null`.
    pub fn from_json(text: &str) -> Result<Option<AgentStatus>, JsonError> {
        let v = stj_read::parse(text)?;
        let Some(m) = stj_read::root_object(&v)? else {
            return Ok(None);
        };
        let state = stj_read::opt_string(stj_read::get_ci(m, "state"), "state")?;
        let error_code = stj_read::opt_string(stj_read::get_ci(m, "errorCode"), "errorCode")?;
        let hint = stj_read::opt_string(stj_read::get_ci(m, "hint"), "hint")?;
        let at_utc = match stj_read::get_ci(m, "atUtc") {
            None => Timestamp::default(),
            Some(v) => stj_read::timestamp(v, "atUtc")?,
        };
        Ok(Some(AgentStatus {
            state: state.unwrap_or_default(),
            error_code,
            hint,
            at_utc,
        }))
    }

    /// Writes `status.json` (creating the data and logs folders first).
    pub fn write(paths: &AgentPaths, status: &AgentStatus) -> io::Result<()> {
        paths.ensure_created()?;
        write_atomic(&paths.status_file(), &status.to_json())
    }

    /// Reads `status.json`; `Ok(None)` when it does not exist.
    pub fn try_read(paths: &AgentPaths) -> Result<Option<AgentStatus>, ReadError> {
        let file = paths.status_file();
        if !file.is_file() {
            return Ok(None);
        }
        Ok(AgentStatus::from_json(&read_text(&file)?)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::stj::NEWLINE;

    #[test]
    fn json_matches_system_text_json() {
        // .NET 10 output of AgentStatus.Write (see the sa-core check).
        let at = Timestamp::parse_iso("2026-11-04T10:00:00.12+07:00").unwrap();
        let s = AgentStatus::new("degraded", Some("SQL_BUSY"), Some("Lỗi <x> & \"y\""), at);
        let nl = NEWLINE;
        assert_eq!(
            s.to_json(),
            format!(
                "{{{nl}  \"state\": \"degraded\",{nl}  \"errorCode\": \"SQL_BUSY\",{nl}  \"hint\": \"L\\u1ED7i \\u003Cx\\u003E \\u0026 \\u0022y\\u0022\",{nl}  \"atUtc\": \"2026-11-04T10:00:00.12+07:00\"{nl}}}"
            )
        );
        let healthy = AgentStatus::new(
            "healthy",
            None,
            None,
            Timestamp::from_unix_seconds(1_793_761_200),
        );
        assert_eq!(
            healthy.to_json(),
            format!(
                "{{{nl}  \"state\": \"healthy\",{nl}  \"errorCode\": null,{nl}  \"hint\": null,{nl}  \"atUtc\": \"2026-11-04T03:00:00+00:00\"{nl}}}"
            )
        );
        assert_eq!(AgentStatus::from_json(&s.to_json()).unwrap(), Some(s));
        assert_eq!(AgentStatus::from_json("null").unwrap(), None);
        assert!(AgentStatus::from_json("{ not json").is_err());
        assert!(AgentStatus::from_json(r#"{"state":1}"#).is_err());
        let partial = AgentStatus::from_json(r#"{"STATE":"x","extra":[1]}"#)
            .unwrap()
            .unwrap();
        assert_eq!(partial.state, "x");
        assert_eq!(partial.at_utc, Timestamp::default());
    }
}
