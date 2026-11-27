//! Strategy tests over the fakes: `LedgerTests`, `MasterTests`, `OpenCheckTests`,
//! `ResetTests`, `TableIsolationTests`, `NullKeyPrecedenceTests`, `NullKeysetTests` and
//! the `CaptureContext.TargetOf` case of `CapturePlanTests`.

use super::context::CaptureContext;
use super::engine::CaptureEngine;
use super::error::ErrorKind;
use super::plan::CapturePlan;
use super::source::VendorRow;
use super::test_support::*;
use crate::contracts::mapping::RawValue;

fn key_of(r: &crate::contracts::wire::SyncRow, k: &str) -> String {
    r.key.get(k).cloned().flatten().unwrap_or_default()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

mod ledger {
    use super::*;

    fn inv(check_no: i32, days_ago: i64) -> VendorRow {
        row(&[
            ("CheckNo", RawValue::I32(check_no)),
            ("CloseTime", local(days_ago, 10)),
            ("SubTotal", dec("100")),
        ])
    }

    fn site(invoices: Vec<VendorRow>) -> Harness {
        let h = Harness::new();
        h.source.table("Invoice").table("ReOpenLog");
        h.source.add("Invoice", invoices);
        h
    }

    fn pinned(invoices: Vec<VendorRow>) -> (Harness, CaptureEngine) {
        let h = site(invoices);
        let e = h.engine();
        e.run_initial_snapshot(&bg()).unwrap();
        h.sink.clear();
        (h, e)
    }

    fn checks(h: &Harness) -> Vec<String> {
        h.sink
            .rows("Invoice")
            .iter()
            .map(|r| key_of(r, "check_no"))
            .collect()
    }

    #[test]
    fn tail_does_nothing_until_the_initial_snapshot_pinned_a_watermark() {
        let h = site(vec![inv(1, 0)]);
        let report = h.engine().run_ledger_tail(&bg()).unwrap();
        assert!(h.sink.batches().is_empty());
        assert!(
            report
                .notes
                .iter()
                .any(|n| n.contains("Invoice") && n.contains("snapshot"))
        );
    }

    #[test]
    fn tail_sends_only_rows_after_the_watermark() {
        let (h, e) = pinned(vec![inv(1, 0), inv(2, 0)]);
        h.source
            .add("Invoice", vec![inv(3, 0), inv(4, 0), inv(5, 0), inv(6, 0)]);
        e.run_ledger_tail(&bg()).unwrap();
        let again = e.run_ledger_tail(&bg()).unwrap();
        assert_eq!(sorted(checks(&h)), ["3", "4", "5", "6"]);
        assert_eq!(again.batches, 0);
    }

    #[test]
    fn non_monotonic_check_number_is_missed_by_the_tail_but_found_by_the_rescan() {
        let (h, e) = pinned(vec![inv(10, 0), inv(20, 0)]);
        h.source.add("Invoice", vec![inv(15, 0)]);
        e.run_ledger_tail(&bg()).unwrap();
        assert!(
            h.sink.rows("Invoice").is_empty(),
            "the watermark is already past key 15"
        );
        e.run_ledger_rescan(&bg()).unwrap();
        assert_eq!(checks(&h), ["15"]);
    }

    #[test]
    fn late_edit_within_the_rescan_window_is_detected_and_older_edit_is_not() {
        let (h, e) = pinned(vec![inv(1, 0), inv(2, 1), inv(3, 10)]);
        h.source.update("Invoice", |_| true, "SubTotal", dec("999"));
        e.run_ledger_rescan(&bg()).unwrap();
        assert_eq!(sorted(checks(&h)), ["1", "2"]);
    }

    #[test]
    fn reopen_log_pulls_an_old_day_into_the_rescan() {
        let (h, e) = pinned(vec![inv(3, 10)]);
        h.source.update("Invoice", |_| true, "SubTotal", dec("5"));
        h.source.add(
            "ReOpenLog",
            vec![row(&[
                ("CheckNo", RawValue::I32(3)),
                ("CloseTime", local(10, 10)),
                ("ReOpenTime", local(0, 11)),
            ])],
        );
        e.run_ledger_rescan(&bg()).unwrap();
        assert_eq!(h.sink.rows("Invoice").len(), 1);
    }

    #[test]
    fn reopen_log_without_order_time_is_read_with_the_columns_the_build_has() {
        let (h, e) = pinned(vec![inv(3, 10)]);
        h.source.update("Invoice", |_| true, "SubTotal", dec("5"));
        h.source.schema(
            "ReOpenLog",
            &[
                "CheckNo",
                "OpenTime",
                "CloseTime",
                "ReOpenTime",
                "CardNumber",
            ],
        );
        h.source.add(
            "ReOpenLog",
            vec![row(&[
                ("CheckNo", RawValue::I32(3)),
                ("CloseTime", local(10, 10)),
                ("ReOpenTime", local(0, 11)),
                ("CardNumber", RawValue::from("4111111111111111")),
            ])],
        );
        let report = e.run_ledger_rescan(&bg()).unwrap();
        assert!(report.table_faults.is_empty());
        assert_eq!(
            h.sink.rows("Invoice").len(),
            1,
            "the re-opened day was rescanned"
        );
        let reads: Vec<_> = h
            .source
            .queries()
            .into_iter()
            .filter(|q| q.table == "ReOpenLog" && q.range_column.as_deref() == Some("ReOpenTime"))
            .collect();
        assert_eq!(reads.len(), 1);
        assert!(
            reads[0].columns.is_none(),
            "the source picks the columns this build has"
        );
        assert!(
            reads[0]
                .exclude_columns
                .as_ref()
                .unwrap()
                .iter()
                .any(|c| c == "CardNumber"),
            "a PAN column is never read"
        );
    }

    #[test]
    fn deleted_row_is_tombstoned_by_the_rescan() {
        let (h, e) = pinned(vec![inv(1, 0), inv(2, 0)]);
        e.run_ledger_rescan(&bg()).unwrap();
        h.sink.clear();
        h.source.remove("Invoice", |r| int(r, "CheckNo") == Some(2));
        e.run_ledger_rescan(&bg()).unwrap();
        let t = h.sink.tombstones("Invoice");
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].reason.as_deref(), Some("deleted"));
    }

    #[test]
    fn row_that_moves_to_another_day_is_updated_not_tombstoned() {
        let (h, e) = pinned(vec![inv(1, 0)]);
        h.source
            .update("Invoice", |_| true, "CloseTime", local(1, 10));
        e.run_ledger_rescan(&bg()).unwrap();
        assert!(h.sink.tombstones("Invoice").is_empty());
        assert_eq!(h.sink.rows("Invoice").len(), 1);
        h.sink.clear();
        e.run_ledger_rescan(&bg()).unwrap();
        assert!(h.sink.batches().is_empty());
    }

    #[test]
    fn last_change_time_is_ignored_as_a_signal() {
        let (h, e) = pinned(vec![inv(1, 0)]);
        e.run_ledger_rescan(&bg()).unwrap();
        let qs = h.source.queries();
        assert!(qs.iter().filter(|q| q.table == "Invoice").all(|q| q.range_column.is_none()
            || q.range_column.as_deref() == Some("CloseTime")));
        assert!(
            !qs.iter()
                .flat_map(|q| q.order_by.iter())
                .any(|c| c == "LastChgTime")
        );
    }

    #[test]
    fn table_without_a_date_column_rescans_its_tail_rows() {
        let h = Harness::new();
        h.source.add(
            "CGTrn",
            vec![
                row(&[("SeqNo", RawValue::I32(1)), ("CGName", RawValue::from("A"))]),
                row(&[("SeqNo", RawValue::I32(2)), ("CGName", RawValue::from("B"))]),
            ],
        );
        let e = h.engine();
        e.run_initial_snapshot(&bg()).unwrap();
        h.sink.clear();
        h.source.update(
            "CGTrn",
            |r| int(r, "SeqNo") == Some(1),
            "CGName",
            RawValue::from("A2"),
        );
        e.run_ledger_rescan(&bg()).unwrap();
        assert_eq!(h.sink.rows("CGTrn").len(), 1);
    }

    #[test]
    fn snapshot_is_chunked_and_resumes_after_a_crash_without_resending() {
        let h = site((1..=5).map(|i| inv(i, 0)).collect());
        *h.sink.fail_after_batches.lock().unwrap() = Some(1);
        let err = h.engine().run_initial_snapshot(&bg()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Io);

        *h.sink.fail_after_batches.lock().unwrap() = None;
        let result = h.engine().run_initial_snapshot(&bg()).unwrap();
        assert!(result.complete);
        assert_eq!(sorted(checks(&h)), ["1", "2", "3", "4", "5"]);
        assert_eq!(h.sink.rows("Invoice").len(), 5);
        assert!(
            h.sink
                .batches()
                .iter()
                .filter(|b| b.table == "Invoice")
                .all(|b| b.rows.len() <= 2)
        );
        assert_eq!(
            h.engine()
                .run_initial_snapshot(&bg())
                .unwrap()
                .report
                .batches,
            0,
            "a finished snapshot is not repeated"
        );
    }

    #[test]
    fn snapshot_loads_the_window_first_then_older_history_and_includes_rows_without_a_date() {
        let h = site(vec![
            inv(1, 0),
            inv(2, 1000),
            row(&[
                ("CheckNo", RawValue::I32(3)),
                ("CloseTime", RawValue::Null),
                ("SubTotal", dec("1")),
            ]),
        ]);
        h.engine().run_initial_snapshot(&bg()).unwrap();
        let order = checks(&h);
        assert_eq!(sorted(order.clone()), ["1", "2", "3"]);
        assert_eq!(
            order.iter().position(|c| c == "2"),
            Some(2),
            "older history is a later phase"
        );
    }

    #[test]
    fn backfill_can_be_switched_off() {
        let mut h = site(vec![inv(1, 0), inv(2, 1000)]);
        h.options.include_backfill = false;
        h.engine().run_initial_snapshot(&bg()).unwrap();
        assert_eq!(checks(&h), ["1"]);
    }

    #[test]
    fn rows_arriving_during_the_snapshot_are_picked_up_by_the_tail() {
        let h = site(vec![inv(1, 0)]);
        let e = h.engine();
        e.run_initial_snapshot(&bg()).unwrap();
        h.source.add("Invoice", vec![inv(2, 0)]);
        h.sink.clear();
        e.run_ledger_tail(&bg()).unwrap();
        assert_eq!(h.sink.rows("Invoice").len(), 1);
    }

    #[test]
    fn source_busy_error_propagates_for_the_scheduler_to_back_off() {
        let h = site(vec![inv(1, 0)]);
        let e = h.engine();
        e.run_initial_snapshot(&bg()).unwrap();
        *h.source.busy_after.lock().unwrap() = Some(h.source.query_count());
        assert!(e.run_ledger_tail(&bg()).unwrap_err().is_busy());
    }
}

mod master {
    use super::*;

    fn site() -> Harness {
        let h = Harness::new();
        h.source.add(
            "DefRVCList",
            vec![
                row(&[
                    ("RVCNo", RawValue::I32(1)),
                    ("RVCName", RawValue::from("Cafe")),
                    ("Picture", RawValue::Bytes(vec![1, 2, 3])),
                ]),
                row(&[
                    ("RVCNo", RawValue::I32(2)),
                    ("RVCName", RawValue::from("Bar")),
                    ("Picture", RawValue::Null),
                ]),
            ],
        );
        h
    }

    fn rvc(n: i64) -> impl Fn(&VendorRow) -> bool {
        move |r| int(r, "RVCNo") == Some(n)
    }

    #[test]
    fn scan_sends_new_rows_once_then_only_changes_and_deletions() {
        let h = site();
        let e = h.engine();
        e.run_masters(&bg()).unwrap();
        assert_eq!(h.sink.rows("DefRVCList").len(), 2);
        h.sink.clear();
        assert_eq!(e.run_masters(&bg()).unwrap().batches, 0);

        h.source
            .update("DefRVCList", rvc(2), "RVCName", RawValue::from("Bar 2"));
        h.source.remove("DefRVCList", rvc(1));
        e.run_masters(&bg()).unwrap();
        assert_eq!(h.sink.rows("DefRVCList").len(), 1);
        let t = h.sink.tombstones("DefRVCList");
        assert_eq!(t.len(), 1);
        assert_eq!(
            t[0].key.get("RVCNo").cloned().flatten().as_deref(),
            Some("1")
        );
    }

    #[test]
    fn image_travels_by_content_hash_and_is_resent_only_when_it_changes() {
        let h = site();
        let e = h.engine();
        e.run_masters(&bg()).unwrap();
        let blobs = h.sink.blobs();
        assert_eq!(blobs.len(), 1);
        assert_eq!(blobs[0].content, [1, 2, 3]);
        h.sink.clear();
        h.sink.clear_blobs();

        h.source
            .update("DefRVCList", rvc(2), "RVCName", RawValue::from("renamed"));
        e.run_masters(&bg()).unwrap();
        assert!(h.sink.blobs().is_empty(), "the picture did not change");

        h.source
            .update("DefRVCList", rvc(1), "Picture", RawValue::Bytes(vec![9]));
        e.run_masters(&bg()).unwrap();
        let blobs = h.sink.blobs();
        assert_eq!(blobs.len(), 1);
        assert_eq!(blobs[0].content, [9]);
    }

    #[test]
    fn blob_ref_addresses_the_vendor_table_and_wire_key_not_the_pg_target() {
        use dl_hash::{Digest, Sha256};
        let h = site();
        h.engine().run_masters(&bg()).unwrap();
        let blobs = h.sink.blobs();
        assert_eq!(blobs.len(), 1);
        let rows = h.sink.rows("DefRVCList");
        let row = rows.iter().find(|r| key_of(r, "RVCNo") == "1").unwrap();
        assert_eq!(blobs[0].ref_.table, "DefRVCList");
        assert_eq!(blobs[0].ref_.column, "Picture");
        assert_eq!(blobs[0].ref_.key, row.key);
        assert_eq!(
            blobs[0].sha256(),
            dl_encoding::hex::encode(Sha256::digest([1u8, 2, 3]))
        );
    }

    #[test]
    fn image_column_does_not_change_the_row_hash() {
        let h = site();
        let e = h.engine();
        e.run_masters(&bg()).unwrap();
        h.sink.clear();
        h.source
            .update("DefRVCList", rvc(1), "Picture", RawValue::Bytes(vec![7, 7]));
        e.run_masters(&bg()).unwrap();
        assert!(h.sink.rows("DefRVCList").is_empty());
    }

    #[test]
    fn exact_duplicate_rows_get_stable_occurrence_keys() {
        let h = Harness::new();
        let r = |c: &str| {
            row(&[
                ("Cashier", RawValue::from(c)),
                ("RVC", RawValue::I32(1)),
                ("Notes", RawValue::from("n")),
            ])
        };
        h.source.add("CashierRVC", vec![r("C1"), r("C1"), r("C2")]);
        let e = h.engine();
        let first = e.run_masters(&bg()).unwrap();
        h.sink.clear();
        let second = e.run_masters(&bg()).unwrap();
        assert_eq!(first.duplicate_keys, 1);
        assert_eq!(second.batches, 0, "occurrence suffixes are deterministic");
    }

    #[test]
    fn unmappable_row_is_reported_and_blocks_tombstones_of_that_table() {
        let h = site();
        let e = h.engine();
        e.run_masters(&bg()).unwrap();
        h.sink.clear();
        h.source
            .update("DefRVCList", rvc(2), "RVCNo", RawValue::Null);
        let report = e.run_masters(&bg()).unwrap();
        assert_eq!(report.rejected.len(), 1);
        assert_eq!(report.rejected[0].rule, "missing_key:RVCNo");
        assert!(h.sink.tombstones("DefRVCList").is_empty());
    }

    #[test]
    fn large_tables_are_read_in_keyset_pages() {
        let h = Harness::new();
        h.source.add(
            "DefRVCList",
            (1..=7)
                .map(|i| {
                    row(&[
                        ("RVCNo", RawValue::I32(i)),
                        ("RVCName", RawValue::String(format!("n{i}"))),
                    ])
                })
                .collect(),
        );
        h.engine().run_masters(&bg()).unwrap();
        assert!(
            h.source
                .queries()
                .iter()
                .filter(|q| q.table == "DefRVCList")
                .all(|q| q.limit == Some(3))
        );
        assert_eq!(h.sink.rows("DefRVCList").len(), 7);
    }

    #[test]
    fn captured_tables_with_excluded_columns_never_select_them() {
        let plan = CapturePlan::build(catalog(), &opts()).unwrap();
        let t = plan
            .masters
            .iter()
            .chain(&plan.ledger)
            .chain(&plan.open)
            .find(|x| !x.exclude.is_empty())
            .unwrap();
        assert_eq!(CaptureContext::query(t).exclude_columns.unwrap(), t.exclude);
    }

    #[test]
    fn null_in_a_key_column_falls_back_to_a_whole_table_read() {
        let mut h = Harness::new();
        h.options.page_rows = 2;
        h.source.add(
            "DefRVCList",
            (1..=3)
                .map(|i| row(&[("RVCNo", RawValue::I32(i))]))
                .collect(),
        );
        let e = h.engine();
        h.source
            .update("DefRVCList", rvc(2), "RVCNo", RawValue::Null);
        e.run_masters(&bg()).unwrap();
    }
}

mod open_check {
    use super::*;

    fn site() -> Harness {
        let h = Harness::new();
        h.source
            .table("TmpInvoice")
            .table("TmpTrn")
            .table("Invoice")
            .table("InvoiceCxl")
            .table("VoidTrn");
        h.source.add(
            "TmpInvoice",
            vec![row(&[
                ("CheckNo", RawValue::I32(100)),
                ("OpenTime", local(0, 10)),
                ("SubTotal", dec("50000")),
            ])],
        );
        h.source.add(
            "TmpTrn",
            vec![row(&[
                ("TrnSeq", RawValue::I32(1)),
                ("CheckNo", RawValue::I32(100)),
                ("ItemPrice", dec("50000")),
            ])],
        );
        h
    }

    #[test]
    fn new_check_is_sent_once_and_header_goes_first() {
        let h = site();
        let e = h.engine();
        let first = e.run_open_checks(&bg()).unwrap();
        let second = e.run_open_checks(&bg()).unwrap();
        assert_eq!(first.batches, 2);
        let tables: Vec<String> = h.sink.batches().iter().map(|b| b.table.clone()).collect();
        assert_eq!(tables, ["TmpInvoice", "TmpTrn"]);
        assert!(h.sink.batches().iter().all(|b| b.stream == "open"));
        assert_eq!(second.batches, 0, "an unchanged check is not re-sent");
    }

    #[test]
    fn changed_line_resends_only_that_check_and_removed_line_is_tombstoned() {
        let h = site();
        h.source.add(
            "TmpTrn",
            vec![row(&[
                ("TrnSeq", RawValue::I32(2)),
                ("CheckNo", RawValue::I32(100)),
                ("ItemPrice", dec("1000")),
            ])],
        );
        let e = h.engine();
        e.run_open_checks(&bg()).unwrap();
        h.sink.clear();
        h.source.update(
            "TmpTrn",
            |r| int(r, "TrnSeq") == Some(1),
            "ItemPrice",
            dec("60000"),
        );
        h.source.remove("TmpTrn", |r| int(r, "TrnSeq") == Some(2));
        e.run_open_checks(&bg()).unwrap();
        assert_eq!(h.sink.rows("TmpTrn").len(), 1);
        let t = h.sink.tombstones("TmpTrn");
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].reason.as_deref(), Some("removed"));
    }

    #[test]
    fn vanished_check_is_tombstoned_only_after_confirmation_with_reason() {
        for (where_, reason) in [
            (Some("Invoice"), "closed"),
            (Some("InvoiceCxl"), "void"),
            (Some("VoidTrn"), "void"),
            (None, "vanished"),
        ] {
            let h = site();
            let e = h.engine();
            e.run_open_checks(&bg()).unwrap();
            h.sink.clear();
            h.source.remove("TmpInvoice", |_| true);
            h.source.remove("TmpTrn", |_| true);
            if let Some(w) = where_ {
                h.source
                    .add(w, vec![row(&[("CheckNo", RawValue::I32(100))])]);
            }
            e.run_open_checks(&bg()).unwrap();
            assert!(
                h.sink.tombstones("TmpInvoice").is_empty(),
                "one missing poll is not enough"
            );
            e.run_open_checks(&bg()).unwrap();
            let t = h.sink.tombstones("TmpInvoice");
            assert_eq!(t.len(), 1, "{reason}");
            assert_eq!(t[0].reason.as_deref(), Some(reason));
            assert_eq!(h.sink.tombstones("TmpTrn").len(), 1);
            h.sink.clear();
            assert_eq!(e.run_open_checks(&bg()).unwrap().batches, 0);
        }
    }

    #[test]
    fn check_that_reappears_resets_the_vanish_counter() {
        let h = site();
        let e = h.engine();
        e.run_open_checks(&bg()).unwrap();
        let header = h.source.rows("TmpInvoice");
        let lines = h.source.rows("TmpTrn");
        h.source.remove("TmpInvoice", |_| true);
        h.source.remove("TmpTrn", |_| true);
        e.run_open_checks(&bg()).unwrap();
        h.source.add("TmpInvoice", header);
        h.source.add("TmpTrn", lines);
        e.run_open_checks(&bg()).unwrap();
        h.source.remove("TmpInvoice", |_| true);
        h.source.remove("TmpTrn", |_| true);
        h.sink.clear();
        e.run_open_checks(&bg()).unwrap();
        assert!(h.sink.tombstones("TmpInvoice").is_empty());
    }

    #[test]
    fn echo_of_an_applied_command_updates_the_shadow_without_sending() {
        let h = site();
        let e = h.engine();
        e.run_open_checks(&bg()).unwrap();
        h.sink.clear();
        h.echo.aggregates.lock().unwrap().insert("100".to_owned());
        h.source
            .update("TmpTrn", |_| true, "ItemPrice", dec("70000"));
        let report = e.run_open_checks(&bg()).unwrap();
        h.echo.aggregates.lock().unwrap().clear();
        let again = e.run_open_checks(&bg()).unwrap();
        assert!(h.sink.rows("TmpTrn").is_empty());
        assert!(report.echoes_suppressed > 0);
        assert_eq!(
            again.batches, 0,
            "the suppressed state is already in the shadow"
        );
    }

    #[test]
    fn agent_restart_with_the_same_state_does_not_resend() {
        let h = site();
        h.engine().run_open_checks(&bg()).unwrap();
        h.sink.clear();
        assert_eq!(h.engine().run_open_checks(&bg()).unwrap().batches, 0);
    }

    #[test]
    fn sequence_numbers_increase_per_stream_across_cycles() {
        let h = site();
        let e = h.engine();
        e.run_open_checks(&bg()).unwrap();
        h.source
            .update("TmpInvoice", |_| true, "SubTotal", dec("1"));
        e.run_open_checks(&bg()).unwrap();
        let seqs: Vec<i64> = h.sink.batches().iter().map(|b| b.seq).collect();
        assert_eq!(seqs, [1, 2, 3]);
    }

    #[test]
    fn missing_optional_master_is_told_apart_from_a_real_gap() {
        // RT-1381..1390: TRVCItem is optional_on_site, RVCItem is a core master.
        let h = Harness::new();
        let report = h.engine().run_masters(&bg()).unwrap();
        assert!(report.missing_tables.iter().any(|t| t == "TRVCItem"));
        assert!(
            report
                .optional_missing_tables
                .iter()
                .any(|t| t == "TRVCItem")
        );
        let unexpected = report.unexpected_missing_tables();
        assert!(!unexpected.iter().any(|t| t == "TRVCItem"));
        assert!(unexpected.iter().any(|t| t == "RVCItem"));
    }

    #[test]
    fn missing_open_tables_are_skipped_not_fatal() {
        let h = Harness::new();
        h.source
            .add("TmpInvoice", vec![row(&[("CheckNo", RawValue::I32(5))])]);
        let report = h.engine().run_open_checks(&bg()).unwrap();
        assert!(report.tables_missing() > 0);
        assert!(!report.missing_tables.iter().any(|t| t == "TmpInvoice"));
        assert_eq!(h.sink.rows("TmpInvoice").len(), 1);
    }

    #[test]
    fn never_sync_columns_are_never_selected() {
        let h = site();
        h.engine().run_open_checks(&bg()).unwrap();
        assert!(
            h.source
                .queries()
                .iter()
                .filter(|q| q.table == "TmpInvoice")
                .all(|q| q
                    .exclude_columns
                    .as_ref()
                    .unwrap()
                    .iter()
                    .any(|c| c == "CardNumber"))
        );
    }
}

mod reset {
    use super::*;

    fn site() -> Harness {
        let h = Harness::new();
        h.source
            .table("TmpInvoice")
            .table("TmpTrn")
            .table("Invoice")
            .table("InvoiceCxl")
            .table("VoidTrn")
            .table("ReOpenLog");
        h.source.add(
            "TmpInvoice",
            vec![row(&[
                ("CheckNo", RawValue::I32(100)),
                ("OpenTime", local(0, 10)),
                ("SubTotal", dec("50000")),
            ])],
        );
        h.source.add(
            "TmpTrn",
            vec![row(&[
                ("TrnSeq", RawValue::I32(1)),
                ("CheckNo", RawValue::I32(100)),
                ("ItemPrice", dec("50000")),
            ])],
        );
        h.source.add(
            "Invoice",
            vec![
                row(&[
                    ("CheckNo", RawValue::I32(1)),
                    ("CloseTime", local(0, 10)),
                    ("SubTotal", dec("10")),
                ]),
                row(&[
                    ("CheckNo", RawValue::I32(2)),
                    ("CloseTime", local(400, 10)),
                    ("SubTotal", dec("20")),
                ]),
            ],
        );
        h.source.add(
            "DefRVCList",
            vec![row(&[
                ("RVCNo", RawValue::I32(1)),
                ("RVCName", RawValue::from("Cafe")),
                ("Picture", RawValue::Bytes(vec![1, 2, 3])),
            ])],
        );
        h
    }

    #[test]
    fn reset_makes_the_next_snapshot_resend_every_row_and_blob_once() {
        let h = site();
        let e = h.engine();
        assert!(e.run_initial_snapshot(&bg()).unwrap().complete);
        h.sink.clear();
        h.sink.clear_blobs();
        e.reset(&bg()).unwrap();
        let again = e.run_initial_snapshot(&bg()).unwrap();
        assert!(again.complete);
        let inv: Vec<String> = h
            .sink
            .rows("Invoice")
            .iter()
            .map(|r| key_of(r, "check_no"))
            .collect();
        assert_eq!(sorted(inv), ["1", "2"]);
        assert_eq!(h.sink.rows("TmpInvoice").len(), 1);
        assert_eq!(h.sink.rows("TmpTrn").len(), 1);
        assert_eq!(h.sink.rows("DefRVCList").len(), 1);
        assert_eq!(h.sink.blobs().len(), 1, "the blob shadow is forgotten too");
        h.sink.clear();
        assert_eq!(e.run_initial_snapshot(&bg()).unwrap().report.batches, 0);
    }

    #[test]
    fn reset_keeps_the_ledger_watermark_so_the_tail_does_not_resend_old_rows() {
        let h = site();
        let e = h.engine();
        e.run_initial_snapshot(&bg()).unwrap();
        e.reset(&bg()).unwrap();
        e.run_initial_snapshot(&bg()).unwrap();
        h.sink.clear();
        h.source.add(
            "Invoice",
            vec![row(&[
                ("CheckNo", RawValue::I32(3)),
                ("CloseTime", local(0, 10)),
                ("SubTotal", dec("30")),
            ])],
        );
        e.run_ledger_tail(&bg()).unwrap();
        let inv: Vec<String> = h
            .sink
            .rows("Invoice")
            .iter()
            .map(|r| key_of(r, "check_no"))
            .collect();
        assert_eq!(inv, ["3"]);
    }

    #[test]
    fn reset_is_safe_to_repeat_and_on_an_empty_state() {
        let h = site();
        let e = h.engine();
        e.reset(&bg()).unwrap();
        e.reset(&bg()).unwrap();
        assert!(e.run_initial_snapshot(&bg()).unwrap().complete);
        assert_eq!(h.sink.rows("Invoice").len(), 2);
    }
}

mod table_isolation {
    use super::*;

    fn site() -> Harness {
        let h = Harness::new();
        h.source.table("Invoice").table("ReOpenLog");
        h.source.add(
            "Invoice",
            vec![row(&[
                ("CheckNo", RawValue::I32(1)),
                ("CloseTime", local(0, 10)),
                ("SubTotal", dec("100")),
            ])],
        );
        h.source.add(
            "DefRVCList",
            vec![row(&[
                ("RVCNo", RawValue::I32(1)),
                ("RVCName", RawValue::from("Cafe")),
                ("Picture", RawValue::Null),
            ])],
        );
        h
    }

    #[test]
    fn a_broken_ledger_table_does_not_hold_back_the_initial_load_and_loads_once_it_reads_again() {
        let h = site();
        h.source
            .schema_broken
            .lock()
            .unwrap()
            .push("Invoice".to_owned());
        let e = h.engine();
        let first = e.run_initial_snapshot(&bg()).unwrap();
        assert!(first.complete);
        assert_eq!(first.report.table_faults.len(), 1);
        assert_eq!(first.report.table_faults[0].table, "Invoice");
        assert_eq!(h.sink.rows("DefRVCList").len(), 1);
        assert!(h.sink.rows("Invoice").is_empty());

        assert_eq!(
            e.run_ledger_load_resume(&bg()).unwrap().table_faults.len(),
            1
        );
        h.source.schema_broken.lock().unwrap().clear();
        let resumed = e.run_ledger_load_resume(&bg()).unwrap();
        assert!(resumed.table_faults.is_empty());
        assert_eq!(h.sink.rows("Invoice").len(), 1);
        h.sink.clear();
        assert_eq!(e.run_ledger_load_resume(&bg()).unwrap().batches, 0);
    }

    #[test]
    fn a_broken_master_table_is_reported_and_scanned_again_on_the_next_run() {
        let h = site();
        h.source
            .schema_broken
            .lock()
            .unwrap()
            .push("DefRVCList".to_owned());
        let e = h.engine();
        let report = e.run_masters(&bg()).unwrap();
        assert_eq!(report.table_faults.len(), 1);
        assert!(report.table_faults[0].error.is_schema_fault());
        h.source.schema_broken.lock().unwrap().clear();
        assert!(e.run_masters(&bg()).unwrap().table_faults.is_empty());
        assert_eq!(h.sink.rows("DefRVCList").len(), 1);
    }

    #[test]
    fn any_other_failure_still_ends_the_job() {
        let h = site();
        *h.source.busy_after.lock().unwrap() = Some(0);
        assert!(h.engine().run_masters(&bg()).unwrap_err().is_busy());
    }
}

mod null_keys {
    use super::*;

    fn para(seq: i32, rvc: RawValue, name: &str, value: &str) -> VendorRow {
        row(&[
            ("nSeq", RawValue::I32(seq)),
            ("RVC", rvc),
            ("ParaName", RawValue::from(name)),
            ("ParaStr", RawValue::from(value)),
            ("ParaType", RawValue::from("S")),
            ("ParaDesc", RawValue::from("d")),
            ("Status", RawValue::I32(1)),
        ])
    }

    fn value_of(r: &crate::contracts::wire::SyncRow) -> String {
        r.columns
            .get("value")
            .cloned()
            .flatten()
            .unwrap_or_default()
    }

    #[test]
    fn null_rvc_rows_are_sent_as_rvc_0_instead_of_rejected() {
        let h = Harness::new();
        h.source.add(
            "GlobPara",
            vec![
                para(1, RawValue::Null, "A", "1"),
                para(2, RawValue::I32(3), "A", "3"),
            ],
        );
        let report = h.engine().run_masters(&bg()).unwrap();
        assert!(report.rejected.is_empty());
        let keys: Vec<String> = h
            .sink
            .rows("GlobPara")
            .iter()
            .map(|r| format!("{}/{}", key_of(r, "rvc_no"), key_of(r, "key")))
            .collect();
        assert_eq!(sorted(keys), ["0/A", "3/A"]);
    }

    #[test]
    fn explicit_rvc_0_row_wins_over_a_null_rvc_row_of_the_same_key() {
        let h = Harness::new();
        h.source.add(
            "GlobPara",
            vec![
                para(1, RawValue::Null, "A", "store"),
                para(2, RawValue::I32(0), "A", "explicit"),
            ],
        );
        let e = h.engine();
        let report = e.run_masters(&bg()).unwrap();
        assert!(report.rejected.is_empty());
        assert_eq!(report.null_keys_shadowed, 1);
        assert_eq!(report.duplicate_keys, 0);
        let rows = h.sink.rows("GlobPara");
        assert_eq!(rows.len(), 1);
        assert_eq!(value_of(&rows[0]), "explicit");
        h.sink.clear();
        assert_eq!(e.run_masters(&bg()).unwrap().batches, 0);
    }

    #[test]
    fn removing_the_explicit_row_brings_the_store_wide_value_back_without_a_tombstone() {
        let h = Harness::new();
        h.source.add(
            "GlobPara",
            vec![
                para(1, RawValue::Null, "A", "store"),
                para(2, RawValue::I32(0), "A", "explicit"),
            ],
        );
        let e = h.engine();
        e.run_masters(&bg()).unwrap();
        h.sink.clear();
        h.source.remove("GlobPara", |r| int(r, "nSeq") == Some(2));
        let report = e.run_masters(&bg()).unwrap();
        assert_eq!(report.null_keys_shadowed, 0);
        assert!(h.sink.tombstones("GlobPara").is_empty());
        let rows = h.sink.rows("GlobPara");
        assert_eq!(rows.len(), 1);
        assert_eq!(value_of(&rows[0]), "store");
    }

    #[test]
    fn rows_with_a_null_later_key_column_load_and_rescan_without_failing_the_session() {
        let h = Harness::new();
        h.source.table("Invoice").table("ReOpenLog");
        for i in 1..=7i64 {
            h.source.add(
                "ReOpenLog",
                vec![row(&[
                    ("CheckNo", RawValue::I32(i as i32)),
                    ("OpenTime", local(0, i as u32)),
                    (
                        "CloseTime",
                        if i % 2 == 0 {
                            local(1, i as u32)
                        } else {
                            RawValue::Null
                        },
                    ),
                    ("ReOpenTime", local(2, i as u32)),
                ])],
            );
        }
        h.source.add(
            "ReOpenLog",
            vec![row(&[
                ("CheckNo", RawValue::I32(3)),
                ("OpenTime", local(0, 3)),
                ("CloseTime", local(1, 3)),
                ("ReOpenTime", local(2, 3)),
            ])],
        );
        let e = h.engine();
        let first = e.run_initial_snapshot(&bg()).unwrap();
        assert!(first.complete);
        assert!(first.report.rejected.is_empty());
        assert_eq!(h.sink.rows("ReOpenLog").len(), 8);
        let again = e.run_masters(&bg()).unwrap();
        assert!(again.table_faults.is_empty());
        assert_eq!(h.sink.rows("ReOpenLog").len(), 8);
    }
}

#[test]
fn every_captured_table_is_sent_under_the_vendor_name_the_cloud_resolves_back_to_it() {
    let plan = CapturePlan::build(catalog(), &opts()).unwrap();
    let mut targets: Vec<String> = catalog()
        .tables
        .iter()
        .filter(|t| t.is_classified() && t.has_pg_target())
        .filter_map(|t| t.pg_target.as_ref().map(|s| s.to_lowercase()))
        .collect();
    targets.sort();
    assert!(
        targets.windows(2).any(|w| w[0] == w[1]),
        "a pg_target is not a unique name for a batch"
    );
    for p in plan.all() {
        let found = catalog()
            .find(&CaptureContext::target_of(p, None).vendor_table)
            .unwrap();
        assert_eq!(found.table, p.table.table, "{}", p.name());
    }
}
