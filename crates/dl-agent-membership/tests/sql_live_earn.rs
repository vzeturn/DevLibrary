//! Earn against a real SQL Server (opt-in, like `sql_live_writes`): the `EarnPoints` job
//! through the real `SmileWriter`, the checkout scan (booking earn, walk-in earn,
//! PMS-cancelled) and the walk-in backfill, all reporting to the fake hub.
//!
//! `MEMBERSHIP_AGENT_TEST_SQL` holds an ADO connection string to a SQL Server whose login
//! may create databases. The test builds a throw-away database from the SMILE fixture
//! schema, seeds folios and `FolioTransaction` rows, checks every body the hub receives,
//! and drops the database. Without the variable it returns at once. The data is synthetic.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dl_agent_membership::checkout::backfill::{self, Plan};
use dl_agent_membership::checkout::{CheckoutScanner, CheckoutSettings, WalkInEarnState};
use dl_agent_membership::earn::EarnEnv;
use dl_agent_membership::earn::pairs::{EarnSettings, PairSource};
use dl_agent_membership::hub::earn::EarnHub;
use dl_agent_membership::hub::gate::TokenGate;
use dl_agent_membership::hub::{HubApi, HubClient};
use dl_agent_membership::identity::Tracker;
use dl_agent_membership::jobs::beds::MODE_FOLD_LEGACY;
use dl_agent_membership::jobs::dispatch::{DispatchSettings, Dispatcher};
use dl_agent_membership::module::TOKEN_RETRY;
use dl_agent_membership::module::state::Shared;
use dl_agent_membership::smile::{TdsSmile, hotel_now, normalize_connection_string};
use dl_agent_membership::sync::{SyncSettings, SyncWorker};
use dl_agent_membership::writer::folio::{FolioWriter, WriteOptions};
use dl_agent_membership::writer::smile_writer::{EarnPath, SmileWriter};
use dl_json::{Value, json};
use dl_tds::{Client, Config};
use support::{FakeHub, HOTEL, TOKEN};

const ENV: &str = "MEMBERSHIP_AGENT_TEST_SQL";
const SCHEMA: &str = include_str!("fixtures/smile_schema.sql");
const BOOKING: &str = "6f1c2d3e-0000-4000-8000-000000000001";
const BOOKING_C: &str = "6f1c2d3e-0000-4000-8000-00000000000c";
const MEMBER: &str = "6f1c2d3e-0000-4000-8000-0000000000aa";

/// Booking MM-LIVE-1: two checked-out rooms (one more `'B'` line that must not count);
/// MM-LIVE-C: cancelled; walk-in group 9001 checked out an hour ago with a phone on
/// `TeleNum` only.
fn seed() -> String {
    format!(
        "\
INSERT INTO TransactionCode (TransactionCode, TransactionSubCode, Description, AutoPost)
VALUES ('110', '1', N'Room', 1), ('MB', '0', N'Minibar', 0);
INSERT INTO Folio (FolioNum, ConfirmNum, ExternalConfirmNumber, UserInfo2, FolioStatus, CheckOutTime,
                   DepartureDate, OriginCode, RmChgTrn, LastName)
VALUES (7001, 8001, 'MM-LIVE-1', 'MEM:{MEMBER};BKG:{BOOKING}', 3, '2026-11-18T11:05:06', '2026-11-18',
        'Membership', 11001, 'GUEST'),
       (7002, 8001, 'MM-LIVE-1', 'MEM:-;BKG:{BOOKING}', 3, '2026-11-18T10:00:00', '2026-11-18',
        'Membership', 11001, 'GUEST');
INSERT INTO Folio (FolioNum, ConfirmNum, ExternalConfirmNumber, UserInfo2, FolioStatus, CancelNum)
VALUES (7003, 8002, 'MM-LIVE-C', 'MEM:-;BKG:{BOOKING_C}', 1, 99);
INSERT INTO Folio (FolioNum, ConfirmNum, ExternalConfirmNumber, FolioStatus, CheckOutTime, DepartureDate,
                   OriginCode, LastName)
VALUES (7010, 9001, NULL, 3, DATEADD(hour, 6, GETUTCDATE()), CAST(GETUTCDATE() AS date), 'WD', 'WALK IN');
INSERT INTO AdditionalName (FolioNum, LastName, TeleNum) VALUES (7010, 'WALK IN', ' 0901234567 ');
INSERT INTO FolioTransaction (FolioNum, TransactionCode, TransactionSubCode, TransactionAmount, FolioBalanceCode)
VALUES ('7001', '110', 1, 1000000.00, 'A'),
       ('7001', 'MB', 0, 200000.00, 'A'),
       ('7002', '110', 1, 500000.00, 'A'),
       ('7002', '110', 1, 300000.00, 'B'),
       ('7010', '110', 1, 400000.00, 'A');"
    )
}

fn connect(cfg: &Config) -> Client {
    let mut last = String::new();
    for _ in 0..30 {
        match Client::connect(cfg) {
            Ok(c) => return c,
            Err(e) => last = format!("{:?}", e.number()),
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    panic!("SQL Server login failed after 30 tries (error {last})");
}

struct Db {
    master: Config,
    name: String,
    conn: String,
}

impl Drop for Db {
    fn drop(&mut self) {
        let sql = format!(
            "ALTER DATABASE [{0}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{0}];",
            self.name
        );
        if let Err(e) = connect(&self.master).batch(&sql) {
            eprintln!("could not drop test database {}: {e}", self.name);
        }
    }
}

fn create(conn: &str) -> Db {
    let base = normalize_connection_string(conn)
        .unwrap_or_else(|_| panic!("{ENV} is not a usable connection string"));
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("dl_mbe_{}_{nanos}", std::process::id());
    let master = base.clone().with_database("master");
    connect(&master)
        .batch(&format!("CREATE DATABASE [{name}];"))
        .expect("CREATE DATABASE");
    let mut c = connect(&base.with_database(&name));
    for (i, b) in SCHEMA
        .split("\nGO\n")
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .enumerate()
    {
        c.batch(b)
            .unwrap_or_else(|e| panic!("schema batch {i}: {e}"));
    }
    c.batch(&seed()).unwrap_or_else(|e| panic!("seed: {e}"));
    c.close();
    let conn = format!("{};Database={name}", conn.trim_end_matches(';'));
    Db { master, name, conn }
}

struct Env;

impl EarnEnv for Env {
    fn fingerprint(&self) -> Option<String> {
        Some("fp-live".into())
    }
    fn hub_extra_bed_code(&self) -> Option<String> {
        None
    }
}

fn client(hub: &FakeHub) -> Arc<HubClient> {
    Arc::new(
        HubClient::new(
            &hub.url(),
            TOKEN,
            HOTEL.parse().unwrap(),
            "dl-agent/test",
            Arc::new(TokenGate::new(TOKEN_RETRY)),
        )
        .unwrap(),
    )
}

fn body(hub: &FakeHub, path: &str) -> Value {
    let seen = hub.to(path);
    assert_eq!(seen.len(), 1, "{path}");
    dl_json::parse(&seen[0].body).unwrap()
}

fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// The raw JSON number text (`1500000.00` keeps its scale).
fn number(hub: &FakeHub, path: &str, key: &str) -> String {
    let raw = &hub.to(path)[0].body;
    let at = raw.find(&format!("\"{key}\":")).unwrap() + key.len() + 3;
    raw[at..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect()
}

fn earn_job_through_smile_writer(db: &Db) {
    let hub = FakeHub::start();
    hub.reply("/api/agent/jobs/ack", 204, "");
    hub.reply("/api/agent/earn", 204, "");
    let payload = format!(r#"{{"bookingId":"{BOOKING}","confirmationCode":"MM-LIVE-1"}}"#);
    hub.reply(
        "/api/agent/jobs/pull",
        200,
        &dl_json::to_string(&json!([{
            "JobId": "earn-1", "Action": "EarnPoints", "BookingId": BOOKING,
            "PayloadJson": payload, "Attempts": 1
        }])),
    );
    let c = client(&hub);
    let smile = Arc::new(TdsSmile::new(&db.conn).unwrap());
    let writer = SmileWriter::new(smile.clone(), FolioWriter::new(WriteOptions::default()))
        .with_earn(EarnPath {
            pairs: PairSource::new(EarnSettings {
                extra_bed_special_code: "EB".into(),
                ..EarnSettings::default()
            }),
            hub: c.clone() as Arc<dyn EarnHub>,
            env: Arc::new(Env),
        });
    let api: Arc<dyn HubApi> = c;
    let tracker = Arc::new(Tracker::new(smile.clone()));
    let mut sync = SyncWorker::new(
        api,
        smile,
        tracker,
        Arc::new(Shared::default()),
        SyncSettings {
            pull_jobs: true,
            max_jobs_per_pull: 5,
            heartbeat: Duration::from_secs(3600),
            capabilities: Vec::new(),
        },
        "dl-agent/test",
    )
    .with_dispatcher(Arc::new(Dispatcher::new(
        Arc::new(writer),
        None,
        DispatchSettings {
            origin_code: "Membership".into(),
            member_discount_code: String::new(),
            extra_bed_mode: MODE_FOLD_LEGACY.into(),
        },
    )));
    sync.step(Instant::now()).unwrap();
    let ack = body(&hub, "/api/agent/jobs/ack");
    assert_eq!(
        ack.get("Success").and_then(Value::as_bool),
        Some(true),
        "{ack:?}"
    );
    let earn = body(&hub, "/api/agent/earn");
    assert_eq!(text(&earn, "FolioRef"), "MM-LIVE-1");
    assert_eq!(
        number(&hub, "/api/agent/earn", "QualifyingRevenue"),
        "1500000.00",
        "both rooms' 110/1 'A' lines; the minibar and the 'B' line do not earn"
    );
    assert_eq!(text(&earn, "CheckedOutAt"), "2026-11-18T04:05:06+00:00");
    assert_eq!(text(&earn, "CheckedOutAtSource"), "PMS_CHECKOUT_TIME");
    assert_eq!(text(&earn, "DbIdentityFingerprint"), "fp-live");
    let lines = earn.get("FolioLines").and_then(Value::as_array).unwrap();
    assert_eq!(lines.len(), 3, "every 'A' line of the live folios");
    assert!(lines.iter().any(|l| text(l, "Description") == "Minibar"));
    let origins = earn
        .get("Classification")
        .and_then(|c| c.get("OriginCodes"))
        .and_then(Value::as_array)
        .unwrap();
    assert_eq!(origins.len(), 1);
}

fn checkout_scan(db: &Db) {
    let hub = FakeHub::start();
    hub.reply(
        "/api/agent/bookings/pending-checkout",
        200,
        &format!(
            r#"[{{"BookingId":"{BOOKING}","ConfirmationCode":"MM-LIVE-1"}},
                {{"BookingId":"{BOOKING_C}","ConfirmationCode":"MM-LIVE-C"}}]"#
        ),
    );
    for route in ["earn", "folio-revenue", "checkout-scan-summary"] {
        hub.reply(&format!("/api/agent/{route}"), 204, "");
    }
    hub.reply(
        "/api/agent/earn/walk-in",
        200,
        r#"{"Outcome":"Parked","PointsAwarded":0}"#,
    );
    hub.reply(
        "/api/agent/bookings/pms-cancelled",
        200,
        r#"{"Cancelled":true}"#,
    );
    let c = client(&hub);
    let smile = TdsSmile::new(&db.conn).unwrap();
    let state = Arc::new(WalkInEarnState::default());
    let mut scanner = CheckoutScanner::new(
        CheckoutSettings {
            enabled: true,
            ..CheckoutSettings::default()
        },
        "MobileNum",
        PairSource::new(EarnSettings {
            extra_bed_special_code: "EB".into(),
            ..EarnSettings::default()
        }),
        state.clone(),
        "dl-agent/test",
    );
    let out = scanner.scan_once(&smile, c.as_ref(), &Env).unwrap();
    assert_eq!(out.reported, 2, "the booking and the walk-in group");
    assert_eq!(state.is_healthy(), Some(true));
    assert_eq!(
        number(&hub, "/api/agent/earn", "QualifyingRevenue"),
        "1500000.00"
    );
    let walk_in = body(&hub, "/api/agent/earn/walk-in");
    assert_eq!(text(&walk_in, "FolioRef"), "CONF:9001");
    assert_eq!(
        text(&walk_in, "GuestPhone"),
        "0901234567",
        "TeleNum, trimmed"
    );
    assert_eq!(text(&walk_in, "GuestName"), "WALK IN");
    assert!(text(&walk_in, "CheckedOutAt").ends_with("+07:00"));
    assert_eq!(
        number(&hub, "/api/agent/earn/walk-in", "QualifyingRevenue"),
        "400000.00"
    );
    let cancel = body(&hub, "/api/agent/bookings/pms-cancelled");
    assert_eq!(text(&cancel, "ConfirmationCode"), "MM-LIVE-C");
    assert_eq!(text(&cancel, "BookingId"), BOOKING_C);
    assert_eq!(hub.to("/api/agent/folio-revenue").len(), 1);
    // The next scan excludes group 9001 in SQL: no second walk-in report.
    scanner.scan_once(&smile, c.as_ref(), &Env).unwrap();
    assert_eq!(hub.to("/api/agent/earn/walk-in").len(), 1);
}

fn backfill_today(db: &Db) {
    let hub = FakeHub::start();
    hub.reply(
        "/api/agent/earn/walk-in",
        200,
        r#"{"Outcome":"Awarded","PointsAwarded":4}"#,
    );
    let state = std::env::temp_dir().join(format!("dl-mbe-backfill-{}.json", db.name));
    let today = hotel_now().date();
    let plan = Plan::try_create(
        Some(&dl_agent_membership::stj::format_date(today.add_days(-1))),
        Some(&dl_agent_membership::stj::format_date(today)),
        None,
        &state,
        today,
    )
    .unwrap();
    let smile = TdsSmile::new(&db.conn).unwrap();
    let c = client(&hub);
    let (s, result) = backfill::run(
        &plan,
        &smile,
        c.as_ref(),
        &Env,
        &PairSource::default(),
        "TeleNum",
    );
    result.unwrap();
    assert_eq!((s.processed, s.awarded, s.failed), (1, 1, 0));
    assert!(s.is_clean());
    let _ = std::fs::remove_file(&state);
}

#[test]
fn earn_runs_against_sql_server() {
    let Ok(conn) = std::env::var(ENV) else {
        eprintln!("skipped: {ENV} is not set");
        return;
    };
    let db = create(&conn);
    earn_job_through_smile_writer(&db);
    checkout_scan(&db);
    backfill_today(&db);
}
