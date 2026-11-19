//! The dispatcher's `SMILE_SPECIAL` path (`HandleCreateAsync` / `HandleModifyAsync`): the
//! preflight inputs are read with the payload's fingerprint and special code, travel with
//! the plan, and decide what the ack reports.

use super::*;
use crate::jobs::beds::{ExistingBeds, MODE_FOLD_LEGACY, MODE_SMILE_SPECIAL, SpecialBedRow};
use crate::jobs::fakes::Pms;
use crate::jobs::preflight::tests::{FP, terms};

const BOOKING: &str = "6f1c2d3e-0000-4000-8000-000000000002";
const ROOM: &str = "6f1c2d3e-0000-4000-8000-000000000201";

fn dispatcher(pms: &Arc<Pms>) -> Dispatcher {
    Dispatcher::new(
        pms.clone(),
        None,
        DispatchSettings {
            origin_code: "AZ".into(),
            member_discount_code: String::new(),
            extra_bed_mode: MODE_SMILE_SPECIAL.into(),
        },
    )
}

fn job(action: &str) -> Job {
    Job {
        job_id: "jb".into(),
        action: action.into(),
        booking_id: Some(BOOKING.parse().unwrap()),
        payload_json: format!(
            r#"{{"bookingId":"{BOOKING}","confirmationCode":"AZ-2",
            "extraBedVerifiedFingerprint":"{FP}","extraBedSpecialCode":"XB",
            "rooms":[{{"Id":"{ROOM}","RoomTypeCode":"DLX","Arrival":"2026-12-01","Departure":"2026-12-03",
            "NumAdult":3,"GuestLastName":"Tran","PricingModel":"OCCUPANCY_V1","ExtraBeds":1,
            "ExtraBedNightly":400000,"nights":[
            {{"Date":"2026-12-01","Net":1700000,"NetExclBed":1300000}},
            {{"Date":"2026-12-02","Net":1700000,"NetExclBed":1300000}}]}}]}}"#
        ),
        attempts: 1,
    }
}

fn special_inputs() -> PreflightInputs {
    PreflightInputs {
        configured_mode: MODE_SMILE_SPECIAL.into(),
        verified_fingerprint: Some(FP.into()),
        terms: Some(terms()),
        configured_smile_hotel_id: None,
    }
}

fn ack(o: Outcome) -> Ack {
    match o {
        Outcome::Ack(a) => a,
        Outcome::Leave(why) => panic!("left: {why}"),
    }
}

#[test]
fn create_reads_the_inputs_with_the_payload_values_and_acks_the_special_bed() {
    let pms = Arc::new(Pms::default());
    *pms.bed_inputs.lock().unwrap() = Some(special_inputs());
    let a = ack(dispatcher(&pms).process(&job("Create")));
    assert!(a.success);
    assert!(pms.calls().contains(&format!("lookup_bed {FP} XB")));
    let plan = pms.last_plan.lock().unwrap().clone().unwrap();
    assert_eq!(plan.extra_bed, Some(special_inputs()));
    let map = &a.folios.unwrap()[0];
    assert_eq!(map.extra_bed_written, Some(1));
    assert_eq!(map.extra_bed_special_qty_written, Some(1));
}

#[test]
fn fold_legacy_inputs_ack_a_folded_bed() {
    let pms = Arc::new(Pms::default());
    let a = ack(dispatcher(&pms).process(&job("Create")));
    let map = &a.folios.unwrap()[0];
    assert_eq!(map.extra_bed_written, Some(0));
    assert_eq!(map.extra_bed_special_qty_written, None);
    assert_eq!(
        pms.last_plan.lock().unwrap().clone().unwrap().extra_bed,
        None
    );
}

#[test]
fn modify_with_rows_as_wanted_is_unchanged_and_reports_what_it_found() {
    let pms = Arc::new(Pms::default());
    *pms.bed_inputs.lock().unwrap() = Some(special_inputs());
    ack(dispatcher(&pms).process(&job("Create")));
    // The folio now carries exactly what SMILE_SPECIAL wrote.
    for f in pms.folios.lock().unwrap().iter_mut() {
        f.rate_amount = Decimal::from(2_600_000);
        f.beds = Some(ExistingBeds {
            extra_bed: Some(1),
            mm_rows: vec![SpecialBedRow {
                qty: 1,
                price: Decimal::from(453_600),
                from_day: 1,
                to_day: 2,
            }],
            foreign_qty: 0,
            note_beds: None,
            special_known: true,
        });
    }
    let a = ack(dispatcher(&pms).process(&job("Modify")));
    assert!(a.success);
    let map = &a.folios.unwrap()[0];
    assert_eq!(map.extra_bed_found, Some(1));
    assert_eq!(map.extra_bed_special_qty_found, Some(1));
    assert_eq!(map.extra_bed_written, None, "nothing was written");
    let plan = pms.last_plan.lock().unwrap().clone().unwrap();
    assert!(plan.updates.is_empty() && plan.creates.is_empty());
}

#[test]
fn modify_after_a_switch_to_fold_rewrites_the_special_bed() {
    let pms = Arc::new(Pms::default());
    *pms.bed_inputs.lock().unwrap() = Some(special_inputs());
    ack(dispatcher(&pms).process(&job("Create")));
    for f in pms.folios.lock().unwrap().iter_mut() {
        f.rate_amount = Decimal::from(2_600_000);
        f.beds = Some(ExistingBeds {
            extra_bed: Some(1),
            mm_rows: vec![SpecialBedRow {
                qty: 1,
                price: Decimal::from(453_600),
                from_day: 1,
                to_day: 2,
            }],
            foreign_qty: 0,
            note_beds: None,
            special_known: true,
        });
    }
    *pms.bed_inputs.lock().unwrap() = Some(PreflightInputs {
        configured_mode: MODE_FOLD_LEGACY.into(),
        ..special_inputs()
    });
    let a = ack(dispatcher(&pms).process(&job("Modify")));
    let map = &a.folios.unwrap()[0];
    assert_eq!(map.extra_bed_written, Some(0));
    assert_eq!(map.extra_bed_special_qty_written, None);
    assert_eq!(
        pms.last_plan.lock().unwrap().clone().unwrap().updates.len(),
        1
    );
}
