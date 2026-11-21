//! `Strategies/TextRevisionTests.cs` (ADR 0123): text values lose their trailing spaces, and
//! a state built before that corrects what it already sent.

use super::codecs::key_codec;
use super::engine::{OPEN_STREAM, TEXT_REVISION, TEXT_REVISION_CURSOR};
use super::state::{ShadowEntry, ShadowWrite, StateCommit};
use super::test_support::*;
use crate::contracts::map::str_map;
use crate::contracts::mapping::RawValue;

const PADDED: &str = "100001      ";

fn site() -> Harness {
    let h = Harness::new();
    h.source
        .table("TmpInvoice")
        .table("TmpTrn")
        .table("InvoiceCxl")
        .table("VoidTrn")
        .table("ReOpenLog");
    h.source.add(
        "Invoice",
        vec![row(&[
            ("CheckNo", RawValue::I32(1)),
            ("CloseTime", local(0, 10)),
            ("SubTotal", dec("10")),
        ])],
    );
    h.source.add(
        "ItemDef",
        vec![row(&[
            ("ICode", PADDED.into()),
            ("IName", "Pho bo      ".into()),
            ("Price", dec("50000")),
        ])],
    );
    h
}

fn key(r: &crate::contracts::wire::SyncRow, k: &str) -> Option<String> {
    r.key.get(k).cloned().flatten()
}

#[test]
fn a_fixed_width_code_is_sent_without_its_padding() {
    let h = site();

    h.engine().run_masters(&bg()).unwrap();

    let rows = h.sink.rows("ItemDef");
    assert_eq!(rows.len(), 1);
    assert_eq!(key(&rows[0], "item_code").as_deref(), Some("100001"));
    assert_eq!(
        rows[0]
            .columns
            .get("item_name")
            .cloned()
            .flatten()
            .as_deref(),
        Some("Pho bo")
    );
}

#[test]
fn a_master_sent_with_padding_before_is_replaced_by_its_trimmed_row_and_a_tombstone_of_the_padded_key()
 {
    let h = site();
    let engine = h.engine();
    engine.run_masters(&bg()).unwrap();
    h.sink.clear();
    // What a build before ADR 0123 left behind: the row is known under its padded key.
    let shadow = h.state.get_shadow("ItemDef", None, &bg()).unwrap();
    let known: Vec<String> = shadow.keys().map(str::to_owned).collect();
    assert_eq!(known.len(), 1);
    let old = key_codec::encode(&str_map([("item_code", Some(PADDED))]));
    let mut seed = StateCommit::default();
    seed.shadow.push(ShadowWrite {
        table: "ItemDef".to_owned(),
        upserts: vec![ShadowEntry::new(&old, "hash-of-the-padded-row", "-", None)],
        delete_keys: known,
    });
    h.state.commit(&seed, &bg()).unwrap();

    engine.run_masters(&bg()).unwrap();

    let rows = h.sink.rows("ItemDef");
    assert_eq!(rows.len(), 1);
    assert_eq!(key(&rows[0], "item_code").as_deref(), Some("100001"));
    let tombs = h.sink.tombstones("ItemDef");
    assert_eq!(tombs.len(), 1);
    assert_eq!(
        tombs[0].key.get("item_code").cloned().flatten().as_deref(),
        Some(PADDED)
    );
    let batches: Vec<_> = h
        .sink
        .batches()
        .into_iter()
        .filter(|b| b.table == "ItemDef")
        .collect();
    let first_row = batches.iter().position(|b| !b.rows.is_empty());
    let first_tomb = batches.iter().position(|b| !b.tombstones.is_empty());
    assert!(
        first_row < first_tomb,
        "the trimmed row is sent before the tombstone of the padded key"
    );
    h.sink.clear();
    assert_eq!(
        engine.run_masters(&bg()).unwrap().batches,
        0,
        "the correction happens once"
    );
}

#[test]
fn a_new_state_has_nothing_to_send_again() {
    let h = site();
    let engine = h.engine();

    assert_eq!(engine.text_upgrade_streams(&bg()).unwrap(), Some(vec![]));
    engine.mark_text_revision(&bg()).unwrap();
    assert_eq!(
        h.state.get_cursor(TEXT_REVISION_CURSOR, &bg()).unwrap(),
        Some(TEXT_REVISION.to_owned())
    );
    assert_eq!(engine.text_upgrade_streams(&bg()).unwrap(), None);
}

#[test]
fn a_state_from_an_older_text_revision_names_its_loaded_ledger_streams_once() {
    let h = site();
    let engine = h.engine();
    assert!(engine.run_initial_snapshot(&bg()).unwrap().complete);
    h.sink.clear();

    let streams = engine.text_upgrade_streams(&bg()).unwrap().unwrap();

    assert!(streams.iter().any(|s| s == "Invoice"), "{streams:?}");
    for not in ["ItemDef", OPEN_STREAM] {
        assert!(
            !streams.iter().any(|s| s == not),
            "masters and open checks correct themselves: {streams:?}"
        );
    }
    let ledger: Vec<&str> = engine.plan().ledger.iter().map(|p| p.name()).collect();
    assert!(streams.iter().all(|s| ledger.contains(&s.as_str())));
    assert!(engine.reset_streams(&streams, &bg()).unwrap());
    engine.mark_text_revision(&bg()).unwrap();
    assert!(engine.run_initial_snapshot(&bg()).unwrap().complete);
    assert_eq!(
        h.sink.rows("Invoice").len(),
        1,
        "the closed check is sent again"
    );
    assert!(
        h.sink.rows("ItemDef").is_empty(),
        "the master kept its shadow"
    );
    assert_eq!(engine.text_upgrade_streams(&bg()).unwrap(), None);
}

#[test]
fn a_different_revision_counts_as_old() {
    let h = site();
    let engine = h.engine();
    assert!(engine.run_initial_snapshot(&bg()).unwrap().complete);
    let mut c = StateCommit::default();
    c.cursors.insert(TEXT_REVISION_CURSOR, Some("0".to_owned()));
    h.state.commit(&c, &bg()).unwrap();

    let streams = engine.text_upgrade_streams(&bg()).unwrap().unwrap();

    assert!(streams.iter().any(|s| s == "Invoice"), "{streams:?}");
}
