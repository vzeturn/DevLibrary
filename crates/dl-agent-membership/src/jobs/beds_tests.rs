use super::*;
use crate::jobs::payload::{Occupancy, parse_payload};

fn room(json_room: &str) -> Room {
    parse_payload(&format!(r#"{{"rooms":[{json_room}]}}"#))
        .unwrap()
        .rooms
        .remove(0)
}

const OCC: &str = r#"{"NumAdult":2,"NumChild":0,"PricingModel":"OCCUPANCY_V1","ExtraBeds":1,
    "ExtraBedNightly":100000,"nights":[{"Date":"2026-12-01","Net":1100000,"NetExclBed":1000000}]}"#;

#[test]
fn every_decision_folds_and_says_why() {
    let occ = room(OCC);
    let d = decide_fold(&occ, MODE_FOLD_LEGACY);
    assert_eq!(d.mode, MODE_FOLD_LEGACY);
    assert_eq!(d.fold_reason, Some(FOLD_CONFIGURED));
    assert_eq!(
        d.bed_note.as_deref(),
        Some("MM: +1 giường phụ (đã gộp trong giá phòng)")
    );
    assert!(!d.use_net_excl_bed && d.extra_bed_to_write == 0 && !d.is_special());
    assert_eq!(
        decide_fold(&occ, MODE_SMILE_SPECIAL).fold_reason,
        Some(FOLD_FINGERPRINT_UNVERIFIED)
    );
    let legacy = room(r#"{"NumAdult":2,"nights":[{"Net":5}]}"#);
    let d = decide_fold(&legacy, MODE_SMILE_SPECIAL);
    assert_eq!(d.fold_reason, Some(FOLD_PAYLOAD_LEGACY));
    assert_eq!(d.bed_note, None);
}

#[test]
fn night_rate_uses_the_bed_free_rate_only_when_asked() {
    let r = room(OCC);
    assert_eq!(night_rate(&r.nights[0], false), "1100000".parse().unwrap());
    assert_eq!(night_rate(&r.nights[0], true), "1000000".parse().unwrap());
}

#[test]
fn the_note_keeps_one_bed_marker_and_fits_the_column() {
    let marker = tcvn3::encode(&bed_note_text(2));
    let base = format!("late arrival | {marker}");
    assert_eq!(strip_bed_note(Some(&base)).as_deref(), Some("late arrival"));
    assert_eq!(extract_bed_marker(Some(&base)), Some(marker.clone()));
    assert_eq!(note_bed_count(Some(&base)), Some(2));
    assert_eq!(strip_bed_note(Some(&marker)), None);
    let note = compose_note(Some(&base), Some(&bed_note_text(1)), None, None).unwrap();
    assert_eq!(
        note,
        format!("late arrival | {}", tcvn3::encode(&bed_note_text(1)))
    );
    let tight = compose_note(Some("abcdefghij"), Some(&bed_note_text(1)), Some(50), None).unwrap();
    assert_eq!(tight.chars().count(), 50.min(tight.chars().count()));
    assert!(tight.ends_with(&tcvn3::encode(&bed_note_text(1))));
    assert_eq!(compose_note(Some("  "), None, Some(5), None), None);
    assert_eq!(
        compose_note(Some("abcdefgh"), None, Some(5), None).as_deref(),
        Some("abcde")
    );
}

#[test]
fn guest_counts_mark_enf_children_or_report_a_mismatch() {
    let mut r = room(OCC);
    r.occupancy = Some(Occupancy {
        num_enf: Some(1),
        ..r.occupancy.unwrap()
    });
    let g = plan_guests(&r, true);
    assert_eq!((g.num_adult, g.num_child, g.num_enf), (2, 0, 1));
    assert_eq!((g.placeholder_adults, g.placeholder_children), (2, 1));
    assert_eq!(g.num_enf_update, Some(1));
    assert!(g.mismatch.is_none());

    r.num_adult = 0;
    let g = plan_guests(&r, true);
    assert!(g.mismatch.is_some());
    assert_eq!((g.num_child, g.num_enf, g.placeholder_children), (1, 0, 0));
    assert_eq!(g.num_enf_update, Some(0));

    let plain = room(r#"{"NumAdult":2,"NumChild":1}"#);
    let g = plan_guests(&plain, false);
    assert_eq!((g.placeholder_children, g.num_enf_update), (0, None));
}
