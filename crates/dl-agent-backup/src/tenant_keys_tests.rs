//! Per-tenant backup data keys through the whole module: the sync loop and executor against
//! the fake hub, which (like the real hub must) hands an agent only its own tenant's keys.

use std::path::Path;

use dl_encoding::base64::URL_SAFE_NO_PAD;
use dl_json::{Value, json};
use dl_tds::CancelToken;
use dl_winsvc::StopSignal;

use crate::Error;
use crate::backup::Steps;
use crate::backup::restore::{RecordIo, RestoreParams, Source, restore_from_record_with};
use crate::core::lock;
use crate::crypto::envelope::{self, BackupKeys, DataKey, MAGIC};
use crate::executor::{Gate, execute};
use crate::fakes::{FakeDest, FakeSql, SqlState};
use crate::sql::Target;
use crate::state::history::BackupRecord;
use crate::state::now_ms;
use crate::testhub::{Rig, default_config, put, seal, steps, text};
use crate::upload::Destination;

const KEY_A: [u8; 32] = [0xa1; 32];
const KEY_B: [u8; 32] = [0xb2; 32];

fn b64(k: &[u8; 32]) -> String {
    URL_SAFE_NO_PAD.encode(k)
}

/// A rig whose hub holds a key for tenant A (the agent's) and one for tenant B.
fn rig(tag: &str, cfg: Value) -> Rig {
    let rig = Rig::new(tag, cfg);
    rig.hub.set(|s| {
        s.backup_keys = vec![
            ("tenant-a".into(), "tenant-a:1".into(), b64(&KEY_A)),
            ("tenant-b".into(), "tenant-b:1".into(), b64(&KEY_B)),
        ];
    });
    rig
}

fn of_type(reports: &[Value], kind: &str) -> Vec<Value> {
    reports
        .iter()
        .filter(|r| text(r, "type") == kind)
        .cloned()
        .collect()
}

/// `plain` sealed under (`id`, `key`), as another agent of that tenant would upload it.
fn sealed_bytes(dir: &Path, plain: &[u8], id: &str, key: [u8; 32]) -> Vec<u8> {
    let (src, dst) = (dir.join("seal-in.bak"), dir.join("seal-out.dlbk"));
    std::fs::write(&src, plain).unwrap();
    let k = DataKey { id: id.into(), key };
    envelope::seal_file(&src, &dst, &k, &CancelToken::new()).unwrap();
    let bytes = std::fs::read(&dst).unwrap();
    std::fs::remove_file(&src).unwrap();
    std::fs::remove_file(&dst).unwrap();
    bytes
}

fn files_in(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn a_backup_is_sealed_under_the_tenants_key_and_the_report_names_it() {
    let rig = rig("keys-backup", default_config());
    let mut sync = rig.sync();
    sync.poll(now_ms()).unwrap();
    assert_eq!(rig.hub.count("/backup-key"), 1, "fetched with the config");
    let job = rig.next_job();
    execute(&rig.core, &Gate::default(), &StopSignal::new(), &job);
    sync.poll(now_ms()).unwrap();

    let report = &of_type(&rig.hub.reports(), "backup")[0];
    assert_eq!(text(report, "status"), "Succeeded");
    assert_eq!(text(report, "encryptionKeyId"), "tenant-a:1");
    assert!(text(report, "cloudFilePath").ends_with(".bak.dlbk"));
    let encrypt = steps(report)
        .into_iter()
        .find(|s| s.0 == "encrypt")
        .unwrap();
    assert_eq!(encrypt.1, "done");
    assert!(encrypt.2.contains("tenant-a:1"), "{}", encrypt.2);

    let uploaded = rig.dest.state().uploaded_content.clone();
    assert_eq!(&uploaded[..8], MAGIC);
    assert!(
        !uploaded
            .windows(b"fake backup".len())
            .any(|w| w == b"fake backup"),
        "the destination never sees the plaintext"
    );
    // Opened with tenant A's key it is the backup SQL Server wrote.
    let f = rig.dir.join("check.dlbk");
    std::fs::write(&f, &uploaded).unwrap();
    let out = rig.dir.join("check.bak");
    envelope::open_file(&f, &out, rig.core.as_ref(), &CancelToken::new()).unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), b"fake backup bytes");
    // Neither the `.bak` nor its sealed staging copy is left on the server.
    assert!(files_in(&rig.dir.join("bak")).is_empty());
}

#[test]
fn an_older_hub_without_keys_gets_unencrypted_backups_as_before() {
    let rig = Rig::new("keys-none", default_config());
    let mut sync = rig.sync();
    sync.poll(now_ms()).unwrap();
    let job = rig.next_job();
    execute(&rig.core, &Gate::default(), &StopSignal::new(), &job);
    sync.poll(now_ms()).unwrap();
    let report = &of_type(&rig.hub.reports(), "backup")[0];
    assert_eq!(text(report, "status"), "Succeeded");
    assert!(report.get("encryptionKeyId").is_none());
    assert_eq!(rig.dest.state().uploaded_content, b"fake backup bytes");
}

#[test]
fn the_hub_never_gives_this_agent_another_tenants_key() {
    let rig = rig("keys-hub", default_config());
    let hub = rig.hub.client();
    let own = hub.backup_key().unwrap().unwrap();
    assert_eq!((own.id.as_str(), own.key), ("tenant-a:1", KEY_A));
    assert_eq!(hub.backup_key_by_id("tenant-a:1").unwrap().key, KEY_A);
    let err = hub.backup_key_by_id("tenant-b:1").unwrap_err();
    // Not an agent rejection (that would degrade the module), a credential problem.
    assert!(matches!(err, Error::Credential(_)), "{err}");
    assert!(err.to_string().contains("another tenant"), "{err}");
    assert!(hub.backup_key_by_id("bad id").is_err());
    assert_eq!(
        rig.hub.count("/backup-key"),
        3,
        "a bad id is not even asked"
    );
}

#[test]
fn a_cached_key_carries_backups_through_a_hub_outage() {
    let rig = rig("keys-outage", default_config());
    assert_eq!(rig.core.current().unwrap().unwrap().id, "tenant-a:1");
    rig.hub.set(|s| s.status = 503);
    assert_eq!(rig.core.current().unwrap().unwrap().id, "tenant-a:1");
    assert_eq!(rig.core.by_id("tenant-a:1").unwrap().key, KEY_A);
    // Nothing cached and the hub down: the backup cannot be sealed and fails (retried).
    let cold = Rig::new("keys-cold", default_config());
    cold.hub.set(|s| s.status = 503);
    assert!(matches!(cold.core.current(), Err(Error::Hub(_))));
}

/// A restore request for history `h1`, kept only at the destination.
fn restore_rig(tag: &str) -> Rig {
    let mut cfg = default_config();
    put(
        &mut cfg,
        "/restoreRequests",
        json!([{"id": "r1", "backupHistoryId": "h1", "targetServerHost": "sql1",
            "targetDatabase": "Sales_copy", "targetPasswordEncrypted": seal("pw-restore"),
            "credentialError": false}]),
    );
    let rig = rig(tag, cfg);
    lock(&rig.core.history)
        .record(BackupRecord {
            schedule_id: "sc1".into(),
            hub_history_id: "h1".into(),
            database_name: "Sales".into(),
            local_file_path: String::new(),
            cloud_file_path: "fake://s3/h1.bak.dlbk".into(),
            provider: "s3".into(),
            created_at: now_ms(),
        })
        .unwrap();
    rig
}

fn restores(rig: &Rig) -> Vec<String> {
    rig.sql
        .state()
        .statements
        .iter()
        .filter(|(q, _)| q.starts_with("RESTORE DATABASE"))
        .map(|(_, args)| args[1].1.clone())
        .collect()
}

#[test]
fn tenant_a_cannot_restore_a_backup_sealed_for_tenant_b() {
    let rig = restore_rig("keys-foreign");
    let foreign = sealed_bytes(&rig.dir, b"tenant B database", "tenant-b:1", KEY_B);
    rig.dest.state().content = foreign;
    rig.sync().poll(now_ms()).unwrap();

    let r = &of_type(&rig.hub.reports(), "restore")[0];
    assert_eq!(text(r, "status"), "Failed");
    assert!(
        text(r, "error").contains("tenant-b:1"),
        "{}",
        text(r, "error")
    );
    assert!(text(r, "error").contains("another tenant"));
    assert!(restores(&rig).is_empty(), "SQL Server is never asked");
    assert!(
        files_in(&rig.dir.join("bak")).is_empty(),
        "neither the download nor any plaintext is left"
    );
    assert!(rig.codes().contains(&"backup_credential_error".to_owned()));
}

#[test]
fn a_restore_opens_its_own_tenants_backup() {
    let rig = restore_rig("keys-own");
    let own = sealed_bytes(&rig.dir, b"tenant A database", "tenant-a:1", KEY_A);
    rig.dest.state().content = own;
    rig.sync().poll(now_ms()).unwrap();

    let r = &of_type(&rig.hub.reports(), "restore")[0];
    assert_eq!(text(r, "status"), "Succeeded", "{}", text(r, "error"));
    let paths = restores(&rig);
    assert_eq!(paths.len(), 1);
    assert!(paths[0].ends_with("restore_r1_plain.bak"), "{}", paths[0]);
    assert!(files_in(&rig.dir.join("bak")).is_empty());
}

#[test]
fn a_backup_from_before_encryption_still_restores() {
    let rig = restore_rig("keys-legacy");
    rig.dest.state().content = b"TAPE plaintext legacy backup".to_vec();
    rig.sync().poll(now_ms()).unwrap();
    let r = &of_type(&rig.hub.reports(), "restore")[0];
    assert_eq!(text(r, "status"), "Succeeded", "{}", text(r, "error"));
    let paths = restores(&rig);
    assert!(paths[0].ends_with("restore_r1.bak"), "{}", paths[0]);
}

#[test]
fn restore_steps_name_the_decrypt_outcome() {
    let dir = crate::testutil::temp_dir("keys-steps");
    let (sql, dest) = (FakeSql::new(SqlState::default()), FakeDest::default());
    dest.state().content = sealed_bytes(&dir, b"db", "tenant-a:1", KEY_A);
    let src = Source {
        local_file_path: String::new(),
        cloud_file_path: "fake://s3/x.bak.dlbk".into(),
        tag: "t1".into(),
        destination: Some(Destination {
            provider: "s3".into(),
            tokens: json!({}),
        }),
    };
    let p = RestoreParams {
        target: Target {
            host: "localhost".into(),
            port: 1433,
            auth_type: "sql".into(),
            username: "sa".into(),
            password: "pw".into(),
        },
        target_database: "Sales_copy".into(),
        with_replace: true,
        backup_file: String::new(),
    };
    let backup_dir = dir.display().to_string();
    let rig = rig("keys-steps-hub", default_config());
    let io = RecordIo {
        uploaders: &dest,
        keys: rig.core.as_ref(),
        backup_dir: &backup_dir,
    };
    let mut steps = Steps::new();
    restore_from_record_with(&sql, &io, &src, p, &CancelToken::new(), &mut steps).unwrap();
    let d = steps.get("decrypt").unwrap();
    assert_eq!(d.status, "done");
    assert!(d.detail.contains("tenant-a:1"));
}

/// Keys the hub cannot hand out right now.
struct Down;

impl BackupKeys for Down {
    fn current(&self) -> Result<Option<DataKey>, Error> {
        Err(Error::Hub("/backup-key: hub answered HTTP 503".into()))
    }
    fn by_id(&self, _id: &str) -> Result<DataKey, Error> {
        Err(Error::Hub("/backup-key: hub answered HTTP 503".into()))
    }
}

#[test]
fn a_backup_whose_key_cannot_be_fetched_fails_before_anything_is_uploaded() {
    let dir = crate::testutil::temp_dir("keys-down");
    let (sql, dest) = (FakeSql::new(SqlState::default()), FakeDest::default());
    let p = crate::backup::Params {
        history_id: "h1".into(),
        schedule_id: "sc1".into(),
        database_name: "Sales".into(),
        backup_type: crate::backup::BackupType::Full,
        target: Target {
            host: "localhost".into(),
            port: 1433,
            auth_type: "sql".into(),
            username: "sa".into(),
            password: "pw".into(),
        },
        destination: Some(Destination {
            provider: "s3".into(),
            tokens: json!({}),
        }),
        backup_dir: dir.display().to_string(),
    };
    let mut steps = Steps::new();
    let err =
        crate::backup::pipeline::run_with(&sql, &dest, &Down, &p, &CancelToken::new(), &mut steps)
            .unwrap_err();
    assert!(matches!(err, Error::Hub(_)), "{err}");
    assert_eq!(steps.get("encrypt").unwrap().status, "error");
    assert!(steps.get("upload").is_none());
    assert!(dest.state().uploaded.is_empty());
}
