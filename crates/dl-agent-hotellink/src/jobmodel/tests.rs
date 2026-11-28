//! Port of `job_test.go`, `pmsresult_test.go`, `statusaction_test.go`, plus golden
//! vectors produced by Go's `encoding/json` (`testdata/go_golden.tsv`).

use super::*;

const GOLDEN: &str = include_str!("testdata/go_golden.tsv");

fn golden(name: &str) -> &'static str {
    GOLDEN
        .lines()
        .find_map(|l| {
            l.split_once('\t')
                .filter(|(k, _)| *k == name)
                .map(|(_, v)| v)
        })
        .unwrap_or_else(|| panic!("golden {name} missing"))
}

pub(crate) fn sample_job() -> Job {
    Job {
        job_id: "0001749543210123-123456".into(),
        hotel_id: "h1".into(),
        action: ACTION_SAVE.into(),
        booking_id: "123456".into(),
        booker: "EXT-1".into(),
        source_name: "Booking.com".into(),
        hl_status: "Confirmed".into(),
        booking_date: "2026-06-01 10:00:00".into(),
        check_in: "2026-06-12".into(),
        check_out: "2026-06-14".into(),
        guest_name: "NGUYEN VAN DUC".into(),
        guest_phone: "0901".into(),
        guest_email: "a@b.c".into(),
        book_status: 1,
        rooms: Some(vec![Room {
            room_type_code: "DLX01".into(),
            quantity: 2,
            adults: 3,
            children: 1,
            include_bf: true,
            breakfast_code: "ABU".into(),
            rates: Some(vec![
                RateNight {
                    date: "2026-06-12".into(),
                    rate: 1_500_000.0,
                    ..Default::default()
                },
                RateNight {
                    date: "2026-06-13".into(),
                    rate: 1_500_000.0,
                    ..Default::default()
                },
            ]),
            ..Default::default()
        }]),
        enqueued_at: "2026-06-10T08:30:00Z".into(),
        ..Default::default()
    }
}

fn full_job() -> Job {
    let mut b = sample_job();
    b.not_before = "2026-06-10T09:00:00Z".into();
    b.last_error = "lỗi <cũ> & x".into();
    b.hotel_name = "Grand Hotel".into();
    b.notification_type = "New".into();
    b.booking_source_code = "BKG".into();
    b.ext_booking_ref = "EXT-REF-999".into();
    b.currency_iso = "VND".into();
    b.amount = 4_500_000.0;
    b.deposit = 1_000_000.5;
    b.tax_fee = 450_000.0;
    b.tax_fee_on_arrival = 100_000.0;
    b.discount = 50_000.0;
    b.payment_fee = 0.1;
    b.ip = "1.2.3.4".into();
    b.additional_comments = "Late check-in".into();
    b.guest_title = "Mr".into();
    b.guest_first_name = "Van Duc".into();
    b.guest_last_name = "Nguyen".into();
    b.guest_address = "123 Lê Lợi".into();
    b.guest_city = "Ho Chi Minh".into();
    b.guest_state = "HCM".into();
    b.guest_country = "VN".into();
    b.guest_postal_code = "70000".into();
    b.payments = vec![HlPayment {
        method: "CreditCard".into(),
        payment_date: "2026-06-01".into(),
        amount: 4_500_000.0,
        status: "Received".into(),
        payment_fee: 1.5,
        kind: "Charge".into(),
    }];
    b.extras = vec![HlExtra {
        service_name: "Parking".into(),
        quantity: 1,
        extra_children: 2,
        deposit: 3.0,
        amount: 100_000.0,
    }];
    b.first_seen_at = "2026-06-15T00:00:00Z".into();
    b.done_at = "2026-06-15T01:00:00Z".into();
    let rooms = b.rooms.as_mut().unwrap();
    let r = &mut rooms[0];
    r.item_id = "it1".into();
    r.room_type_hl = "Deluxe Ocean View".into();
    r.room_code = "RC01".into();
    r.rate_plan_id = "RP-999".into();
    r.rate_plan_code = "BAR".into();
    r.infants = 1;
    r.extra_adults = 2;
    r.extra_children = 1;
    r.hl_amount = 3_000_000.0;
    r.hl_deposit = 500_000.0;
    r.hl_discount = 100_000.0;
    r.commission = 50_000.25;
    r.item_status = "Confirmed".into();
    r.special_name = "Honeymoon Package".into();
    r.rates.as_mut().unwrap()[0].extras = vec![
        RateNightExtra {
            kind: "X".into(),
            amount: 1.0,
        },
        RateNightExtra::default(),
    ];
    rooms.push(Room {
        room_type_code: "STD".into(),
        quantity: 1,
        ..Default::default()
    });
    b
}

fn mapping_rows() -> Vec<MappingRow> {
    vec![
        MappingRow {
            room_id: "101".into(),
            room_type_code: "STD".into(),
            split_part: Some(1),
            total_part: Some(2),
            is_booking: false,
        },
        MappingRow {
            room_id: String::new(),
            room_type_code: "DLX".into(),
            is_booking: true,
            ..Default::default()
        },
        MappingRow {
            room_id: "102".into(),
            room_type_code: "STD".into(),
            ..Default::default()
        },
    ]
}

#[test]
fn json_matches_go_golden() {
    assert_eq!(sample_job().to_json(), golden("sample"));
    assert_eq!(full_job().to_json(), golden("full"));
    assert_eq!(Job::default().to_json(), golden("empty"));
    let mut rows = String::new();
    encode::mapping_rows(&mut rows, &mapping_rows());
    assert_eq!(rows, golden("mappingRows"));
    let manual = PmsResult {
        action: "cancel".into(),
        needs_manual: true,
        reason: "r".into(),
        ..Default::default()
    };
    assert_eq!(manual.to_json(), golden("manual"));
    let policy = CancelPolicy {
        on_cancel: OpPolicy {
            allow_when_room_assigned: true,
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(policy.to_json(), golden("policy"));
    let rows = mapping_rows();
    let cmd = MappingCmd {
        cmd_id: "c1".into(),
        hotel_id: "h1".into(),
        op: MAPPING_UPDATE.into(),
        row: rows[0].clone(),
        orig: Some(rows[1].clone()),
        enqueued_at: "t".into(),
    };
    assert_eq!(cmd.to_json(), golden("cmd"));
}

#[test]
fn hashes_match_go_golden() {
    assert_eq!(sample_job().content_hash(), golden("sampleHash"));
    assert_eq!(full_job().content_hash(), golden("fullHash"));
    assert_eq!(Job::default().content_hash(), golden("emptyHash"));
    assert_eq!(mapping_hash(&mapping_rows()), golden("mappingHash"));
    assert_eq!(mapping_hash(&[]), golden("mappingHashEmpty"));
}

#[test]
fn decode_round_trips_golden() {
    for name in ["sample", "full", "empty"] {
        let v = dl_json::parse(golden(name)).unwrap();
        assert_eq!(Job::from_value(&v).to_json(), golden(name), "{name}");
    }
    let v = dl_json::parse(golden("cmd")).unwrap();
    assert_eq!(MappingCmd::from_value(&v).to_json(), golden("cmd"));
}

#[test]
fn action_valid() {
    for a in [ACTION_SAVE, ACTION_CANCEL, ACTION_MANUAL, ACTION_IGNORE] {
        assert!(super::action_valid(a), "{a}");
    }
    assert!(!super::action_valid("bogus"));
}

#[test]
fn new_job_id_sorts_by_time() {
    let got = new_job_id(1_749_543_210_123, "123456");
    assert_eq!(got, "0001749543210123-123456");
    assert!(got < new_job_id(1_749_543_210_124, "000001"));
}

#[test]
fn content_hash_stable() {
    let a = sample_job();
    let mut b = sample_job();
    b.job_id = "9999999999999999-123456".into();
    b.attempts = 3;
    b.not_before = "2026-06-10T09:00:00Z".into();
    b.enqueued_at = "2026-06-11T00:00:00Z".into();
    b.last_error = "lỗi cũ".into();
    assert_eq!(a.content_hash(), b.content_hash());
    let mut c = sample_job();
    c.rooms.as_mut().unwrap()[0].rates.as_mut().unwrap()[0].rate = 1_600_000.0;
    assert_ne!(a.content_hash(), c.content_hash());
    let mut d = sample_job();
    d.action = ACTION_CANCEL.into();
    assert_ne!(a.content_hash(), d.content_hash());
}

#[test]
fn room_variant_is_agent_local() {
    // RT-1474: the reconciler's variant suffix never reaches the wire or the hash.
    let mut j = sample_job();
    j.rooms.as_mut().unwrap()[0].variant = "e7f122ef".into();
    assert_eq!(j.to_json(), golden("sample"));
    assert_eq!(j.content_hash(), golden("sampleHash"));
    let back = Job::from_value(&dl_json::parse(&j.to_json()).unwrap());
    assert!(back.rooms()[0].variant.is_empty());
}

#[test]
fn content_hash_ignores_hl_fields() {
    let mut f = full_job();
    // full_job adds a second room and an item id, which are business fields.
    f.rooms.as_mut().unwrap().pop();
    f.rooms.as_mut().unwrap()[0].item_id.clear();
    assert_eq!(sample_job().content_hash(), f.content_hash());
}

#[test]
fn content_hash_sensitive_to_business_fields() {
    let base = sample_job().content_hash();
    type Mutation = (&'static str, fn(&mut Job));
    let muts: Vec<Mutation> = vec![
        ("ItemID", |j| {
            j.rooms.as_mut().unwrap()[0].item_id = "X".into()
        }),
        ("RoomTypeCode", |j| {
            j.rooms.as_mut().unwrap()[0].room_type_code = "X".into()
        }),
        ("Quantity", |j| j.rooms.as_mut().unwrap()[0].quantity = 99),
        ("Adults", |j| j.rooms.as_mut().unwrap()[0].adults = 99),
        ("Children", |j| j.rooms.as_mut().unwrap()[0].children = 99),
        ("IncludeBF", |j| {
            let r = &mut j.rooms.as_mut().unwrap()[0];
            r.include_bf = !r.include_bf;
        }),
        ("BreakfastCode", |j| {
            j.rooms.as_mut().unwrap()[0].breakfast_code = "X".into()
        }),
        ("RateDate", |j| {
            j.rooms.as_mut().unwrap()[0].rates.as_mut().unwrap()[0].date = "1999-01-01".into()
        }),
        ("RateAmount", |j| {
            j.rooms.as_mut().unwrap()[0].rates.as_mut().unwrap()[0].rate = 1.0
        }),
        ("GuestName", |j| j.guest_name = "X".into()),
        ("BookStatus", |j| j.book_status = 9),
        ("PaymentCode", |j| j.payment_code = "X".into()),
    ];
    for (name, m) in muts {
        let mut j = sample_job();
        m(&mut j);
        assert_ne!(base, j.content_hash(), "{name}");
    }
}

#[test]
fn pms_result_omits_empty_reason_and_manual() {
    let r = PmsResult {
        action: "save".into(),
        completed: true,
        folios: vec![PmsFolio {
            folio_num: 100_001.0,
            confirm_num: 500_001.0,
            external_conf: "EXT-1".into(),
            book_status: 1,
            folio_status: 2,
            room_code: "DLX".into(),
            room_type_code: "DLX".into(),
            arrival_date: "2026-06-15".into(),
            departure_date: "2026-06-17".into(),
            ..Default::default()
        }],
        synced_at: "2026-06-15T03:04:05Z".into(),
        ..Default::default()
    };
    let s = r.to_json();
    assert!(
        !s.contains("\"reason\"") && !s.contains("\"needsManual\""),
        "{s}"
    );
    for tag in [
        r#""action":"save""#,
        r#""folioNum":100001"#,
        r#""externalConfirmNumber":"EXT-1""#,
        r#""cancelNum":0"#,
        r#""syncedAt":"2026-06-15T03:04:05Z""#,
    ] {
        assert!(s.contains(tag), "{tag} in {s}");
    }
    let back = PmsResult::from_value(&dl_json::parse(&s).unwrap());
    assert_eq!(back, r);
}

#[test]
fn cancel_policy_legacy_flat() {
    let v = dl_json::parse(
        r#"{"allowCancelWhenRoomAssigned":true,"allowCancelWhenAbnormalBookStatus":true,"allowCancelWhenCheckedIn":false}"#,
    )
    .unwrap();
    let p = policy_from_value(&v);
    assert!(p.on_cancel.allow_when_room_assigned && p.on_cancel.allow_when_abnormal_book_status);
    assert!(!p.on_cancel.allow_when_checked_in);
    assert_eq!(p.on_modify, OpPolicy::default());
}

#[test]
fn cancel_policy_nested_round_trip() {
    let v = dl_json::parse(
        r#"{"onCancel":{"allowWhenRoomAssigned":true},"onModify":{"allowWhenCheckedIn":true}}"#,
    )
    .unwrap();
    let p = policy_from_value(&v);
    assert!(p.on_cancel.allow_when_room_assigned && p.on_modify.allow_when_checked_in);
    let p2 = policy_from_value(&dl_json::parse(&p.to_json()).unwrap());
    assert_eq!(p, p2);
}

#[test]
fn cancel_policy_empty() {
    let p = policy_from_value(&dl_json::parse("{}").unwrap());
    assert_eq!(p, CancelPolicy::default());
}

/// RT-0702: `auditGaps` is omitted when empty and round-trips when set (Go `omitempty`).
#[test]
fn pms_result_audit_gaps_are_omitted_when_empty_and_round_trip() {
    let mut r = PmsResult {
        action: "save".into(),
        completed: true,
        synced_at: "2026-06-15T03:04:05Z".into(),
        ..Default::default()
    };
    assert!(!r.to_json().contains("auditGaps"), "{}", r.to_json());
    r.audit_gaps = vec!["FolioLog HL:Create folio 100001: \"x\" locked".into()];
    let s = r.to_json();
    assert!(
        s.ends_with(r#","auditGaps":["FolioLog HL:Create folio 100001: \"x\" locked"]}"#),
        "{s}"
    );
    let back = PmsResult::from_value(&dl_json::parse(&s).unwrap());
    assert_eq!(back.audit_gaps, r.audit_gaps);
}
