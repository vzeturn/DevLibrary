//! Port of `PrintRelay/PrintJournal.cs`: the relay's durable job journal (ADR 0098).

use crate::contracts::json::{dto, dto_member, opt_str};
use crate::contracts::time::DateTimeOffset;
use crate::reporting::{Exception, Logger, Value};
use crate::util::stj::{self, Obj, ReadError, Style, Writer};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Where a journaled job stands (`PrintJobStates`, ADR 0022 §6).
pub mod print_job_states {
    /// Accepted, not yet started: safe to print after a restart.
    pub const RECEIVED: &str = "received";
    /// Bytes may have reached the printer: never printed again.
    pub const PRINTING: &str = "printing";
    /// The outcome is decided and waits for the cloud to take the ack.
    pub const ACKING: &str = "acking";
}

/// The logger category (`ILogger<PrintJournal>`).
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.PrintRelay.PrintJournal";

const EXTENSION: &str = ".json";
const TEMP_EXTENSION: &str = ".json.tmp";
const CORRUPT_EXTENSION: &str = ".corrupt";

/// One job on disk (`PrintJournalEntry`); the payload is dropped once the outcome is
/// decided.
#[derive(Debug, Clone, PartialEq)]
pub struct PrintJournalEntry {
    /// Job id.
    pub job_id: dl_uuid::Uuid,
    /// Branch.
    pub rvc_no: i32,
    /// Printer host or `spooler:<queue>`.
    pub host: String,
    /// Printer port.
    pub port: i32,
    /// When the cloud gives up on the job.
    pub expires_at: DateTimeOffset,
    /// Delivery attempts so far (from the cloud).
    pub attempts: i32,
    /// One of [`print_job_states`].
    pub state: String,
    /// The ESC/POS bytes, until the outcome is decided.
    pub payload: Option<Vec<u8>>,
    /// Printed.
    pub ok: bool,
    /// Why not.
    pub error: Option<String>,
    /// Whether the cloud may push it again.
    pub retryable: bool,
}

impl PrintJournalEntry {
    /// The decided entry (`Outcome`): state `acking`, no payload.
    pub fn outcome(&self, ok: bool, error: Option<String>, retryable: bool) -> PrintJournalEntry {
        PrintJournalEntry {
            state: print_job_states::ACKING.to_owned(),
            payload: None,
            ok,
            error,
            retryable,
            ..self.clone()
        }
    }

    /// `PrintJournalJson` (snake_case, nulls omitted), compact.
    pub fn to_json(&self) -> String {
        let mut w = Writer::new(Style::Compact);
        w.begin_object();
        w.key("job_id").unescaped(&self.job_id.to_string());
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("host").string(&self.host);
        w.key("port").int(self.port.into());
        dto_member(&mut w, "expires_at", self.expires_at);
        w.key("attempts").int(self.attempts.into());
        w.key("state").string(&self.state);
        if let Some(p) = &self.payload {
            w.key("payload")
                .unescaped(&dl_encoding::base64::STANDARD.encode(p));
        }
        w.key("ok").bool(self.ok);
        opt_str(&mut w, "error", &self.error);
        w.key("retryable").bool(self.retryable);
        w.end_object();
        w.finish()
    }

    /// Reads an entry like the source-generated context (missing members default; JSON
    /// null is `Ok(None)`).
    pub fn from_json(json: &str) -> Result<Option<PrintJournalEntry>, ReadError> {
        let v = stj::parse(json)?;
        let Some(o) = Obj::of(&v, stj::SNAKE, "$")? else {
            return Ok(None);
        };
        Ok(Some(PrintJournalEntry::read(&o)?))
    }

    fn read(o: &Obj<'_>) -> Result<PrintJournalEntry, ReadError> {
        let payload = match o.opt_string("payload")? {
            None => None,
            Some(b64) => Some(dl_encoding::base64::STANDARD.decode(&b64).map_err(|_| {
                ReadError::new(
                    &o.path_of("payload"),
                    "The JSON value is not in a supported Base64 format.",
                )
            })?),
        };
        let job_id = match o.get_non_null("job_id") {
            None => dl_uuid::Uuid::NIL,
            Some(_) => o.guid("job_id")?,
        };
        let expires_at = match o.get_non_null("expires_at") {
            None => DateTimeOffset::MIN,
            Some(_) => dto(o, "expires_at")?,
        };
        Ok(PrintJournalEntry {
            job_id,
            rvc_no: o.int("rvc_no")?,
            host: o.opt_string("host")?.unwrap_or_default(),
            port: o.int("port")?,
            expires_at,
            attempts: o.int("attempts")?,
            state: o.opt_string("state")?.unwrap_or_default(),
            payload,
            ok: o.opt_bool("ok")?.unwrap_or(false),
            error: o.opt_string("error")?,
            retryable: o.opt_bool("retryable")?.unwrap_or(false),
        })
    }
}

/// One JSON file per job, written to a temp file, flushed to disk and renamed over the
/// old one (`PrintJournal`), so a crash leaves either the old or the new state.
#[derive(Debug)]
pub struct PrintJournal {
    dir: PathBuf,
    log: Logger,
}

fn has_suffix(name: &str, suffix: &str) -> bool {
    name.len() >= suffix.len()
        && name.is_char_boundary(name.len() - suffix.len())
        && if cfg!(windows) {
            name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
        } else {
            name.ends_with(suffix)
        }
}

impl PrintJournal {
    /// The journal in `dir` (created; owner-only on Unix).
    pub fn new(dir: impl Into<PathBuf>, log: &Logger) -> io::Result<PrintJournal> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(PrintJournal {
            dir,
            log: log.for_category(CATEGORY),
        })
    }

    /// The folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_of(&self, job_id: dl_uuid::Uuid) -> PathBuf {
        self.dir.join(format!("{job_id}{EXTENSION}"))
    }

    /// Writes (replaces) the entry durably.
    pub fn write(&self, entry: &PrintJournalEntry) -> io::Result<()> {
        let path = self.path_of(entry.job_id);
        let temp = self.dir.join(format!("{}{TEMP_EXTENSION}", entry.job_id));
        let bytes = entry.to_json();
        {
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                o.mode(0o600);
            }
            let mut f = o.open(&temp)?;
            f.write_all(bytes.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&temp, &path)
    }

    /// Removes the job's file.
    pub fn delete(&self, job_id: dl_uuid::Uuid) -> io::Result<()> {
        let path = self.path_of(job_id);
        if path.is_file() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    /// Every readable entry, and the file names set aside. Leftover temp files are
    /// deleted; a file that cannot be read is renamed `.corrupt`, never dropped.
    pub fn load(&self) -> io::Result<(Vec<PrintJournalEntry>, Vec<String>)> {
        let mut names: Vec<String> = Vec::new();
        for e in std::fs::read_dir(&self.dir)? {
            let e = e?;
            if e.file_type()?.is_file() {
                names.push(e.file_name().to_string_lossy().into_owned());
            }
        }
        for n in names.iter().filter(|n| has_suffix(n, TEMP_EXTENSION)) {
            std::fs::remove_file(self.dir.join(n))?;
        }
        let mut entries = Vec::new();
        let mut bad = Vec::new();
        for name in names.iter().filter(|n| has_suffix(n, EXTENSION)) {
            let file = self.dir.join(name);
            let stem = &name[..name.len() - EXTENSION.len()];
            let bytes = std::fs::read(&file)?;
            let text = String::from_utf8_lossy(&bytes);
            let entry = match PrintJournalEntry::from_json(
                text.strip_prefix('\u{feff}').unwrap_or(&text),
            ) {
                Ok(e) => e,
                Err(ex) => {
                    self.log.error_ex(
                        Exception::new("System.Text.Json.JsonException", ex.to_string()),
                        "Print journal file {File} is not valid JSON",
                        &[Value::from(file.display().to_string())],
                    );
                    None
                }
            };
            let valid = entry.filter(|e| {
                is_canonical(stem) == Some(e.job_id)
                    && matches!(
                        e.state.as_str(),
                        print_job_states::RECEIVED
                            | print_job_states::PRINTING
                            | print_job_states::ACKING
                    )
            });
            match valid {
                Some(e) => entries.push(e),
                None => {
                    let mut target = file.as_os_str().to_owned();
                    target.push(CORRUPT_EXTENSION);
                    std::fs::rename(&file, PathBuf::from(target))?;
                    bad.push(name.clone());
                }
            }
        }
        Ok((entries, bad))
    }
}

/// The id of a file name in the lowercase `D` form, else `None`.
fn is_canonical(name: &str) -> Option<dl_uuid::Uuid> {
    let id: dl_uuid::Uuid = name.parse().ok()?;
    (id.to_string() == name).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        std::env::temp_dir()
            .join("pda-sync-agent-tests")
            .join(dl_uuid::Uuid::new_v4().simple().to_string())
    }

    fn entry(state: &str) -> PrintJournalEntry {
        PrintJournalEntry {
            job_id: dl_uuid::Uuid::new_v4(),
            rvc_no: 1,
            host: "192.168.1.50".into(),
            port: 9100,
            expires_at: DateTimeOffset::now_utc()
                .checked_add(std::time::Duration::from_secs(300))
                .unwrap(),
            attempts: 1,
            state: state.into(),
            payload: Some(vec![1, 2, 3]),
            ok: false,
            error: None,
            retryable: false,
        }
    }

    #[test]
    fn write_then_load_round_trips_the_latest_state() {
        let dir = temp_dir();
        let j = PrintJournal::new(&dir, &Logger::none()).unwrap();
        let e = entry(print_job_states::RECEIVED);
        j.write(&e).unwrap();
        j.write(&PrintJournalEntry {
            state: print_job_states::PRINTING.into(),
            ..e.clone()
        })
        .unwrap();
        let (loaded, corrupt) = j.load().unwrap();
        assert!(corrupt.is_empty());
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].state, print_job_states::PRINTING);
        assert_eq!(loaded[0].payload.as_deref(), Some(&[1u8, 2, 3][..]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn outcome_drops_the_payload() {
        let d = entry(print_job_states::RECEIVED).outcome(false, Some("boom".into()), true);
        assert_eq!(d.state, print_job_states::ACKING);
        assert!(d.payload.is_none());
        assert!(d.retryable);
    }

    #[test]
    fn the_file_is_what_the_csharp_context_writes() {
        let mut e = entry(print_job_states::RECEIVED);
        e.job_id = "0f8fad5b-d9cb-469f-a165-70867728950e".parse().unwrap();
        e.expires_at = DateTimeOffset::parse("2026-11-04T10:00:00+07:00").unwrap();
        assert_eq!(
            e.to_json(),
            r#"{"job_id":"0f8fad5b-d9cb-469f-a165-70867728950e","rvc_no":1,"host":"192.168.1.50","port":9100,"expires_at":"2026-11-04T10:00:00+07:00","attempts":1,"state":"received","payload":"AQID","ok":false,"retryable":false}"#
        );
        let d = e.outcome(false, Some("a<b".into()), true);
        assert_eq!(
            d.to_json(),
            r#"{"job_id":"0f8fad5b-d9cb-469f-a165-70867728950e","rvc_no":1,"host":"192.168.1.50","port":9100,"expires_at":"2026-11-04T10:00:00+07:00","attempts":1,"state":"acking","ok":false,"error":"a\u003Cb","retryable":true}"#
        );
        assert_eq!(PrintJournalEntry::from_json(&d.to_json()).unwrap(), Some(d));
    }

    #[test]
    fn load_deletes_leftover_temp_files_and_sets_aside_unreadable_ones() {
        let dir = temp_dir();
        let j = PrintJournal::new(&dir, &Logger::none()).unwrap();
        let good = entry(print_job_states::RECEIVED);
        j.write(&good).unwrap();
        std::fs::write(
            dir.join(format!("{}.json.tmp", dl_uuid::Uuid::new_v4())),
            "{",
        )
        .unwrap();
        let torn = dir.join(format!("{}.json", dl_uuid::Uuid::new_v4()));
        std::fs::write(&torn, "{not json").unwrap();
        let renamed = dir.join(format!("{}.json", dl_uuid::Uuid::new_v4().simple()));
        std::fs::copy(dir.join(format!("{}.json", good.job_id)), &renamed).unwrap();

        let (loaded, corrupt) = j.load().unwrap();

        assert_eq!(
            loaded.iter().map(|e| e.job_id).collect::<Vec<_>>(),
            [good.job_id]
        );
        assert_eq!(corrupt.len(), 2);
        assert!(std::fs::read_dir(&dir).unwrap().all(|e| {
            !e.unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".json.tmp")
        }));
        let mut t = torn.into_os_string();
        t.push(".corrupt");
        assert!(PathBuf::from(t).is_file());
        let mut r = renamed.into_os_string();
        r.push(".corrupt");
        assert!(PathBuf::from(r).is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_removes_the_job_file() {
        let dir = temp_dir();
        let j = PrintJournal::new(&dir, &Logger::none()).unwrap();
        let e = entry(print_job_states::RECEIVED);
        j.write(&e).unwrap();
        j.delete(e.job_id).unwrap();
        assert!(j.load().unwrap().0.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
