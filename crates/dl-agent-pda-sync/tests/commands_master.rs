//! `master.upsert` on the in-memory fake (ADR 0147): the write-down, store-first and
//! cloud-first conflict cases, replay, and the allow-list rejections. The real SQL Server
//! cases are in `commands_master_sql.rs`.

mod commands_support;

use std::sync::Arc;

use commands_support::{FakeStore, clock, item_class_row};
use dl_agent_pda_sync::commands::SiteCommandExecutor;
use dl_agent_pda_sync::contracts::mapping::{CatalogRowMapper, MapperOptions};
use dl_agent_pda_sync::contracts::time::TimeZone;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, MasterUpsertResult, SyncCommand, SyncCommandResult, site_command_types,
};
use dl_agent_pda_sync::contracts::{SyncCatalog, WireJson};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_agent_pda_sync::util::stj;
use dl_uuid::Uuid;

fn mapper() -> Arc<CatalogRowMapper> {
    Arc::new(CatalogRowMapper::new(
        SyncCatalog::load().unwrap().clone(),
        MapperOptions::new(TimeZone::fixed("ICT", 7 * 60)),
    ))
}

struct T {
    store: Arc<FakeStore>,
}

impl T {
    fn new(classes: &[(i32, &str)]) -> T {
        let store = FakeStore::new();
        for (code, name) in classes {
            store.state().item_classes.insert(*code, (*name).to_owned());
        }
        T { store }
    }

    fn executor(&self, with_mapper: bool) -> SiteCommandExecutor {
        let e = SiteCommandExecutor::new(self.store.clone(), clock(1_794_729_600), &Logger::none());
        if with_mapper {
            e.with_mapper(mapper())
        } else {
            e
        }
    }

    fn run(&self, c: &SyncCommand) -> SyncCommandResult {
        self.executor(true).execute(c, &Ctx::background()).unwrap()
    }

    /// The hash an editor read: the mirror's hash of the row as the store holds it.
    fn hash(&self, key: i32) -> String {
        let name = self.store.state().item_classes[&key].clone();
        mapper()
            .canonicalize("IClassDef", &item_class_row(key, &name))
            .unwrap()
            .hash
    }

    fn name(&self, key: i32) -> String {
        self.store.state().item_classes[&key].clone()
    }
}

fn upsert(entity: &str, key: &str, fields: &str, expected: Option<&str>) -> SyncCommand {
    let expected = expected.map_or(String::new(), |h| format!(r#","expected_hash":"{h}""#));
    let payload = stj::parse(&format!(
        r#"{{"entity":"{entity}","key":"{key}","fields":{fields}{expected},"actor":"office-1"}}"#
    ))
    .unwrap();
    SyncCommand {
        command_id: Uuid::new_v4(),
        type_: site_command_types::MASTER_UPSERT.into(),
        aggregate_key: format!("master:{entity}:{key}"),
        expected_hash: None,
        payload,
        created_by: "office-1".into(),
        expires_at_utc: None,
        contract_version: 1,
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
    }
}

fn rename(key: i32, name: &str, expected: Option<&str>) -> SyncCommand {
    upsert(
        "item_class",
        &key.to_string(),
        &format!(r#"{{"ClsName":"{name}"}}"#),
        expected,
    )
}

fn refused(r: &SyncCommandResult, reason: &str) {
    assert_eq!(r.status, CommandStatus::Rejected, "{:?}", r.reason);
    assert_eq!(r.reason.as_deref(), Some(reason));
}

fn result_of(r: &SyncCommandResult) -> MasterUpsertResult {
    assert_eq!(r.status, CommandStatus::Applied, "{:?}", r.reason);
    MasterUpsertResult::from_element(r.result.as_ref().unwrap()).unwrap()
}

#[test]
fn write_down_applies_and_answers_the_hash_of_the_row_re_read() {
    let t = T::new(&[(7, "Old name")]);
    let seen = t.hash(7);
    let r = t.run(&rename(7, "New name", Some(&seen)));
    let res = result_of(&r);
    assert_eq!((res.entity.as_str(), res.key.as_str()), ("item_class", "7"));
    assert_eq!(t.name(7), "New name");
    assert_eq!(res.row_hash, t.hash(7));
    assert_ne!(res.row_hash, seen);
    // The read plan is capture's: the TCVN3 column as bytes.
    assert!(t.store.calls().iter().any(|c| c.contains("bytes=ClsName")));
}

#[test]
fn store_first_edit_after_the_editor_read_is_refused_and_nothing_is_written() {
    let t = T::new(&[(7, "Old name")]);
    let seen = t.hash(7);
    t.store.state().item_classes.insert(7, "Store edit".into());
    refused(
        &t.run(&rename(7, "Cloud edit", Some(&seen))),
        "master_changed",
    );
    assert_eq!(t.name(7), "Store edit");
    assert!(
        !t.store
            .calls()
            .iter()
            .any(|c| c.starts_with("master update"))
    );
}

#[test]
fn cloud_first_then_a_pos_edit_blocks_the_next_cloud_edit_that_used_the_old_hash() {
    let t = T::new(&[(7, "Old name")]);
    let first = result_of(&t.run(&rename(7, "Cloud name", Some(&t.hash(7)))));
    // The POS edits after the cloud: not blocked (it writes straight to SQL) and the
    // cloud edit based on the hash from before it is refused.
    t.store.state().item_classes.insert(7, "POS name".into());
    refused(
        &t.run(&rename(7, "Stale edit", Some(&first.row_hash))),
        "master_changed",
    );
    assert_eq!(t.name(7), "POS name");
    // A fresh read of the POS row is a valid base again.
    let fresh = t.hash(7);
    result_of(&t.run(&rename(7, "After POS", Some(&fresh))));
    assert_eq!(t.name(7), "After POS");
}

#[test]
fn a_redelivered_command_id_answers_the_logged_result_without_a_second_write() {
    let t = T::new(&[(7, "Old name")]);
    let cmd = rename(7, "New name", Some(&t.hash(7)));
    let first = t.run(&cmd);
    let again = t.run(&cmd);
    assert_eq!(first, again);
    assert_eq!(
        t.store
            .calls()
            .iter()
            .filter(|c| c.starts_with("master update"))
            .count(),
        1
    );
}

#[test]
fn a_row_the_store_does_not_have_is_refused() {
    let t = T::new(&[]);
    refused(&t.run(&rename(9, "X", Some("h"))), "master_row_not_found");
}

#[test]
fn the_allow_list_fails_closed() {
    let t = T::new(&[(7, "Old name")]);
    let h = t.hash(7);
    refused(
        &t.run(&upsert("cashier", "7", r#"{"ClsName":"X"}"#, Some(&h))),
        "master_unsupported_entity",
    );
    refused(
        &t.run(&upsert(
            "item_class",
            "7",
            r#"{"ClsName":"X","LastChangeBy":"x"}"#,
            Some(&h),
        )),
        "master_field_not_allowed",
    );
    refused(
        &t.run(&upsert("item_class", "7", r#"{"Tax1Code":"x"}"#, Some(&h))),
        "master_field_not_allowed",
    );
    refused(&t.run(&rename(7, "X", None)), "master_create_unsupported");
    assert_eq!(t.name(7), "Old name");
    assert!(
        t.store.calls().is_empty(),
        "refused before the store was touched"
    );
}

#[test]
fn values_the_agent_cannot_store_are_invalid_payloads() {
    let t = T::new(&[(7, "Old name")]);
    let h = t.hash(7);
    for (fields, why) in [
        (r#"{"ClsName":"Ca phê"}"#, "ClsName must be ASCII"),
        (r#"{"ClsName":"tab\there"}"#, "ClsName must be ASCII"),
        (r#"{"ClsName":null}"#, "ClsName is required"),
        (
            r#"{"ClsName":"01234567890123456789012345678901234567890123456789X"}"#,
            "ClsName must be at most 50 characters",
        ),
    ] {
        let r = t.run(&upsert("item_class", "7", fields, Some(&h)));
        assert_eq!(r.status, CommandStatus::Rejected);
        assert_eq!(
            r.reason.as_deref(),
            Some(format!("invalid_payload: {why}").as_str())
        );
    }
    for key in ["abc", "-1", "7.5", " 7", "99999999999"] {
        let r = t.run(&upsert("item_class", key, r#"{"ClsName":"X"}"#, Some(&h)));
        assert_eq!(
            r.reason.as_deref(),
            Some("invalid_payload: Key must be an integer ClsCode"),
            "{key}"
        );
    }
    assert_eq!(t.name(7), "Old name");
}

#[test]
fn an_invalid_payload_is_rejected_like_the_other_verbs() {
    let t = T::new(&[]);
    for payload in [
        r#"{"entity":"item_class","key":"7","fields":{},"actor":"a"}"#,
        r#"{"entity":"","key":"7","fields":{"ClsName":"X"},"actor":"a"}"#,
        r#"{"entity":"item_class","key":"7","fields":{"ClsName":"X"},"actor":""}"#,
        r#"[1]"#,
    ] {
        let mut c = rename(7, "X", Some("h"));
        c.payload = stj::parse(payload).unwrap();
        let r = t.run(&c);
        assert_eq!(r.status, CommandStatus::Rejected, "{payload}");
        assert!(
            r.reason
                .as_deref()
                .is_some_and(|x| x.starts_with("invalid_payload: ")),
            "{payload}: {:?}",
            r.reason
        );
    }
}

#[test]
fn without_a_mapper_the_type_is_unknown() {
    let t = T::new(&[(7, "Old name")]);
    let r = t
        .executor(false)
        .execute(&rename(7, "X", Some("h")), &Ctx::background())
        .unwrap();
    refused(&r, "unknown_command_type");
}
