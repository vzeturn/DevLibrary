//! The write path end to end over the real `HubClient` against the fake hub: jobs pulled,
//! run through the dispatcher and the write gate, and acked over HTTP. The PMS is an
//! in-memory writer (the SQL itself is covered by the recorder, golden and live tests).

mod support;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dl_agent_membership::hub::gate::TokenGate;
use dl_agent_membership::hub::{HubApi, HubClient};
use dl_agent_membership::identity::Tracker;
use dl_agent_membership::jobs::bed_terms::{ExtraBedTerms, SpecialService, TaxCodes};
use dl_agent_membership::jobs::beds::{MODE_FOLD_LEGACY, MODE_SMILE_SPECIAL};
use dl_agent_membership::jobs::dispatch::{DispatchSettings, Dispatcher};
use dl_agent_membership::jobs::gate::WriteGate;
use dl_agent_membership::jobs::payload::LinkFolioPayload;
use dl_agent_membership::jobs::plan::{
    BookingPlan, PmsWriter, ProfileRequest, WriteError, Written,
};
use dl_agent_membership::jobs::preflight::PreflightInputs;
use dl_agent_membership::jobs::room_diff::ExistingFolio;
use dl_agent_membership::jobs::travel_agent::TravelAgent;
use dl_agent_membership::module::TOKEN_RETRY;
use dl_agent_membership::module::state::Shared;
use dl_agent_membership::sync::{SyncSettings, SyncWorker};
use dl_decimal::Decimal;
use dl_json::{Value, json};
use dl_uuid::Uuid;
use support::{FakeHub, FakeSmile, HOTEL, TOKEN};

const BOOKING: &str = "6f1c2d3e-0000-4000-8000-000000000001";
const ROOM: &str = "6f1c2d3e-0000-4000-8000-000000000101";

/// SMILE in memory: folios keyed by the booking's rooms, every write counted.
#[derive(Default)]
struct MemPms {
    folios: Mutex<Vec<ExistingFolio>>,
    writes: Mutex<Vec<String>>,
    refuse: Mutex<Option<String>>,
    bed_inputs: Mutex<Option<PreflightInputs>>,
    bed_reads: Mutex<Vec<(Option<String>, Option<String>)>>,
}

impl MemPms {
    fn writes(&self) -> Vec<String> {
        self.writes.lock().unwrap().clone()
    }
}

impl PmsWriter for MemPms {
    fn lookup_by_confirmation(&self, _ext: &str) -> Result<Vec<ExistingFolio>, WriteError> {
        Ok(self.folios.lock().unwrap().clone())
    }
    fn lookup_by_folio_nums(&self, _nums: &[Decimal]) -> Result<Vec<ExistingFolio>, WriteError> {
        Ok(Vec::new())
    }
    fn read_extra_bed_inputs(
        &self,
        fingerprint: Option<&str>,
        code: Option<&str>,
    ) -> Result<Option<PreflightInputs>, WriteError> {
        self.bed_reads
            .lock()
            .unwrap()
            .push((fingerprint.map(str::to_owned), code.map(str::to_owned)));
        Ok(self.bed_inputs.lock().unwrap().clone())
    }
    fn write_booking(&self, plan: &BookingPlan) -> Result<Written, WriteError> {
        if let Some(code) = self.refuse.lock().unwrap().clone() {
            return Err(WriteError::Refused {
                code,
                message: "refused by the fake".into(),
            });
        }
        let mut folios = self.folios.lock().unwrap();
        let before = folios.len();
        let mut created = Vec::new();
        for r in &plan.creates {
            // `ReplayLookup`: a live folio of this room under the same key is updated,
            // never inserted again (the SQL is pinned by folio_tests and sql_live).
            if let Some(f) = folios
                .iter()
                .find(|f| f.room_item_id == r.room_item_id && f.cancel_num.is_zero())
            {
                created.push((r.room_item_id, f.folio_num));
                self.writes.lock().unwrap().push("replay update".into());
                continue;
            }
            let num = Decimal::from(1001 + folios.len() as i64);
            created.push((r.room_item_id, num));
            folios.push(ExistingFolio {
                folio_num: num,
                room_item_id: r.room_item_id,
                room_type_booked: r.room_type_code.clone(),
                arrival: r.arrival,
                departure: r.departure,
                rate_amount: r.nights.iter().map(|n| n.net).sum(),
                room_code: String::new(),
                folio_status: 1,
                cancel_num: Decimal::ZERO,
                guest: None,
                beds: None,
            });
        }
        let inserted = folios.len() - before;
        if inserted > 0 || !plan.updates.is_empty() || !plan.cancels.is_empty() {
            self.writes.lock().unwrap().push(format!(
                "write c{inserted} u{} x{}",
                plan.updates.len(),
                plan.cancels.len()
            ));
        }
        Ok(Written {
            created,
            confirm_num: Some(Decimal::from(500)),
        })
    }
    fn cancel_folio(&self, folio_num: Decimal) -> Result<(), WriteError> {
        self.writes
            .lock()
            .unwrap()
            .push(format!("cancel {folio_num}"));
        Ok(())
    }
    fn has_posted_ref(&self, _: &[Decimal], _: &str) -> Result<bool, WriteError> {
        Ok(false)
    }
    fn post_discount(&self, _: Decimal, _: Decimal, _: &str, _: &str) -> Result<(), WriteError> {
        Ok(())
    }
    fn link_folios(&self, _: &LinkFolioPayload) -> Result<(), WriteError> {
        Ok(())
    }
    fn ensure_profile(&self, _: &ProfileRequest) -> Result<Decimal, WriteError> {
        Ok(Decimal::from(42))
    }
    fn find_travel_agent(&self, _: i64) -> Result<Option<TravelAgent>, WriteError> {
        Ok(None)
    }
    fn report_earn(&self, _: Uuid, _: &str) -> Result<(), WriteError> {
        Ok(())
    }
}

struct Rig {
    hub: FakeHub,
    pms: Arc<MemPms>,
    sync: SyncWorker,
    shared: Arc<Shared>,
}

fn rig(sync_config: Option<&str>) -> Rig {
    let hub = FakeHub::start();
    if let Some(c) = sync_config {
        hub.reply("/api/agent/sync-config", 200, c);
    }
    hub.reply("/api/agent/jobs/ack", 204, "");
    let smile = Arc::new(FakeSmile::default());
    let client: Arc<dyn HubApi> = Arc::new(
        HubClient::new(
            &hub.url(),
            TOKEN,
            HOTEL.parse().unwrap(),
            "dl-agent/test",
            Arc::new(TokenGate::new(TOKEN_RETRY)),
        )
        .unwrap(),
    );
    let tracker = Arc::new(Tracker::new(smile.clone()));
    let pms = Arc::new(MemPms::default());
    let dispatcher = Dispatcher::new(
        pms.clone(),
        Some(Arc::new(WriteGate::new(client.clone(), tracker.clone()))),
        DispatchSettings {
            origin_code: "AZ".into(),
            member_discount_code: String::new(),
            extra_bed_mode: MODE_FOLD_LEGACY.into(),
        },
    );
    let shared = Arc::new(Shared::default());
    let sync = SyncWorker::new(
        client,
        smile,
        tracker,
        shared.clone(),
        SyncSettings {
            pull_jobs: true,
            max_jobs_per_pull: 20,
            heartbeat: Duration::from_secs(3600),
            capabilities: Vec::new(),
        },
        "dl-agent/test",
    )
    .with_dispatcher(Arc::new(dispatcher));
    Rig {
        hub,
        pms,
        sync,
        shared,
    }
}

fn create_job(id: &str) -> String {
    job(id, "Create")
}

fn job(id: &str, action: &str) -> String {
    let payload = format!(
        r#"{{"bookingId":"{BOOKING}","confirmationCode":"AZ-1","memberId":"6f1c2d3e-0000-4000-8000-0000000000aa",
            "member":{{"FullName":"Test Member"}},
            "rooms":[{{"Id":"{ROOM}","RoomTypeCode":"DLX","Arrival":"2026-12-01","Departure":"2026-12-02",
            "NumAdult":2,"GuestLastName":"Guest","nights":[{{"Date":"2026-12-01","Net":1000000}}]}}]}}"#
    );
    dl_json::to_string(&json!([{
        "JobId": id, "Action": action, "BookingId": BOOKING,
        "PayloadJson": payload, "Attempts": 2
    }]))
}

fn acks(hub: &FakeHub) -> Vec<Value> {
    hub.to("/api/agent/jobs/ack")
        .iter()
        .map(|s| dl_json::parse(&s.body).unwrap())
        .collect()
}

fn field<'a>(v: &'a Value, k: &str) -> Option<&'a Value> {
    v.get(k).filter(|x| !x.is_null())
}

#[test]
fn a_pulled_job_is_executed_and_acked_with_its_folios() {
    let mut r = rig(None);
    r.hub.reply("/api/agent/jobs/pull", 200, &create_job("j1"));
    r.sync.step(Instant::now()).unwrap();
    assert_eq!(r.pms.writes(), ["write c1 u0 x0"]);
    let acks = acks(&r.hub);
    assert_eq!(acks.len(), 1);
    let a = &acks[0];
    assert_eq!(field(a, "JobId").and_then(Value::as_str), Some("j1"));
    assert_eq!(field(a, "Success").and_then(Value::as_bool), Some(true));
    assert_eq!(field(a, "Attempts").and_then(Value::as_i64), Some(2));
    let folios = field(a, "Folios").and_then(Value::as_array).unwrap();
    assert_eq!(
        folios[0].get("RoomItemId").and_then(Value::as_str),
        Some(ROOM)
    );
    assert!(!r.hub.to("/api/agent/jobs/ack")[0].body.contains(TOKEN));
}

#[test]
fn a_failed_write_is_acked_with_its_code() {
    let mut r = rig(None);
    *r.pms.refuse.lock().unwrap() = Some("POSSIBLE_DUPLICATE".into());
    r.hub.reply("/api/agent/jobs/pull", 200, &create_job("j2"));
    r.sync.step(Instant::now()).unwrap();
    assert!(r.pms.writes().is_empty());
    let a = &acks(&r.hub)[0];
    assert_eq!(field(a, "Success").and_then(Value::as_bool), Some(false));
    assert_eq!(
        field(a, "ErrorCode").and_then(Value::as_str),
        Some("POSSIBLE_DUPLICATE")
    );
    assert!(field(a, "ErrorClass").is_some());
}

#[test]
fn a_closed_gate_writes_nothing_and_acks_the_refusal() {
    let mut r = rig(Some(
        r#"{"RoomInventorySyncEnabled":true,"AvailabilitySyncEnabled":true,
            "BaselineIdentity":{"Server":"OTHERSRV","Database":"OTHER_DB","CreateDate":"2019-05-06T07:08:09"},
            "WriteGate":null,"ExtraBedSpecialCode":null}"#,
    ));
    r.hub.reply("/api/agent/jobs/pull", 200, &create_job("j3"));
    r.sync.step(Instant::now()).unwrap();
    assert!(r.pms.writes().is_empty());
    assert!(r.pms.folios.lock().unwrap().is_empty());
    let a = &acks(&r.hub)[0];
    assert_eq!(field(a, "Success").and_then(Value::as_bool), Some(false));
    assert_eq!(
        field(a, "ErrorCode").and_then(Value::as_str),
        Some("PMS_IDENTITY_MISMATCH")
    );
    assert_eq!(
        field(a, "ErrorClass").and_then(Value::as_str),
        Some("PMS_ENVIRONMENT")
    );
}

#[test]
fn a_replayed_job_does_not_write_twice() {
    let mut r = rig(None);
    r.hub.reply("/api/agent/jobs/pull", 200, &create_job("j4"));
    let t0 = Instant::now();
    r.sync.step(t0).unwrap();
    // The hub did not take the ack (or redelivers it): the same job comes again.
    r.sync.step(t0 + Duration::from_secs(5)).unwrap();
    assert_eq!(r.pms.writes(), ["write c1 u0 x0", "replay update"]);
    assert_eq!(
        r.pms.folios.lock().unwrap().len(),
        1,
        "one folio, never two"
    );
    let acks = acks(&r.hub);
    assert_eq!(acks.len(), 2);
    for a in &acks {
        assert_eq!(field(a, "Success").and_then(Value::as_bool), Some(true));
    }
    let first = field(&acks[0], "Folios").map(dl_json::to_string);
    let second = field(&acks[1], "Folios").map(dl_json::to_string);
    assert_eq!(first, second, "the replay acks the same folio map");
}

#[test]
fn a_replayed_modify_finds_the_folio_and_writes_nothing() {
    let mut r = rig(None);
    r.hub.reply("/api/agent/jobs/pull", 200, &create_job("j5"));
    let t0 = Instant::now();
    r.sync.step(t0).unwrap();
    r.hub
        .reply("/api/agent/jobs/pull", 200, &job("j6", "Modify"));
    r.sync.step(t0 + Duration::from_secs(5)).unwrap();
    r.sync.step(t0 + Duration::from_secs(10)).unwrap();
    assert_eq!(r.pms.writes(), ["write c1 u0 x0"]);
    let acks = acks(&r.hub);
    assert_eq!(acks.len(), 3);
    assert!(
        acks.iter()
            .all(|a| field(a, "Success").and_then(Value::as_bool) == Some(true))
    );
}

/// A PMS-priced special (ADR-0106/0109): the hotel's pick, priced from the catalogue.
fn pms_priced_inputs() -> PreflightInputs {
    let tax = TaxCodes {
        tax_code: Some("V8".into()),
        service_charge_code: Some("S5".into()),
        s_tax_code: None,
    };
    PreflightInputs {
        configured_mode: MODE_SMILE_SPECIAL.into(),
        verified_fingerprint: None,
        terms: Some(ExtraBedTerms {
            smile_hotel_id: "VIC".into(),
            surcharge_amount: Some(Decimal::ZERO),
            trn_code: None,
            trn_sub: None,
            stock_total: Some(10),
            special: Some(SpecialService {
                code: "XB".into(),
                trn_code: Some("110".into()),
                trn_sub: Some("9".into()),
                open_price: Some(0),
                include_in_room_charge: Some(0),
                amount_charge: Some(Decimal::from(500_000)),
            }),
            special_tax: Some(tax.clone()),
            room_tax: Some(tax),
            special_by_date: Some(true),
            smile_hotel_date: None,
            structural_fingerprint: "fp".into(),
            content_hash: "c".into(),
            warnings: Vec::new(),
            rate_code_plus_plus: Some(false),
            special_sc_percent: None,
            special_vat_percent: None,
            pms_priced: true,
        }),
        configured_smile_hotel_id: None,
    }
}

fn bed_job(id: &str) -> String {
    let payload = format!(
        r#"{{"bookingId":"{BOOKING}","confirmationCode":"AZ-1","extraBedSpecialCode":"XB",
            "rooms":[{{"Id":"{ROOM}","RoomTypeCode":"DLX","Arrival":"2026-12-01","Departure":"2026-12-03",
            "NumAdult":3,"GuestLastName":"Guest","PricingModel":"OCCUPANCY_V1","ExtraBeds":1,
            "ExtraBedNightly":400000,"nights":[
            {{"Date":"2026-12-01","Net":1700000,"NetExclBed":1300000}},
            {{"Date":"2026-12-02","Net":1700000,"NetExclBed":1300000}}]}}]}}"#
    );
    dl_json::to_string(&json!([{
        "JobId": id, "Action": "Create", "BookingId": BOOKING,
        "PayloadJson": payload, "Attempts": 1
    }]))
}

#[test]
fn a_smile_special_create_acks_the_special_bed_and_the_hub_code_reaches_the_writer() {
    let mut r = rig(Some(
        r#"{"RoomInventorySyncEnabled":true,"AvailabilitySyncEnabled":true,
            "BaselineIdentity":null,"WriteGate":null,"ExtraBedSpecialCode":" XB "}"#,
    ));
    *r.pms.bed_inputs.lock().unwrap() = Some(pms_priced_inputs());
    r.hub.reply("/api/agent/jobs/pull", 200, &bed_job("jb1"));
    r.sync.step(Instant::now()).unwrap();
    assert_eq!(
        r.pms.bed_reads.lock().unwrap().as_slice(),
        [(None, Some("XB".to_owned()))]
    );
    let a = &acks(&r.hub)[0];
    assert_eq!(field(a, "Success").and_then(Value::as_bool), Some(true));
    let f = &field(a, "Folios").and_then(Value::as_array).unwrap()[0];
    assert_eq!(f.get("ExtraBedWritten").and_then(Value::as_i64), Some(1));
    assert_eq!(
        f.get("ExtraBedSpecialQtyWritten").and_then(Value::as_i64),
        Some(1)
    );
    // `SetHubExtraBedSpecialCode`: the sync-config pick, trimmed, for the next write.
    assert_eq!(r.shared.hub_special_code.get().as_deref(), Some("XB"));
}
