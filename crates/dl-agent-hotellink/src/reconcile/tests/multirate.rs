//! Port of Go `reconcile_multirate_test.go` (RT-1474): items of the same room type and rate
//! plan but different prices, guests or breakfast keep their own folio group and price.

use super::*;
use crate::jobmodel::RateNightExtra;
use crate::reconcile::build::{room_item_key, room_key, room_signature_hash};
use crate::reconcile::merge_rooms_by_item;

/// `multiRateJob`: Booking.com 5288911547, two DLX01 / rp-std rooms at 724,557 and
/// 913,557 VND that used to be merged, so both folios got the first room's price.
fn multi_rate_job() -> Job {
    let mut job = save_job();
    job.check_in = "2026-11-27".into();
    job.check_out = "2026-11-28".into();
    let mut base = room0(&mut job).clone();
    base.quantity = 1;
    base.rate_plan_id = "rp-std".into();
    let (mut r1, mut r2) = (base.clone(), base);
    r1.rates = Some(vec![rn("2026-11-27", 724557.0)]);
    r2.rates = Some(vec![rn("2026-11-27", 913557.0)]);
    job.rooms = Some(vec![r1, r2]);
    job
}

#[test]
fn multi_rate_rooms_keep_own_rates() {
    let repo = FakeRepo::new();
    new_reconciler(&repo)
        .apply(&multi_rate_job())
        .expect("apply");
    let s = repo.st();
    assert_eq!(s.inserted.len(), 2, "one folio per room");
    let mut got: Vec<f64> = Vec::new();
    for ins in &s.inserted {
        assert_eq!(ins.rates.len(), 1, "{:?}", ins.rates);
        assert_eq!(
            ins.f.rate_amount, ins.rates[0].rate_amount,
            "folio rate must be its own night rate"
        );
        got.push(ins.rates[0].rate_amount);
    }
    got.sort_by(f64::total_cmp);
    assert_eq!(got, [724557.0, 913557.0], "each room keeps its own price");
    assert_ne!(
        s.inserted[0].f.user_info1, s.inserted[1].f.user_info1,
        "folio item keys must differ"
    );
}

#[test]
fn multi_rate_keys_match_go_agent() {
    // Expected values printed by the Go agent's roomSignatureHash (sr-hotel-link-api
    // 0339a9f) for the same rooms, so folios either agent created are matched by the other.
    let repo = FakeRepo::new();
    new_reconciler(&repo)
        .apply(&multi_rate_job())
        .expect("apply");
    let keys: Vec<String> = repo
        .st()
        .inserted
        .iter()
        .map(|i| i.f.user_info1.clone())
        .collect();
    assert_eq!(
        keys,
        [
            "rt:DLX01|rp:rp-std|v:e7f122ef#0",
            "rt:DLX01|rp:rp-std|v:9128f06b#0"
        ]
    );
}

#[test]
fn signature_hash_matches_go_formatting() {
    // Go `%t` and `%.2f` (a tie rounds to even, 0.005 rounds up) and the per-night extras.
    let room = Room {
        adults: 2,
        rates: Some(vec![
            RateNight {
                date: "2026-11-27".into(),
                rate: 1500000.125,
                extras: vec![
                    RateNightExtra {
                        kind: "X".into(),
                        amount: 1.5,
                    },
                    RateNightExtra::default(),
                ],
            },
            rn("2026-11-28", 0.005),
        ]),
        ..Room::default()
    };
    assert_eq!(room_signature_hash(&room), "8ecd276d");
}

#[test]
fn merge_splits_different_rates_keeps_equal_ones_together() {
    let mut rooms = multi_rate_job().rooms.expect("rooms");
    rooms.push(rooms[0].clone()); // identical to room 0 -> merges with it
    let got = merge_rooms_by_item(&rooms);
    assert_eq!(got.len(), 2, "two variants");
    assert_eq!((got[0].quantity, got[1].quantity), (2, 1));
    assert_ne!(
        room_key(&got[0]),
        room_key(&got[1]),
        "distinct variant keys"
    );
}

#[test]
fn single_variant_keeps_legacy_key() {
    let mut job = save_job();
    let mut proto = room0(&mut job).clone();
    proto.quantity = 1;
    let got = merge_rooms_by_item(&vec![proto; 3]);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].quantity, 3);
    assert!(
        got[0].variant.is_empty(),
        "no variant: {:?}",
        got[0].variant
    );
    assert_eq!(
        room_key(&got[0]),
        room_item_key(&got[0].room_type_code, &got[0].rate_plan_id),
        "non-conflicting rooms keep the legacy key"
    );
}

#[test]
fn multi_rate_rerun_is_noop() {
    let repo = FakeRepo::new();
    let r = new_reconciler(&repo);
    let job = multi_rate_job();
    r.apply(&job).expect("book");
    reset_to_inserted(&repo, 2);
    r.apply(&job).expect("re-apply");
    assert_eq!(
        counts(&repo),
        (0, 0, 0, 0),
        "the same job again changes nothing (no cancel and recreate)"
    );
}

#[test]
fn multi_rate_modify_updates_each_folio_with_its_own_rates() {
    let repo = FakeRepo::new();
    let r = new_reconciler(&repo);
    let mut job = multi_rate_job();
    r.apply(&job).expect("book");
    reset_to_inserted(&repo, 2);
    job.guest_name = "TRAN THI B".into();
    r.apply(&job).expect("modify");
    let s = repo.st();
    assert!(
        s.inserted.is_empty() && s.cancelled.is_empty(),
        "a guest change keeps both folios"
    );
    assert_eq!(s.updates.len(), 2, "both folios updated: {:?}", s.updates);
    assert!(
        s.replaced_rates.is_empty(),
        "each folio already has its own rates: {:?}",
        s.replaced_rates
    );
}
