//! Port of `reconcile_splititems_test.go`: HL sends one room item per physical room.

use super::*;
use crate::reconcile::{merge_rooms_by_item, merge_rooms_conflicts};

/// `splitItemsJob`: `n` identical quantity-1 room items.
fn split_items_job(n: usize) -> Job {
    let mut job = save_job();
    let mut proto = room0(&mut job).clone();
    proto.quantity = 1;
    job.rooms = Some(vec![proto; n]);
    job
}

fn split_rooms(n: usize) -> Vec<Room> {
    split_items_job(n).rooms.expect("rooms")
}

/// `splitItemFolios`: `n` unassigned DLX01 folios with distinct seq keys.
fn split_item_repo(n: usize) -> Arc<FakeRepo> {
    let folios = (0..n)
        .map(|i| Folio {
            id: 5 + i as i32,
            folio_num: 100000.0 + i as f64,
            room_code: String::new(),
            user_info1: format!("rt:DLX01#{i}"),
            ..existing_folio()
        })
        .collect();
    repo_with(folios)
}

#[test]
fn split_items_create_one_folio_per_item() {
    let repo = FakeRepo::new();
    new_reconciler(&repo)
        .apply(&split_items_job(4))
        .expect("apply");
    assert_eq!(repo.st().inserted.len(), 4);
}

#[test]
fn split_items_folios_get_distinct_seq_keys() {
    let repo = FakeRepo::new();
    new_reconciler(&repo)
        .apply(&split_items_job(4))
        .expect("apply");
    let mut keys: Vec<String> = repo
        .st()
        .inserted
        .iter()
        .map(|i| i.f.user_info1.clone())
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        ["rt:DLX01#0", "rt:DLX01#1", "rt:DLX01#2", "rt:DLX01#3"]
    );
}

#[test]
fn merge_keeps_zero_quantity() {
    let mut rooms = split_rooms(3);
    for r in &mut rooms {
        r.quantity = 0;
    }
    let got = merge_rooms_by_item(&rooms);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].quantity, 0);
}

#[test]
fn merge_sums_quantity_across_items() {
    let mut rooms = split_rooms(2);
    rooms[0].quantity = 2;
    let got = merge_rooms_by_item(&rooms);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].quantity, 3);
}

#[test]
fn merge_keeps_distinct_keys_separate() {
    let mut rooms = split_rooms(2);
    rooms[1].room_type_code = "STD01".into();
    assert_eq!(merge_rooms_by_item(&rooms).len(), 2);
}

#[test]
fn merge_rooms_conflicts_detects_divergent_items() {
    let mut rooms = split_rooms(2);
    rooms[1].adults = 1;
    assert_eq!(merge_rooms_conflicts(&rooms).len(), 1);
}

#[test]
fn merge_rooms_conflicts_silent_when_identical() {
    assert!(merge_rooms_conflicts(&split_rooms(4)).is_empty());
}

#[test]
fn split_items_modify_no_change_does_not_cancel() {
    let repo = split_item_repo(4);
    new_reconciler(&repo)
        .apply(&split_items_job(4))
        .expect("apply");
    let s = repo.st();
    assert!(s.cancelled.is_empty() && s.inserted.is_empty());
}

#[test]
fn split_items_modify_increase_creates_only_missing() {
    let repo = split_item_repo(2);
    new_reconciler(&repo)
        .apply(&split_items_job(4))
        .expect("apply");
    let s = repo.st();
    assert_eq!(s.inserted.len(), 2);
    assert!(s.cancelled.is_empty());
}

#[test]
fn split_items_modify_decrease_cancels_surplus_once() {
    let repo = split_item_repo(4);
    new_reconciler(&repo)
        .apply(&split_items_job(2))
        .expect("apply");
    let s = repo.st();
    let total: usize = s.cancelled.iter().map(Vec::len).sum();
    assert_eq!(total, 2);
    assert!(s.inserted.is_empty());
}
