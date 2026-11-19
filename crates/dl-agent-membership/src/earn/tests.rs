//! The earn job reads against the recording session (statement order, parameters,
//! failure paths), golden SQL (`tests/fixtures/golden/sql/earn_job.json`, rewrite with
//! `GOLDEN_SQL_WRITE=1`) and the report bodies against the .NET export
//! (`tests/fixtures/golden/earn_reports.txt`).

use dl_decimal::Decimal;
use dl_tds::Value;
use dl_time::{Date, DateTime, OffsetDateTime, Time};
use dl_uuid::Uuid;

use super::job;
use super::pairs::{EarnSettings, PairSource};
use super::sql::{self, Stay};
use super::wire::{
    EarnReport, RevenueLine, ScanSummary, WalkInEarnReport, folio_revenue_j, pms_cancel_j,
};
use crate::jobs::plan::WriteError;
use crate::smile::SmileError;
use crate::wire::StayClassification;
use crate::writer::recorder::Recorder;
use crate::writer::session::Rec;

const BOOKING: &str = "6f1c2d3e-0000-4000-8000-000000000001";

fn s(v: &str) -> Value {
    Value::String(v.into())
}

fn dec(v: &str) -> Value {
    Value::Decimal(v.parse().unwrap())
}

fn dt(h: u8, mi: u8) -> DateTime {
    DateTime::new(
        Date::new(2026, 11, 18).unwrap(),
        Time::new(h, mi, 6, 0).unwrap(),
    )
}

pub(crate) fn golden_sql(name: &str, r: &Recorder) {
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

/// One room-charge code (`110/1`, `AutoPost = 1`) and a stay with two lines.
pub(crate) fn smile(auto_post_rows: i64) -> Recorder {
    let mut r = Recorder::default();
    let catalog = (0..auto_post_rows)
        .map(|i| {
            Rec::new(vec![
                ("Code", s(&format!("11{i}"))),
                ("Sub", s("1")),
                ("AutoPost", Value::I32(1)),
            ])
        })
        .collect();
    r.rows("EBedSurChgTrnCode", catalog);
    r.rows(
        "RmChgTrn",
        vec![Rec::new(vec![("RmChgTrn", Value::I32(11001))])],
    );
    r.rows(
        "AS Qualifies",
        vec![
            Rec::new(vec![
                ("TransactionCode", s("110")),
                ("TransactionSubCode", s("1")),
                ("Description", s("Room")),
                ("Amount", dec("1500000.00")),
                ("Qualifies", Value::I32(1)),
            ]),
            Rec::new(vec![
                ("TransactionCode", s("MB")),
                ("TransactionSubCode", s("0")),
                ("Description", Value::Null),
                ("Amount", dec("200000.00")),
                ("Qualifies", Value::I32(0)),
            ]),
        ],
    );
    r.rows(
        "AS K,",
        vec![
            Rec::new(vec![("K", s("O")), ("V", s("Membership"))]),
            Rec::new(vec![("K", s("S")), ("V", s("0"))]),
            Rec::new(vec![("K", s("R")), ("V", s(""))]),
        ],
    );
    r.rows(
        "MAX(CheckOutTime)",
        vec![Rec::new(vec![("", Value::DateTime(dt(11, 5)))])],
    );
    r
}

fn pairs(member_discount: &str) -> PairSource {
    PairSource::new(EarnSettings {
        charge_codes: Vec::new(),
        member_discount_code: member_discount.into(),
        extra_bed_special_code: "EB".into(),
    })
}

#[test]
fn the_job_reads_like_handle_earn_async() {
    let mut r = smile(1);
    let report = job::read(
        &mut r,
        &pairs("MD"),
        Some(" XB "),
        BOOKING.parse().unwrap(),
        "MM-TEST-1",
    )
    .unwrap();
    assert_eq!(report.folio_ref, "MM-TEST-1");
    assert_eq!(
        report.qualifying_revenue,
        "1500000.00".parse::<Decimal>().unwrap()
    );
    assert_eq!(report.folio_lines.as_ref().map(Vec::len), Some(2));
    assert_eq!(
        report.classification.as_ref().unwrap().origin_codes,
        vec!["Membership".to_owned()]
    );
    assert!(
        report
            .classification
            .as_ref()
            .unwrap()
            .segment_codes
            .is_empty()
    );
    assert_eq!(
        crate::stj::format_dto(report.checked_out_at),
        "2026-11-18T04:05:06+00:00",
        "SMILE local time 11:05 (+07:00) is 04:05 UTC"
    );
    assert_eq!(
        report.checked_out_at_source.as_deref(),
        Some("PMS_CHECKOUT_TIME")
    );
    // The hub's extra-bed code wins over the configured one.
    let catalog = &r.log[1];
    assert_eq!(catalog.params[0].2.as_deref(), Some("XB"));
    golden_sql("earn_job", &r);
}

#[test]
fn ambiguous_room_charge_fails_the_job_before_any_revenue_read() {
    let mut r = smile(2);
    let e = job::read(
        &mut r,
        &pairs(""),
        None,
        BOOKING.parse().unwrap(),
        "MM-TEST-1",
    )
    .unwrap_err();
    assert_eq!(
        e,
        WriteError::Coded {
            code: "UNKNOWN".into(),
            message: "PMS_EARN_ROOM_CODE_AMBIGUOUS: không xác định được mã tiền phòng SMILE — chưa tích điểm MM-TEST-1.".into(),
        }
    );
    assert_eq!(r.log.len(), 2, "RmChgTrn and the catalogue only");
}

#[test]
fn a_failed_breakdown_falls_back_to_the_total_without_lines() {
    let mut r = smile(1);
    r.on("AS Qualifies", |_| Err(SmileError::Server(8114)));
    r.rows("ISNULL(SUM", vec![Rec::new(vec![("", dec("1500000.00"))])]);
    let report = job::read(
        &mut r,
        &pairs(""),
        None,
        BOOKING.parse().unwrap(),
        "MM-TEST-1",
    )
    .unwrap();
    assert_eq!(report.folio_lines, None);
    assert_eq!(
        report.qualifying_revenue,
        "1500000.00".parse::<Decimal>().unwrap()
    );
    // No checkout time in SMILE: the agent clock.
    let mut r = smile(1);
    r.rows("MAX(CheckOutTime)", vec![Rec::new(vec![("", Value::Null)])]);
    let report = job::read(&mut r, &pairs(""), None, Uuid::NIL, "X").unwrap();
    assert_eq!(report.checked_out_at_source.as_deref(), Some("AGENT_CLOCK"));
}

#[test]
fn a_classification_failure_fails_the_job() {
    let mut r = smile(1);
    r.on("AS K,", |_| Err(SmileError::Server(207)));
    let e = job::read(&mut r, &pairs(""), None, Uuid::NIL, "X").unwrap_err();
    assert_eq!(e, WriteError::Smile(SmileError::Server(207)));
}

#[test]
fn walk_in_reads_use_the_confirm_num_scope() {
    let mut r = smile(1);
    let stay = Stay::WalkIn(Decimal::from(9001i64));
    sql::revenue_scan(&mut r, &stay, &[]).unwrap();
    sql::classification(&mut r, &stay).unwrap();
    assert!(
        r.log[0]
            .text
            .contains("f.ConfirmNum = @Conf AND f.ExternalConfirmNumber IS NULL")
    );
    assert!(!r.log[0].text.contains("Qualifies"), "no pair: no flag");
    assert_eq!(r.log[0].params[0].1, "Decimal");
    assert!(
        r.log[1]
            .text
            .contains("f.ConfirmNum = @Key AND f.ExternalConfirmNumber IS NULL")
    );
}

fn golden_lines() -> Vec<(String, String)> {
    let path = format!(
        "{}/tests/fixtures/golden/earn_reports.txt",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let (k, v) = l.split_once('=').unwrap();
            (k.to_owned(), v.to_owned())
        })
        .collect()
}

fn utc(ms: u32) -> OffsetDateTime {
    OffsetDateTime::new(
        DateTime::new(
            Date::new(2026, 11, 18).unwrap(),
            Time::new(4, 5, 6, ms * 1_000_000).unwrap(),
        ),
        dl_time::Offset::UTC,
    )
}

#[test]
fn report_bodies_match_the_dotnet_export() {
    let lines = vec![
        RevenueLine {
            transaction_code: "110".into(),
            transaction_sub_code: "1".into(),
            description: Some("Tiền phòng <Deluxe> & \"đêm\"".into()),
            amount: "1500000.00".parse().unwrap(),
        },
        RevenueLine {
            transaction_code: "MD".into(),
            transaction_sub_code: "0".into(),
            description: None,
            amount: "-50000".parse().unwrap(),
        },
    ];
    let cls = StayClassification {
        origin_codes: vec!["Membership".into(), "HU".into()],
        segment_codes: vec![],
        rate_codes: vec!["BAR1".into()],
    };
    let booking: Uuid = BOOKING.parse().unwrap();
    let got = |name: &str| -> String {
        match name {
            "earn_full" => EarnReport {
                booking_id: booking,
                folio_ref: "MM-TEST-1".into(),
                qualifying_revenue: "1450000.00".parse().unwrap(),
                checked_out_at: utc(0),
                folio_lines: Some(lines.clone()),
                classification: Some(cls.clone()),
                db_identity_fingerprint: Some("abc123".into()),
                checked_out_at_source: Some("PMS_CHECKOUT_TIME".into()),
            }
            .to_j()
            .to_json(),
            "earn_min" => EarnReport {
                booking_id: booking,
                folio_ref: "MM-TEST-1".into(),
                qualifying_revenue: Decimal::ZERO,
                checked_out_at: utc(789),
                folio_lines: None,
                classification: Some(StayClassification::default()),
                db_identity_fingerprint: None,
                checked_out_at_source: Some("AGENT_CLOCK".into()),
            }
            .to_j()
            .to_json(),
            "walk_in" | "walk_in_min" => {
                let full = name == "walk_in";
                WalkInEarnReport {
                    guest_phone: if full { "0901 234 567" } else { "0901234567" }.into(),
                    folio_ref: if full { "CONF:9001" } else { "CONF:9002" }.into(),
                    qualifying_revenue: if full {
                        Decimal::from(2_000_000i64)
                    } else {
                        Decimal::ZERO
                    },
                    checked_out_at: utc(0),
                    guest_name: full.then(|| "NGUYEN VAN A".into()),
                    folio_lines: full.then(|| lines.clone()),
                    classification: Some(if full {
                        cls.clone()
                    } else {
                        StayClassification::default()
                    }),
                    db_identity_fingerprint: full.then(|| "abc123".into()),
                }
                .to_j()
                .to_json()
            }
            "folio_revenue" => folio_revenue_j("MM-TEST-1", &lines).to_json(),
            "pms_cancel" => pms_cancel_j(booking, "MM-TEST-2").to_json(),
            "summary" | "summary_min" => {
                let full = name == "summary";
                ScanSummary {
                    business_date: Date::new(2026, 11, 18).unwrap(),
                    folios_seen: if full { 5 } else { 0 },
                    folios_reported: if full { 2 } else { 0 },
                    folios_skipped: if full { 3 } else { 0 },
                    skip_reasons: if full {
                        vec![("NO_MEMBER".into(), 1), ("REPORT_FAILED".into(), 2)]
                    } else {
                        vec![]
                    },
                    agent_version: full.then(|| "1.2.3.0".into()),
                }
                .to_j()
                .to_json()
            }
            other => panic!("no case for {other}"),
        }
    };
    let all = golden_lines();
    assert_eq!(all.len(), 8);
    for (name, want) in all {
        assert_eq!(got(&name), want, "{name}");
    }
}
