//! `CheckoutScanner` against the recording session and an in-memory hub: which bookings
//! earn, the walk-in path and its memory, the PMS-cancelled report, the summary, and the
//! failure paths. Golden SQL: `tests/fixtures/golden/sql/checkout_*.json`.

use std::sync::{Arc, Mutex};

use dl_decimal::Decimal;
use dl_tds::Value;
use dl_time::{Date, DateTime, OffsetDateTime, Time};
use dl_uuid::Uuid;

use super::scanner::{CheckoutDb, CheckoutError, CheckoutScanner};
use super::settings::CheckoutSettings;
use super::sql;
use super::walk_in::WalkInEarnState;
use crate::earn::EarnEnv;
use crate::earn::pairs::{EarnSettings, PairSource};
use crate::earn::sql::EarnDb;
use crate::earn::tests::{golden_sql, smile};
use crate::earn::wire::{
    EarnReport, PendingBooking, RevenueLine, ScanSummary, WalkInEarnReport, WalkInOutcome,
    WalkInResult,
};
use crate::hub::HubError;
use crate::hub::earn::EarnHub;
use crate::smile::SmileError;
use crate::writer::recorder::Recorder;
use crate::writer::session::Rec;

const MEMBER: &str = "6f1c2d3e-0000-4000-8000-0000000000aa";
const BOOKING_A: &str = "6f1c2d3e-0000-4000-8000-00000000000a";
const BOOKING_B: &str = "6f1c2d3e-0000-4000-8000-00000000000b";
const BOOKING_C: &str = "6f1c2d3e-0000-4000-8000-00000000000c";

pub(crate) struct Db(pub(crate) Mutex<Recorder>);

impl CheckoutDb for Db {
    fn with_db(
        &self,
        f: &mut dyn FnMut(&mut dyn EarnDb) -> Result<(), CheckoutError>,
    ) -> Result<(), CheckoutError> {
        let mut r = self.0.lock().unwrap();
        f(&mut *r)
    }
}

impl Db {
    pub(crate) fn texts(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .texts()
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
}

#[derive(Default)]
pub(crate) struct Hub {
    pending: Vec<PendingBooking>,
    calls: Mutex<Vec<String>>,
    earns: Mutex<Vec<EarnReport>>,
    pub(crate) walk_ins: Mutex<Vec<WalkInEarnReport>>,
    summaries: Mutex<Vec<ScanSummary>>,
    fail_earn: Option<String>,
    old_hub: bool,
}

impl Hub {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn call(&self, c: String) {
        self.calls.lock().unwrap().push(c);
    }
}

impl EarnHub for Hub {
    fn report_earn(&self, r: &EarnReport) -> Result<(), HubError> {
        self.call(format!("earn {}", r.folio_ref));
        if self.fail_earn.as_deref() == Some(r.folio_ref.as_str()) {
            return Err(HubError::Status {
                code: 500,
                route: "earn",
            });
        }
        self.earns.lock().unwrap().push(r.clone());
        Ok(())
    }
    fn report_walk_in_earn(&self, r: &WalkInEarnReport) -> Result<WalkInResult, HubError> {
        self.call(format!("walk-in {}", r.folio_ref));
        self.walk_ins.lock().unwrap().push(r.clone());
        Ok(WalkInResult {
            outcome: WalkInOutcome::Parked,
            points_awarded: 0,
        })
    }
    fn pending_checkout(&self, max: u32, grace: u32) -> Result<Vec<PendingBooking>, HubError> {
        self.call(format!("pending {max} {grace}"));
        Ok(self.pending.clone())
    }
    fn report_pms_cancelled(&self, id: Uuid, code: &str) -> Result<Option<bool>, HubError> {
        self.call(format!("pms-cancelled {code} {id}"));
        Ok((!self.old_hub).then_some(true))
    }
    fn report_folio_revenue(&self, folio_ref: &str, lines: &[RevenueLine]) -> Result<(), HubError> {
        self.call(format!("folio-revenue {folio_ref} {}", lines.len()));
        Ok(())
    }
    fn report_scan_summary(&self, s: &ScanSummary) -> Result<(), HubError> {
        self.call(format!(
            "summary {}/{}/{}",
            s.folios_seen, s.folios_reported, s.folios_skipped
        ));
        self.summaries.lock().unwrap().push(s.clone());
        Ok(())
    }
}

pub(crate) struct Env;

impl EarnEnv for Env {
    fn fingerprint(&self) -> Option<String> {
        Some("fp".into())
    }
    fn hub_extra_bed_code(&self) -> Option<String> {
        None
    }
}

fn pending(id: &str, code: &str) -> PendingBooking {
    PendingBooking {
        booking_id: id.parse().unwrap(),
        confirmation_code: code.into(),
    }
}

fn s(v: &str) -> Value {
    Value::String(v.into())
}

fn local(h: u8) -> DateTime {
    DateTime::new(
        Date::new(2026, 11, 18).unwrap(),
        Time::new(h, 0, 0, 0).unwrap(),
    )
}

fn scanner(walk_in: bool, state: Arc<WalkInEarnState>) -> CheckoutScanner {
    CheckoutScanner::new(
        CheckoutSettings {
            enabled: true,
            walk_in_enabled: walk_in,
            ..CheckoutSettings::default()
        },
        "MobileNum",
        PairSource::new(EarnSettings {
            extra_bed_special_code: "EB".into(),
            ..EarnSettings::default()
        }),
        state,
        "dl-agent/test",
    )
    .with_clock(fixed_now)
}

fn fixed_now() -> OffsetDateTime {
    OffsetDateTime::new(local(5), dl_time::Offset::UTC)
}

/// SMILE: A (member) and B (no member) checked out, C fully cancelled.
fn bookings_smile() -> Recorder {
    let mut r = smile(1);
    r.rows(
        "LIKE @Tag",
        vec![
            Rec::new(vec![
                ("FolioNum", Value::Decimal(Decimal::from(7001i64))),
                ("ConfirmationCode", s("MM-A")),
                ("UserInfo2", s(&format!("MEM:{MEMBER};BKG:{BOOKING_A}"))),
                ("CheckOutTime", Value::DateTime(local(11))),
            ]),
            Rec::new(vec![
                ("FolioNum", Value::Decimal(Decimal::from(7002i64))),
                ("ConfirmationCode", s("MM-B")),
                ("UserInfo2", s(&format!("MEM:-;BKG:{BOOKING_B}"))),
                ("CheckOutTime", Value::Null),
            ]),
            Rec::new(vec![
                ("FolioNum", Value::Decimal(Decimal::from(7003i64))),
                ("ConfirmationCode", s("MM-X")),
                ("UserInfo2", s("front desk text")),
                ("CheckOutTime", Value::Null),
            ]),
        ],
    );
    r.rows(
        "HAVING SUM(CASE WHEN ISNULL(CancelNum",
        vec![Rec::new(vec![("ExternalConfirmNumber", s("MM-C"))])],
    );
    r
}

fn booking_hub() -> Hub {
    Hub {
        pending: vec![
            pending(BOOKING_A, "MM-A"),
            pending(BOOKING_B, "MM-B"),
            pending(BOOKING_C, "MM-C"),
        ],
        ..Hub::default()
    }
}

#[test]
fn checked_out_member_bookings_earn_and_cancelled_ones_are_reported() {
    let db = Db(Mutex::new(bookings_smile()));
    let hub = booking_hub();
    let mut sc = scanner(false, Arc::default());
    let out = sc.scan_once(&db, &hub, &Env).unwrap();
    assert_eq!((out.pending, out.reported), (3, 1));
    assert_eq!(
        hub.calls(),
        vec![
            "pending 100 1".to_owned(),
            "earn MM-A".into(),
            "folio-revenue MM-A 2".into(),
            format!("pms-cancelled MM-C {BOOKING_C}"),
            "summary 2/1/1".into(),
        ]
    );
    let earn = hub.earns.lock().unwrap()[0].clone();
    assert_eq!(earn.booking_id, BOOKING_A.parse::<Uuid>().unwrap());
    assert_eq!(earn.db_identity_fingerprint.as_deref(), Some("fp"));
    assert_eq!(
        crate::stj::format_dto(earn.checked_out_at),
        "2026-11-18T04:00:00+00:00"
    );
    assert_eq!(
        hub.summaries.lock().unwrap()[0].skip_reasons,
        vec![("NO_MEMBER".to_owned(), 1)]
    );
    golden_sql("checkout_bookings", &db.0.lock().unwrap());
    // Unchanged tally: no second summary.
    sc.scan_once(&db, &hub, &Env).unwrap();
    assert_eq!(
        hub.calls()
            .iter()
            .filter(|c| c.starts_with("summary"))
            .count(),
        1
    );
}

#[test]
fn a_failed_booking_does_not_stop_the_others_and_an_old_hub_stops_cancel_reports() {
    let db = Db(Mutex::new(bookings_smile()));
    let hub = Hub {
        fail_earn: Some("MM-A".into()),
        old_hub: true,
        ..booking_hub()
    };
    let mut sc = scanner(false, Arc::default());
    let out = sc.scan_once(&db, &hub, &Env).unwrap();
    assert_eq!(out.reported, 0);
    assert!(!hub.calls().iter().any(|c| c.starts_with("folio-revenue")));
    let s = &hub.summaries.lock().unwrap()[0];
    assert_eq!(
        s.skip_reasons,
        vec![("REPORT_FAILED".to_owned(), 1), ("NO_MEMBER".to_owned(), 1)]
    );
    assert_eq!(
        hub.calls()
            .iter()
            .filter(|c| c.starts_with("pms-cancelled"))
            .count(),
        1
    );
}

#[test]
fn ambiguous_room_charge_reports_no_earn_but_still_reports_cancellations() {
    let mut r = bookings_smile();
    let two = vec![
        Rec::new(vec![
            ("Code", s("110")),
            ("Sub", s("1")),
            ("AutoPost", Value::I32(1)),
        ]),
        Rec::new(vec![
            ("Code", s("111")),
            ("Sub", s("1")),
            ("AutoPost", Value::I32(1)),
        ]),
    ];
    r.rows("EBedSurChgTrnCode", two);
    let db = Db(Mutex::new(r));
    let hub = booking_hub();
    scanner(false, Arc::default())
        .scan_once(&db, &hub, &Env)
        .unwrap();
    assert!(!hub.calls().iter().any(|c| c.starts_with("earn")));
    assert!(hub.calls().iter().any(|c| c.starts_with("pms-cancelled")));
}

#[test]
fn nothing_pending_and_no_walk_in_never_opens_smile() {
    let db = Db(Mutex::new(Recorder::default()));
    let hub = Hub::default();
    scanner(false, Arc::default())
        .scan_once(&db, &hub, &Env)
        .unwrap();
    assert!(db.texts().is_empty());
    assert_eq!(hub.calls(), vec!["pending 100 1".to_owned()]);
}

fn walk_in_smile() -> Recorder {
    let mut r = smile(1);
    r.rows(
        "CROSS APPLY",
        vec![Rec::new(vec![
            ("ConfirmNum", Value::Decimal(Decimal::from(9001i64))),
            ("FirstFolioNum", Value::Decimal(Decimal::from(8001i64))),
            ("CheckedOutAt", Value::DateTime(local(10))),
            ("GuestPhone", s("0901234567")),
            ("GuestName", s("  ")),
        ])],
    );
    r
}

#[test]
fn walk_ins_report_once_and_are_excluded_afterwards() {
    let db = Db(Mutex::new(walk_in_smile()));
    let hub = Hub::default();
    let state = Arc::new(WalkInEarnState::default());
    let mut sc = scanner(true, state.clone());
    assert_eq!(sc.scan_once(&db, &hub, &Env).unwrap().reported, 1);
    assert_eq!(state.is_healthy(), Some(true));
    {
        let w = &hub.walk_ins.lock().unwrap()[0];
        assert_eq!(w.folio_ref, "CONF:9001");
        assert_eq!(w.guest_name, None);
        assert_eq!(
            crate::stj::format_dto(w.checked_out_at),
            "2026-11-18T10:00:00+07:00",
            "walk-in checkout keeps the hotel offset"
        );
    }
    golden_sql("checkout_walk_in", &db.0.lock().unwrap());
    // The next scan excludes the group in SQL (padded to 16) and skips it if it comes back.
    sc.scan_once(&db, &hub, &Env).unwrap();
    assert_eq!(hub.walk_ins.lock().unwrap().len(), 1);
    let last_walk_in = db
        .texts()
        .into_iter()
        .rfind(|t| t.contains("CROSS APPLY"))
        .unwrap();
    assert!(last_walk_in.contains("AND ConfirmNum NOT IN (@Ex0, @Ex1,"));
    assert!(last_walk_in.contains("@Ex15)"));
}

#[test]
fn a_missing_phone_column_turns_walk_ins_off_but_not_booking_earn() {
    let mut r = bookings_smile();
    r.on("CROSS APPLY", |_| Err(SmileError::Server(207)));
    let db = Db(Mutex::new(r));
    let hub = booking_hub();
    let state = Arc::new(WalkInEarnState::default());
    let mut sc = scanner(true, state.clone());
    assert_eq!(sc.scan_once(&db, &hub, &Env).unwrap().reported, 1);
    assert_eq!(state.is_healthy(), Some(false));
    let before = db.texts().len();
    sc.scan_once(&db, &hub, &Env).unwrap();
    assert!(
        !db.texts()[before..]
            .iter()
            .any(|t| t.contains("CROSS APPLY")),
        "walk-ins stay off"
    );
    // An invalid column name is refused before any SQL.
    let db = Db(Mutex::new(Recorder::default()));
    let mut sc = CheckoutScanner::new(
        CheckoutSettings {
            enabled: true,
            ..CheckoutSettings::default()
        },
        "Mobile;DROP",
        PairSource::default(),
        Arc::default(),
        "v",
    );
    sc.scan_once(&db, &Hub::default(), &Env).unwrap();
    assert!(db.texts().is_empty());
}

#[test]
fn sql_helpers_follow_dotnet() {
    let (m, b) = sql::parse_user_info2(Some(&format!(" MEM:{MEMBER} ; BKG:{BOOKING_A};")));
    assert_eq!(m, Some(MEMBER.parse().unwrap()));
    assert_eq!(b, BOOKING_A.parse::<Uuid>().unwrap());
    assert_eq!(
        sql::parse_user_info2(Some("MEM:-;BKG:x")),
        (None, Uuid::NIL)
    );
    assert_eq!(sql::safe_identifier("MobileNum").unwrap(), "[MobileNum]");
    assert_eq!(sql::safe_identifier("_x1").unwrap(), "[_x1]");
    for bad in ["", "1Mobile", "Mobile Num", "a]b", "Số"] {
        assert!(sql::safe_identifier(bad).is_err(), "{bad}");
    }
    let nums: Vec<Decimal> = (0..17).map(Decimal::from).collect();
    let (clause, params) = sql::padded_exclusion(&nums);
    assert_eq!(params.len(), 64);
    assert!(clause.ends_with("@Ex63)"));
    let (_, params) = sql::padded_exclusion(&(0..600).map(Decimal::from).collect::<Vec<_>>());
    assert_eq!(params.len(), 600, "never drops an exclusion");
    assert_eq!(sql::padded_exclusion(&[]).0, "");
}
