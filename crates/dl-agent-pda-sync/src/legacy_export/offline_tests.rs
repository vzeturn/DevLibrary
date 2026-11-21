//! Unit tests of `take_offline` (`offline.rs`) and the new payload members; the command flow
//! is in `tests/legacy_export_offline.rs`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use super::events::{MemoryEvents, RunEvents};
use super::offline::{
    CommitState, EVENT_KEPT, EVENT_KEPT_UNKNOWN, EVENT_REMOVED, LegacySite, OFFLINE_PAGE,
    Placement, discover, expand_env, finish, looks_like_pda, parse_paths, place,
};
use super::payload::LegacyExportPayload;
use super::sources::AppPoolProbe;
use super::sources::{OFFLINE_FILE, maintenance_gate, stages};
use crate::config::codes;
use crate::contracts::WireJson;
use crate::reporting::{Logger, LoggerConfig};

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir()
            .join("pda-legacy-offline-unit")
            .join(dl_uuid::Uuid::new_v4().simple().to_string());
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }

    fn text(&self) -> String {
        self.0.to_str().unwrap().to_owned()
    }
}

struct Site {
    paths: std::result::Result<Vec<String>, String>,
    settled: AtomicU32,
}

impl Site {
    fn with(paths: Vec<String>) -> Site {
        Site {
            paths: Ok(paths),
            settled: AtomicU32::new(0),
        }
    }
}

impl LegacySite for Site {
    fn physical_paths(&self) -> std::result::Result<Vec<String>, String> {
        self.paths.clone()
    }

    fn settle(&self) {
        self.settled.fetch_add(1, Ordering::SeqCst);
    }
}

fn pda_app(dir: &Path, dll_in_bin: bool, dll: &str) {
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(dir.join("web.config"), b"<configuration/>").unwrap();
    let at = if dll_in_bin {
        dir.join("bin")
    } else {
        dir.to_path_buf()
    };
    std::fs::write(at.join(dll), b"MZ").unwrap();
}

fn run_events() -> (Arc<MemoryEvents>, RunEvents) {
    let mem = Arc::new(MemoryEvents::default());
    let events = RunEvents::new(mem.clone(), "run-1");
    (mem, events)
}

fn log() -> Logger {
    Logger::new(LoggerConfig::default())
}

#[test]
fn the_payload_round_trips_take_offline_and_keep_tenant_and_old_clouds_omit_them() {
    let old = LegacyExportPayload::from_json(
        r#"{"source":"auto","allow_live":false,"commit":true,"prefer_legacy":false,"viet_qr_only":false,"reason":"r"}"#,
    )
    .unwrap();
    assert!(!old.take_offline && !old.keep_tenant);

    let p = LegacyExportPayload {
        source: "auto".to_owned(),
        commit: true,
        reason: "cutover".to_owned(),
        take_offline: true,
        keep_tenant: true,
        ..LegacyExportPayload::default()
    };
    let json = p.to_json();
    assert!(json.contains("\"take_offline\":true"), "{json}");
    assert!(json.contains("\"keep_tenant\":true"), "{json}");
    assert_eq!(LegacyExportPayload::from_json(&json).unwrap(), p);
}

#[test]
fn environment_variables_expand_and_unknown_ones_stay() {
    let lookup = |n: &str| (n.eq_ignore_ascii_case("SystemDrive")).then(|| "C:".to_owned());
    assert_eq!(
        expand_env(r"%SystemDrive%\inetpub\pda", &lookup),
        r"C:\inetpub\pda"
    );
    assert_eq!(expand_env(r"%NOPE%\x", &lookup), r"%NOPE%\x");
    assert_eq!(expand_env("50% off", &lookup), "50% off");
    assert_eq!(expand_env("%%", &lookup), "%%");
}

#[test]
fn appcmd_output_becomes_unique_trimmed_paths() {
    let lookup = |n: &str| (n == "SystemDrive").then(|| "C:".to_owned());
    let text = "%SystemDrive%\\inetpub\\wwwroot\r\n\r\nD:\\Sites\\Pda\\\nd:\\sites\\pda\n\"E:\\Other\"\nC:\\\n";
    assert_eq!(
        parse_paths(text, &lookup),
        vec![
            r"C:\inetpub\wwwroot".to_owned(),
            r"D:\Sites\Pda".to_owned(),
            r"E:\Other".to_owned(),
            r"C:\".to_owned(),
        ]
    );
}

#[test]
fn discovery_needs_exactly_one_legacy_pda_app() {
    let tmp = Tmp::new();
    let pda = tmp.0.join("pda");
    let other = tmp.0.join("site");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("web.config"), b"<configuration/>").unwrap();
    pda_app(&pda, false, "MM.PdaOrder.Api.dll");
    assert!(looks_like_pda(&pda) && !looks_like_pda(&other));

    // None.
    let none = discover(&Site::with(vec![other.to_str().unwrap().to_owned()])).unwrap_err();
    assert_eq!(
        (none.code.as_str(), none.refusal, none.stage),
        (codes::LEGACY_SITE_UNRESOLVED, true, stages::PRECHECK)
    );
    assert!(
        none.message.contains("found 0 IIS applications"),
        "{}",
        none.message
    );

    // One.
    let one = Site::with(vec![
        other.to_str().unwrap().to_owned(),
        pda.to_str().unwrap().to_owned(),
    ]);
    assert_eq!(discover(&one).unwrap(), pda.to_str().unwrap());

    // Two (the second keeps its assembly in bin, named MM.PDA*): refused, no path named.
    let second = tmp.0.join("pda2");
    pda_app(&second, true, "mm.pda.web.DLL");
    let two = discover(&Site::with(vec![
        pda.to_str().unwrap().to_owned(),
        second.to_str().unwrap().to_owned(),
    ]))
    .unwrap_err();
    assert!(
        two.message.contains("found 2 IIS applications"),
        "{}",
        two.message
    );
    assert!(!two.message.contains(&tmp.text()), "{}", two.message);

    // appcmd failing is a refusal that names the problem.
    let broken = Site {
        paths: Err("appcmd could not list".to_owned()),
        settled: AtomicU32::new(0),
    };
    assert_eq!(
        discover(&broken).unwrap_err().code,
        codes::LEGACY_SITE_UNRESOLVED
    );
}

#[test]
fn placement_writes_once_waits_and_names_itself_in_the_gate_evidence() {
    let tmp = Tmp::new();
    let site = Site::with(Vec::new());
    let p = place(&tmp.text(), &site).unwrap();
    assert!(p.created);
    assert_eq!(site.settled.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read_to_string(tmp.0.join(OFFLINE_FILE)).unwrap(),
        OFFLINE_PAGE
    );

    struct NoPool;
    impl AppPoolProbe for NoPool {
        fn state(&self, _p: &str) -> std::result::Result<Option<String>, String> {
            Ok(None)
        }
    }
    let m = maintenance_gate(Some(&tmp.text()), None, false, &NoPool).unwrap();
    assert_eq!(m.mode, "maintenance");
    assert_eq!(
        p.evidence(&m.evidence),
        format!("{OFFLINE_FILE} placed by the agent in {}", tmp.text())
    );

    // A missing site root is refused before anything is written.
    let gone = place(&tmp.0.join("nope").to_string_lossy(), &site).unwrap_err();
    assert_eq!(gone.code, codes::BAD_ARGUMENTS);
}

#[test]
fn a_file_that_was_already_there_is_never_touched() {
    let tmp = Tmp::new();
    std::fs::write(tmp.0.join(OFFLINE_FILE), b"operator's own").unwrap();
    let site = Site::with(Vec::new());
    let p = place(&tmp.text(), &site).unwrap();
    assert!(!p.created);
    assert_eq!(site.settled.load(Ordering::SeqCst), 0);
    assert_eq!(p.evidence("x present"), "x present");

    let (mem, events) = run_events();
    finish(
        &p,
        CommitState::NotCommitted,
        stages::CLEANUP,
        &events,
        &log(),
    )
    .unwrap();
    assert_eq!(
        std::fs::read(tmp.0.join(OFFLINE_FILE)).unwrap(),
        b"operator's own"
    );
    assert!(mem.codes().is_empty());
}

#[test]
fn the_agents_file_is_removed_without_a_commit_and_kept_after_one() {
    let tmp = Tmp::new();
    let site = Site::with(Vec::new());

    let p = place(&tmp.text(), &site).unwrap();
    let (mem, events) = run_events();
    finish(
        &p,
        CommitState::NotCommitted,
        stages::CLEANUP,
        &events,
        &log(),
    )
    .unwrap();
    assert!(!tmp.0.join(OFFLINE_FILE).exists());
    assert_eq!(mem.codes(), vec![EVENT_REMOVED.to_owned()]);

    let p = place(&tmp.text(), &site).unwrap();
    let (mem, events) = run_events();
    finish(&p, CommitState::Committed, stages::CLEANUP, &events, &log()).unwrap();
    assert!(tmp.0.join(OFFLINE_FILE).is_file());
    assert_eq!(mem.codes(), vec![EVENT_KEPT.to_owned()]);
}

#[test]
fn an_unknown_commit_outcome_keeps_the_file_and_tells_the_operator() {
    use super::sources::ExportError;
    let at = |stage, code: &str, refused: bool| {
        if refused {
            ExportError::refused(stage, code, "x")
        } else {
            ExportError::new(stage, code, "x")
        }
    };
    let of = |e: &ExportError| CommitState::of(Err(e));
    // A timeout or transport failure while committing: the cloud may have committed.
    assert_eq!(
        of(&at(stages::COMMIT, "TIMEOUT", false)),
        CommitState::Unknown
    );
    assert_eq!(
        of(&at(stages::COMMIT, codes::LEGACY_UPLOAD_FAILED, false)),
        CommitState::Unknown
    );
    // A definite cloud answer at the commit: nothing committed.
    assert_eq!(
        of(&at(stages::COMMIT, "TENANT_ACTIVE", true)),
        CommitState::NotCommitted
    );
    assert_eq!(
        of(&at(
            stages::COMMIT,
            codes::LEGACY_UPLOAD_NOT_COMMITTED,
            false
        )),
        CommitState::NotCommitted
    );
    assert_eq!(
        of(&at(stages::DRY_RUN, "TIMEOUT", false)),
        CommitState::NotCommitted
    );
    assert_eq!(
        of(&at(stages::POST_COMMIT, "COUNT_CHECK_FAILED", false)),
        CommitState::Committed
    );
    assert_eq!(CommitState::of(Ok(true)), CommitState::Committed);
    assert_eq!(CommitState::of(Ok(false)), CommitState::NotCommitted);

    let tmp = Tmp::new();
    let p = place(&tmp.text(), &Site::with(Vec::new())).unwrap();
    let (mem, events) = run_events();
    finish(&p, CommitState::Unknown, stages::CLEANUP, &events, &log()).unwrap();
    assert!(tmp.0.join(OFFLINE_FILE).is_file());
    assert_eq!(mem.codes(), vec![EVENT_KEPT_UNKNOWN.to_owned()]);
    assert!(mem.events()[0].message.contains("Check the import result"));
}

#[test]
fn a_removal_failure_is_returned_not_swallowed() {
    let tmp = Tmp::new();
    // Something replaced the agent's file with a folder: remove_file fails.
    std::fs::create_dir_all(tmp.0.join(OFFLINE_FILE).join("x")).unwrap();
    let p = Placement {
        root: tmp.text(),
        created: true,
    };
    let (mem, events) = run_events();
    let e = finish(
        &p,
        CommitState::NotCommitted,
        stages::CLEANUP,
        &events,
        &log(),
    )
    .unwrap_err();
    assert_eq!(
        (e.code.as_str(), e.stage),
        (codes::LEGACY_SITE_OFFLINE_FAILED, stages::CLEANUP)
    );
    assert!(e.message.contains("could not be removed"), "{}", e.message);
    assert!(mem.codes().is_empty());
}
