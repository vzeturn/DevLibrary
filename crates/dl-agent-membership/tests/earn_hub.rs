//! The `EarnPoints` job end to end over the real `HubClient` against the fake hub: the job
//! is pulled, its SMILE reads run (on a canned session), the `AgentEarnReport` is posted to
//! `api/agent/earn` byte for byte as .NET writes it, and the job is acked `Success`.

mod support;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dl_agent_membership::earn::EarnEnv;
use dl_agent_membership::earn::job;
use dl_agent_membership::earn::pairs::{EarnSettings, PairSource};
use dl_agent_membership::earn::sql::EarnDb;
use dl_agent_membership::hub::earn::EarnHub;
use dl_agent_membership::hub::gate::TokenGate;
use dl_agent_membership::hub::{HubApi, HubClient};
use dl_agent_membership::identity::Tracker;
use dl_agent_membership::jobs::beds::MODE_FOLD_LEGACY;
use dl_agent_membership::jobs::dispatch::{DispatchSettings, Dispatcher};
use dl_agent_membership::jobs::payload::LinkFolioPayload;
use dl_agent_membership::jobs::plan::{
    BookingPlan, PmsWriter, ProfileRequest, WriteError, Written,
};
use dl_agent_membership::jobs::room_diff::ExistingFolio;
use dl_agent_membership::jobs::travel_agent::TravelAgent;
use dl_agent_membership::module::TOKEN_RETRY;
use dl_agent_membership::module::state::Shared;
use dl_agent_membership::smile::{CodeRow, EarnCatalog, SmileError};
use dl_agent_membership::sync::{SyncSettings, SyncWorker};
use dl_agent_membership::writer::session::{Called, Rec, Session};
use dl_agent_membership::writer::smile_writer::EarnPath;
use dl_decimal::Decimal;
use dl_json::{Value, json};
use dl_tds::{Param, Value as Sql};
use dl_time::{Date, DateTime, Time};
use dl_uuid::Uuid;
use support::{FakeHub, FakeSmile, HOTEL, TOKEN};

const BOOKING: &str = "6f1c2d3e-0000-4000-8000-000000000001";

/// SMILE for one checked-out booking: room charge `110/1` (`AutoPost = 1`), two lines.
#[derive(Default)]
struct Canned {
    texts: Vec<String>,
}

impl Session for Canned {
    fn rows(&mut self, sql: &str, _: &[Param]) -> Result<Vec<Rec>, SmileError> {
        self.texts.push(sql.to_owned());
        let s = |v: &str| Sql::String(v.into());
        Ok(if sql.contains("AS Qualifies") {
            vec![
                Rec::new(vec![
                    ("TransactionCode", s("110")),
                    ("TransactionSubCode", s("1")),
                    ("Description", s("Room")),
                    ("Amount", Sql::Decimal("1500000.00".parse().unwrap())),
                    ("Qualifies", Sql::I32(1)),
                ]),
                Rec::new(vec![
                    ("TransactionCode", s("MB")),
                    ("TransactionSubCode", s("0")),
                    ("Description", Sql::Null),
                    ("Amount", Sql::Decimal("200000.00".parse().unwrap())),
                    ("Qualifies", Sql::I32(0)),
                ]),
            ]
        } else if sql.contains("AS K,") {
            vec![Rec::new(vec![("K", s("O")), ("V", s("Membership"))])]
        } else if sql.contains("MAX(CheckOutTime)") {
            let at = DateTime::new(
                Date::new(2026, 11, 18).unwrap(),
                Time::new(11, 5, 6, 0).unwrap(),
            );
            vec![Rec::new(vec![("", Sql::DateTime(at))])]
        } else {
            Vec::new()
        })
    }
    fn execute(&mut self, _: &str, _: &[Param]) -> Result<u64, SmileError> {
        panic!("earn never writes to SMILE")
    }
    fn call(&mut self, _: &str, _: &[Param]) -> Result<Called, SmileError> {
        panic!("earn never calls a procedure")
    }
    fn batch(&mut self, _: &str) -> Result<(), SmileError> {
        panic!("earn never runs a batch")
    }
}

impl EarnDb for Canned {
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

struct Env;

impl EarnEnv for Env {
    fn fingerprint(&self) -> Option<String> {
        Some("fp-1".into())
    }
    fn hub_extra_bed_code(&self) -> Option<String> {
        None
    }
}

/// A writer that only knows earn: the job's reads on [`Canned`], the report through the
/// real `HubClient`.
struct EarnOnly {
    path: EarnPath,
    reads: Mutex<Vec<String>>,
}

impl PmsWriter for EarnOnly {
    fn lookup_by_confirmation(&self, _: &str) -> Result<Vec<ExistingFolio>, WriteError> {
        Err(WriteError::NotPorted("test"))
    }
    fn lookup_by_folio_nums(&self, _: &[Decimal]) -> Result<Vec<ExistingFolio>, WriteError> {
        Err(WriteError::NotPorted("test"))
    }
    fn write_booking(&self, _: &BookingPlan) -> Result<Written, WriteError> {
        Err(WriteError::NotPorted("test"))
    }
    fn cancel_folio(&self, _: Decimal) -> Result<(), WriteError> {
        Err(WriteError::NotPorted("test"))
    }
    fn has_posted_ref(&self, _: &[Decimal], _: &str) -> Result<bool, WriteError> {
        Err(WriteError::NotPorted("test"))
    }
    fn post_discount(&self, _: Decimal, _: Decimal, _: &str, _: &str) -> Result<(), WriteError> {
        Err(WriteError::NotPorted("test"))
    }
    fn link_folios(&self, _: &LinkFolioPayload) -> Result<(), WriteError> {
        Err(WriteError::NotPorted("test"))
    }
    fn ensure_profile(&self, _: &ProfileRequest) -> Result<Decimal, WriteError> {
        Err(WriteError::NotPorted("test"))
    }
    fn find_travel_agent(&self, _: i64) -> Result<Option<TravelAgent>, WriteError> {
        Err(WriteError::NotPorted("test"))
    }
    fn report_earn(&self, booking_id: Uuid, ext: &str) -> Result<(), WriteError> {
        let mut db = Canned::default();
        let report = job::read(&mut db, &self.path.pairs, None, booking_id, ext)?;
        self.reads.lock().unwrap().extend(db.texts);
        self.path.report(report)
    }
}

fn rig(earn_status: u16) -> (FakeHub, SyncWorker, Arc<EarnOnly>) {
    let hub = FakeHub::start();
    hub.reply("/api/agent/jobs/ack", 204, "");
    hub.reply("/api/agent/earn", earn_status, "");
    let client = Arc::new(
        HubClient::new(
            &hub.url(),
            TOKEN,
            HOTEL.parse().unwrap(),
            "dl-agent/test",
            Arc::new(TokenGate::new(TOKEN_RETRY)),
        )
        .unwrap(),
    );
    let smile = Arc::new(FakeSmile::default());
    let tracker = Arc::new(Tracker::new(smile.clone()));
    let pms = Arc::new(EarnOnly {
        path: EarnPath {
            pairs: PairSource::new(EarnSettings {
                charge_codes: Vec::new(),
                member_discount_code: "MD".into(),
                extra_bed_special_code: "EB".into(),
            }),
            hub: client.clone() as Arc<dyn EarnHub>,
            env: Arc::new(Env),
        },
        reads: Mutex::new(Vec::new()),
    });
    let api: Arc<dyn HubApi> = client;
    let dispatcher = Dispatcher::new(
        pms.clone(),
        None,
        DispatchSettings {
            origin_code: "AZ".into(),
            member_discount_code: "MD".into(),
            extra_bed_mode: MODE_FOLD_LEGACY.into(),
        },
    );
    let sync = SyncWorker::new(
        api,
        smile,
        tracker,
        Arc::new(Shared::default()),
        SyncSettings {
            pull_jobs: true,
            max_jobs_per_pull: 20,
            heartbeat: Duration::from_secs(3600),
            capabilities: Vec::new(),
        },
        "dl-agent/test",
    )
    .with_dispatcher(Arc::new(dispatcher));
    (hub, sync, pms)
}

fn earn_job(id: &str) -> String {
    let payload = format!(r#"{{"bookingId":"{BOOKING}","confirmationCode":"MM-TEST-1"}}"#);
    dl_json::to_string(&json!([{
        "JobId": id, "Action": "EarnPoints", "BookingId": BOOKING,
        "PayloadJson": payload, "Attempts": 1
    }]))
}

fn ack(hub: &FakeHub) -> Value {
    let acks = hub.to("/api/agent/jobs/ack");
    assert_eq!(acks.len(), 1);
    dl_json::parse(&acks[0].body).unwrap()
}

#[test]
fn an_earn_job_posts_the_dotnet_report_and_acks_success() {
    let (hub, mut sync, pms) = rig(204);
    hub.reply("/api/agent/jobs/pull", 200, &earn_job("e1"));
    sync.step(Instant::now()).unwrap();
    let posts = hub.to("/api/agent/earn");
    assert_eq!(posts.len(), 1);
    assert_eq!(
        posts[0].body,
        format!(
            "{{\"BookingId\":\"{BOOKING}\",\"FolioRef\":\"MM-TEST-1\",\"QualifyingRevenue\":1500000.00,\
             \"CheckedOutAt\":\"2026-11-18T04:05:06+00:00\",\"FolioLines\":[\
             {{\"TransactionCode\":\"110\",\"TransactionSubCode\":\"1\",\"Description\":\"Room\",\"Amount\":1500000.00}},\
             {{\"TransactionCode\":\"MB\",\"TransactionSubCode\":\"0\",\"Description\":null,\"Amount\":200000.00}}],\
             \"Classification\":{{\"OriginCodes\":[\"Membership\"],\"SegmentCodes\":[],\"RateCodes\":[]}},\
             \"DbIdentityFingerprint\":\"fp-1\",\"CheckedOutAtSource\":\"PMS_CHECKOUT_TIME\"}}"
        )
    );
    assert_eq!(
        posts[0].header("Content-Type"),
        Some("application/json; charset=utf-8")
    );
    let a = ack(&hub);
    assert_eq!(a.get("JobId").and_then(Value::as_str), Some("e1"));
    assert_eq!(a.get("Success").and_then(Value::as_bool), Some(true));
    let reads = pms.reads.lock().unwrap();
    assert_eq!(
        reads.len(),
        4,
        "RmChgTrn, breakdown, classification, checkout time"
    );
}

#[test]
fn a_held_report_still_acks_success() {
    let (hub, mut sync, _) = rig(202);
    hub.reply("/api/agent/jobs/pull", 200, &earn_job("e2"));
    sync.step(Instant::now()).unwrap();
    assert_eq!(hub.to("/api/agent/earn").len(), 1);
    assert_eq!(
        ack(&hub).get("Success").and_then(Value::as_bool),
        Some(true)
    );
}

#[test]
fn a_refused_report_fails_the_job_as_network() {
    let (hub, mut sync, _) = rig(500);
    hub.reply("/api/agent/jobs/pull", 200, &earn_job("e3"));
    sync.step(Instant::now()).unwrap();
    let a = ack(&hub);
    assert_eq!(a.get("Success").and_then(Value::as_bool), Some(false));
    assert_eq!(a.get("ErrorCode").and_then(Value::as_str), Some("NETWORK"));
    assert_eq!(
        a.get("ErrorClass").and_then(Value::as_str),
        Some("TRANSIENT")
    );
}
