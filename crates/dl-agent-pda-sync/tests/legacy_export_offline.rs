//! `legacy.export` with `take_offline` and `keep_tenant` (RT-0390): the agent puts the legacy
//! site offline itself, removes its `app_offline.htm` when nothing was committed, keeps it
//! after a commit, and passes `keepTenant` on every upload.

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, LegacyImportAck, LegacyImportTableAck, SyncCommand, SyncCommandResult,
};
use dl_agent_pda_sync::legacy_export::events::{LegacyEvents, MemoryEvents};
use dl_agent_pda_sync::legacy_export::offline::{LegacySite, OFFLINE_PAGE};
use dl_agent_pda_sync::legacy_export::sources::{
    AppPoolProbe, BrandingSource, ExportError, LegacySqlSource, VietQrRead, VietQrSource,
};
use dl_agent_pda_sync::legacy_export::upload::{
    BundleUploader, BundleZip, TransportFailure, UploadAnswer, UploadQuery,
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

const OFFLINE: &str = "app_offline.htm";

struct Sql;
impl LegacySqlSource for Sql {
    fn database(&self) -> String {
        "SMILE_POS".to_owned()
    }
    fn rvc_list(&self) -> Result<Vec<(i32, String)>, ExportError> {
        Ok(vec![(1, "Restaurant".to_owned())])
    }
    fn pda_tables(&self) -> Result<Vec<(String, i64)>, ExportError> {
        Ok(Vec::new())
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

/// The app pool is running: only `app_offline.htm` opens the gate.
struct Started;
impl AppPoolProbe for Started {
    fn state(&self, _p: &str) -> Result<Option<String>, String> {
        Ok(Some("Started".to_owned()))
    }
}

struct NoBrand;
impl BrandingSource for NoBrand {
    fn read(&self) -> Vec<(String, String)> {
        Vec::new()
    }
}

struct NoVietQr;
impl VietQrSource for NoVietQr {
    fn read(&self) -> VietQrRead {
        VietQrRead::NotSet
    }
}

struct Site {
    paths: Vec<String>,
    settled: AtomicU32,
}
impl LegacySite for Site {
    fn physical_paths(&self) -> Result<Vec<String>, String> {
        Ok(self.paths.clone())
    }
    fn settle(&self) {
        self.settled.fetch_add(1, Ordering::SeqCst);
    }
}

/// Records each upload's query, whether the site was offline then, and the manifest.
struct Uploader {
    refuse_dry: bool,
    root: PathBuf,
    calls: Mutex<Vec<(bool, UploadQuery, bool)>>,
    manifest: Mutex<Option<String>>,
}

impl BundleUploader for Uploader {
    fn send(
        &self,
        zip: &BundleZip,
        dry_run: bool,
        query: UploadQuery,
        _ctx: &Ctx,
    ) -> Result<UploadAnswer, TransportFailure> {
        let offline = self.root.join(OFFLINE).is_file();
        self.calls.lock().unwrap().push((dry_run, query, offline));
        let bytes = std::fs::read(&zip.path).unwrap();
        let manifest = dl_zip::ZipArchive::new(std::io::Cursor::new(bytes))
            .unwrap()
            .read_by_name("manifest.json", 1 << 20)
            .unwrap();
        *self.manifest.lock().unwrap() = Some(String::from_utf8(manifest).unwrap());
        let mut ack = LegacyImportAck {
            slug: "demo".to_owned(),
            dry_run,
            committed: !dry_run,
            manifest_sha256: zip.bundle.manifest_sha256.clone(),
            tables: vec![LegacyImportTableAck {
                source: "pda_config".to_owned(),
                target: "pda_config".to_owned(),
                source_rows: 1,
                same: 1,
                ..LegacyImportTableAck::default()
            }],
            ..LegacyImportAck::default()
        };
        if dry_run && self.refuse_dry {
            ack.refusal_code = Some("TENANT_ACTIVE".to_owned());
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

struct Fx {
    env: TestEnv,
    pda_db: PathBuf,
    root: PathBuf,
    events: Arc<MemoryEvents>,
}

impl Fx {
    fn new() -> Fx {
        let env = TestEnv::new();
        let pda_db = env.dir.join("MM-PDA").join("pda.db");
        std::fs::create_dir_all(pda_db.parent().unwrap()).unwrap();
        let db = Connection::open(&pda_db, OpenFlags::default()).unwrap();
        db.execute_batch(
            "CREATE TABLE schema_version (version INTEGER);
             INSERT INTO schema_version VALUES (3);
             CREATE TABLE pda_config (rvc_no INTEGER, key TEXT, value TEXT);
             INSERT INTO pda_config VALUES (1, 'currency', 'VND');",
        )
        .unwrap();
        drop(db);
        // The legacy web app: web.config plus its assembly beside it.
        let root = env.dir.join("inetpub").join("pda");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("web.config"), b"<configuration/>").unwrap();
        std::fs::write(root.join("MM.PdaOrder.Api.dll"), b"MZ").unwrap();
        Fx {
            env,
            pda_db,
            root,
            events: Arc::new(MemoryEvents::default()),
        }
    }

    fn run(
        &self,
        refuse_dry: bool,
        pda_db: &Path,
        extra: &str,
    ) -> (SyncCommandResult, Arc<Uploader>) {
        let up = Arc::new(Uploader {
            refuse_dry,
            root: self.root.clone(),
            calls: Mutex::new(Vec::new()),
            manifest: Mutex::new(None),
        });
        let events = self.events.clone();
        let env = LegacyExportEnv {
            work_dir: self.env.dir.join("legacy-export"),
            pda_db: pda_db.to_path_buf(),
            sql: Arc::new(|_p| Ok(Arc::new(Sql) as Arc<dyn LegacySqlSource>)),
            pools: Arc::new(Started),
            site: Arc::new(Site {
                paths: vec![
                    self.env.dir.join("elsewhere").display().to_string(),
                    self.root.display().to_string(),
                ],
                settled: AtomicU32::new(0),
            }),
            branding: Arc::new(NoBrand),
            vietqr: Arc::new(NoVietQr),
            uploader: up.clone(),
            events: Arc::new(move |_run| events.clone() as Arc<dyn LegacyEvents>),
            clock: self.env.time.clone(),
            zone: Arc::new(dl_agent_pda_sync::contracts::TimeZone::utc()),
            exporter: "PdaSyncAgent test".to_owned(),
            max_bytes: 95 * 1024 * 1024,
            app_version: Arc::new(|_| None),
        };
        let h = LegacyExportCommandHandler::new(env, &self.env.log);
        let payload = stj::parse(&format!(
            r#"{{"source":"pda-db","app_pool":"PdaApp","allow_live":false,"prefer_legacy":false,"viet_qr_only":false,"reason":"cutover","take_offline":true{extra}}}"#
        ))
        .unwrap();
        let r = h.execute(&command(payload), &Ctx::background()).unwrap();
        h.wait_idle();
        (r, up)
    }

    fn codes(&self) -> Vec<String> {
        self.events.codes()
    }
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

#[test]
fn a_commit_keeps_the_site_offline_and_every_upload_carries_keep_tenant() {
    let fx = Fx::new();
    let (r, up) = fx.run(false, &fx.pda_db, r#","commit":true,"keep_tenant":true"#);
    assert_eq!(r.status, CommandStatus::Applied, "{:?}", r.reason);
    let calls = up.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 3, "dry run, commit, post-commit dry run");
    assert!(
        calls
            .iter()
            .all(|(_, q, offline)| q.keep_tenant && *offline),
        "{calls:?}"
    );
    assert_eq!(
        std::fs::read_to_string(fx.root.join(OFFLINE)).unwrap(),
        OFFLINE_PAGE
    );
    let manifest = up.manifest.lock().unwrap().clone().unwrap();
    assert!(
        manifest.contains("app_offline.htm placed by the agent in"),
        "{manifest}"
    );
    let codes = fx.codes();
    assert!(
        codes.contains(&"LEGACY_SITE_OFFLINE_PLACED".to_owned()),
        "{codes:?}"
    );
    assert!(
        codes.contains(&"LEGACY_SITE_OFFLINE_KEPT".to_owned()),
        "{codes:?}"
    );
    assert!(
        codes.contains(&"LEGACY_EXPORT_UPLOADED".to_owned()),
        "{codes:?}"
    );
}

#[test]
fn without_keep_tenant_the_query_leaves_it_out_and_a_dry_run_brings_the_site_back() {
    let fx = Fx::new();
    let (r, up) = fx.run(false, &fx.pda_db, r#","commit":false"#);
    assert_eq!(r.status, CommandStatus::Applied, "{:?}", r.reason);
    let calls = up.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert!(!calls[0].1.keep_tenant && calls[0].2);
    assert!(
        !fx.root.join(OFFLINE).exists(),
        "no commit: the site is back online"
    );
    assert!(
        fx.codes()
            .contains(&"LEGACY_SITE_OFFLINE_REMOVED".to_owned())
    );
}

#[test]
fn a_refused_dry_run_removes_the_agents_file() {
    let fx = Fx::new();
    let (r, _) = fx.run(true, &fx.pda_db, r#","commit":true"#);
    assert_eq!(
        r.status,
        CommandStatus::Applied,
        "accepted, then refused by the cloud"
    );
    assert!(!fx.root.join(OFFLINE).exists());
    let codes = fx.codes();
    assert!(
        codes.contains(&"LEGACY_EXPORT_FAILED".to_owned()),
        "{codes:?}"
    );
    assert!(
        codes.contains(&"LEGACY_SITE_OFFLINE_REMOVED".to_owned()),
        "{codes:?}"
    );
}

#[test]
fn a_precheck_refusal_after_placing_removes_the_file_again() {
    let fx = Fx::new();
    // No pda.db: the store gate refuses after the site was put offline.
    let missing = fx.env.dir.join("nowhere").join("pda.db");
    let (r, up) = fx.run(false, &missing, r#","commit":true"#);
    assert_eq!(r.status, CommandStatus::Rejected);
    assert!(
        r.reason
            .as_deref()
            .unwrap()
            .starts_with("legacy_store_missing: "),
        "{:?}",
        r.reason
    );
    assert!(up.calls.lock().unwrap().is_empty());
    assert!(!fx.root.join(OFFLINE).exists());
    assert_eq!(
        fx.codes(),
        vec![
            "LEGACY_SITE_OFFLINE_PLACED".to_owned(),
            "LEGACY_SITE_OFFLINE_REMOVED".to_owned(),
            "LEGACY_EXPORT_FAILED".to_owned(),
        ]
    );
}

#[test]
fn an_existing_app_offline_is_left_alone_even_without_a_commit() {
    let fx = Fx::new();
    std::fs::write(fx.root.join(OFFLINE), b"operator's notice").unwrap();
    let (r, _) = fx.run(false, &fx.pda_db, r#","commit":false"#);
    assert_eq!(r.status, CommandStatus::Applied, "{:?}", r.reason);
    assert_eq!(
        std::fs::read(fx.root.join(OFFLINE)).unwrap(),
        b"operator's notice"
    );
    let codes = fx.codes();
    assert!(
        !codes.contains(&"LEGACY_SITE_OFFLINE_REMOVED".to_owned()),
        "{codes:?}"
    );
}

#[test]
fn no_legacy_app_found_is_a_specific_rejection() {
    let fx = Fx::new();
    std::fs::remove_file(fx.root.join("MM.PdaOrder.Api.dll")).unwrap();
    let (r, _) = fx.run(false, &fx.pda_db, r#","commit":true"#);
    assert_eq!(r.status, CommandStatus::Rejected);
    let reason = r.reason.unwrap();
    assert!(
        reason.starts_with("legacy_site_unresolved: take_offline: found 0 IIS applications"),
        "{reason}"
    );
    assert!(!fx.root.join(OFFLINE).exists());
}
