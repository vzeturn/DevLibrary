use super::*;
use crate::jobs::beds::MODE_FOLD_LEGACY;
use crate::jobs::fakes::{self, Hub, Pms};
use crate::jobs::result::{MANUAL, POSSIBLE_DUPLICATE};

const BOOKING: &str = "6f1c2d3e-0000-4000-8000-000000000001";
const ROOM: &str = "6f1c2d3e-0000-4000-8000-000000000101";

fn dispatcher(pms: &Arc<Pms>, gate: Option<Arc<WriteGate>>, code: &str) -> Dispatcher {
    Dispatcher::new(
        pms.clone(),
        gate,
        DispatchSettings {
            origin_code: "AZ".into(),
            member_discount_code: code.into(),
            extra_bed_mode: MODE_FOLD_LEGACY.into(),
        },
    )
}

fn job(id: &str, action: &str, payload: &str) -> Job {
    Job {
        job_id: id.into(),
        action: action.into(),
        booking_id: Some(BOOKING.parse().unwrap()),
        payload_json: payload.into(),
        attempts: 2,
    }
}

fn booking(extra: &str) -> String {
    format!(
        r#"{{"bookingId":"{BOOKING}","confirmationCode":"AZ-1","memberId":"6f1c2d3e-0000-4000-8000-0000000000aa",
            "member":{{"FullName":"Le Hoa"}}{extra},
            "rooms":[{{"Id":"{ROOM}","RoomTypeCode":"DLX","Arrival":"2026-12-01","Departure":"2026-12-02",
            "NumAdult":2,"GuestLastName":"Tran","nights":[{{"Date":"2026-12-01","Net":1000000}}]}}]}}"#
    )
}

fn ack(o: Outcome) -> Ack {
    match o {
        Outcome::Ack(a) => a,
        Outcome::Leave(why) => panic!("left: {why}"),
    }
}

#[test]
fn create_writes_the_rooms_and_acks_the_folios_with_the_lease() {
    let pms = Arc::new(Pms::default());
    let a = ack(dispatcher(&pms, None, "").process(&job("j1", "Create", &booking(""))));
    assert!(a.success);
    assert_eq!(a.attempts, Some(2));
    assert_eq!(a.smile_confirm_num, Some(Decimal::from(500)));
    let folios = a.folios.unwrap();
    assert_eq!(folios[0].folio_num, Decimal::from(1001));
    assert_eq!(folios[0].extra_bed_written, Some(0));
    assert_eq!(pms.writes(), vec!["profile Le", "write c1 u0 x0 p77"]);
}

#[test]
fn a_replayed_modify_finds_its_folio_and_changes_no_room() {
    let pms = Arc::new(Pms::default());
    let d = dispatcher(&pms, None, "");
    assert!(ack(d.process(&job("j1", "Modify", &booking("")))).success);
    let writes = pms.writes().len();
    let again = ack(d.process(&job("j1", "Modify", &booking(""))));
    assert!(again.success);
    assert_eq!(pms.writes().len(), writes + 1, "{:?}", pms.writes());
    assert_eq!(pms.writes().last().unwrap(), "write c0 u0 x0 p0");
}

#[test]
fn a_duplicate_refusal_goes_to_the_operator() {
    let pms = Arc::new(Pms::default());
    *pms.refuse.lock().unwrap() = Some(POSSIBLE_DUPLICATE.into());
    let a = ack(dispatcher(&pms, None, "").process(&job("j1", "Create", &booking(""))));
    assert!(!a.success);
    assert_eq!(a.error_code.as_deref(), Some(POSSIBLE_DUPLICATE));
    assert_eq!(a.error_class, Some("NEEDS_OPERATOR"));
    assert!(a.error.unwrap().starts_with("MANUAL: "));
}

#[test]
fn folios_the_hub_knows_but_smile_lost_are_not_recreated() {
    let pms = Arc::new(Pms::default());
    let p = booking(
        r#","expectedFolios":[{"FolioNum":9,"RoomItemId":"6f1c2d3e-0000-4000-8000-000000000101"}]"#,
    );
    let a = ack(dispatcher(&pms, None, "").process(&job("j1", "Modify", &p)));
    assert_eq!(a.error_code.as_deref(), Some("FOLIO_NOT_FOUND"));
    assert!(pms.writes().is_empty());
}

#[test]
fn cancel_is_idempotent() {
    let pms = Arc::new(Pms::default());
    let d = dispatcher(&pms, None, "");
    d.process(&job("j0", "Create", &booking("")));
    assert!(ack(d.process(&job("j1", "Cancel", &booking("")))).success);
    assert!(ack(d.process(&job("j1", "Cancel", &booking("")))).success);
    let cancels = pms
        .writes()
        .iter()
        .filter(|c| c.starts_with("cancel"))
        .count();
    assert_eq!(cancels, 1);
}

#[test]
fn a_closed_gate_refuses_writes_without_touching_smile_but_earn_still_runs() {
    let hub = Arc::new(Hub::default());
    hub.config.lock().unwrap().baseline_identity = Some(fakes::identity("OTHER_DB"));
    let gate = Arc::new(WriteGate::new(hub, fakes::tracker("SMILE_DB")));
    let pms = Arc::new(Pms::default());
    let d = dispatcher(&pms, Some(gate), "");
    let a = ack(d.process(&job("j1", "Create", &booking(""))));
    assert!(!a.success);
    assert_eq!(a.error_code.as_deref(), Some("PMS_IDENTITY_MISMATCH"));
    assert_eq!(a.error_class, Some("PMS_ENVIRONMENT"));
    assert!(pms.calls().is_empty());
    assert!(ack(d.process(&job("j2", "EarnPoints", &booking("")))).success);
    assert_eq!(pms.calls(), vec![format!("earn {BOOKING}")]);
}

#[test]
fn an_open_gate_lets_the_write_through() {
    let hub = Arc::new(Hub::default());
    hub.config.lock().unwrap().baseline_identity = Some(fakes::identity("SMILE_DB"));
    let gate = Arc::new(WriteGate::new(hub, fakes::tracker("SMILE_DB")));
    let pms = Arc::new(Pms::default());
    assert!(
        ack(dispatcher(&pms, Some(gate), "").process(&job("j1", "Create", &booking("")))).success
    );
}

#[test]
fn unknown_actions_and_bad_payloads_fail_only_that_job() {
    let pms = Arc::new(Pms::default());
    let d = dispatcher(&pms, None, "");
    let a = ack(d.process(&job("j1", "Teleport", "{}")));
    assert_eq!(a.error.as_deref(), Some("JobAction không hỗ trợ: Teleport"));
    let a = ack(d.process(&job("j2", "Create", "{")));
    assert_eq!(a.error_code.as_deref(), Some("UNKNOWN"));
    assert!(pms.calls().is_empty());
}

#[test]
fn a_profile_failure_writes_no_folio() {
    let pms = Arc::new(Pms::default());
    *pms.fail_profile.lock().unwrap() = Some(208);
    let a = ack(dispatcher(&pms, None, "").process(&job("j1", "Create", &booking(""))));
    assert_eq!(a.error_code.as_deref(), Some("PROFILE_UNAVAILABLE"));
    assert_eq!(a.error.as_deref(), Some("PROFILE_UNAVAILABLE: SQL 208"));
    assert!(pms.writes().is_empty());
}

#[test]
fn an_unported_write_is_left_for_the_lease_to_expire() {
    let pms = Arc::new(Pms::default());
    *pms.not_ported.lock().unwrap() = true;
    let o = dispatcher(&pms, None, "").process(&job("j1", "Create", &booking("")));
    assert!(matches!(o, Outcome::Leave(_)));
}

#[test]
fn a_voucher_is_posted_once() {
    let pms = Arc::new(Pms::default());
    let d = dispatcher(&pms, None, "MDISC");
    d.process(&job("j0", "Create", &booking("")));
    let v = booking(
        r#","voucher":{"Code":"V-10","Type":"PercentOff","Value":0.1,"MaxDiscountAmount":80000}"#,
    );
    assert!(ack(d.process(&job("j1", "RedeemVoucher", &v))).success);
    assert!(ack(d.process(&job("j1", "RedeemVoucher", &v))).success);
    let posted = pms.posted.lock().unwrap().clone();
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0].1, Decimal::from(80000));
    assert_eq!(posted[0].2, "MDISC");
}

#[test]
fn a_voucher_without_a_usable_discount_code_goes_to_the_operator() {
    let pms = Arc::new(Pms::default());
    for code in ["", "TOOLONG"] {
        let d = dispatcher(&pms, None, code);
        d.process(&job("j0", "Create", &booking("")));
        let v = booking(r#","voucher":{"Code":"V","Type":"AmountOff","Value":5}"#);
        let a = ack(d.process(&job("j1", "RedeemVoucher", &v)));
        assert_eq!(a.error_code.as_deref(), Some(MANUAL));
    }
    assert!(pms.posted.lock().unwrap().is_empty());
}

#[test]
fn a_credit_reward_is_posted_once_under_its_reference() {
    let pms = Arc::new(Pms::default());
    let d = dispatcher(&pms, None, "");
    d.process(&job("j0", "Create", &booking("")));
    let r = booking(
        r#","reward":{"GrantId":"00112233-4455-6677-8899-aabbccddeeff","Kind":"SpaCredit",
            "RuleJson":"{\"creditFolioCode\":\"SPA\"}","Value":200000}"#,
    );
    let a = ack(d.process(&job("j1", "GrantReward", &r)));
    assert_eq!(
        a.reward_grant_id,
        Some("00112233-4455-6677-8899-aabbccddeeff".parse().unwrap())
    );
    d.process(&job("j1", "GrantReward", &r));
    let posted = pms.posted.lock().unwrap().clone();
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0].3, "RWD:MyIRAFVEd2aImaq7zN3u_w");
}

#[test]
fn link_folio_stamps_through_the_writer() {
    let pms = Arc::new(Pms::default());
    let p = format!(
        r#"{{"bookingId":"{BOOKING}","memberId":"6f1c2d3e-0000-4000-8000-0000000000aa",
            "externalConfirmNumber":"AZ-1","confirmNum":5,
            "folios":[{{"FolioNum":1,"BookingRoomId":"{ROOM}"}}]}}"#
    );
    assert!(ack(dispatcher(&pms, None, "").process(&job("j1", "LinkFolio", &p))).success);
    assert_eq!(pms.writes(), vec!["link 1"]);
}
