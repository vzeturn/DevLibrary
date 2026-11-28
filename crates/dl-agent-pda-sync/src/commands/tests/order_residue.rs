//! The residue SQL is the .NET agent's text (`SqlOrderResidueSource.cs`), and the cleanup only
//! deletes what the order verbs are already granted, never a sent info row (RT-1345).

use super::super::batches as b;
use super::super::sql_order_residue::*;
use super::super::sql_table::lock_parameters;
use super::sql::{bound, free};
use dl_tds::Param;

#[test]
fn cleanup_sql_only_deletes_what_the_order_verbs_are_already_granted_and_never_a_sent_info_row() {
    assert!(DROP_INFO.contains("ISNULL(PToOrder, 0) = 0"));
    assert!(DROP_INFO.contains("POrderTime IS NULL"));
    assert!(EMPTY_CHECKS.contains("WSID = 'NTPDA'"));
    assert!(EMPTY_CHECKS.contains("Status = 1"));
    assert!(CLOSE_OPEN_LOG.contains("NOT EXISTS"));
    assert!(ORPHAN_SEND_AUDITS.starts_with("SELECT"));
    for read in [
        APPLIED_LOG,
        EMPTY_CHECKS,
        ORPHAN_INFO,
        STALE_OPEN_LOGS,
        ORPHAN_SEND_AUDITS,
        PDA_SEND_ORDER_EXISTS,
        STILL_EMPTY,
    ] {
        assert!(read.starts_with("SELECT"), "{read}");
    }
}

#[test]
fn every_statement_binds_exactly_its_variables() {
    let mut guard = lock_parameters(4, 400000101);
    guard.push(Param::new("MinAge", 120));
    let mut cancel = lock_parameters(4, 400000101);
    cancel.push(super::super::sql::text_param("CancelBy", Some("tester")));
    let info = [
        Param::new("CheckNo", 400000101i64),
        Param::new("TrnNo", 1i64),
    ];
    let age = [Param::new("MinAge", 120)];
    for (sql, params) in [
        (EMPTY_CHECKS, &age[..]),
        (STILL_EMPTY, &guard[..]),
        (b::CANCEL_ORDER, &cancel[..]),
        (DROP_INFO, &info[..]),
        (CLOSE_OPEN_LOG, &lock_parameters(4, 400000101)[..]),
        (APPLIED_LOG, &[][..]),
        (ORPHAN_INFO, &[][..]),
        (STALE_OPEN_LOGS, &[][..]),
        (ORPHAN_SEND_AUDITS, &[][..]),
        (PDA_SEND_ORDER_EXISTS, &[][..]),
    ] {
        assert_eq!(free(sql, true), bound(params), "{sql}");
    }
}
