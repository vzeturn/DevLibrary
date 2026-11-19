//! `Reporting/ToolRunTests.cs`: a verb run: STARTED/FINISHED/FAILED, the tools log and its
//! footer, and a cloud that never changes the exit code.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dl_agent_pda_sync::config::AgentPaths;
use dl_agent_pda_sync::config::codes;
use dl_agent_pda_sync::contracts::wire::{SiteEvent, site_event_levels as lv, site_event_sources};
use dl_agent_pda_sync::reporting::agent_logging::TOOLS_LOG_PREFIX;
use dl_agent_pda_sync::reporting::tool_run::{Console, EXIT_FAILED};
use dl_agent_pda_sync::reporting::{
    Exception, RunStages, SiteEventQueueStats, SiteEventSendResult, SiteEventSendStatus,
    SiteEventTransport, ToolRun, ToolRunOptions,
};
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_json::{Map, Value as JsonValue};

const EXIT_OK: i32 = 0;

struct ScriptedTransport {
    status: Mutex<SiteEventSendStatus>,
    calls: Mutex<usize>,
    accepted: Mutex<Vec<SiteEvent>>,
}

impl ScriptedTransport {
    fn new() -> Arc<ScriptedTransport> {
        Arc::new(ScriptedTransport {
            status: Mutex::new(SiteEventSendStatus::Accepted),
            calls: Mutex::default(),
            accepted: Mutex::default(),
        })
    }
    fn accepted(&self) -> Vec<SiteEvent> {
        self.accepted.lock().unwrap().clone()
    }
    fn codes(&self) -> Vec<String> {
        self.accepted().into_iter().map(|e| e.code).collect()
    }
    fn calls(&self) -> usize {
        *self.calls.lock().unwrap()
    }
}

impl SiteEventTransport for ScriptedTransport {
    fn send(
        &self,
        events: &[SiteEvent],
        _ctx: &Ctx,
    ) -> Result<SiteEventSendResult, dl_agent_pda_sync::transport::Error> {
        *self.calls.lock().unwrap() += 1;
        let status = *self.status.lock().unwrap();
        if status == SiteEventSendStatus::Accepted {
            self.accepted.lock().unwrap().extend(events.iter().cloned());
        }
        Ok(SiteEventSendResult {
            status,
            http_status: 200,
            error: None,
        })
    }
}

struct Env {
    dir: PathBuf,
    out: Arc<Mutex<Vec<u8>>>,
    transport: Arc<ScriptedTransport>,
}

impl Env {
    fn new() -> Env {
        let dir = std::env::temp_dir()
            .join("pda-sync-agent-tests")
            .join(dl_uuid::Uuid::new_v4().simple().to_string());
        std::fs::create_dir_all(&dir).unwrap();
        Env {
            dir,
            out: Arc::default(),
            transport: ScriptedTransport::new(),
        }
    }
    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }
    fn paths(&self) -> AgentPaths {
        AgentPaths::new(&self.dir)
    }
    fn console(&self) -> String {
        String::from_utf8(self.out.lock().unwrap().clone()).unwrap()
    }
    fn options(&self, source: &str, verb: &str, failure: &str) -> ToolRunOptions {
        let mut o = ToolRunOptions::new(source, verb, failure);
        o.data_dir = Some(Env::s(&self.dir));
        o.temp_dir = self.dir.join("tmp");
        o.out = self.out.clone() as Console;
        o
    }
    fn export(&self, with_transport: bool) -> ToolRunOptions {
        let mut o = self.options(
            site_event_sources::LEGACY_EXPORT,
            "legacy-export",
            codes::LEGACY_EXPORT_FAILED,
        );
        if with_transport {
            let t = self.transport.clone();
            o.transport = Some(Arc::new(move |_| {
                Some(t.clone() as Arc<dyn SiteEventTransport>)
            }));
        }
        o
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn get<'a>(e: &'a SiteEvent, k: &str) -> Option<&'a JsonValue> {
    e.detail.as_ref().and_then(|d| d.get(k))
}

#[test]
fn a_clean_run_reports_started_and_finished_and_prints_its_log_and_run_id() {
    let env = Env::new();
    let mut run_id = String::new();
    let mut start = Map::new();
    start.insert("dry_run", false);
    let exit = ToolRun::execute(env.export(true), Some(start), |run| {
        run_id = run.run_id().to_owned();
        run.line("hello");
        Ok(EXIT_OK)
    })
    .unwrap();

    assert_eq!(exit, EXIT_OK);
    assert!(run_id.starts_with("legacy-export-"));
    assert_eq!(
        env.transport.codes(),
        ["LEGACY_EXPORT_STARTED", "LEGACY_EXPORT_FINISHED"]
    );
    let acc = env.transport.accepted();
    assert!(
        acc.iter()
            .all(|e| e.source == site_event_sources::LEGACY_EXPORT
                && e.run_id.as_deref() == Some(run_id.as_str()))
    );
    assert_eq!(
        get(&acc[0], "dry_run").and_then(JsonValue::as_bool),
        Some(false)
    );
    assert_eq!(
        get(&acc[1], "exit_code").and_then(JsonValue::as_i64),
        Some(0)
    );
    assert_eq!(SiteEventQueueStats::read(env.paths().events_db()).depth, 0);

    let console = env.console();
    assert!(console.starts_with("hello"), "{console}");
    // Console.Out.WriteLine: every line, the footer included, ends with the platform
    // newline (`\r\n` on Windows) like the C# verbs (E1 side-by-side).
    let nl = dl_agent_pda_sync::util::stj::NEWLINE;
    assert!(
        console.starts_with(&format!("hello{nl}log: ")),
        "{console:?}"
    );
    assert!(console.ends_with(nl), "{console:?}");
    if cfg!(windows) {
        assert_eq!(
            console.matches('\n').count(),
            console.matches("\r\n").count()
        );
    }
    let files: Vec<PathBuf> = std::fs::read_dir(env.paths().logs_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(TOOLS_LOG_PREFIX)
        })
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let file = &files[0];
    assert!(
        console.contains(&format!("log: {} (run {run_id})", file.display())),
        "{console}"
    );
    assert!(!console.contains("site events:"));
    let text = std::fs::read_to_string(file).unwrap();
    let hello = text
        .lines()
        .map(|l| dl_agent_pda_sync::util::stj::parse(l).unwrap())
        .find(|v| v.get("msg").and_then(JsonValue::as_str) == Some("hello"))
        .expect("the console line is logged");
    assert_eq!(
        hello.get("run_id").and_then(JsonValue::as_str),
        Some(run_id.as_str())
    );
    assert_eq!(
        hello.get("source").and_then(JsonValue::as_str),
        Some(site_event_sources::LEGACY_EXPORT)
    );
}

#[test]
fn a_refusal_is_its_own_event_and_becomes_the_failed_code() {
    let env = Env::new();
    let exit = ToolRun::execute(env.export(true), None, |run| {
        run.fail(codes::LEGACY_SITE_LIVE, "The legacy site is live", lv::WARN);
        Ok(EXIT_FAILED)
    })
    .unwrap();

    assert_eq!(exit, EXIT_FAILED);
    assert_eq!(
        env.transport.codes(),
        [
            "LEGACY_EXPORT_STARTED",
            codes::LEGACY_SITE_LIVE,
            "LEGACY_EXPORT_FAILED"
        ]
    );
    let acc = env.transport.accepted();
    assert_eq!(acc[1].level, lv::WARN);
    assert_eq!(acc[2].level, lv::ERROR);
    assert_eq!(
        get(&acc[2], "code").and_then(JsonValue::as_str),
        Some(codes::LEGACY_SITE_LIVE)
    );
    assert_eq!(
        get(&acc[2], "exit_code").and_then(JsonValue::as_i64),
        Some(i64::from(EXIT_FAILED))
    );
}

#[test]
fn an_unexpected_error_is_one_failed_event_and_is_returned() {
    let env = Env::new();
    let err = ToolRun::execute(env.export(true), None, |_| {
        Err(Box::new(Exception::new(
            "System.InvalidOperationException",
            "boom",
        )))
    })
    .unwrap_err();

    assert_eq!(err.message, "boom");
    assert_eq!(
        env.transport.codes(),
        ["LEGACY_EXPORT_STARTED", "LEGACY_EXPORT_FAILED"]
    );
    let failed = &env.transport.accepted()[1];
    assert!(
        failed.message.contains("InvalidOperationException: boom"),
        "{}",
        failed.message
    );
    assert_eq!(
        get(failed, "code").and_then(JsonValue::as_str),
        Some(codes::LEGACY_EXPORT_FAILED)
    );
    assert_eq!(
        get(failed, "exception_type").and_then(JsonValue::as_str),
        Some("System.InvalidOperationException")
    );
    assert!(env.console().contains("log: "));
}

#[test]
fn without_an_identity_the_events_stay_queued_and_the_console_says_where() {
    let env = Env::new();
    let exit = ToolRun::execute(env.export(false), None, |_| Ok(EXIT_OK)).unwrap();

    assert_eq!(exit, EXIT_OK);
    assert_eq!(SiteEventQueueStats::read(env.paths().events_db()).depth, 2);
    let console = env.console();
    assert!(
        console.contains(&format!(
            "site events: 2 queued in {}",
            env.paths().events_db().display()
        )),
        "{console}"
    );
    assert!(console.contains("next successful enroll"));
}

#[test]
fn events_an_export_queued_beside_its_bundle_are_sent_by_the_next_run_on_the_agent_machine() {
    let env = Env::new();
    // The web server has no agent: the export logs and queues beside the bundle it writes.
    let bundle = env.dir.join("handover").join("legacy-bundle.zip");
    let stranded = env.dir.join("handover").join("logs").join("events.db");
    let mut export_run = String::new();
    let mut o = env.export(false);
    o.data_dir = None;
    o.output_path = Some(Env::s(&bundle));
    ToolRun::execute(o, None, |run| {
        export_run = run.run_id().to_owned();
        Ok(EXIT_OK)
    })
    .unwrap();
    assert_eq!(SiteEventQueueStats::read(&stranded).depth, 2);
    assert_eq!(env.transport.calls(), 0);

    // The folder travels with the bundle to the SQL Server machine, where the agent is enrolled.
    let mut archive_run = String::new();
    let mut o = env.options(
        site_event_sources::LEGACY_ARCHIVE,
        "legacy-archive",
        codes::LEGACY_ARCHIVE_FAILED,
    );
    o.output_path = Some(Env::s(&bundle));
    let t = env.transport.clone();
    o.transport = Some(Arc::new(move |_| {
        Some(t.clone() as Arc<dyn SiteEventTransport>)
    }));
    ToolRun::execute(o, None, |run| {
        archive_run = run.run_id().to_owned();
        Ok(EXIT_OK)
    })
    .unwrap();

    assert_eq!(
        env.transport.codes(),
        [
            "LEGACY_EXPORT_STARTED",
            "LEGACY_EXPORT_FINISHED",
            "LEGACY_ARCHIVE_STARTED",
            "LEGACY_ARCHIVE_FINISHED"
        ]
    );
    let acc = env.transport.accepted();
    assert!(
        acc[..2]
            .iter()
            .all(|e| e.source == site_event_sources::LEGACY_EXPORT
                && e.run_id.as_deref() == Some(export_run.as_str()))
    );
    assert!(
        acc[2..]
            .iter()
            .all(|e| e.source == site_event_sources::LEGACY_ARCHIVE
                && e.run_id.as_deref() == Some(archive_run.as_str()))
    );
    assert_eq!(SiteEventQueueStats::read(&stranded).depth, 0);
    assert_eq!(SiteEventQueueStats::read(env.paths().events_db()).depth, 0);
}

#[test]
fn a_cloud_that_fails_pauses_the_flushes_and_never_changes_the_exit_code() {
    let env = Env::new();
    *env.transport.status.lock().unwrap() = SiteEventSendStatus::Failed;
    let exit = ToolRun::execute(env.export(true), None, |run| {
        run.stage(lv::INFO, "LEGACY_EXPORT_TABLE", "orders: 3 rows", None);
        Ok(EXIT_OK)
    })
    .unwrap();

    assert_eq!(exit, EXIT_OK);
    assert_eq!(
        env.transport.calls(),
        2,
        "the stage flush waits out the pause; the final flush tries once more"
    );
    assert_eq!(SiteEventQueueStats::read(env.paths().events_db()).depth, 3);
}

#[test]
fn with_no_writable_folder_the_run_still_works_and_reports_it() {
    let env = Env::new();
    let blocker = env.dir.join("blocker");
    std::fs::write(&blocker, "a file where a folder should be").unwrap();
    let mut o = env.export(true);
    o.data_dir = Some(Env::s(&blocker));
    o.temp_dir = blocker.clone();

    let exit = ToolRun::execute(o, None, |_| Ok(EXIT_OK)).unwrap();

    assert_eq!(exit, EXIT_OK);
    assert_eq!(
        env.transport.codes(),
        [
            "TOOLS_LOG_UNAVAILABLE",
            "LEGACY_EXPORT_STARTED",
            "LEGACY_EXPORT_FINISHED"
        ]
    );
    assert!(env.console().contains("log: (no writable log folder)"));
}
