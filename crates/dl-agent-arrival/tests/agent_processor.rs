//! `RequestProcessor` (src/agent/processor.rs): the six `AgentRequestProcessorTests` facts of
//! the .NET agent, plus the failure answers and lane routing.

mod common;

use std::path::Path;
use std::sync::{Arc, PoisonError};

use common::{TempDir, folios, new_tenant_key};
use dl_agent_arrival::agent::clock::SystemClock;
use dl_agent_arrival::agent::executor::{ExecError, FakeExecutor};
use dl_agent_arrival::agent::health::{self, Health, RecordingSink};
use dl_agent_arrival::agent::journal::Journal;
use dl_agent_arrival::agent::processor::{Lane, Processor};
use dl_agent_arrival::agent::secrets::TenantKeyStore;
use dl_agent_arrival::agent::vault::LegalVault;
use dl_agent_arrival::contracts::errors;
use dl_agent_arrival::contracts::values::to_base64;
use dl_agent_arrival::contracts::{Params, PmsRequest, PmsResponse, Status, key_vault, vault_ops};

struct Rig {
    processor: Processor,
    pms: Arc<FakeExecutor>,
    sink: Arc<RecordingSink>,
}

/// `Make()`: a processor over a fresh tenant key and vault, journaling into `dir`.
fn make(dir: &Path) -> Rig {
    let pms = FakeExecutor::new(vec![Ok(vec![folios(&[(1, "A"), (2, "B")])])]);
    make_with(dir, Arc::clone(&pms))
}

fn make_with(dir: &Path, pms: Arc<FakeExecutor>) -> Rig {
    let sink = Arc::new(RecordingSink::default());
    let health = Arc::new(Health::new(sink.clone(), health::REPORT_INTERVAL));
    let journal =
        Arc::new(Journal::open(&dir.join("idem.jsonl"), Arc::new(SystemClock)).expect("journal"));
    let keys = Arc::new(TenantKeyStore::new(
        &new_tenant_key(),
        dir.join("agent-tenant-key.bin"),
    ));
    let vault = LegalVault::new(
        dir.join(format!("vault-{:016x}", dl_rand::u64())),
        Arc::clone(&keys),
    );
    let processor = Processor::new(pms.clone(), 10, journal, keys, vault, health, 5);
    Rig {
        processor,
        pms,
        sink,
    }
}

fn request(id: &str, operation: &str, params: &[(&str, Option<&str>)]) -> PmsRequest {
    let mut parameters = Params::new();
    for (k, v) in params {
        parameters.insert(*k, v.map(str::to_string));
    }
    PmsRequest {
        request_id: id.into(),
        operation: operation.into(),
        parameters,
        base_etag: None,
        idempotency_key: None,
        projection: None,
    }
}

fn read(base_etag: Option<&str>) -> PmsRequest {
    let mut r = request(
        &format!("{:016x}", dl_rand::u64()),
        "booking.list-checkin",
        &[],
    );
    r.base_etag = base_etag.map(str::to_string);
    r
}

fn value(r: &PmsResponse, column: &str) -> String {
    r.result_sets[0].rows[0]
        .get(column)
        .and_then(|v| v.as_str())
        .expect("value column")
        .to_string()
}

#[test]
fn answers_not_modified_when_the_cloud_already_has_the_data() {
    let dir = TempDir::new("proc");
    let rig = make(dir.path());
    let first = rig.processor.handle(&read(None));
    let second = rig.processor.handle(&read(first.etag.as_deref()));

    assert_eq!(first.status, Status::Ok);
    assert_eq!(second.status, Status::NotModified);
    assert!(second.result_sets.is_empty());
}

#[test]
fn answers_with_only_changed_rows_when_the_cloud_copy_is_known() {
    let dir = TempDir::new("proc");
    let rig = make(dir.path());
    let first = rig.processor.handle(&read(None));
    *rig.pms
        .answers
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = vec![Ok(vec![folios(&[(1, "A"), (2, "B*")])])];

    let second = rig.processor.handle(&read(first.etag.as_deref()));

    assert_eq!(second.status, Status::Delta);
    assert_eq!(second.result_sets[0].rows.len(), 1);
}

#[test]
fn rejects_requests_outside_the_catalog_without_touching_the_pms() {
    let dir = TempDir::new("proc");
    let rig = make(dir.path());
    let req = request("r", "xp_cmdshell", &[]);
    assert_eq!(rig.processor.route(&req), Lane::Fast);
    let r = rig.processor.handle(&req);

    assert_eq!(r.status, Status::Rejected);
    assert_eq!(rig.pms.call_count(), 0);
    assert_eq!(rig.sink.codes(), vec![health::OP_REFUSED.to_string()]);
}

#[test]
fn retried_write_is_answered_from_the_journal_not_executed_twice() {
    let dir = TempDir::new("proc");
    let rig = make(dir.path());
    let mut write = request("r1", "checkin.online", &[("FolioNum", Some("7"))]);
    write.idempotency_key = Some("checkin-7".into());
    assert_eq!(rig.processor.route(&write), Lane::Pms);

    rig.processor.handle(&write);
    write.request_id = "r2".into();
    assert_eq!(rig.processor.route(&write), Lane::Fast);
    let retry = rig.processor.handle(&write);

    assert_eq!(rig.pms.call_count(), 1);
    assert_eq!(retry.request_id, "r2");

    // Survives an agent restart.
    let after_restart = make(dir.path());
    write.request_id = "r3".into();
    after_restart.processor.handle(&write);
    assert_eq!(after_restart.pms.call_count(), 0);
}

#[test]
fn key_operations_wrap_and_unwrap_locally_without_touching_the_pms() {
    let dir = TempDir::new("proc");
    let rig = make(dir.path());
    let key = new_tenant_key();
    let op = |name: &str, value: &str, ctx: &str| {
        request(
            &format!("{:016x}", dl_rand::u64()),
            name,
            &[("key", Some(value)), ("context", Some(ctx))],
        )
    };

    let wrapped = rig
        .processor
        .handle(&op(key_vault::OP_WRAP, &key, "tenant-dek|a|1"));
    let wrapped_value = value(&wrapped, "key");
    let back = rig
        .processor
        .handle(&op(key_vault::OP_UNWRAP, &wrapped_value, "tenant-dek|a|1"));
    assert_eq!(value(&back, "key"), key);

    let wrong_context =
        rig.processor
            .handle(&op(key_vault::OP_UNWRAP, &wrapped_value, "tenant-dek|b|1"));
    assert_eq!(wrong_context.status, Status::Rejected);
    assert_eq!(
        wrong_context.error.as_deref(),
        Some("Key operation rejected.")
    );
    assert_eq!(rig.pms.call_count(), 0);
}

#[test]
fn vault_stores_returns_and_refuses_bad_paths_or_checksums() {
    let dir = TempDir::new("proc");
    let rig = make(dir.path());
    let data = [1u8, 2, 3, 4, 5];
    let op = |name: &str, path: &str, bytes: Option<&[u8]>, sha: Option<&str>| {
        let b64 = bytes.map(to_base64);
        let sha = sha
            .map(str::to_string)
            .or_else(|| bytes.map(vault_ops::sha256_hex));
        request(
            &format!("{:016x}", dl_rand::u64()),
            name,
            &[
                ("path", Some(path)),
                ("data", b64.as_deref()),
                ("sha256", sha.as_deref()),
            ],
        )
    };

    let put = rig
        .processor
        .handle(&op(vault_ops::OP_PUT, "GuestId/a.png", Some(&data), None));
    assert_eq!(value(&put, "value"), vault_ops::sha256_hex(&data));

    let got = rig
        .processor
        .handle(&op(vault_ops::OP_GET, "GuestId/a.png", None, None));
    assert_eq!(value(&got, "value"), to_base64(&data));

    let evil = rig
        .processor
        .handle(&op(vault_ops::OP_PUT, "../evil.png", Some(&data), None));
    assert_eq!(evil.status, Status::Rejected);
    let bad_sha = rig.processor.handle(&op(
        vault_ops::OP_PUT,
        "GuestId/b.png",
        Some(&data),
        Some("00"),
    ));
    assert_eq!(bad_sha.status, Status::Rejected);
    assert_eq!(rig.pms.call_count(), 0);

    let missing = rig
        .processor
        .handle(&op(vault_ops::OP_GET, "GuestId/none.png", None, None));
    assert_eq!(missing.status, Status::Ok);
    assert!(missing.result_sets[0].rows.is_empty());
}

#[test]
fn pms_failures_carry_only_the_error_number() {
    let dir = TempDir::new("proc");
    let pms = FakeExecutor::new(vec![
        Err(ExecError::Sql {
            number: errors::LOCK_TIMEOUT,
        }),
        Err(ExecError::Sql { number: 547 }),
        Err(ExecError::Unavailable("the connection was lost".into())),
        Err(ExecError::Other("row 'Nguyen' did not convert".into())),
    ]);
    let rig = make_with(dir.path(), pms);

    let locked = rig.processor.handle(&read(None));
    assert_eq!(locked.status, Status::Failed);
    assert_eq!(locked.error, Some(errors::describe(errors::LOCK_TIMEOUT)));
    assert!(errors::is_record_locked(locked.error.as_deref()));
    assert_eq!(
        rig.processor.handle(&read(None)).error.as_deref(),
        Some("PMS error 547")
    );
    assert_eq!(
        rig.processor.handle(&read(None)).error.as_deref(),
        Some("PMS error -1")
    );
    assert!(rig.sink.codes().contains(&health::PMS_FAILED.to_string()));
    let other = rig.processor.handle(&read(None));
    assert_eq!(other.error.as_deref(), Some("Agent error"));
    assert_eq!(
        rig.processor.handle(&read(None)).error.as_deref(),
        Some("Agent error")
    );
}
