//! REDEEM with several vouchers, ported from MM-MEMBERSHIP `JobDispatcherTests.MultiVoucher`
//! (commit 58a4524): one line per voucher on the first live folio, exactly the hub's
//! `appliedAmount`, in `sequence` order, idempotent per code; a legacy single `voucher`
//! keeps the old rule. Plus the golden SQL of a two-voucher redeem
//! (`tests/fixtures/golden/sql/redeem_two_vouchers.json`, rewrite with `GOLDEN_SQL_WRITE=1`).

use std::sync::{Arc, Mutex};

use dl_tds::Value;
use dl_time::DateTime;
use dl_uuid::Uuid;

use super::super::{DispatchSettings, Outcome};
use super::*;
use crate::jobs::beds::MODE_FOLD_LEGACY;
use crate::jobs::fakes::Pms;
use crate::jobs::payload::LinkFolioPayload;
use crate::jobs::plan::{BookingPlan, PmsWriter, ProfileRequest, Written};
use crate::jobs::result::{MANUAL, MANUAL_PREFIX};
use crate::jobs::room_diff::ExistingFolio;
use crate::jobs::travel_agent::TravelAgent;
use crate::writer::folio::FolioWriter;
use crate::writer::recorder::Recorder;
use crate::writer::session::Rec;

const BOOKING: &str = "6f1c2d3e-0000-4000-8000-000000000001";

fn dec(n: i64) -> Decimal {
    Decimal::from(n)
}

fn dispatcher(pms: Arc<dyn PmsWriter>) -> Dispatcher {
    Dispatcher::new(
        pms,
        None,
        DispatchSettings {
            origin_code: "AZ".into(),
            member_discount_code: "MDISC".into(),
            extra_bed_mode: MODE_FOLD_LEGACY.into(),
        },
    )
}

fn redeem(extra: &str) -> Job {
    Job {
        job_id: "j1".into(),
        action: "RedeemVoucher".into(),
        booking_id: Some(BOOKING.parse().unwrap()),
        payload_json: format!(
            r#"{{"bookingId":"{BOOKING}","confirmationCode":"AZ-1","memberId":"6f1c2d3e-0000-4000-8000-0000000000aa"{extra}}}"#
        ),
        attempts: 1,
    }
}

fn ack(o: Outcome) -> Ack {
    match o {
        Outcome::Ack(a) => a,
        Outcome::Leave(why) => panic!("left: {why}"),
    }
}

/// `VouchersJson`: `(code, sequence, type, value, appliedAmount)`.
fn vouchers_json(vs: &[(&str, i32, &str, &str, Option<&str>)]) -> String {
    let items: Vec<String> = vs
        .iter()
        .map(|(code, seq, kind, value, applied)| {
            format!(
                r#"{{"VoucherId":"6f1c2d3e-0000-4000-8000-0000000000bb","Code":"{code}","Type":"{kind}","Value":{value},"Sequence":{seq},"AppliedAmount":{}}}"#,
                applied.unwrap_or("null")
            )
        })
        .collect();
    format!(r#","Vouchers":[{}]"#, items.join(","))
}

/// `VoucherJson`: the legacy single voucher `WELCOME10`.
fn voucher_json(kind: &str, value: &str) -> String {
    format!(
        r#","Voucher":{{"VoucherId":"6f1c2d3e-0000-4000-8000-0000000000cc","Code":"WELCOME10","Type":"{kind}","Value":{value}}}"#
    )
}

/// The two live folios' first-night rates add up to 3,000,000, but the hub priced the
/// whole stay: PCT15 comes to 450,000, which no folio figure yields; it is posted verbatim.
const THREE: [(&str, i32, &str, &str, Option<&str>); 3] = [
    ("PCT15", 3, "PercentOff", "0.15", Some("450000")),
    ("AMT100", 1, "AmountOff", "100000", Some("100000")),
    ("AMT200", 2, "AmountOff", "200000", Some("200000")),
];

fn folio(num: i64, rate: i64) -> ExistingFolio {
    ExistingFolio {
        folio_num: dec(num),
        room_item_id: Uuid::NIL,
        room_type_booked: "DLX".into(),
        arrival: crate::jobs::payload::DEFAULT_DATE,
        departure: crate::jobs::payload::DEFAULT_DATE,
        rate_amount: dec(rate),
        room_code: String::new(),
        folio_status: 1,
        cancel_num: Decimal::ZERO,
        guest: None,
        beds: None,
    }
}

/// `TwoLiveRooms`: folios 700 (2,000,000) and 701 (1,000,000); `posted` refs are already on
/// a live folio from an earlier delivery.
fn two_live_rooms(posted: &[&str]) -> Arc<Pms> {
    let pms = Arc::new(Pms::default());
    *pms.folios.lock().unwrap() = vec![folio(700, 2_000_000), folio(701, 1_000_000)];
    *pms.posted.lock().unwrap() = posted
        .iter()
        .map(|r| (dec(700), dec(1), "MDISC".to_owned(), (*r).to_owned()))
        .collect();
    pms
}

/// The discounts this delivery posted, as `(folio, amount, code, ref)`.
fn new_posts(pms: &Pms, seeded: usize) -> Vec<(Decimal, Decimal, String, String)> {
    pms.posted.lock().unwrap()[seeded..].to_vec()
}

fn line(folio: i64, amount: i64, reference: &str) -> (Decimal, Decimal, String, String) {
    (dec(folio), dec(amount), "MDISC".into(), reference.into())
}

fn manual_reason(a: &Ack) -> &str {
    assert!(!a.success);
    assert_eq!(a.error_code.as_deref(), Some(MANUAL));
    a.error.as_deref().unwrap_or_default()
}

/// `Redeem_nhieu_voucher_ghi_dung_so_Hub_da_ap_theo_thu_tu_sequence`.
#[test]
fn several_vouchers_post_the_hub_amounts_in_sequence_order() {
    let pms = two_live_rooms(&[]);
    let a = ack(dispatcher(pms.clone()).process(&redeem(&vouchers_json(&THREE))));
    assert!(a.success);
    assert_eq!(
        new_posts(&pms, 0),
        vec![
            line(700, 100_000, "AMT100"),
            line(700, 200_000, "AMT200"),
            line(700, 450_000, "PCT15"),
        ]
    );
}

/// `Redeem_giao_lai_sau_dong_dau_chi_ghi_phan_con_thieu`.
#[test]
fn a_redelivery_after_the_first_line_posts_only_the_missing_ones() {
    let pms = two_live_rooms(&["AMT100"]);
    let a = ack(dispatcher(pms.clone()).process(&redeem(&vouchers_json(&THREE))));
    assert!(a.success);
    assert_eq!(
        new_posts(&pms, 1),
        vec![line(700, 200_000, "AMT200"), line(700, 450_000, "PCT15")]
    );
}

/// `Redeem_giao_lai_khi_moi_voucher_da_ghi_thi_Ok_va_khong_ghi_lap`.
#[test]
fn a_redelivery_with_every_voucher_posted_is_ok_and_posts_nothing() {
    let pms = two_live_rooms(&["AMT100", "AMT200", "PCT15"]);
    let a = ack(dispatcher(pms.clone()).process(&redeem(&vouchers_json(&THREE))));
    assert!(a.success);
    assert!(new_posts(&pms, 3).is_empty());
}

/// `Redeem_mang_vouchers_thang_voucher_cu`.
#[test]
fn the_array_wins_over_the_legacy_voucher() {
    let pms = two_live_rooms(&[]);
    let payload = voucher_json("AmountOff", "999000")
        + &vouchers_json(&[("AMT100", 1, "AmountOff", "100000", Some("100000"))]);
    ack(dispatcher(pms.clone()).process(&redeem(&payload)));
    assert_eq!(new_posts(&pms, 0), vec![line(700, 100_000, "AMT100")]);
}

/// `Redeem_mot_voucher_thieu_appliedAmount_thi_ca_bo_chuyen_quay_khong_ghi_dong_nao`.
#[test]
fn a_voucher_without_applied_amount_sends_the_whole_set_to_the_desk() {
    let pms = two_live_rooms(&[]);
    let a = ack(dispatcher(pms.clone()).process(&redeem(&vouchers_json(&[
        ("AMT100", 1, "AmountOff", "100000", Some("100000")),
        ("SPA", 2, "FreeService", "0", None),
    ]))));
    let reason = manual_reason(&a);
    assert!(reason.starts_with(MANUAL_PREFIX), "{reason}");
    assert!(
        reason.contains("SPA") && reason.contains("appliedAmount"),
        "{reason}"
    );
    assert_eq!(
        reason,
        format!(
            "{MANUAL_PREFIX} voucher SPA thiếu số tiền Hub đã áp (appliedAmount) — áp tay tại quầy."
        )
    );
    assert!(new_posts(&pms, 0).is_empty());
    assert!(pms.calls().is_empty(), "nothing read before the check");
}

/// `Redeem_appliedAmount_bang_0_thi_ca_bo_chuyen_quay_khong_ghi_dong_nao`.
#[test]
fn a_zero_applied_amount_sends_the_whole_set_to_the_desk() {
    let pms = two_live_rooms(&[]);
    let a = ack(dispatcher(pms.clone()).process(&redeem(&vouchers_json(&[
        ("AMT100", 1, "AmountOff", "100000", Some("100000")),
        ("PCT10", 2, "PercentOff", "0.10", Some("0")),
    ]))));
    assert!(manual_reason(&a).contains("PCT10"));
    assert!(new_posts(&pms, 0).is_empty());
}

/// `Redeem_mang_rong_va_khong_co_voucher_cu_thi_khong_ghi_dong_nao`.
#[test]
fn an_empty_array_without_a_legacy_voucher_posts_nothing() {
    let pms = two_live_rooms(&[]);
    let a = ack(dispatcher(pms.clone()).process(&redeem(r#","Vouchers":[]"#)));
    assert_eq!(
        manual_reason(&a),
        format!("{MANUAL_PREFIX} payload thiếu thông tin voucher.")
    );
    assert!(new_posts(&pms, 0).is_empty());
}

/// `Redeem_payload_cu_PercentOff_van_tinh_tren_tong_folio_nhu_truoc`.
#[test]
fn a_legacy_percent_off_still_computes_on_the_folio_total() {
    let pms = two_live_rooms(&[]);
    let a = ack(dispatcher(pms.clone()).process(&redeem(&voucher_json("PercentOff", "0.10"))));
    assert!(a.success);
    assert_eq!(new_posts(&pms, 0), vec![line(700, 300_000, "WELCOME10")]);
}

/// `Redeem_payload_cu_AmountOff_lon_hon_tong_folio_van_ghi_nguyen_Value`: `RateAmount` is
/// only the first-night rate, so a legacy `AmountOff` is never capped by the folio total.
#[test]
fn a_legacy_amount_off_above_the_folio_total_posts_its_full_value() {
    let pms = two_live_rooms(&[]);
    let a = ack(dispatcher(pms.clone()).process(&redeem(&voucher_json("AmountOff", "5000000"))));
    assert!(a.success);
    assert_eq!(new_posts(&pms, 0), vec![line(700, 5_000_000, "WELCOME10")]);
}

/// The stacked path keeps the order of the .NET checks: no live folio, then the discount
/// code (empty or too long), each before any line is posted.
#[test]
fn the_stacked_path_needs_a_live_folio_and_a_usable_discount_code() {
    let one = vouchers_json(&[("AMT100", 1, "AmountOff", "100000", Some("100000"))]);
    let pms = Arc::new(Pms::default());
    let a = ack(dispatcher(pms.clone()).process(&redeem(&one)));
    assert_eq!(
        manual_reason(&a),
        format!("{MANUAL_PREFIX} không có folio để áp voucher.")
    );
    for code in ["", "TOOLONG"] {
        let pms = two_live_rooms(&[]);
        let d = Dispatcher::new(
            pms.clone(),
            None,
            DispatchSettings {
                origin_code: "AZ".into(),
                member_discount_code: code.into(),
                extra_bed_mode: MODE_FOLD_LEGACY.into(),
            },
        );
        let a = ack(d.process(&redeem(&one)));
        assert!(
            manual_reason(&a).contains("Earn:MemberDiscountCode"),
            "{code}"
        );
        assert!(new_posts(&pms, 0).is_empty());
    }
}

/// [`PmsWriter`] over a [`Recorder`]: the live folios are canned, the duplicate guard and
/// the discount line go through the real `FolioWriter` SQL at a fixed hotel time.
struct SqlPms {
    rec: Mutex<Recorder>,
    folios: Vec<ExistingFolio>,
}

impl PmsWriter for SqlPms {
    fn lookup_by_confirmation(&self, _: &str) -> Result<Vec<ExistingFolio>, WriteError> {
        Ok(self.folios.clone())
    }
    fn lookup_by_folio_nums(&self, _: &[Decimal]) -> Result<Vec<ExistingFolio>, WriteError> {
        Err(WriteError::NotPorted("lookup_by_folio_nums"))
    }
    fn write_booking(&self, _: &BookingPlan) -> Result<Written, WriteError> {
        Err(WriteError::NotPorted("write_booking"))
    }
    fn cancel_folio(&self, _: Decimal) -> Result<(), WriteError> {
        Err(WriteError::NotPorted("cancel_folio"))
    }
    fn has_posted_ref(&self, folios: &[Decimal], reference: &str) -> Result<bool, WriteError> {
        FolioWriter::default().has_posted_ref(&mut *self.rec.lock().unwrap(), folios, reference)
    }
    fn post_discount(
        &self,
        folio_num: Decimal,
        amount: Decimal,
        code: &str,
        reference: &str,
    ) -> Result<(), WriteError> {
        let now = DateTime::parse_iso("2026-12-01T14:05:09").unwrap();
        FolioWriter::default().post_discount(
            &mut *self.rec.lock().unwrap(),
            now,
            folio_num,
            amount,
            code,
            reference,
        )
    }
    fn link_folios(&self, _: &LinkFolioPayload) -> Result<(), WriteError> {
        Err(WriteError::NotPorted("link_folios"))
    }
    fn ensure_profile(&self, _: &ProfileRequest) -> Result<Decimal, WriteError> {
        Err(WriteError::NotPorted("ensure_profile"))
    }
    fn find_travel_agent(&self, _: i64) -> Result<Option<TravelAgent>, WriteError> {
        Err(WriteError::NotPorted("find_travel_agent"))
    }
    fn report_earn(&self, _: Uuid, _: &str) -> Result<(), WriteError> {
        Err(WriteError::NotPorted("report_earn"))
    }
}

fn golden(name: &str, r: &Recorder) {
    let path = format!(
        "{}/tests/fixtures/golden/sql/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let got = dl_json::to_string_pretty(&r.to_json());
    if std::env::var_os("GOLDEN_SQL_WRITE").is_some() {
        std::fs::write(&path, format!("{got}\n")).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    assert_eq!(got.trim_end(), want.trim_end(), "golden SQL {name}");
}

const TWO: [(&str, i32, &str, &str, Option<&str>); 2] = [
    ("PCT15", 2, "PercentOff", "0.15", Some("450000")),
    ("AMT100", 1, "AmountOff", "100000", Some("100000")),
];

/// Golden SQL of a two-voucher redeem: per voucher (in `sequence` order) the
/// `HasPostedRefAsync` guard over both live folios, then `PostOneTransaction` on the first.
#[test]
fn a_two_voucher_redeem_matches_the_golden_sql() {
    let pms = Arc::new(SqlPms {
        rec: Mutex::new(Recorder::default()),
        folios: vec![folio(700, 2_000_000), folio(701, 1_000_000)],
    });
    let a = ack(dispatcher(pms.clone()).process(&redeem(&vouchers_json(&TWO))));
    assert!(a.success, "{:?}", a.error);
    let r = pms.rec.lock().unwrap();
    assert_eq!(r.log.len(), 4);
    let kinds: Vec<&str> = r.log.iter().map(|s| s.kind).collect();
    assert_eq!(kinds, ["text", "proc", "text", "proc"]);
    golden("redeem_two_vouchers", &r);
}

/// Replay of the same job once `AMT100` is on SMILE: both guards run, one line is posted.
#[test]
fn a_replayed_two_voucher_redeem_posts_only_the_missing_line() {
    let mut rec = Recorder::default();
    rec.on("FROM FolioTransaction", |st| {
        let hit = st
            .params
            .iter()
            .any(|(n, _, v)| n == "Ref" && v.as_deref() == Some("AMT100"));
        Ok(crate::writer::session::Called {
            rows: vec![Rec::new(vec![("", Value::I32(i32::from(hit)))])],
            status: Some(0),
        })
    });
    let pms = Arc::new(SqlPms {
        rec: Mutex::new(rec),
        folios: vec![folio(700, 2_000_000), folio(701, 1_000_000)],
    });
    let a = ack(dispatcher(pms.clone()).process(&redeem(&vouchers_json(&TWO))));
    assert!(a.success);
    let r = pms.rec.lock().unwrap();
    let kinds: Vec<&str> = r.log.iter().map(|s| s.kind).collect();
    assert_eq!(kinds, ["text", "text", "proc"]);
    let refs: Vec<_> = r.log[2]
        .params
        .iter()
        .filter(|(n, _, _)| n == "RefNumber")
        .map(|(_, _, v)| v.clone())
        .collect();
    assert_eq!(refs, vec![Some("PCT15".to_owned())]);
}
