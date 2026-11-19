//! The checkout scan over the real `HubClient` against the fake hub: `pending-checkout`
//! is asked, the earn report, the folio-revenue audit, the walk-in report, the
//! PMS-cancelled report and the daily summary go out as the .NET agent sends them, and an
//! older hub (404/405/401 on `pms-cancelled`) turns that report off without failing.

mod support;

use std::sync::{Arc, Mutex};

use dl_agent_membership::checkout::{
    CheckoutDb, CheckoutError, CheckoutScanner, CheckoutSettings, WalkInEarnState,
};
use dl_agent_membership::earn::EarnEnv;
use dl_agent_membership::earn::pairs::{EarnSettings, PairSource};
use dl_agent_membership::earn::sql::EarnDb;
use dl_agent_membership::hub::HubClient;
use dl_agent_membership::hub::gate::TokenGate;
use dl_agent_membership::module::TOKEN_RETRY;
use dl_agent_membership::smile::{CodeRow, EarnCatalog, SmileError};
use dl_agent_membership::writer::session::{Called, Rec, Session};
use dl_decimal::Decimal;
use dl_json::Value;
use dl_tds::{Param, Value as Sql};
use dl_time::{Date, DateTime, Time};
use support::{FakeHub, HOTEL, TOKEN};

const MEMBER: &str = "6f1c2d3e-0000-4000-8000-0000000000aa";
const BOOKING_A: &str = "6f1c2d3e-0000-4000-8000-00000000000a";
const BOOKING_C: &str = "6f1c2d3e-0000-4000-8000-00000000000c";

fn s(v: &str) -> Sql {
    Sql::String(v.into())
}

fn local(h: u8) -> DateTime {
    DateTime::new(
        Date::new(2026, 11, 18).unwrap(),
        Time::new(h, 0, 0, 0).unwrap(),
    )
}

/// SMILE: booking MM-A checked out (member), MM-C cancelled, walk-in group 9001.
struct Smile;

impl Session for Smile {
    fn rows(&mut self, sql: &str, _: &[Param]) -> Result<Vec<Rec>, SmileError> {
        Ok(if sql.contains("LIKE @Tag") {
            vec![Rec::new(vec![
                ("FolioNum", Sql::Decimal(Decimal::from(7001i64))),
                ("ConfirmationCode", s("MM-A")),
                ("UserInfo2", s(&format!("MEM:{MEMBER};BKG:{BOOKING_A}"))),
                ("CheckOutTime", Sql::DateTime(local(11))),
            ])]
        } else if sql.contains("HAVING SUM(CASE WHEN ISNULL(CancelNum") {
            vec![Rec::new(vec![("ExternalConfirmNumber", s("MM-C"))])]
        } else if sql.contains("CROSS APPLY") {
            vec![Rec::new(vec![
                ("ConfirmNum", Sql::Decimal(Decimal::from(9001i64))),
                ("FirstFolioNum", Sql::Decimal(Decimal::from(8001i64))),
                ("CheckedOutAt", Sql::DateTime(local(10))),
                ("GuestPhone", s("0901 234 567")),
                ("GuestName", s("NGUYEN VAN A")),
            ])]
        } else if sql.contains("AS Qualifies") {
            vec![Rec::new(vec![
                ("TransactionCode", s("110")),
                ("TransactionSubCode", s("1")),
                ("Description", s("Room")),
                ("Amount", Sql::Decimal("1500000.00".parse().unwrap())),
                ("Qualifies", Sql::I32(1)),
            ])]
        } else if sql.contains("AS K,") {
            vec![Rec::new(vec![("K", s("O")), ("V", s("WD"))])]
        } else {
            Vec::new()
        })
    }
    fn execute(&mut self, _: &str, _: &[Param]) -> Result<u64, SmileError> {
        panic!("the checkout scan never writes to SMILE")
    }
    fn call(&mut self, _: &str, _: &[Param]) -> Result<Called, SmileError> {
        panic!("the checkout scan never calls a procedure")
    }
    fn batch(&mut self, _: &str) -> Result<(), SmileError> {
        panic!("the checkout scan never runs a batch")
    }
}

impl EarnDb for Smile {
    fn earn_catalog(&mut self, _: &str) -> Result<EarnCatalog, SmileError> {
        Ok(EarnCatalog {
            transaction_codes: vec![(
                CodeRow {
                    code: "110".into(),
                    sub: Some("1".into()),
                },
                1,
            )],
            ..EarnCatalog::default()
        })
    }
}

struct Db(Mutex<Smile>);

impl CheckoutDb for Db {
    fn with_db(
        &self,
        f: &mut dyn FnMut(&mut dyn EarnDb) -> Result<(), CheckoutError>,
    ) -> Result<(), CheckoutError> {
        f(&mut *self.0.lock().unwrap())
    }
}

struct Env;

impl EarnEnv for Env {
    fn fingerprint(&self) -> Option<String> {
        Some("fp-1".into())
    }
    fn hub_extra_bed_code(&self) -> Option<String> {
        None
    }
}

fn rig(cancel_status: u16) -> (FakeHub, HubClient, CheckoutScanner) {
    let hub = FakeHub::start();
    hub.reply(
        "/api/agent/bookings/pending-checkout",
        200,
        &format!(
            r#"[{{"bookingId":"{BOOKING_A}","confirmationCode":"MM-A"}},
                {{"bookingId":"{BOOKING_C}","confirmationCode":"MM-C"}}]"#
        ),
    );
    hub.reply("/api/agent/earn", 204, "");
    hub.reply(
        "/api/agent/earn/walk-in",
        200,
        r#"{"outcome":"Awarded","pointsAwarded":15}"#,
    );
    hub.reply("/api/agent/folio-revenue", 204, "");
    hub.reply("/api/agent/checkout-scan-summary", 204, "");
    hub.reply(
        "/api/agent/bookings/pms-cancelled",
        cancel_status,
        r#"{"cancelled":true}"#,
    );
    let client = HubClient::new(
        &hub.url(),
        TOKEN,
        HOTEL.parse().unwrap(),
        "dl-agent/test",
        Arc::new(TokenGate::new(TOKEN_RETRY)),
    )
    .unwrap();
    let scanner = CheckoutScanner::new(
        CheckoutSettings {
            enabled: true,
            ..CheckoutSettings::default()
        },
        "MobileNum",
        PairSource::new(EarnSettings::default()),
        Arc::new(WalkInEarnState::default()),
        "dl-agent/test",
    );
    (hub, client, scanner)
}

fn json(body: &str) -> Value {
    dl_json::parse(body).unwrap()
}

#[test]
fn a_scan_reports_earn_walk_in_cancel_and_summary_over_http() {
    let (hub, client, mut scanner) = rig(200);
    let db = Db(Mutex::new(Smile));
    let out = scanner.scan_once(&db, &client, &Env).unwrap();
    assert_eq!((out.pending, out.reported), (2, 2));
    let pending = hub.to("/api/agent/bookings/pending-checkout");
    assert_eq!(pending[0].path(), "/api/agent/bookings/pending-checkout");
    assert_eq!(pending[0].method, "GET");
    assert_eq!(
        pending[0].target,
        "/api/agent/bookings/pending-checkout?max=100&graceDays=1"
    );
    assert_eq!(
        hub.to("/api/agent/earn")[0].body,
        format!(
            "{{\"BookingId\":\"{BOOKING_A}\",\"FolioRef\":\"MM-A\",\"QualifyingRevenue\":1500000.00,\
             \"CheckedOutAt\":\"2026-11-18T04:00:00+00:00\",\"FolioLines\":[{{\"TransactionCode\":\"110\",\
             \"TransactionSubCode\":\"1\",\"Description\":\"Room\",\"Amount\":1500000.00}}],\
             \"Classification\":{{\"OriginCodes\":[\"WD\"],\"SegmentCodes\":[],\"RateCodes\":[]}},\
             \"DbIdentityFingerprint\":\"fp-1\",\"CheckedOutAtSource\":\"PMS_CHECKOUT_TIME\"}}"
        )
    );
    let walk_in = json(&hub.to("/api/agent/earn/walk-in")[0].body);
    assert_eq!(
        walk_in.get("FolioRef").and_then(Value::as_str),
        Some("CONF:9001")
    );
    assert_eq!(
        walk_in.get("CheckedOutAt").and_then(Value::as_str),
        Some("2026-11-18T10:00:00+07:00")
    );
    assert_eq!(
        walk_in.get("GuestPhone").and_then(Value::as_str),
        Some("0901 234 567")
    );
    assert_eq!(
        hub.to("/api/agent/folio-revenue")[0].body,
        "{\"FolioRef\":\"MM-A\",\"Lines\":[{\"TransactionCode\":\"110\",\"TransactionSubCode\":\"1\",\
         \"Description\":\"Room\",\"Amount\":1500000.00}]}"
    );
    assert_eq!(
        hub.to("/api/agent/bookings/pms-cancelled")[0].body,
        format!("{{\"BookingId\":\"{BOOKING_C}\",\"ConfirmationCode\":\"MM-C\"}}")
    );
    // The walk-in counts on its checkout day, the booking on the scan day (.NET HotelDay).
    let summaries = hub.to("/api/agent/checkout-scan-summary");
    let seen: i64 = summaries
        .iter()
        .map(|r| {
            json(&r.body)
                .get("FoliosSeen")
                .and_then(Value::as_i64)
                .unwrap()
        })
        .sum();
    assert_eq!(seen, 2);
    assert!(
        summaries
            .iter()
            .any(|r| r.body.contains("\"BusinessDate\":\"2026-11-18\""))
    );
    assert!(
        summaries
            .iter()
            .all(|r| r.body.contains("\"SkipReasons\":{}"))
    );
    assert!(hub.requests().iter().all(|r| !r.body.contains(TOKEN)));
}

#[test]
fn an_older_hub_without_pms_cancelled_is_not_a_failure() {
    for status in [404, 405, 401] {
        let (hub, client, mut scanner) = rig(status);
        let db = Db(Mutex::new(Smile));
        assert!(scanner.scan_once(&db, &client, &Env).is_ok(), "{status}");
        assert_eq!(hub.to("/api/agent/bookings/pms-cancelled").len(), 1);
    }
}

#[test]
fn a_failed_pending_checkout_fails_the_scan_without_touching_smile() {
    let (hub, client, mut scanner) = rig(200);
    hub.reply("/api/agent/bookings/pending-checkout", 500, "");
    let db = Db(Mutex::new(Smile));
    let err = scanner.scan_once(&db, &client, &Env).unwrap_err();
    assert!(
        err.to_string().contains("pending-checkout answered 500"),
        "{err}"
    );
    assert!(hub.to("/api/agent/earn").is_empty());
}
