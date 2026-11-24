//! `OpenCheckTwoPhaseTests` (RT-0759, ADR 0153): unchanged open checks are detected by a
//! per-check signature instead of re-reading every row.

use super::test_support::*;
use crate::contracts::mapping::RawValue;
use crate::util::ctx::Ctx;

fn site(checks: i32, lines_per_check: i32, full_every: i32) -> Harness {
    let mut h = Harness::new();
    h.options.open_full_read_every = full_every;
    h.source
        .table("TmpInvoice")
        .table("TmpTrn")
        .table("Invoice")
        .table("InvoiceCxl")
        .table("VoidTrn");
    for c in 1..=checks {
        h.source.add(
            "TmpInvoice",
            vec![row(&[
                ("CheckNo", RawValue::I32(c)),
                ("OpenTime", local(0, 10)),
                ("SubTotal", dec("50000")),
            ])],
        );
        for l in 1..=lines_per_check {
            h.source.add(
                "TmpTrn",
                vec![row(&[
                    ("TrnSeq", RawValue::I32(c * 100 + l)),
                    ("CheckNo", RawValue::I32(c)),
                    ("ItemPrice", dec(&(1000 * l).to_string())),
                ])],
            );
        }
    }
    h
}

fn check(no: i32) -> impl Fn(&super::source::VendorRow) -> bool {
    move |r| int(r, "CheckNo") == Some(i64::from(no))
}

#[test]
fn idle_cycle_reads_no_rows_but_the_first_cycle_reads_everything() {
    let h = site(20, 5, 30);
    let e = h.engine();

    let first = e.run_open_checks(&bg()).unwrap();
    let idle = e.run_open_checks(&bg()).unwrap();

    assert!(first.open_full_read);
    assert_eq!(first.open_rows_read, 20 + 20 * 5);
    assert!(!idle.open_full_read);
    assert_eq!(
        idle.open_rows_read, 0,
        "only the per-check signature is read when nothing changed"
    );
    assert_eq!(idle.batches, 0);
}

#[test]
fn one_changed_check_reads_only_that_check_and_sends_only_its_changes() {
    let h = site(20, 5, 30);
    let e = h.engine();
    e.run_open_checks(&bg()).unwrap();
    h.sink.clear();

    h.source.update(
        "TmpTrn",
        |r| int(r, "TrnSeq") == Some(703),
        "ItemPrice",
        dec("9999"),
    );
    let report = e.run_open_checks(&bg()).unwrap();

    assert!(
        report.open_rows_read <= 2 * (1 + 5) + 6,
        "the changed check is read (and re-read once to stabilise): {}",
        report.open_rows_read
    );
    assert_eq!(h.sink.rows("TmpTrn").len(), 1);
    assert!(h.sink.rows("TmpInvoice").is_empty());
}

#[test]
fn new_check_is_picked_up_between_full_reads() {
    let h = site(2, 5, 30);
    let e = h.engine();
    e.run_open_checks(&bg()).unwrap();
    h.sink.clear();

    h.source.add(
        "TmpInvoice",
        vec![row(&[
            ("CheckNo", RawValue::I32(99)),
            ("OpenTime", local(0, 10)),
            ("SubTotal", dec("1")),
        ])],
    );
    h.source.add(
        "TmpTrn",
        vec![row(&[
            ("TrnSeq", RawValue::I32(9901)),
            ("CheckNo", RawValue::I32(99)),
            ("ItemPrice", dec("1")),
        ])],
    );
    let report = e.run_open_checks(&bg()).unwrap();

    assert!(!report.open_full_read);
    assert_eq!(h.sink.rows("TmpInvoice").len(), 1);
    assert_eq!(h.sink.rows("TmpTrn").len(), 1);
}

#[test]
fn vanished_check_is_still_tombstoned_after_confirmation_without_a_full_read() {
    let h = site(2, 5, 30);
    let e = h.engine();
    e.run_open_checks(&bg()).unwrap();
    h.sink.clear();

    h.source.remove("TmpInvoice", check(2));
    h.source.remove("TmpTrn", check(2));
    h.source
        .add("Invoice", vec![row(&[("CheckNo", RawValue::I32(2))])]);
    e.run_open_checks(&bg()).unwrap();
    assert!(
        h.sink.tombstones("TmpInvoice").is_empty(),
        "one missing poll is not enough"
    );
    let second = e.run_open_checks(&bg()).unwrap();

    assert!(!second.open_full_read);
    let t = h.sink.tombstones("TmpInvoice");
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].reason.as_deref(), Some("closed"));
    assert_eq!(h.sink.tombstones("TmpTrn").len(), 5);
}

#[test]
fn periodic_full_read_heals_a_change_the_signature_missed() {
    let h = site(2, 5, 3);
    let e = h.engine();
    e.run_open_checks(&bg()).unwrap();
    h.sink.clear();

    // A signature collision: the row changes but the source keeps reporting the old signature.
    let frozen = h.source.live_signatures("TmpTrn", "CheckNo");
    h.source.update(
        "TmpTrn",
        |r| int(r, "TrnSeq") == Some(101),
        "ItemPrice",
        dec("7777"),
    );
    h.source
        .frozen_signatures
        .lock()
        .unwrap()
        .push(("TmpTrn".to_owned(), frozen));

    assert_eq!(e.run_open_checks(&bg()).unwrap().batches, 0);
    assert_eq!(e.run_open_checks(&bg()).unwrap().batches, 0);
    let full = e.run_open_checks(&bg()).unwrap();

    assert!(full.open_full_read);
    assert_eq!(h.sink.rows("TmpTrn").len(), 1);
}

#[test]
fn source_without_signatures_falls_back_to_a_full_read_every_cycle() {
    let h = site(2, 5, 30);
    *h.source.no_signatures.lock().unwrap() = true;
    let e = h.engine();
    e.run_open_checks(&bg()).unwrap();

    let second = e.run_open_checks(&bg()).unwrap();

    assert!(second.open_full_read);
    assert_eq!(second.open_rows_read, 2 + 10);
    assert_eq!(second.batches, 0);
}

#[test]
fn disabled_two_phase_reads_everything_every_cycle() {
    let h = site(2, 5, 1);
    let e = h.engine();
    e.run_open_checks(&bg()).unwrap();

    let second = e.run_open_checks(&bg()).unwrap();

    assert!(second.open_full_read);
    assert_eq!(h.source.signature_query_count(), 0);
}

#[test]
fn a_reset_of_the_open_stream_forgets_the_signatures() {
    let h = site(2, 5, 30);
    let e = h.engine();
    let ctx = Ctx::background();
    e.run_open_checks(&ctx).unwrap();
    assert!(!e.run_open_checks(&ctx).unwrap().open_full_read);

    assert!(e.reset_streams(&["open".to_owned()], &ctx).unwrap());
    let after_stream_reset = e.run_open_checks(&ctx).unwrap();
    e.reset(&ctx).unwrap();
    let after_reset = e.run_open_checks(&ctx).unwrap();

    assert!(after_stream_reset.open_full_read);
    assert!(after_reset.open_full_read);
    assert_eq!(after_reset.open_rows_read, 2 + 10);
    assert!(
        after_reset.batches > 0,
        "the reset state is filled again from the full read"
    );
}
