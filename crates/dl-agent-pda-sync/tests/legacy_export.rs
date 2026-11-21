//! `legacy.export` (RT-0390, ADR 0136): command flow, bundle, upload sequence, cleanup, events.

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dl_agent_pda_sync::config::codes;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, LegacyImportAck, LegacyImportTableAck, SyncCommand, SyncCommandResult,
};
use dl_agent_pda_sync::legacy_export::events::{LegacyEvents, MemoryEvents};
use dl_agent_pda_sync::legacy_export::sources::{
    AppPoolProbe, BrandingSource, ExportError, LegacySqlSource, VietQrRead, VietQrSource, stages,
};
use dl_agent_pda_sync::legacy_export::upload::{
    BundleUploader, BundleZip, TransportFailure, UploadAnswer,
};
use dl_agent_pda_sync::legacy_export::{
    LEGACY_EXPORT, LegacyExportCommandHandler, LegacyExportEnv,
};
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_agent_pda_sync::util::stj;
use dl_json::Value as Json;
use dl_sqlite::{Connection, OpenFlags};
use dl_uuid::Uuid;
use support::TestEnv;

const SECRET: &str = "hunter2-secret";
const GUEST: &str = "Nguyen Van Guest";

// ---- fakes

struct FakeSql {
    rvc_calls: AtomicU32,
    fail_from_call: u32,
}

impl FakeSql {
    fn ok() -> Arc<FakeSql> {
        Arc::new(FakeSql {
            rvc_calls: AtomicU32::new(0),
            fail_from_call: u32::MAX,
        })
    }

    /// Answers the pre-check, then fails like a dropped connection.
    fn dies_after_precheck() -> Arc<FakeSql> {
        Arc::new(FakeSql {
            rvc_calls: AtomicU32::new(0),
            fail_from_call: 2,
        })
    }
}

impl LegacySqlSource for FakeSql {
    fn database(&self) -> String {
        "SMILE_POS".to_owned()
    }

    fn rvc_list(&self) -> Result<Vec<(i32, String)>, ExportError> {
        let n = self.rvc_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if n >= self.fail_from_call {
            let mut e = ExportError::new(
                stages::EXPORT,
                codes::SQL_UNREACHABLE,
                "SQL Server 'box' cannot be reached (error 53).",
            );
            e.sql_number = Some(53);
            return Err(e);
        }
        Ok(vec![(1, "Restaurant".to_owned()), (2, "Bar".to_owned())])
    }

    fn pda_tables(&self) -> Result<Vec<(String, i64)>, ExportError> {
        Ok(vec![("dbo.Pda_Old".to_owned(), 3)])
    }

    fn columns(&self, _q: &str) -> Result<Vec<String>, ExportError> {
        Ok(Vec::new())
    }

    fn for_each_row(
        &self,
        _q: &str,
        _c: &[String],
        _s: &mut dyn FnMut(Vec<dl_tds::Value>) -> Result<(), ExportError>,
    ) -> Result<(), ExportError> {
        Ok(())
    }
}

struct Stopped;
impl AppPoolProbe for Stopped {
    fn state(&self, _p: &str) -> Result<Option<String>, String> {
        Ok(Some("Stopped".to_owned()))
    }
}

struct NoBrand;
impl BrandingSource for NoBrand {
    fn read(&self) -> Vec<(String, String)> {
        vec![("Name".to_owned(), "Old Brand".to_owned())]
    }
}

struct Account(VietQrRead);
impl VietQrSource for Account {
    fn read(&self) -> VietQrRead {
        self.0.clone()
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Clean,
    RefuseDry(&'static str),
    GapAfterCommit,
}

#[derive(Debug, Clone)]
struct Call {
    dry_run: bool,
    prefer_legacy: bool,
    zip_existed: bool,
}

struct FakeUploader {
    mode: Mode,
    calls: Mutex<Vec<Call>>,
    zips: Mutex<Vec<Vec<u8>>>,
    committed: AtomicBool,
    gate: Mutex<Option<Receiver<()>>>,
    entered: Mutex<Option<Sender<()>>>,
}

impl FakeUploader {
    fn new(mode: Mode) -> Arc<FakeUploader> {
        Arc::new(FakeUploader {
            mode,
            calls: Mutex::new(Vec::new()),
            zips: Mutex::new(Vec::new()),
            committed: AtomicBool::new(false),
            gate: Mutex::new(None),
            entered: Mutex::new(None),
        })
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
}

fn table(target: &str, inserted: i64, same: i64) -> LegacyImportTableAck {
    LegacyImportTableAck {
        source: target.to_owned(),
        target: target.to_owned(),
        source_rows: inserted + same,
        inserted,
        same,
        ..LegacyImportTableAck::default()
    }
}

impl BundleUploader for FakeUploader {
    fn send(
        &self,
        zip: &BundleZip,
        dry_run: bool,
        prefer_legacy: bool,
        _ctx: &Ctx,
    ) -> Result<UploadAnswer, TransportFailure> {
        if let Some(tx) = self.entered.lock().unwrap().take() {
            tx.send(()).unwrap();
        }
        if let Some(rx) = self.gate.lock().unwrap().as_ref() {
            rx.recv_timeout(Duration::from_secs(20)).unwrap();
        }
        self.calls.lock().unwrap().push(Call {
            dry_run,
            prefer_legacy,
            zip_existed: zip.path.is_file(),
        });
        self.zips
            .lock()
            .unwrap()
            .push(std::fs::read(&zip.path).unwrap());
        let after_commit = self.committed.load(Ordering::SeqCst);
        let mut ack = LegacyImportAck {
            slug: "demo".to_owned(),
            dry_run,
            committed: !dry_run,
            manifest_sha256: zip.bundle.manifest_sha256.clone(),
            tenant_status: "onboarding".to_owned(),
            data_mode: "cloud".to_owned(),
            ..LegacyImportAck::default()
        };
        if let (true, Mode::RefuseDry(code)) = (dry_run, self.mode) {
            ack.refusal_code = Some(code.to_owned());
            ack.refusal = Some(format!("rows differ for {GUEST}"));
        }
        ack.tables = if after_commit {
            let inserted = i64::from(dry_run && self.mode == Mode::GapAfterCommit) * 2;
            vec![table("pda_config", inserted, 5 - inserted)]
        } else {
            vec![table("pda_config", 5, 0)]
        };
        if !dry_run {
            self.committed.store(true, Ordering::SeqCst);
        }
        Ok(UploadAnswer {
            status: 200,
            token_refreshed: false,
            ack: Some(ack),
            problem_code: None,
            problem: None,
        })
    }
}

// ---- fixture

struct Fx {
    env: TestEnv,
    work: PathBuf,
    pda_db: PathBuf,
    events: Arc<MemoryEvents>,
}

impl Fx {
    fn new() -> Fx {
        let env = TestEnv::new();
        let work = env.dir.join("legacy-export");
        let pda_db = env.dir.join("MM-PDA").join("pda.db");
        std::fs::create_dir_all(pda_db.parent().unwrap()).unwrap();
        let db = Connection::open(&pda_db, OpenFlags::default()).unwrap();
        db.execute_batch(&format!(
            "CREATE TABLE schema_version (version INTEGER);
             INSERT INTO schema_version VALUES (3);
             CREATE TABLE pda_config (rvc_no INTEGER, key TEXT, value TEXT);
             INSERT INTO pda_config VALUES (1, 'currency', 'VND');
             INSERT INTO pda_config VALUES (1, 'smtp.password', '{SECRET}');
             CREATE TABLE pda_item_meta (item_no INTEGER, note TEXT, img BLOB);
             INSERT INTO pda_item_meta VALUES (7, 'no peanuts for {GUEST}', x'cafe');
             INSERT INTO pda_item_meta VALUES (8, NULL, NULL);
             CREATE TABLE pda_mystery (id INTEGER);
             INSERT INTO pda_mystery VALUES (1);"
        ))
        .unwrap();
        drop(db);
        Fx {
            env,
            work,
            pda_db,
            events: Arc::new(MemoryEvents::default()),
        }
    }

    fn handler(
        &self,
        sql: Arc<dyn LegacySqlSource>,
        up: Arc<dyn BundleUploader>,
        vq: VietQrRead,
    ) -> LegacyExportCommandHandler {
        let events = self.events.clone();
        let env = LegacyExportEnv {
            work_dir: self.work.clone(),
            pda_db: self.pda_db.clone(),
            sql: Arc::new(move |_p| Ok(sql.clone())),
            pools: Arc::new(Stopped),
            branding: Arc::new(NoBrand),
            vietqr: Arc::new(Account(vq)),
            uploader: up,
            events: Arc::new(move |_run| events.clone() as Arc<dyn LegacyEvents>),
            clock: self.env.time.clone(),
            zone: Arc::new(dl_agent_pda_sync::contracts::TimeZone::utc()),
            exporter: "PdaSyncAgent test".to_owned(),
            max_bytes: 95 * 1024 * 1024,
            app_version: Arc::new(|_| None),
        };
        LegacyExportCommandHandler::new(env, &self.env.log)
    }
}

fn payload(extra: &str) -> Json {
    stj::parse(&format!(
        r#"{{"source":"pda-db","app_pool":"PdaApp","allow_live":false,"commit":false,"prefer_legacy":false,"viet_qr_only":false,"reason":"cutover of the store"{extra}}}"#
    ))
    .unwrap()
}

fn command(payload: Json) -> SyncCommand {
    SyncCommand {
        command_id: Uuid::new_v4(),
        type_: LEGACY_EXPORT.to_owned(),
        aggregate_key: "legacy:export".to_owned(),
        expected_hash: None,
        payload,
        created_by: "operator".to_owned(),
        expires_at_utc: None,
        contract_version: 1,
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
    }
}

fn run_id(r: &SyncCommandResult) -> String {
    match r.result.as_ref().and_then(|v| v.get("runId")) {
        Some(Json::String(s)) => s.clone(),
        other => panic!("no runId in {other:?}"),
    }
}

fn zip_entries(bytes: &[u8]) -> Vec<String> {
    let zip = dl_zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
    zip.entries().iter().map(|e| e.name().to_owned()).collect()
}

fn zip_table_text(bytes: &[u8], name: &str) -> String {
    let mut zip = dl_zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).unwrap();
    let gz = zip.read_by_name(name, 1 << 24).unwrap();
    String::from_utf8(dl_flate::gzip::decompress(&gz, 1 << 24).unwrap()).unwrap()
}

fn work_is_empty(work: &Path) -> bool {
    !work.exists() || std::fs::read_dir(work).unwrap().next().is_none()
}

fn all_event_text(events: &MemoryEvents) -> String {
    events
        .events()
        .iter()
        .map(|e| format!("{} {} {} {:?}", e.code, e.level, e.message, e.detail))
        .collect::<Vec<_>>()
        .join("\n")
}

fn exec(h: &LegacyExportCommandHandler, c: &SyncCommand) -> SyncCommandResult {
    h.execute(c, &Ctx::background()).unwrap()
}

// ---- the command: reject reasons and the ack

#[test]
fn an_invalid_payload_and_a_newer_contract_are_rejected_with_a_reason() {
    let fx = Fx::new();
    let h = fx.handler(
        FakeSql::ok(),
        FakeUploader::new(Mode::Clean),
        VietQrRead::NotSet,
    );
    let r = exec(
        &h,
        &command(stj::parse(r#"{"source":"nowhere","reason":"x"}"#).unwrap()),
    );
    assert_eq!(r.status, CommandStatus::Rejected);
    assert_eq!(
        r.reason.as_deref(),
        Some("invalid_payload: source must be auto, pda-db or from-sql")
    );
    let r = exec(&h, &command(Json::String("nope".to_owned())));
    assert_eq!(
        r.reason.as_deref(),
        Some("invalid_payload: payload is not a legacy.export object")
    );
    let mut c = command(payload(""));
    c.contract_version = 2;
    assert_eq!(
        exec(&h, &c).reason.as_deref(),
        Some("contract_version_unsupported")
    );
    assert!(h.running().is_none());
}

#[test]
fn sql_down_at_the_precheck_is_a_specific_rejection_and_nothing_runs() {
    let fx = Fx::new();
    let sql = Arc::new(FakeSql {
        rvc_calls: AtomicU32::new(0),
        fail_from_call: 1,
    });
    let h = fx.handler(sql, FakeUploader::new(Mode::Clean), VietQrRead::NotSet);
    let r = exec(&h, &command(payload("")));
    assert_eq!(r.status, CommandStatus::Rejected);
    assert!(
        r.reason
            .as_deref()
            .unwrap()
            .starts_with("legacy_sql_unreachable: "),
        "{:?}",
        r.reason
    );
    assert!(h.running().is_none());
    let ev = fx.events.events();
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].code, "LEGACY_EXPORT_FAILED");
    assert_eq!(ev[0].detail.get("stage"), Some(&Json::from("precheck")));
    assert_eq!(
        ev[0].detail.get("error_code"),
        Some(&Json::from("SQL_UNREACHABLE"))
    );
}

#[test]
fn a_closed_maintenance_gate_rejects_and_a_stopped_pool_opens_it() {
    let fx = Fx::new();
    // No site root and no pool named: the gate cannot be evaluated, which counts as closed.
    let h = fx.handler(
        FakeSql::ok(),
        FakeUploader::new(Mode::Clean),
        VietQrRead::NotSet,
    );
    let none = stj::parse(
        r#"{"source":"pda-db","allow_live":false,"commit":false,"prefer_legacy":false,"viet_qr_only":false,"reason":"r"}"#,
    )
    .unwrap();
    let r = exec(&h, &command(none));
    assert!(
        r.reason
            .as_deref()
            .unwrap()
            .starts_with("legacy_site_live: "),
        "{:?}",
        r.reason
    );
    assert!(r.reason.as_deref().unwrap().contains("counts as closed"));

    // A stopped app pool is the evidence of maintenance.
    let r = exec(&h, &command(payload("")));
    assert_eq!(r.status, CommandStatus::Applied);
    h.wait_idle();
    let started = fx
        .events
        .events()
        .into_iter()
        .find(|e| e.code == "LEGACY_EXPORT_STARTED")
        .unwrap();
    assert_eq!(started.source, "legacy-export");
}

#[test]
fn a_missing_pda_db_is_rejected_with_the_store_explanation() {
    let fx = Fx::new();
    std::fs::remove_file(&fx.pda_db).unwrap();
    let h = fx.handler(
        FakeSql::ok(),
        FakeUploader::new(Mode::Clean),
        VietQrRead::NotSet,
    );
    let r = exec(&h, &command(payload("")));
    assert!(
        r.reason
            .as_deref()
            .unwrap()
            .starts_with("legacy_store_missing: "),
        "{:?}",
        r.reason
    );
}

#[test]
fn the_ack_is_immediate_and_a_second_command_is_rejected_while_one_runs() {
    let fx = Fx::new();
    let up = FakeUploader::new(Mode::Clean);
    let (release, rx) = channel();
    let (entered_tx, entered) = channel();
    *up.gate.lock().unwrap() = Some(rx);
    *up.entered.lock().unwrap() = Some(entered_tx);
    let h = fx.handler(FakeSql::ok(), up.clone(), VietQrRead::NotSet);

    let first = command(payload(""));
    let r = exec(&h, &first);
    assert_eq!(r.status, CommandStatus::Applied);
    let id = run_id(&r);
    assert!(id.parse::<Uuid>().is_ok());
    assert_eq!(
        r.result.as_ref().unwrap().get("accepted"),
        Some(&Json::Bool(true))
    );
    // The run is now blocked inside the upload; the command channel was not.
    entered.recv_timeout(Duration::from_secs(20)).unwrap();
    assert_eq!(h.running().as_deref(), Some(id.as_str()));

    let second = exec(&h, &command(payload("")));
    assert_eq!(second.status, CommandStatus::Rejected);
    assert_eq!(
        second.reason.as_deref(),
        Some("legacy_export_already_running")
    );
    // A redelivery of the accepted command gets the same run id, not a second run.
    assert_eq!(run_id(&exec(&h, &first)), id);

    release.send(()).unwrap();
    h.wait_idle();
    assert!(h.running().is_none());
    assert_eq!(up.calls().len(), 1);
}

// ---- the bundle and the upload sequence

#[test]
fn a_dry_run_only_command_exports_zips_uploads_once_and_deletes_everything() {
    let fx = Fx::new();
    let up = FakeUploader::new(Mode::Clean);
    let h = fx.handler(
        FakeSql::ok(),
        up.clone(),
        VietQrRead::Read(vec![
            ("BankBin".to_owned(), "970436".to_owned()),
            ("AccountNo".to_owned(), "0123456789".to_owned()),
        ]),
    );
    assert_eq!(
        exec(&h, &command(payload(r#","prefer_legacy":false"#))).status,
        CommandStatus::Applied
    );
    h.wait_idle();

    let calls = up.calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].dry_run && calls[0].zip_existed);

    let zips = up.zips.lock().unwrap();
    let names = zip_entries(&zips[0]);
    for expected in [
        "manifest.json",
        "tables/pda_config.jsonl.gz",
        "tables/pda_item_meta.jsonl.gz",
        "tables/dbo.DefRVCList.jsonl.gz",
        "tables/registry.branding.jsonl.gz",
        "tables/registry.vietqr.jsonl.gz",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "{expected} missing from {names:?}"
        );
    }
    assert!(
        !names.iter().any(|n| n.contains("mystery")),
        "an unknown table is left out"
    );

    // The credential row never leaves; the other config row does.
    let config = zip_table_text(&zips[0], "tables/pda_config.jsonl.gz");
    assert!(
        config.contains("currency") && !config.contains(SECRET),
        "{config}"
    );
    let items = zip_table_text(&zips[0], "tables/pda_item_meta.jsonl.gz");
    assert!(
        items.contains(r"\xcafe"),
        "a blob is a bytea hex literal: {items}"
    );
    let rvc = zip_table_text(&zips[0], "tables/dbo.DefRVCList.jsonl.gz");
    assert!(rvc.contains("Restaurant") && rvc.contains("Bar"));
    let manifest = String::from_utf8(
        dl_zip::ZipArchive::new(std::io::Cursor::new(zips[0].clone()))
            .unwrap()
            .read_by_name("manifest.json", 1 << 20)
            .unwrap(),
    )
    .unwrap();
    assert!(
        manifest.contains("\"format\": \"pda-legacy-bundle\"")
            && manifest.contains("pda_config[1:smtp.password]")
    );
    assert!(
        manifest.contains("pda_mystery"),
        "the unknown table is listed as left out"
    );
    drop(zips);

    // Local bundle and zip are gone; the events say so without a single row value.
    assert!(work_is_empty(&fx.work));
    let codes = fx.events.codes();
    assert_eq!(
        codes.first().map(String::as_str),
        Some("LEGACY_EXPORT_STARTED")
    );
    assert_eq!(
        codes.last().map(String::as_str),
        Some("LEGACY_EXPORT_UPLOADED")
    );
    let text = all_event_text(&fx.events);
    assert!(
        !text.contains(SECRET) && !text.contains(GUEST) && !text.contains("0123456789"),
        "{text}"
    );
    assert!(
        text.contains("****6789"),
        "only the masked account is reported"
    );
    let uploaded = fx.events.events().into_iter().last().unwrap();
    assert_eq!(uploaded.source, "legacy-upload");
    assert_eq!(
        uploaded.detail.get("outcome"),
        Some(&Json::from("dry_run_only"))
    );
    assert_eq!(
        uploaded.detail.get("local_deleted"),
        Some(&Json::Bool(true))
    );
}

#[test]
fn commit_uploads_a_dry_run_then_the_commit_then_a_second_dry_run_of_the_same_bundle() {
    let fx = Fx::new();
    let up = FakeUploader::new(Mode::Clean);
    let h = fx.handler(FakeSql::ok(), up.clone(), VietQrRead::NotSet);
    exec(
        &h,
        &command(payload(r#","commit":true,"prefer_legacy":true"#)),
    );
    h.wait_idle();

    let calls = up.calls();
    assert_eq!(
        calls.iter().map(|c| c.dry_run).collect::<Vec<_>>(),
        vec![true, false, true],
        "dry run, commit, count check"
    );
    assert!(calls.iter().all(|c| c.prefer_legacy && c.zip_existed));
    let zips = up.zips.lock().unwrap();
    assert!(
        zips.windows(2).all(|w| w[0] == w[1]),
        "the same bundle every time"
    );
    drop(zips);
    assert!(work_is_empty(&fx.work));
    let last = fx.events.events().into_iter().last().unwrap();
    assert_eq!(last.code, "LEGACY_EXPORT_UPLOADED");
    assert_eq!(last.detail.get("outcome"), Some(&Json::from("committed")));
    assert!(last.detail.get("post_commit_dry_run").is_some());
}

#[test]
fn nothing_is_committed_when_the_dry_run_is_not_clean() {
    let fx = Fx::new();
    let up = FakeUploader::new(Mode::RefuseDry("ROWS_DIFFER"));
    let h = fx.handler(FakeSql::ok(), up.clone(), VietQrRead::NotSet);
    exec(&h, &command(payload(r#","commit":true"#)));
    h.wait_idle();

    assert_eq!(up.calls().len(), 1, "no commit after a refused dry run");
    assert!(work_is_empty(&fx.work));
    let failed = fx
        .events
        .events()
        .into_iter()
        .find(|e| e.code == "LEGACY_EXPORT_FAILED")
        .unwrap();
    assert_eq!(failed.source, "legacy-upload");
    assert_eq!(failed.detail.get("stage"), Some(&Json::from("dry_run")));
    assert_eq!(
        failed.detail.get("error_code"),
        Some(&Json::from("ROWS_DIFFER"))
    );
    assert!(failed.detail.get("run_id").is_some());
    assert!(
        !all_event_text(&fx.events).contains(GUEST),
        "the cloud's free text is never forwarded"
    );
    assert!(
        !fx.events
            .codes()
            .contains(&"LEGACY_EXPORT_UPLOADED".to_owned())
    );
}

#[test]
fn a_count_check_gap_after_the_commit_is_a_failure_and_the_files_are_still_deleted() {
    let fx = Fx::new();
    let up = FakeUploader::new(Mode::GapAfterCommit);
    let h = fx.handler(FakeSql::ok(), up.clone(), VietQrRead::NotSet);
    exec(&h, &command(payload(r#","commit":true"#)));
    h.wait_idle();

    assert_eq!(up.calls().len(), 3);
    assert!(work_is_empty(&fx.work));
    let failed = fx
        .events
        .events()
        .into_iter()
        .find(|e| e.code == "LEGACY_EXPORT_FAILED")
        .unwrap();
    assert_eq!(
        failed.detail.get("stage"),
        Some(&Json::from("post_commit_dry_run"))
    );
    assert_eq!(
        failed.detail.get("error_code"),
        Some(&Json::from("COUNT_CHECK_FAILED"))
    );
    assert_eq!(failed.detail.get("table"), Some(&Json::from("pda_config")));
}

#[test]
fn an_export_failure_reports_stage_and_code_never_row_data_and_cleans_up() {
    let fx = Fx::new();
    let up = FakeUploader::new(Mode::Clean);
    let h = fx.handler(
        FakeSql::dies_after_precheck(),
        up.clone(),
        VietQrRead::NotSet,
    );
    exec(&h, &command(payload("")));
    h.wait_idle();

    assert!(
        up.calls().is_empty(),
        "nothing is uploaded after a failed export"
    );
    assert!(
        work_is_empty(&fx.work),
        "the half-written bundle is deleted"
    );
    let failed = fx
        .events
        .events()
        .into_iter()
        .find(|e| e.code == "LEGACY_EXPORT_FAILED")
        .unwrap();
    assert_eq!(failed.source, "legacy-export");
    assert_eq!(failed.level, "error");
    assert_eq!(failed.detail.get("stage"), Some(&Json::from("export")));
    assert_eq!(
        failed.detail.get("error_code"),
        Some(&Json::from("SQL_UNREACHABLE"))
    );
    assert_eq!(failed.detail.get("sql_number"), Some(&Json::from(53)));
    let text = all_event_text(&fx.events);
    for forbidden in [SECRET, GUEST, "cafe", "VND"] {
        assert!(!text.contains(forbidden), "{forbidden} leaked into {text}");
    }
}

#[test]
fn a_stale_bundle_of_an_earlier_process_is_removed_when_the_next_run_starts() {
    let fx = Fx::new();
    let stale = fx.work.join("old-run").join("bundle");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("manifest.json"), b"guest data").unwrap();
    let h = fx.handler(
        FakeSql::ok(),
        FakeUploader::new(Mode::Clean),
        VietQrRead::NotSet,
    );
    exec(&h, &command(payload("")));
    h.wait_idle();
    assert!(work_is_empty(&fx.work));
}

#[test]
fn vietqr_only_exports_the_one_registry_table_without_sql_or_a_gate() {
    let fx = Fx::new();
    let up = FakeUploader::new(Mode::Clean);
    let vq = VietQrRead::Read(vec![
        ("BankBin".to_owned(), "970436".to_owned()),
        ("AccountNo".to_owned(), "0123456789".to_owned()),
    ]);
    let h = fx.handler(FakeSql::dies_after_precheck(), up.clone(), vq);
    let only = stj::parse(
        r#"{"source":"auto","allow_live":false,"commit":false,"prefer_legacy":false,"viet_qr_only":true,"reason":"vietqr"}"#,
    )
    .unwrap();
    assert_eq!(exec(&h, &command(only)).status, CommandStatus::Applied);
    h.wait_idle();
    let zips = up.zips.lock().unwrap();
    assert_eq!(
        zip_entries(&zips[0]).len(),
        2,
        "manifest and registry.vietqr only"
    );
    assert!(zip_entries(&zips[0]).contains(&"tables/registry.vietqr.jsonl.gz".to_owned()));
    drop(zips);

    // Without an account there is nothing to move: a specific rejection.
    let fx2 = Fx::new();
    let h2 = fx2.handler(
        FakeSql::ok(),
        FakeUploader::new(Mode::Clean),
        VietQrRead::NotSet,
    );
    let only = stj::parse(r#"{"source":"auto","viet_qr_only":true,"reason":"vietqr"}"#).unwrap();
    let r = exec(&h2, &command(only));
    assert!(
        r.reason
            .as_deref()
            .unwrap()
            .starts_with("legacy_vietqr_not_set: "),
        "{:?}",
        r.reason
    );
}
