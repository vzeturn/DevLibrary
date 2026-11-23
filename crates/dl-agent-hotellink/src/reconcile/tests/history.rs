//! RT-0702 (Go `reconcile_history_test.go`): every write path leaves a FolioLog row, new
//! folios carry the agent clerk, and a row that cannot be written is reported as a gap.

use super::*;
use crate::jobmodel::ACTION_CANCEL;
use crate::pms::{
    AGENT_CANCEL_CLERK_ID, AGENT_CLERK_ID, FOLIO_LOG_CANCEL, FOLIO_LOG_CREATE, FOLIO_LOG_RECREATE,
    FOLIO_LOG_UPDATE,
};

fn cancel_job() -> Job {
    Job {
        action: ACTION_CANCEL.into(),
        ..save_job()
    }
}

#[test]
fn new_booking_logs_create_and_carries_agent_clerk() {
    let repo = FakeRepo::new();
    let res = new_reconciler(&repo).apply(&save_job()).expect("apply");
    let s = repo.st();
    assert_eq!(s.inserted.len(), 1);
    assert_eq!(s.inserted[0].f.reservation_clerk_id, AGENT_CLERK_ID);
    assert_eq!(s.inserted[0].f.last_change_clerk_id, AGENT_CLERK_ID);
    assert_eq!(s.folio_logs.len(), 1, "{:?}", s.folio_logs);
    let l = &s.folio_logs[0];
    assert_eq!(l.field, FOLIO_LOG_CREATE);
    assert_eq!(l.folio_num, s.inserted[0].f.folio_num);
    assert_eq!(l.old, "");
    for part in [
        "hl=123456",
        "ext=EXT-1",
        "type=DLX01",
        "stay=2026-06-12..2026-06-14",
        "adults=3",
        "children=1",
        "rates=2026-06-12=1500000;2026-06-13=1600000",
    ] {
        assert!(l.new.contains(part), "{part} in {}", l.new);
    }
    assert!(!l.new.contains("NGUYEN"), "no guest identity: {}", l.new);
    assert!(res.audit_gaps.is_empty());
}

#[test]
fn recreating_an_agent_cancelled_booking_logs_recreate() {
    let repo = FakeRepo::new();
    repo.st().agent_cancelled = vec![99001.0, 99002.0];
    new_reconciler(&repo).apply(&save_job()).expect("apply");
    let s = repo.st();
    assert_eq!(s.folio_logs.len(), 1);
    assert_eq!(s.folio_logs[0].field, FOLIO_LOG_RECREATE);
    assert_eq!(s.folio_logs[0].old, "cancelled folios 99001,99002");
}

#[test]
fn cancel_logs_one_row_per_folio_on_any_state() {
    let repo = repo_with(vec![
        Folio {
            room_code: String::new(),
            ..existing_folio()
        },
        Folio {
            id: 6,
            folio_num: 100001.0,
            room_code: String::new(),
            ..existing_folio()
        },
    ]);
    new_reconciler(&repo).apply(&cancel_job()).expect("apply");
    let s = repo.st();
    assert_eq!(s.cancelled.len(), 1);
    assert!(
        s.folio_logs.is_empty(),
        "cancel rows use the any-state call"
    );
    assert_eq!(s.folio_logs_any.len(), 2, "{:?}", s.folio_logs_any);
    for l in &s.folio_logs_any {
        assert_eq!(l.field, FOLIO_LOG_CANCEL);
        assert_eq!(l.old, "live");
        assert!(l.new.contains("HL cancel"), "{}", l.new);
        assert!(l.new.contains("hl=123456 ext=EXT-1"), "{}", l.new);
    }
}

#[test]
fn modify_logs_one_update_row_without_guest_values() {
    let repo = repo_with(vec![Folio {
        room_code: String::new(),
        ..existing_folio()
    }]);
    let mut job = save_job();
    job.guest_phone = "0912".into();
    rates0(&mut job); // keep rates as the job
    room0(&mut job).adults = 2;
    new_reconciler(&repo).apply(&job).expect("apply");
    let s = repo.st();
    let upd: Vec<_> = s
        .folio_logs
        .iter()
        .filter(|l| l.field == FOLIO_LOG_UPDATE)
        .collect();
    assert_eq!(upd.len(), 1, "{:?}", s.folio_logs);
    let l = upd[0];
    assert!(l.old.contains("NumAdult=3"), "{}", l.old);
    assert!(l.old.contains("TelNumM=(guest)"), "{}", l.old);
    assert!(l.new.starts_with("hl=123456: "), "{}", l.new);
    assert!(l.new.contains("NumAdult=2"), "{}", l.new);
    assert!(l.new.contains("TelNumM=(guest changed)"), "{}", l.new);
    assert!(!l.new.contains("0912") && !l.old.contains("0901"), "{l:?}");
}

#[test]
fn unchanged_modify_logs_nothing() {
    let repo = repo_with(vec![existing_folio()]);
    new_reconciler(&repo).apply(&save_job()).expect("apply");
    let s = repo.st();
    assert!(s.folio_logs.is_empty() && s.folio_logs_any.is_empty());
}

#[test]
fn failed_history_rows_are_reported_as_audit_gaps_not_errors() {
    let repo = FakeRepo::new();
    repo.st().folio_log_err = Some("log table locked".into());
    let res = new_reconciler(&repo).apply(&save_job()).expect("apply ok");
    assert!(res.completed);
    assert_eq!(repo.st().inserted.len(), 1, "folio still created");
    assert_eq!(res.audit_gaps.len(), 1, "{:?}", res.audit_gaps);
    assert!(
        res.audit_gaps[0].contains("HL:Create"),
        "{}",
        res.audit_gaps[0]
    );
    assert!(res.audit_gaps[0].contains("log table locked"));

    // The next apply starts with a clean list.
    repo.st().folio_log_err = None;
    let res2 = new_reconciler(&repo)
        .apply(&Job {
            action: ACTION_CANCEL.into(),
            ..save_job()
        })
        .expect("apply");
    assert!(res2.audit_gaps.is_empty(), "{:?}", res2.audit_gaps);
}

#[test]
fn agent_cancel_clerk_fits_varchar3() {
    assert_eq!(AGENT_CANCEL_CLERK_ID.len(), 3);
    assert_eq!(AGENT_CLERK_ID.len(), 3);
    assert_ne!(AGENT_CANCEL_CLERK_ID, AGENT_CLERK_ID);
}
