//! Mirrors MM-MEMBERSHIP `tests/MM.Membership.Agent.Tests/Smile/ExtraBedPreflightTests.cs`.

use dl_time::Date;

use super::*;
use crate::jobs::bed_terms::{SpecialService, TaxCodes, structural_fingerprint};
use crate::jobs::beds::{FOLD_CONFIGURED, MODE_FOLD_LEGACY};
use crate::jobs::payload::{Night, Occupancy};

pub(crate) const FP: &str = "fp-verified";

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

fn victory_eb() -> SpecialService {
    SpecialService {
        code: "EB".into(),
        trn_code: Some("110".into()),
        trn_sub: Some("9".into()),
        open_price: Some(1),
        include_in_room_charge: Some(0),
        amount_charge: Some(Decimal::ZERO),
    }
}

fn v8s5() -> TaxCodes {
    TaxCodes {
        tax_code: Some("V8".into()),
        service_charge_code: Some("S5".into()),
        s_tax_code: None,
    }
}

pub(crate) fn terms() -> ExtraBedTerms {
    ExtraBedTerms {
        smile_hotel_id: "VIC".into(),
        surcharge_amount: Some(Decimal::ZERO),
        trn_code: Some("0".into()),
        trn_sub: Some("0".into()),
        stock_total: Some(48),
        special: Some(victory_eb()),
        special_tax: Some(v8s5()),
        room_tax: Some(v8s5()),
        special_by_date: Some(true),
        smile_hotel_date: Date::new(2026, 10, 20).ok(),
        structural_fingerprint: FP.into(),
        content_hash: "hash".into(),
        warnings: Vec::new(),
        rate_code_plus_plus: Some(true),
        special_sc_percent: Some(d("5")),
        special_vat_percent: Some(d("8")),
        pms_priced: false,
    }
}

fn pms_xb(amount: &str) -> SpecialService {
    SpecialService {
        code: "XB".into(),
        open_price: Some(0),
        amount_charge: Some(d(amount)),
        ..victory_eb()
    }
}

fn pms_terms(amount: &str, plus_plus: bool) -> ExtraBedTerms {
    ExtraBedTerms {
        special: Some(pms_xb(amount)),
        rate_code_plus_plus: Some(plus_plus),
        ..terms()
    }
    .as_pms_priced()
}

fn inputs(t: Option<ExtraBedTerms>) -> PreflightInputs {
    PreflightInputs {
        configured_mode: MODE_SMILE_SPECIAL.into(),
        verified_fingerprint: Some(FP.into()),
        terms: t,
        configured_smile_hotel_id: None,
    }
}

/// DLX 3A: Net 1,700,000 = room 1,300,000 + bed 400,000 per night.
fn v1_room(nights: i64, beds: i32) -> Room {
    let d1 = Date::new(2026, 11, 1).unwrap();
    Room {
        room_item_id: dl_uuid::Uuid::NIL,
        room_type_code: "DLX".into(),
        arrival: d1,
        departure: d1.add_days(nights),
        num_adult: 3,
        num_child: 0,
        guest_first_name: "An".into(),
        guest_last_name: "Nguyen".into(),
        nights: (0..nights)
            .map(|i| Night {
                date: d1.add_days(i),
                net: Decimal::from(1_300_000 + i64::from(beds) * 400_000),
                net_excl_bed: Some(Decimal::from(1_300_000)),
            })
            .collect(),
        removed: false,
        guest: None,
        occupancy: Some(Occupancy {
            pricing_model: Some("OCCUPANCY_V1".into()),
            extra_beds: Some(beds),
            extra_bed_nightly: (beds > 0).then(|| Decimal::from(400_000)),
            extra_bed_nights: None,
            adults: Some(3),
            children_half: Some(0),
            children_free: Some(0),
            num_enf: None,
        }),
    }
}

fn row(qty: i32, price: i64, from: i32, to: i32) -> SpecialBedRow {
    SpecialBedRow {
        qty,
        price: Decimal::from(price),
        from_day: from,
        to_day: to,
    }
}

#[test]
fn price_grosses_up_a_plus_plus_hotel_and_keeps_a_net_one() {
    let cases = [
        ("400000", true, "5", "8", "453600"),
        ("400000", false, "5", "8", "400000"),
        ("333333", true, "5", "8", "378000"),
        ("400000", true, "0", "0", "400000"),
    ];
    for (amount, pp, sc, vat, want) in cases {
        assert_eq!(
            special_price(d(amount), pp, d(sc), d(vat)),
            Some(d(want)),
            "{amount} {pp}"
        );
    }
}

#[test]
fn every_check_holds_so_the_bed_is_one_band_at_the_grossed_up_price() {
    let b = decide(&v1_room(2, 1), &inputs(Some(terms())));
    assert_eq!(b.mode, MODE_SMILE_SPECIAL);
    assert!(b.is_special());
    assert_eq!(b.fold_reason, None);
    assert_eq!(b.bed_note, None);
    assert!(b.use_net_excl_bed);
    assert_eq!(b.extra_bed_to_write, 1);
    assert!(!b.native_surcharge_active);
    assert_eq!(b.special_code.as_deref(), Some("EB"));
    assert_eq!(b.special_rows, vec![row(1, 453_600, 1, 2)]);
}

#[test]
fn nights_at_different_bed_prices_become_bands() {
    let mut r = v1_room(3, 2);
    if let Some(o) = r.occupancy.as_mut() {
        o.extra_bed_nights = Some(vec![d("400000"), d("400000"), d("500000")]);
    }
    let t = ExtraBedTerms {
        rate_code_plus_plus: Some(false),
        ..terms()
    };
    let b = decide(&r, &inputs(Some(t)));
    assert_eq!(
        b.special_rows,
        vec![row(2, 400_000, 1, 2), row(2, 500_000, 3, 3)]
    );
}

#[test]
fn a_per_night_list_that_does_not_match_the_nights_folds_as_legacy() {
    let mut r = v1_room(2, 1);
    if let Some(o) = r.occupancy.as_mut() {
        o.extra_bed_nights = Some(vec![d("400000")]);
    }
    let b = decide(&r, &inputs(Some(terms())));
    assert_eq!(b.fold_reason, Some(FOLD_PAYLOAD_LEGACY));
}

#[test]
fn rule_13_native_surcharge_keeps_extra_bed_0_but_writes_the_row() {
    let t = ExtraBedTerms {
        surcharge_amount: Some(d("250000")),
        ..terms()
    };
    let b = decide(&v1_room(2, 1), &inputs(Some(t)));
    assert!(b.is_special() && b.native_surcharge_active);
    assert_eq!(b.extra_bed_to_write, 0);
    assert_eq!(b.special_rows.len(), 1);
}

#[test]
fn a_room_without_beds_is_special_with_no_rows() {
    let b = decide(&v1_room(2, 0), &inputs(Some(terms())));
    assert!(b.is_special());
    assert!(b.special_rows.is_empty());
    assert_eq!(b.extra_bed_to_write, 0);
}

#[test]
fn the_first_failing_check_folds_the_room_with_its_reason() {
    type Change = fn(ExtraBedTerms) -> Option<ExtraBedTerms>;
    type Case<'a> = (&'a str, Option<&'a str>, Change, Option<&'a str>, &'a str);
    let same: Change = Some;
    let cases: Vec<Case<'_>> = vec![
        (MODE_FOLD_LEGACY, Some(FP), same, None, FOLD_CONFIGURED),
        (
            MODE_SMILE_SPECIAL,
            None,
            same,
            None,
            FOLD_FINGERPRINT_UNVERIFIED,
        ),
        (
            MODE_SMILE_SPECIAL,
            Some("  "),
            same,
            None,
            FOLD_FINGERPRINT_UNVERIFIED,
        ),
        (
            MODE_SMILE_SPECIAL,
            Some("fp-other"),
            same,
            None,
            FOLD_FINGERPRINT_DRIFT,
        ),
        (
            MODE_SMILE_SPECIAL,
            Some(FP),
            |_| None,
            None,
            FOLD_FINGERPRINT_DRIFT,
        ),
        (
            MODE_SMILE_SPECIAL,
            Some(FP),
            |t| Some(ExtraBedTerms { special: None, ..t }),
            None,
            FOLD_SPECIAL_MISSING,
        ),
        (
            MODE_SMILE_SPECIAL,
            Some(FP),
            |t| {
                Some(ExtraBedTerms {
                    special: Some(SpecialService {
                        open_price: Some(0),
                        ..victory_eb()
                    }),
                    ..t
                })
            },
            None,
            FOLD_SPECIAL_UNUSABLE,
        ),
        (
            MODE_SMILE_SPECIAL,
            Some(FP),
            |t| {
                Some(ExtraBedTerms {
                    special: Some(SpecialService {
                        amount_charge: Some(Decimal::from(100)),
                        ..victory_eb()
                    }),
                    ..t
                })
            },
            None,
            FOLD_SPECIAL_UNUSABLE,
        ),
        (
            MODE_SMILE_SPECIAL,
            Some(FP),
            |t| {
                Some(ExtraBedTerms {
                    special_sc_percent: None,
                    ..t
                })
            },
            None,
            FOLD_SPECIAL_UNUSABLE,
        ),
        (
            MODE_SMILE_SPECIAL,
            Some(FP),
            |t| {
                Some(ExtraBedTerms {
                    special_by_date: Some(false),
                    ..t
                })
            },
            None,
            FOLD_SPECIAL_BY_DATE_OFF,
        ),
        (
            MODE_SMILE_SPECIAL,
            Some(FP),
            |t| {
                Some(ExtraBedTerms {
                    special_by_date: None,
                    ..t
                })
            },
            None,
            FOLD_SPECIAL_BY_DATE_OFF,
        ),
        (
            MODE_SMILE_SPECIAL,
            Some(FP),
            same,
            Some("OTHER"),
            FOLD_HOTEL_ID_MISMATCH,
        ),
    ];
    for (mode, fp, change, hotel, reason) in cases {
        let i = PreflightInputs {
            configured_mode: mode.into(),
            verified_fingerprint: fp.map(str::to_owned),
            terms: change(terms()),
            configured_smile_hotel_id: hotel.map(str::to_owned),
        };
        let b = decide(&v1_room(2, 1), &i);
        assert_eq!(b.fold_reason, Some(reason), "{mode} {fp:?} {hotel:?}");
        assert!(!b.is_special());
        assert!(b.special_rows.is_empty());
        assert_eq!(b.extra_bed_to_write, 0);
        assert!(!b.use_net_excl_bed);
        assert!(b.bed_note.is_some());
    }
}

#[test]
fn matching_hotel_id_and_fingerprint_ignore_case_and_spaces() {
    let i = PreflightInputs {
        configured_mode: MODE_SMILE_SPECIAL.into(),
        verified_fingerprint: Some(" FP-VERIFIED ".into()),
        terms: Some(terms()),
        configured_smile_hotel_id: Some(" vic ".into()),
    };
    assert!(decide(&v1_room(2, 1), &i).is_special());
}

#[test]
fn without_inputs_the_writer_folds_whatever_the_mode() {
    assert_eq!(
        decide_extra_bed(&v1_room(2, 1), None, MODE_SMILE_SPECIAL).fold_reason,
        Some(FOLD_FINGERPRINT_UNVERIFIED)
    );
}

#[test]
fn a_pms_priced_special_writes_the_catalogue_price() {
    for (pp, price) in [(true, 453_600), (false, 400_000)] {
        let t = pms_terms(&price.to_string(), pp);
        let i = PreflightInputs {
            verified_fingerprint: Some(t.structural_fingerprint.clone()),
            ..inputs(Some(t))
        };
        let b = decide(&v1_room(2, 1), &i);
        assert!(b.is_special());
        assert_eq!(b.special_code.as_deref(), Some("XB"));
        assert_eq!(b.special_rows, vec![row(1, price, 1, 2)]);
    }
    // ADR-0109: the catalogue price, whatever the booking was quoted at.
    let i = PreflightInputs {
        verified_fingerprint: None,
        ..inputs(Some(pms_terms("500000", true)))
    };
    assert_eq!(
        decide(&v1_room(2, 1), &i).special_rows,
        vec![row(1, 500_000, 1, 2)]
    );
}

#[test]
fn a_pms_priced_special_needs_no_vm_proof_but_the_agent_priced_one_does() {
    let no_fp = |t| PreflightInputs {
        verified_fingerprint: None,
        ..inputs(Some(t))
    };
    assert_eq!(
        decide(&v1_room(2, 1), &no_fp(pms_terms("453600", true))).fold_reason,
        None
    );
    assert_eq!(
        decide(&v1_room(2, 1), &no_fp(terms())).fold_reason,
        Some(FOLD_FINGERPRINT_UNVERIFIED)
    );
}

#[test]
fn a_pms_priced_plus_plus_hotel_without_its_rates_folds() {
    let t = ExtraBedTerms {
        special_vat_percent: None,
        ..pms_terms("453600", true)
    };
    let i = PreflightInputs {
        verified_fingerprint: Some(t.structural_fingerprint.clone()),
        ..inputs(Some(t))
    };
    assert_eq!(
        decide(&v1_room(2, 1), &i).fold_reason,
        Some(FOLD_SPECIAL_UNUSABLE)
    );
}

#[test]
fn the_pms_priced_fingerprint_ignores_the_price_but_not_the_structure() {
    let base = pms_terms("453600", true).structural_fingerprint;
    assert_eq!(pms_terms("500000", true).structural_fingerprint, base);
    let with = |sp: SpecialService, tax: TaxCodes| {
        ExtraBedTerms {
            special: Some(sp),
            special_tax: Some(tax.clone()),
            room_tax: Some(tax),
            ..terms()
        }
        .as_pms_priced()
        .structural_fingerprint
    };
    let xb = pms_xb("453600");
    for sp in [
        SpecialService {
            code: "EB2".into(),
            ..xb.clone()
        },
        SpecialService {
            trn_code: Some("120".into()),
            ..xb.clone()
        },
        SpecialService {
            trn_sub: Some("10".into()),
            ..xb.clone()
        },
        SpecialService {
            include_in_room_charge: Some(1),
            ..xb.clone()
        },
    ] {
        assert_ne!(with(sp, v8s5()), base);
    }
    let v10 = TaxCodes {
        tax_code: Some("V10".into()),
        ..v8s5()
    };
    let s0 = TaxCodes {
        service_charge_code: Some("S0".into()),
        ..v8s5()
    };
    assert_ne!(with(xb.clone(), v10), base);
    assert_ne!(with(xb, s0), base);
}

#[test]
fn the_agent_priced_fingerprint_covers_the_open_price_zero_amount() {
    let hash = |sp: &SpecialService| {
        structural_fingerprint(
            "VIC",
            Some(sp),
            Some(&v8s5()),
            Some(&v8s5()),
            Some(true),
            Some(true),
            false,
        )
    };
    let priced = SpecialService {
        amount_charge: Some(Decimal::from(100)),
        ..victory_eb()
    };
    assert_ne!(hash(&victory_eb()), hash(&priced));
}

#[test]
fn bands_merge_runs_and_reject_a_short_list() {
    let r = v1_room(3, 1);
    assert_eq!(bands(&r), Some(vec![(Decimal::from(400_000), 1, 3)]));
    assert_eq!(bands(&v1_room(2, 0)), Some(Vec::new()));
}
