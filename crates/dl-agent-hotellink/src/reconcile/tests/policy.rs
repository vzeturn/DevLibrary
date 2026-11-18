//! Hub cancel/modify policy cases.

use super::*;
use crate::jobmodel::CancelPolicy;

fn assigned() -> Folio {
    Folio {
        room_code: "101".into(),
        book_status: 1,
        folio_status: 1,
        ..Folio::default()
    }
}

#[test]
fn reconciler_policy_snapshot_atomic() {
    let r = new_reconciler(&FakeRepo::new());
    let (p, from_hub) = r.policy_view();
    assert!(!from_hub && p == CancelPolicy::default());
    assert!(!r.policy_from_hub());

    let want = CancelPolicy {
        on_cancel: OpPolicy {
            allow_when_room_assigned: true,
            allow_when_checked_in: true,
            ..OpPolicy::default()
        },
        ..CancelPolicy::default()
    };
    r.set_policy(want, true);
    assert_eq!(r.policy_view(), (want, true));
    assert!(r.policy_from_hub());

    r.set_policy(CancelPolicy::default(), true);
    assert!(!r.policy_view().0.on_cancel.allow_when_room_assigned, "overwrite works");
}

#[test]
fn has_conflicting_folios_hub_policy_room_assigned() {
    let folios = [assigned()];
    let allow = OpPolicy {
        allow_when_room_assigned: true,
        ..OpPolicy::default()
    };
    let r = new_reconciler(&FakeRepo::new());
    r.set_policy(
        CancelPolicy {
            on_cancel: allow,
            ..CancelPolicy::default()
        },
        true,
    );
    assert!(!r.has_conflicting_folios(&folios, allow).expect("ok"));

    let r2 = new_reconciler(&FakeRepo::new());
    r2.set_policy(CancelPolicy::default(), true);
    assert!(
        r2.has_conflicting_folios(&folios, OpPolicy::default())
            .expect("ok")
    );
}

#[test]
fn has_conflicting_folios_checked_in_still_blocks_by_default() {
    let folios = [Folio {
        folio_status: 2,
        book_status: 1,
        ..Folio::default()
    }];
    let r = new_reconciler(&FakeRepo::new());
    let op = OpPolicy {
        allow_when_room_assigned: true,
        allow_when_abnormal_book_status: true,
        ..OpPolicy::default()
    };
    r.set_policy(
        CancelPolicy {
            on_cancel: op,
            ..CancelPolicy::default()
        },
        true,
    );
    assert!(r.has_conflicting_folios(&folios, op).expect("ok"), "checked-in blocks");

    let op2 = OpPolicy {
        allow_when_checked_in: true,
        ..op
    };
    r.set_policy(
        CancelPolicy {
            on_cancel: op2,
            ..CancelPolicy::default()
        },
        true,
    );
    assert!(!r.has_conflicting_folios(&folios, op2).expect("ok"));
}

#[test]
fn has_conflicting_folios_legacy_reads_db() {
    let folios = [assigned()];
    let repo = FakeRepo::new();
    repo.st().global_params.insert("CancelByRoom".into(), 0);
    let r = new_reconciler(&repo);
    assert!(
        !r.has_conflicting_folios(&folios, OpPolicy::default())
            .expect("ok")
    );
    repo.st().global_params.insert("CancelByRoom".into(), 1);
    assert!(
        r.has_conflicting_folios(&folios, OpPolicy::default())
            .expect("ok")
    );
}

#[test]
fn policy_split_room_assigned_cancel_vs_modify() {
    let folio = assigned();
    let r = new_reconciler(&FakeRepo::new());
    r.set_policy(
        CancelPolicy {
            on_cancel: OpPolicy {
                allow_when_room_assigned: true,
                ..OpPolicy::default()
            },
            on_modify: OpPolicy::default(),
        },
        true,
    );
    let (p, _) = r.policy_view();
    assert!(
        !r.has_conflicting_folios(std::slice::from_ref(&folio), p.on_cancel)
            .expect("ok")
    );
    assert!(r.is_protected_folio(&folio, p.on_modify));
}
