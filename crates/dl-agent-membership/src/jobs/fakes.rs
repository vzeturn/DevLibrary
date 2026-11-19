//! Hand-written fakes for the job tests: a hub that records acks and serves a sync-config,
//! a SMILE writer that records every call, and an identity probe.

use std::sync::{Arc, Mutex};

use dl_decimal::Decimal;
use dl_time::{Date, DateTime, Time};
use dl_uuid::Uuid;

use super::payload::LinkFolioPayload;
use super::plan::{BookingPlan, PmsWriter, ProfileRequest, WriteError, Written};
use super::result::Ack;
use super::room_diff::ExistingFolio;
use super::travel_agent::TravelAgent;
use crate::hub::{HubApi, HubError};
use crate::identity::{ForeignState, IdentityProbe, Snapshot, Tracker};
use crate::smile::SmileError;
use crate::wire::parse::{Job, ObservationResult, SyncConfig};
use crate::wire::{
    AvailabilityRequest, DbIdentity, FolioObservation, Heartbeat, RoomInventoryRequest,
    RoomTypeCatalogRequest,
};

/// A hub serving `config` and recording acks; `fail_acks` makes every ack fail.
#[derive(Default)]
pub struct Hub {
    pub config: Mutex<SyncConfig>,
    pub jobs: Mutex<Vec<Job>>,
    pub acks: Mutex<Vec<Ack>>,
    pub fail_acks: Mutex<bool>,
}

impl HubApi for Hub {
    fn heartbeat(&self, _: &Heartbeat) -> Result<bool, HubError> {
        Ok(false)
    }
    fn sync_config(&self) -> Result<SyncConfig, HubError> {
        Ok(self.config.lock().unwrap().clone())
    }
    fn pull_jobs(&self, max: u32) -> Result<Vec<Job>, HubError> {
        let mut jobs = self.jobs.lock().unwrap();
        let n = jobs.len().min(max as usize);
        Ok(jobs.drain(..n).collect())
    }
    fn post_availability(&self, _: &AvailabilityRequest) -> Result<(), HubError> {
        Ok(())
    }
    fn post_room_inventory(&self, _: &RoomInventoryRequest) -> Result<(), HubError> {
        Ok(())
    }
    fn post_room_types(&self, _: &RoomTypeCatalogRequest) -> Result<(), HubError> {
        Ok(())
    }
    fn post_folio_observations(
        &self,
        _: &[FolioObservation],
        _: &str,
        _: Option<&str>,
    ) -> Result<ObservationResult, HubError> {
        Ok(ObservationResult::default())
    }
    fn ack_job(&self, ack: &Ack) -> Result<(), HubError> {
        if *self.fail_acks.lock().unwrap() {
            return Err(HubError::Timeout("ack"));
        }
        self.acks.lock().unwrap().push(ack.clone());
        Ok(())
    }
}

/// The SMILE database every fake reads.
pub fn identity(database: &str) -> DbIdentity {
    DbIdentity {
        server: "SMILESRV".into(),
        database: database.into(),
        create_date: DateTime::new(Date::new(2020, 1, 1).unwrap(), Time::MIDNIGHT),
        database_guid: None,
        family_guid: None,
        service_broker_guid: None,
    }
}

/// An identity probe always reading `database`.
pub struct Probe(pub &'static str);

impl IdentityProbe for Probe {
    fn read_identity(&self) -> Result<Snapshot, SmileError> {
        let now = DateTime::new(Date::new(2026, 11, 19).unwrap(), Time::MIDNIGHT);
        Ok(Snapshot {
            identity: identity(self.0),
            pms_now: now,
            last_foreign_change: Some(now),
            foreign_state: ForeignState::Known,
        })
    }
}

/// A tracker that already read `database`.
pub fn tracker(database: &'static str) -> Arc<Tracker> {
    let t = Arc::new(Tracker::new(Arc::new(Probe(database))));
    t.refresh_if_due(true);
    t
}

/// A recording SMILE writer. Folios created are numbered from 1001; a replayed
/// confirmation code finds them again, as the real duplicate-safe lookup does.
#[derive(Default)]
pub struct Pms {
    pub calls: Mutex<Vec<String>>,
    pub folios: Mutex<Vec<ExistingFolio>>,
    pub posted: Mutex<Vec<(Decimal, Decimal, String, String)>>,
    pub fail_profile: Mutex<Option<i32>>,
    pub refuse: Mutex<Option<String>>,
    pub not_ported: Mutex<bool>,
}

impl Pms {
    fn call(&self, what: impl Into<String>) {
        self.calls.lock().unwrap().push(what.into());
    }

    /// Every call so far.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    /// Calls that write.
    pub fn writes(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|c| !c.starts_with("lookup") && !c.starts_with("has_posted"))
            .collect()
    }
}

impl PmsWriter for Pms {
    fn lookup_by_confirmation(&self, ext: &str) -> Result<Vec<ExistingFolio>, WriteError> {
        self.call(format!("lookup {ext}"));
        Ok(self.folios.lock().unwrap().clone())
    }
    fn lookup_by_folio_nums(&self, nums: &[Decimal]) -> Result<Vec<ExistingFolio>, WriteError> {
        self.call(format!("lookup_nums {}", nums.len()));
        Ok(Vec::new())
    }
    fn write_booking(&self, plan: &BookingPlan) -> Result<Written, WriteError> {
        if *self.not_ported.lock().unwrap() {
            return Err(WriteError::NotPorted("SMILE_SPECIAL extra beds"));
        }
        if let Some(code) = self.refuse.lock().unwrap().clone() {
            return Err(WriteError::Refused {
                code,
                message: "refused".into(),
            });
        }
        self.call(format!(
            "write c{} u{} x{} p{}",
            plan.creates.len(),
            plan.updates.len(),
            plan.cancels.len(),
            plan.profile_number
        ));
        let mut folios = self.folios.lock().unwrap();
        let mut created = Vec::new();
        for r in &plan.creates {
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
        Ok(Written {
            created,
            confirm_num: Some(Decimal::from(500)),
        })
    }
    fn cancel_folio(&self, folio_num: Decimal) -> Result<(), WriteError> {
        self.call(format!("cancel {folio_num}"));
        for f in self.folios.lock().unwrap().iter_mut() {
            if f.folio_num == folio_num {
                f.cancel_num = Decimal::from(1);
            }
        }
        Ok(())
    }
    fn has_posted_ref(&self, _: &[Decimal], reference: &str) -> Result<bool, WriteError> {
        self.call(format!("has_posted {reference}"));
        Ok(self.posted.lock().unwrap().iter().any(|p| p.3 == reference))
    }
    fn post_discount(
        &self,
        folio_num: Decimal,
        amount: Decimal,
        code: &str,
        reference: &str,
    ) -> Result<(), WriteError> {
        self.call(format!("post {folio_num} {amount} {code}"));
        self.posted.lock().unwrap().push((
            folio_num,
            amount,
            code.to_owned(),
            reference.to_owned(),
        ));
        Ok(())
    }
    fn link_folios(&self, link: &LinkFolioPayload) -> Result<(), WriteError> {
        self.call(format!("link {}", link.folios.len()));
        Ok(())
    }
    fn ensure_profile(&self, req: &ProfileRequest) -> Result<Decimal, WriteError> {
        if let Some(n) = *self.fail_profile.lock().unwrap() {
            return Err(WriteError::Smile(SmileError::Server(n)));
        }
        self.call(format!("profile {}", req.last_name));
        Ok(Decimal::from(77))
    }
    fn find_travel_agent(&self, code: i64) -> Result<Option<TravelAgent>, WriteError> {
        self.call(format!("travel_agent {code}"));
        Ok(None)
    }
    fn report_earn(&self, booking_id: Uuid, _: &str) -> Result<(), WriteError> {
        self.call(format!("earn {booking_id}"));
        Ok(())
    }
}
