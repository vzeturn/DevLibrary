//! The REDEEM `vouchers` array (voucher stacking, spec 2026-10-15 §3), ported from
//! MM-MEMBERSHIP `JobPayloadParserVouchersTests`: the array wins over the legacy `voucher`,
//! is ordered by `sequence`, and a legacy payload still yields exactly one voucher.

use super::*;

const LEGACY: &str = r#""Voucher":{"VoucherId":"eeeeeeee-1111-2222-3333-444444444444","Code":"OLD","Type":"AmountOff","Value":50000}"#;

fn entry(code: &str, sequence: i32, kind: &str, value: &str, applied: Option<&str>) -> String {
    let applied = applied.map_or(String::new(), |a| format!(r#","AppliedAmount":{a}"#));
    format!(
        r#"{{"VoucherId":"6f1c2d3e-0000-4000-8000-0000000000bb","Code":"{code}","Type":"{kind}","Value":{value},"MaxDiscountAmount":null,"ProgramName":"P","Sequence":{sequence}{applied}}}"#
    )
}

fn parse(parts: &[&str]) -> Result<JobPayload, String> {
    let tail: String = parts.iter().map(|p| format!(",{p}")).collect();
    parse_payload(&format!(r#"{{"Action":"RedeemVoucher"{tail}}}"#))
}

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

fn codes(p: &JobPayload) -> Vec<&str> {
    p.vouchers().iter().map(|v| v.code.as_str()).collect()
}

/// `Vouchers_duoc_xep_theo_sequence_va_mang_so_Hub_tinh`.
#[test]
fn vouchers_are_ordered_by_sequence_and_carry_the_hub_amount() {
    let arr = format!(
        r#""Vouchers":[{},{}]"#,
        entry("B", 2, "PercentOff", "0.15", Some("300000")),
        entry("A", 1, "AmountOff", "100000", Some("100000"))
    );
    let p = parse(&[&arr]).unwrap();
    assert_eq!(codes(&p), ["A", "B"]);
    let v = p.vouchers();
    assert_eq!(v[0].sequence, 1);
    assert_eq!(v[0].applied_amount, Some(d("100000")));
    assert_eq!(v[1].kind, "PercentOff");
    assert_eq!(v[1].value, d("0.15"));
    assert_eq!(v[1].max_discount_amount, None);
    assert_eq!(v[1].applied_amount, Some(d("300000")));
}

/// `Vouchers_thang_voucher_cu` (camelCase key: property names match case-insensitively).
#[test]
fn the_array_wins_over_the_legacy_voucher() {
    let arr = format!(
        r#""vouchers":[{}]"#,
        entry("NEW", 1, "AmountOff", "100000", None)
    );
    let p = parse(&[LEGACY, &arr]).unwrap();
    assert_eq!(codes(&p), ["NEW"]);
}

/// `Payload_cu_chi_co_voucher_cho_dung_mot_voucher_nhu_truoc`.
#[test]
fn a_legacy_payload_yields_its_single_voucher_unchanged() {
    let p = parse(&[LEGACY]).unwrap();
    let v = p.vouchers();
    assert_eq!(v.len(), 1);
    assert_eq!(Some(&v[0]), p.voucher.as_ref());
    assert_eq!(v[0].sequence, 0);
    assert_eq!(v[0].applied_amount, None);
    assert_eq!(p.stacked_vouchers, None);
}

/// `Mang_rong_hoac_null_thi_roi_ve_voucher_cu`.
#[test]
fn an_empty_or_null_array_falls_back_to_the_legacy_voucher() {
    for arr in [r#""Vouchers":[]"#, r#""Vouchers":null"#] {
        let p = parse(&[LEGACY, arr]).unwrap();
        assert_eq!(codes(&p), ["OLD"], "{arr}");
        assert_eq!(p.stacked_vouchers, None);
    }
}

/// `Khong_co_voucher_nao_thi_danh_sach_rong`.
#[test]
fn no_voucher_at_all_is_an_empty_list() {
    assert!(parse(&[r#""Vouchers":[]"#]).unwrap().vouchers().is_empty());
    assert!(parse(&[]).unwrap().vouchers().is_empty());
}

/// `Phan_tu_khong_phai_object_lam_hong_parse_chu_khong_bi_bo_qua`.
#[test]
fn an_element_that_is_not_an_object_fails_the_parse() {
    for bad in [r#""x""#, "null", "42"] {
        let arr = format!(
            r#""Vouchers":[{},{bad}]"#,
            entry("A", 1, "AmountOff", "100000", None)
        );
        let e = parse(&[&arr]).unwrap_err();
        assert_eq!(
            e, "REDEEM payload has a `vouchers` entry that is not an object.",
            "{bad}"
        );
    }
}

/// `GetInt` / `AppliedAmount`: a non-integer sequence reads 0, a non-number amount is absent;
/// equal sequences keep the hub's order (stable `OrderBy`).
#[test]
fn odd_fields_read_as_absent_and_equal_sequences_keep_the_hub_order() {
    let arr = r#""Vouchers":[{"Code":"X","Sequence":"2","AppliedAmount":"5"},
        {"Code":"Y","Sequence":1.5,"AppliedAmount":null},{"Code":"Z","Sequence":0,"AppliedAmount":7}]"#;
    let p = parse(&[arr]).unwrap();
    assert_eq!(codes(&p), ["X", "Y", "Z"]);
    assert!(p.vouchers().iter().all(|v| v.sequence == 0));
    assert_eq!(p.vouchers()[0].applied_amount, None);
    assert_eq!(p.vouchers()[1].applied_amount, None);
    assert_eq!(p.vouchers()[2].applied_amount, Some(d("7")));
}
