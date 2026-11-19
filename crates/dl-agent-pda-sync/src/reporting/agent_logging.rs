//! One place for the agent's JSON-lines logs (port of `Reporting/AgentLogging.cs`): the
//! service writes `logs/agent-.log`, the verbs `logs/tools-.log`; both roll daily and at
//! 50 MB and keep 14 files.

use super::file_sink::RollingFileSink;
use super::log::{Level, Logger, LoggerConfig, Property, Sink};
use crate::config::AgentIdentity;
use crate::config::paths::AgentPaths;
use crate::config::time::{SharedClock, SystemClock, Timestamp, now_local};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// File name prefix of the service log.
pub const SERVICE_LOG_PREFIX: &str = "agent-";
/// File name prefix of the verbs' log.
pub const TOOLS_LOG_PREFIX: &str = "tools-";
/// Size at which a log file rolls over.
pub const FILE_SIZE_LIMIT_BYTES: u64 = 50 * 1024 * 1024;
/// Log files kept.
pub const RETAINED_FILES: usize = 14;
/// Folder under the temp directory for logs of runs without a data folder.
pub const TEMP_FOLDER: &str = "PdaSyncAgent";
/// The namespace whose loggers write from `Debug` up.
pub const REPORTING_NAMESPACE: &str = "MM.PdaOrder.SyncAgent.Reporting";
/// `SiteEventSources.Agent`: the running service.
pub const SOURCE_AGENT: &str = "agent";

/// Where a verb writes its JSON-lines log and queues its site events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolLogTarget {
    /// The logs folder.
    pub logs_dir: PathBuf,
    /// The site event queue.
    pub events_db: PathBuf,
    /// `data` (the agent data folder), `output` (beside the verb's output) or `temp`.
    pub kind: String,
}

impl ToolLogTarget {
    fn new(logs_dir: PathBuf, events_db: PathBuf, kind: &str) -> ToolLogTarget {
        ToolLogTarget {
            logs_dir,
            events_db,
            kind: kind.to_owned(),
        }
    }
}

/// `{verb}-{yyyyMMddTHHmmssZ}-{6 hex}`: sortable, unique enough per machine, at most 64
/// characters for the verbs' names.
pub fn new_run_id(verb: &str, now: Timestamp) -> String {
    let (y, mo, d, h, mi, s, _) = now.to_utc().parts();
    let rnd = dl_rand::secure_bytes::<3>().unwrap_or_else(|_| {
        let v = dl_rand::u64().to_le_bytes();
        [v[0], v[1], v[2]]
    });
    format!(
        "{verb}-{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z-{}",
        dl_encoding::hex::encode(rnd)
    )
}

fn base(
    source: &str,
    run_id: &str,
    identity: Option<&AgentIdentity>,
    clock: SharedClock,
) -> LoggerConfig {
    let mut enrich = vec![
        Property::new("source", source),
        Property::new("run_id", run_id),
    ];
    if let Some(id) = identity {
        enrich.push(Property::new("agent_id", id.agent_id_text()));
        enrich.push(Property::new("tenant", id.tenant_id.as_str()));
    }
    LoggerConfig {
        minimum: Level::Information,
        overrides: vec![(REPORTING_NAMESPACE.to_owned(), Level::Debug)],
        enrich,
        sinks: Vec::new(),
        clock,
    }
}

/// The service's logger: `logs/agent-.log`, and `bridge` (the site event bridge) from
/// `Warning` up.
pub fn create_service_logger(
    paths: &AgentPaths,
    run_id: &str,
    identity: &AgentIdentity,
    bridge: Option<Arc<dyn Sink>>,
) -> Logger {
    let mut c = base(SOURCE_AGENT, run_id, Some(identity), SystemClock::shared());
    let file = RollingFileSink::new(
        &paths.logs_dir().join(format!("{SERVICE_LOG_PREFIX}.log")),
        FILE_SIZE_LIMIT_BYTES,
        RETAINED_FILES,
    );
    c.sinks.push((Arc::new(file), Level::Verbose));
    if let Some(b) = bridge {
        c.sinks.push((b, Level::Warning));
    }
    Logger::new(c)
}

/// A verb's logger: `bridge` from `Warning` up, then `logs/tools-.log` in `target` when
/// there is one. `source` is the verb's site event source (`enroll`, `legacy-export`, ...).
pub fn create_tool_logger(
    target: Option<&ToolLogTarget>,
    source: &str,
    run_id: &str,
    identity: Option<&AgentIdentity>,
    bridge: Option<Arc<dyn Sink>>,
    clock: SharedClock,
) -> Logger {
    let mut c = base(source, run_id, identity, clock);
    if let Some(b) = bridge {
        c.sinks.push((b, Level::Warning));
    }
    if let Some(t) = target {
        let file = RollingFileSink::new(
            &t.logs_dir.join(format!("{TOOLS_LOG_PREFIX}.log")),
            FILE_SIZE_LIMIT_BYTES,
            RETAINED_FILES,
        );
        c.sinks.push((Arc::new(file), Level::Verbose));
    }
    Logger::new(c)
}

/// The file the verbs' log writes today (a size roll adds `_001`, ... to the name).
pub fn tools_log_file(target: &ToolLogTarget) -> PathBuf {
    target.logs_dir.join(format!(
        "{TOOLS_LOG_PREFIX}{}.log",
        now_local().date_compact()
    ))
}

/// `<temp>\PdaSyncAgent\logs`.
pub fn temp_logs_dir(temp_dir: &Path) -> PathBuf {
    temp_dir.join(TEMP_FOLDER).join("logs")
}

/// `Path.GetDirectoryName(Path.GetFullPath(path).TrimEnd(separators))`.
fn parent(path: &str) -> Option<PathBuf> {
    let full = std::path::absolute(path).ok()?;
    let text = full.to_string_lossy();
    let trimmed = text.trim_end_matches(['/', std::path::MAIN_SEPARATOR]);
    let p = Path::new(trimmed).parent()?;
    if p.as_os_str().is_empty() {
        return None;
    }
    Some(p.to_path_buf())
}

/// Where a run without the agent data folder logs and queues: a `logs` folder beside the
/// verb's path, then the temp folder.
pub fn fallback_targets(output_path: Option<&str>, temp_dir: &Path) -> Vec<ToolLogTarget> {
    let mut targets = Vec::new();
    if let Some(out) = output_path.filter(|p| !p.trim().is_empty())
        && let Some(parent) = parent(out)
    {
        let logs = parent.join("logs");
        targets.push(ToolLogTarget::new(
            logs.clone(),
            logs.join("events.db"),
            "output",
        ));
    }
    let temp = temp_logs_dir(temp_dir);
    targets.push(ToolLogTarget::new(
        temp.clone(),
        temp.join("events.db"),
        "temp",
    ));
    targets
}

fn can_write(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let probe = dir.join(format!(".write-probe-{}", dl_uuid::Uuid::new_v4().simple()));
    std::fs::write(&probe, b"").map_err(|e| e.to_string())?;
    std::fs::remove_file(&probe).map_err(|e| e.to_string())
}

/// The first writable of: the agent data folder, a `logs` folder beside the verb's
/// output, the temp folder. `None` when none is writable (the verb then runs without a log
/// file); `problems` gets `<dir>: <why>` for each candidate skipped.
pub fn resolve_tool_target(
    data_dir: Option<&str>,
    output_path: Option<&str>,
    temp_dir: &Path,
    problems: &mut Vec<String>,
) -> Option<ToolLogTarget> {
    let mut candidates = Vec::new();
    if let Some(d) = data_dir.filter(|d| !d.trim().is_empty()) {
        let paths = AgentPaths::new(d);
        candidates.push(ToolLogTarget::new(
            paths.logs_dir(),
            paths.events_db(),
            "data",
        ));
    }
    candidates.extend(fallback_targets(output_path, temp_dir));
    for c in candidates {
        match can_write(&c.logs_dir) {
            Ok(()) => return Some(c),
            Err(e) => problems.push(format!("{}: {e}", c.logs_dir.display())),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir {
        dir: PathBuf,
        blocker: PathBuf,
    }

    impl Dir {
        fn new() -> Dir {
            let dir = std::env::temp_dir()
                .join("pda-sync-agent-tests")
                .join(dl_uuid::Uuid::new_v4().simple().to_string());
            std::fs::create_dir_all(&dir).unwrap();
            let blocker = dir.join("blocker");
            std::fs::write(&blocker, "a file where a folder should be").unwrap();
            Dir { dir, blocker }
        }
        fn temp(&self) -> PathBuf {
            self.dir.join("tmp")
        }
        fn s(p: &Path) -> String {
            p.to_string_lossy().into_owned()
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn the_data_folder_comes_first() {
        let d = Dir::new();
        let data = d.dir.join("data");
        let mut problems = Vec::new();
        let out = Dir::s(&d.dir.join("out").join("bundle.zip"));
        let t = resolve_tool_target(Some(&Dir::s(&data)), Some(&out), &d.temp(), &mut problems);
        let paths = AgentPaths::new(&data);
        assert_eq!(
            t,
            Some(ToolLogTarget::new(
                paths.logs_dir(),
                paths.events_db(),
                "data"
            ))
        );
        assert!(problems.is_empty());
    }

    #[test]
    fn an_unwritable_data_folder_falls_back_to_a_logs_folder_beside_the_output() {
        let d = Dir::new();
        let mut problems = Vec::new();
        let out = Dir::s(&d.dir.join("out").join("bundle.zip"));
        let t = resolve_tool_target(
            Some(&Dir::s(&d.blocker)),
            Some(&out),
            &d.temp(),
            &mut problems,
        )
        .unwrap();
        assert_eq!(t.kind, "output");
        assert_eq!(t.logs_dir, d.dir.join("out").join("logs"));
        assert_eq!(
            t.events_db,
            d.dir.join("out").join("logs").join("events.db")
        );
        assert_eq!(problems.len(), 1);
        assert!(
            problems[0].starts_with(&Dir::s(&d.blocker)),
            "{}",
            problems[0]
        );
    }

    #[test]
    fn then_the_temp_folder() {
        let d = Dir::new();
        let mut problems = Vec::new();
        let out = Dir::s(&d.blocker.join("sub").join("bundle.zip"));
        let t = resolve_tool_target(
            Some(&Dir::s(&d.blocker)),
            Some(&out),
            &d.temp(),
            &mut problems,
        )
        .unwrap();
        assert_eq!(t.kind, "temp");
        assert_eq!(t.logs_dir, temp_logs_dir(&d.temp()));
        assert_eq!(problems.len(), 2);
    }

    #[test]
    fn nowhere_writable_is_null_with_every_reason() {
        let d = Dir::new();
        let mut problems = Vec::new();
        assert_eq!(
            resolve_tool_target(Some(&Dir::s(&d.blocker)), None, &d.blocker, &mut problems),
            None
        );
        assert_eq!(problems.len(), 2);
    }

    #[test]
    fn run_ids_are_sortable_unique_and_short() {
        let at = Timestamp::parse_iso("2026-11-13T15:00:00+07:00").unwrap();
        let a = new_run_id("legacy-export", at);
        let b = new_run_id("legacy-export", at);
        assert!(a.starts_with("legacy-export-20261113T080000Z-"), "{a}");
        let hex = &a["legacy-export-20261113T080000Z-".len()..];
        assert!(
            hex.len() == 6
                && hex
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        );
        assert_ne!(a, b);
        assert!(a.len() <= 64);
    }
}
