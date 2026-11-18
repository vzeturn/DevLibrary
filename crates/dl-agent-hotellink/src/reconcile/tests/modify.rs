//! Modify (existing folio) cases.

use super::*;

/// A folio of `saveJob()`'s booking (DLX01, unassigned unless `room_code` is set).
fn base(id: i32, folio_num: f64, room_code: &str, key: &str) -> Folio {
    Folio {
        id,
        folio_num,
        room_code: room_code.into(),
        user_info1: key.into(),
        ..existing_folio()
    }
}

fn unassigned() -> Folio {
    base(5, 100000.0, "", "rt:DLX01#0")
}

#[test]
fn modify_no_change_is_noop() {
    let repo = repo_with(vec![existing_folio()]);
    new_reconciler(&repo).apply(&job_with_item()).expect("apply");
    assert_eq!(counts(&repo), (0, 0, 0, 0), "no change must be a no-op");
    assert_eq!(repo.st().confirm_num_calls, 0, "modify takes no new ConfirmNum");
}

#[test]
fn lock_by_room_skips_modify() {
    let repo = repo_with(vec![existing_folio()]);
    repo.st().global_params.insert("CancelByRoom".into(), 1);
    let mut job = job_with_item();
    rates0(&mut job)[0].rate = 1700000.0;
    let res = new_reconciler(&repo).apply(&job).expect("apply");
    assert_eq!(counts(&repo), (0, 0, 0, 0), "lock by room skips entirely");
    assert!(res.completed && !res.needs_manual, "benign skip: {res:?}");
    assert!(!res.reason.is_empty(), "skip carries a reason");
    assert_eq!(res.folios.len(), 1, "reports the current PMS folio");
}

#[test]
fn booker_fallback_prevents_double_when_ext_conf_mismatch() {
    let repo = FakeRepo::new();
    {
        let mut s = repo.st();
        s.global_params.insert("CancelByRoom".into(), 1);
        let mut f = existing_folio();
        f.booker = "EXT-1".into();
        s.folios_by_ext = Some(HashMap::new());
        s.folios_by_booker = Some(HashMap::from([("EXT-1".to_owned(), vec![f.clone()])]));
        s.folio_rates
            .insert(100000, rates_for(100000.0, f.arrival_date));
    }
    let res = new_reconciler(&repo).apply(&job_with_item()).expect("apply");
    assert_eq!(repo.st().inserted.len(), 0, "no double folio");
    assert!(res.completed && !res.needs_manual, "lock-by-room skip: {res:?}");
}

#[test]
fn lock_by_room_no_room_assigned_runs_diff() {
    let repo = repo_with(vec![unassigned()]);
    repo.st().global_params.insert("CancelByRoom".into(), 1);
    let mut job = job_with_item();
    rates0(&mut job)[0].rate = 1700000.0;
    new_reconciler(&repo).apply(&job).expect("apply");
    assert_eq!(repo.st().updates.len(), 1, "unassigned → diff runs");
}

#[test]
fn room_assigned_folio_skipped_green_without_flag() {
    let repo = repo_with(vec![existing_folio()]);
    let mut job = job_with_item();
    rates0(&mut job)[0].rate = 1700000.0;
    let res = new_reconciler(&repo).apply(&job).expect("apply");
    assert!(res.completed && !res.needs_manual, "green skip: {res:?}");
    assert!(res.reason.contains("bỏ qua"), "reason: {}", res.reason);
    let (u, i, c, _) = counts(&repo);
    assert_eq!((u, i, c), (0, 0, 0));
}

#[test]
fn modify_room_assigned_folio_with_cancel_by_room_skipped() {
    let repo = repo_with(vec![base(5, 100000.0, "205", "rt:DLX01#0")]);
    repo.st().global_params.insert("CancelByRoom".into(), 1);
    let mut job = job_with_item();
    rates0(&mut job)[0].rate = 1900000.0;
    let res = new_reconciler(&repo).apply(&job).expect("apply");
    assert!(res.completed && !res.needs_manual, "{res:?}");
    let (u, i, c, _) = counts(&repo);
    assert_eq!((u, i, c), (0, 0, 0));
}

#[test]
fn modify_price_only_updates_rate_not_room() {
    let repo = repo_with(vec![unassigned()]);
    let mut job = job_with_item();
    rates0(&mut job)[0].rate = 1700000.0;
    new_reconciler(&repo).apply(&job).expect("apply");
    let s = repo.st();
    assert!(s.inserted.is_empty() && s.cancelled.is_empty(), "no create/cancel");
    assert_eq!(s.updates.len(), 1);
    assert_eq!(s.updates[0].0, 100000.0);
    assert_eq!(s.updates[0].1.get("RateAmount"), Some(&Val::F64(1700000.0)));
    assert_eq!(s.replaced_rates.len(), 1, "FolioRate replaced");
}

#[test]
fn modify_guest_name_updates_name_and_additional_name() {
    let repo = repo_with(vec![unassigned()]);
    let mut job = job_with_item();
    room0(&mut job).rates = Some(rates_as_job());
    job.guest_name = "TRAN THI HONG".into();
    new_reconciler(&repo).apply(&job).expect("apply");
    let s = repo.st();
    assert_eq!(s.updates.len(), 1);
    assert_eq!(
        s.updates[0].1.get("LastName"),
        Some(&Val::Str("TRAN THI HONG".into()))
    );
    assert_eq!(s.updated_names.len(), 1);
    assert_eq!(s.updated_names[0].1, "TRAN THI HONG");
    assert!(s.replaced_rates.is_empty(), "rates unchanged");
}

#[test]
fn modify_quantity_increase() {
    let repo = repo_with(vec![existing_folio()]);
    let mut job = job_with_item();
    room0(&mut job).quantity = 2;
    new_reconciler(&repo).apply(&job).expect("apply");
    let s = repo.st();
    assert!(s.cancelled.is_empty());
    assert_eq!(s.inserted.len(), 1);
    assert_eq!(s.inserted[0].f.user_info1, "rt:DLX01#1");
    assert_eq!(s.inserted[0].f.confirm_num, 499000.0, "shared ConfirmNum");
}

#[test]
fn modify_quantity_decrease_cancels_unassigned_first() {
    let repo = repo_with(vec![
        base(5, 100000.0, "101", "rt:DLX01#0"),
        base(6, 100001.0, "", "rt:DLX01#1"),
    ]);
    new_reconciler(&repo).apply(&job_with_item()).expect("apply");
    let s = repo.st();
    assert_eq!(s.cancelled.len(), 1);
    assert_eq!(s.cancelled[0].len(), 1);
    assert_eq!(s.cancelled[0][0].id, 6, "unassigned cancelled first");
}

#[test]
fn modify_quantity_decrease_updates_survivor_not_cancelled() {
    let repo = repo_with(vec![
        base(6, 100001.0, "", "rt:DLX01#1"),
        base(5, 100000.0, "", "rt:DLX01#0"),
    ]);
    let mut job = job_with_item();
    room0(&mut job).rates = Some(vec![rn("2026-06-12", 1750000.0), rn("2026-06-13", 1750000.0)]);
    new_reconciler(&repo).apply(&job).expect("apply");
    let s = repo.st();
    assert_eq!(s.cancelled.len(), 1);
    assert_eq!(s.cancelled[0].len(), 1);
    assert_eq!(s.cancelled[0][0].id, 6);
    assert!(
        s.updates.iter().all(|u| u.0 != 100001.0),
        "never update the folio being cancelled"
    );
    assert!(s.updates.iter().any(|u| u.0 == 100000.0), "survivor updated");
}

#[test]
fn modify_room_type_change_unassigns_room() {
    let repo = repo_with(vec![unassigned()]);
    let mut job = job_with_item();
    room0(&mut job).room_type_code = "SUITE".into();
    new_reconciler(&repo).apply(&job).expect("apply");
    let s = repo.st();
    assert_eq!((s.cancelled.len(), s.inserted.len()), (1, 1), "cancel-recreate");
    assert_eq!(s.inserted[0].f.room_type_code, "SUITE");
}

#[test]
fn modify_room_type_change_atomic_on_create_failure() {
    let repo = repo_with(vec![unassigned()]);
    repo.st().insert_err = Some("insert failed (simulated)".into());
    let mut job = job_with_item();
    room0(&mut job).room_type_code = "SUITE".into();
    assert!(new_reconciler(&repo).apply(&job).is_err());
    assert!(repo.st().cancelled.is_empty(), "no cancel when create fails");
}

fn suite_folio(folio_status: i32) -> Folio {
    Folio {
        id: 9,
        folio_num: 100009.0,
        confirm_num: 499000.0,
        book_status: 1,
        folio_status,
        user_info1: "rt:SUITE#0".into(),
        room_type_code: "SUITE".into(),
        arrival_date: day(2026, 6, 12),
        departure_date: day(2026, 6, 14),
        rate_amount: 5000000.0,
        ..Folio::default()
    }
}

fn repo_a_plus(b: Folio) -> Arc<FakeRepo> {
    let repo = FakeRepo::new();
    {
        let mut s = repo.st();
        s.folios = vec![existing_folio(), b];
        s.folio_rates
            .insert(100000, rates_for(100000.0, day(2026, 6, 12)));
    }
    repo
}

#[test]
fn modify_item_removed_cancels() {
    let repo = repo_a_plus(suite_folio(1));
    new_reconciler(&repo).apply(&job_with_item()).expect("apply");
    let s = repo.st();
    assert_eq!(s.cancelled.len(), 1);
    assert_eq!(s.cancelled[0][0].id, 9);
    assert!(s.inserted.is_empty());
}

#[test]
fn modify_item_removed_protected_folio_not_cancelled() {
    let repo = repo_a_plus(suite_folio(2));
    let res = new_reconciler(&repo).apply(&job_with_item()).expect("green");
    assert!(res.completed && !res.needs_manual, "{res:?}");
    assert!(res.reason.contains("bỏ qua"), "reason: {}", res.reason);
    let s = repo.st();
    assert!(s.cancelled.is_empty() && s.inserted.is_empty());
}

#[test]
fn modify_checked_in_folio_skipped_green() {
    let repo = FakeRepo::new();
    repo.st().folios = vec![Folio {
        id: 5,
        folio_num: 100000.0,
        folio_status: 2,
        room_type_code: "DLX01".into(),
        confirm_num: 499000.0,
        ..Folio::default()
    }];
    let res = new_reconciler(&repo).apply(&save_job()).expect("no error");
    assert!(res.completed && !res.needs_manual, "{res:?}");
    assert!(res.reason.contains("bỏ qua"), "reason: {}", res.reason);
    let (u, i, c, _) = counts(&repo);
    assert_eq!((u, i, c), (0, 0, 0));
}

#[test]
fn modify_no_change_is_noop_empty_item_id() {
    let repo = FakeRepo::new();
    let r = new_reconciler(&repo);
    let job = save_job();
    r.apply(&job).expect("book");
    reset_to_inserted(&repo, 1);
    r.apply(&job).expect("re-apply");
    let user_info1 = repo.st().folios[0].user_info1.clone();
    assert_eq!(counts(&repo), (0, 0, 0, 0), "no-op (UserInfo1 {user_info1:?})");
}

#[test]
fn modify_rate_plan_distinguishes_items() {
    let repo = FakeRepo::new();
    let r = new_reconciler(&repo);
    let mut job = save_job();
    let room = |rp: &str, a: f64, b: f64| Room {
        room_type_code: "DLX01".into(),
        rate_plan_id: rp.into(),
        quantity: 1,
        adults: 2,
        rates: Some(vec![rn("2026-06-12", a), rn("2026-06-13", b)]),
        ..Room::default()
    };
    job.rooms = Some(vec![
        room("A", 1500000.0, 1600000.0),
        room("B", 2000000.0, 2100000.0),
    ]);
    r.apply(&job).expect("book");
    reset_to_inserted(&repo, 2);
    r.apply(&job).expect("re-apply");
    let s = repo.st();
    assert!(s.inserted.is_empty() && s.cancelled.is_empty(), "two rate plans → no-op");
}

/// Makes the inserted folios the live ones and clears the recorded writes.
fn reset_to_inserted(repo: &FakeRepo, want: usize) {
    let mut s = repo.st();
    assert_eq!(s.inserted.len(), want, "inserted");
    let ins = std::mem::take(&mut s.inserted);
    s.folios = ins.iter().map(|i| i.f.clone()).collect();
    s.folio_rates = ins
        .iter()
        .map(|i| (i.f.folio_num as i64, i.rates.clone()))
        .collect();
    s.cancelled.clear();
    s.updates.clear();
    s.replaced_rates.clear();
    s.confirm_num_calls = 0;
}

#[test]
fn modify_legacy_empty_prefix_folio_matches() {
    let repo = repo_with(vec![base(5, 100000.0, "", "#0")]);
    new_reconciler(&repo).apply(&save_job()).expect("apply");
    let s = repo.st();
    assert!(s.inserted.is_empty() && s.cancelled.is_empty(), "legacy '#0' matches");
}

#[test]
fn modify_empty_rates_returns_error() {
    let repo = FakeRepo::new();
    repo.st().folios = vec![Folio {
        folio_num: 100001.0,
        confirm_num: 500001.0,
        room_type_code: "DLX01".into(),
        arrival_date: day(2026, 6, 12),
        departure_date: day(2026, 6, 14),
        user_info1: "rt:DLX01#0".into(),
        ..Folio::default()
    }];
    let mut job = save_job();
    room0(&mut job).rates = None;
    assert!(new_reconciler(&repo).apply(&job).is_err(), "empty rates → error");
}
