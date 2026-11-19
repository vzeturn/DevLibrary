use dl_agent::selftest::Status;
use dl_json::json;

use super::*;
use crate::fakes::{DestState, FakeDest, FakeSql, SqlState};
use crate::hub::DatabaseConfig;

fn temp(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!("bk-st-{tag}-{}-{nanos}", std::process::id()))
}

fn remote(servers: Vec<ServerConfig>) -> Remote {
    Remote {
        config: ConfigResponse {
            servers,
            ..ConfigResponse::default()
        },
        key: None,
    }
}

fn server(dbs: &[&str]) -> ServerConfig {
    ServerConfig {
        id: "s1".into(),
        host: r".\SQLEXPRESS".into(),
        databases: dbs
            .iter()
            .map(|d| DatabaseConfig {
                database_name: (*d).into(),
                ..DatabaseConfig::default()
            })
            .collect(),
        ..ServerConfig::default()
    }
}

#[test]
fn a_module_without_enrollment_is_not_enrolled() {
    let dir = temp("none").join("bench").join("backup");
    let ctx = ModuleContext::standalone(NAME, json!({}), dir.clone());
    let checks = run_with(
        &ctx,
        Arc::new(FakeSql::default()),
        Arc::new(FakeDest::default()),
    );
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, Status::Skip);
    assert!(checks[0].detail.contains("bench"), "{}", checks[0].detail);
    assert!(!dir.exists(), "the self-test created nothing");
}

#[test]
fn express_backs_up_without_compression() {
    let sql = FakeSql::new(SqlState {
        info: crate::sql::ServerInfo {
            engine_edition: 4,
            edition: "Express Edition (64-bit)".into(),
            major: 15,
            minor: 0,
            transport: "as-configured".into(),
        },
        ..SqlState::default()
    });
    let (perm, comp) = probe_server(&sql, &remote(vec![]), &server(&["SMILE_FO"]), r"D:\Bak");
    assert_eq!(perm.status, Status::Pass, "{}", perm.detail);
    assert!(perm.detail.contains(r"D:\Bak"));
    let comp = comp.unwrap();
    assert_eq!(comp.status, Status::Pass);
    assert!(comp.detail.contains("no COMPRESSION"), "{}", comp.detail);
}

#[test]
fn a_flagged_password_or_a_dead_server_fails() {
    let mut s = server(&["SMILE_FO"]);
    s.credential_error = true;
    let (perm, comp) = probe_server(&FakeSql::default(), &remote(vec![]), &s, "");
    assert_eq!(perm.status, Status::Fail);
    assert!(comp.is_none());

    let sql = FakeSql::new(SqlState {
        connect_error: Some(Error::Sql("connect to .: refused".into())),
        ..SqlState::default()
    });
    let (perm, _) = probe_server(&sql, &remote(vec![]), &server(&[]), "");
    assert_eq!(perm.status, Status::Fail);
    assert!(perm.remedy.is_some());
}

#[test]
fn a_destination_round_trip_cleans_up() {
    let dest = FakeDest(Arc::new(std::sync::Mutex::new(DestState {
        content: PROBE_BYTES.to_vec(),
        ..DestState::default()
    })));
    let tmp = temp("rt");
    std::fs::create_dir_all(&tmp).unwrap();
    let up = dest
        .open(&Destination::from_plain("LocalFolder", "{}", None).unwrap())
        .unwrap();
    let o = round_trip(up.as_ref(), &tmp, "t1");
    let left = std::fs::read_dir(&tmp).unwrap().count();
    std::fs::remove_dir_all(&tmp).unwrap();
    assert_eq!(o.status, Status::Pass, "{}", o.detail);
    assert_eq!(left, 0, "the local probe folder is removed");
    let s = dest.state();
    assert_eq!(s.uploaded.len(), 1);
    assert_eq!(s.deleted.len(), 1, "the probe object is deleted");
}

#[test]
fn a_destination_that_cannot_delete_fails() {
    let dest = FakeDest(Arc::new(std::sync::Mutex::new(DestState {
        content: PROBE_BYTES.to_vec(),
        delete_error: Some(Error::Upload("403".into())),
        ..DestState::default()
    })));
    let up = dest
        .open(&Destination::from_plain("AWSS3", "{}", None).unwrap())
        .unwrap();
    let o = round_trip(
        up.as_ref(),
        &std::env::temp_dir(),
        &format!("del-{}", std::process::id()),
    );
    assert_eq!(o.status, Status::Fail);
    assert!(
        o.detail.contains("stays at the destination"),
        "{}",
        o.detail
    );
}

#[test]
fn schedules_without_a_destination_skip() {
    let r = Arc::new(remote(vec![]));
    let up: Arc<dyn Uploaders> = Arc::new(FakeDest::default());
    let checks = destinations(&up, &r);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, Status::Skip);
}
