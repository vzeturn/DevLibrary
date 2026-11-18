//! Port of Go `reconcile_test.go` and `reconcile_splititems_test.go`: a hand-written fake
//! repository and the reconciler cases.

mod cancel;
mod lookup;
mod modify;
mod policy;
mod save;
mod split;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use dl_time::{Date, DateTime, OffsetDateTime};

use super::{Reconciler, parse_day};
use crate::error::{Error, Result};
use crate::gofmt::scan_int;
use crate::jobmodel::{ACTION_SAVE, Job, OpPolicy, RateNight, Room};
use crate::pms::{
    AdditionalName, ClientContact, ClientSts, Folio, FolioRate, PmsRepo, SmileClient, Val,
};

/// `insertedFolio`.
#[derive(Debug, Clone)]
pub(super) struct Inserted {
    pub f: Folio,
    pub rates: Vec<FolioRate>,
    pub an: AdditionalName,
}

/// `fakeRepo`'s fields. Maps keyed by a folio number use `n as i64`.
#[derive(Default)]
pub(super) struct State {
    /// What `GetFolios` returns when `folios_by_ext` is `None`.
    pub folios: Vec<Folio>,
    pub folios_by_ext: Option<HashMap<String, Vec<Folio>>>,
    pub folios_by_booker: Option<HashMap<String, Vec<Folio>>>,
    pub global_params: HashMap<String, i64>,
    /// By `ClientName`, in insertion order.
    pub clients: Vec<SmileClient>,
    pub contacts: HashMap<String, ClientContact>,
    pub client_sts: HashMap<i64, ClientSts>,
    pub next_folio_num: f64,
    pub next_confirm_num: f64,
    pub folio_rates: HashMap<i64, Vec<FolioRate>>,
    pub cancel_err: Option<String>,
    pub insert_err: Option<String>,
    pub inserted: Vec<Inserted>,
    pub cancelled: Vec<Vec<Folio>>,
    pub confirm_num_calls: usize,
    pub updates: Vec<(f64, BTreeMap<&'static str, Val>)>,
    pub replaced_rates: HashMap<i64, Vec<FolioRate>>,
    pub updated_names: Vec<(f64, String, String)>,
    /// Replaces `folios` after an insert.
    pub folios_after_insert: Option<Vec<Folio>>,
    /// `HasUserCancelledFolio` is true for these keys (Booker or BookingID).
    pub cancelled_booking: HashMap<String, bool>,
}

/// `fakeRepo`.
pub(super) struct FakeRepo(Mutex<State>);

impl FakeRepo {
    pub fn new() -> Arc<FakeRepo> {
        Arc::new(FakeRepo(Mutex::new(State {
            next_folio_num: 100000.0,
            next_confirm_num: 500000.0,
            ..State::default()
        })))
    }

    pub fn st(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl PmsRepo for FakeRepo {
    fn get_folios(&self, ext: &str) -> Result<Vec<Folio>> {
        let s = self.st();
        Ok(match &s.folios_by_ext {
            Some(m) => m.get(ext).cloned().unwrap_or_default(),
            None => s.folios.clone(),
        })
    }
    fn get_folios_by_booker(&self, booker: &str) -> Result<Vec<Folio>> {
        let s = self.st();
        Ok(s.folios_by_booker
            .as_ref()
            .and_then(|m| m.get(booker).cloned())
            .unwrap_or_default())
    }
    fn has_user_cancelled_folio(&self, booker: &str, booking_id: &str) -> Result<bool> {
        let s = self.st();
        let hit = |k: &str| s.cancelled_booking.get(k).copied().unwrap_or(false);
        Ok(hit(booker) || hit(booking_id))
    }
    fn get_new_confirm_num(&self) -> Result<f64> {
        let mut s = self.st();
        s.confirm_num_calls += 1;
        s.next_confirm_num += 1.0;
        Ok(s.next_confirm_num)
    }
    fn get_next_folio_num(&self) -> Result<f64> {
        let mut s = self.st();
        s.next_folio_num += 1.0;
        Ok(s.next_folio_num)
    }
    fn insert_folio_with_rates(
        &self,
        f: &Folio,
        rates: &[FolioRate],
        an: &AdditionalName,
    ) -> Result<()> {
        let mut s = self.st();
        if let Some(e) = &s.insert_err {
            return Err(Error::other(e.clone()));
        }
        s.inserted.push(Inserted {
            f: f.clone(),
            rates: rates.to_vec(),
            an: an.clone(),
        });
        if let Some(after) = s.folios_after_insert.clone() {
            s.folios = after;
        }
        Ok(())
    }
    fn cancel_folios(&self, folios: &[Folio]) -> Result<()> {
        let mut s = self.st();
        if let Some(e) = &s.cancel_err {
            return Err(Error::other(e.clone()));
        }
        s.cancelled.push(folios.to_vec());
        Ok(())
    }
    fn update_folio_fields(
        &self,
        folio_num: f64,
        set: &BTreeMap<&'static str, Val>,
    ) -> Result<()> {
        if !set.is_empty() {
            self.st().updates.push((folio_num, set.clone()));
        }
        Ok(())
    }
    fn get_folio_rates(&self, folio_num: f64) -> Result<Vec<FolioRate>> {
        Ok(self
            .st()
            .folio_rates
            .get(&(folio_num as i64))
            .cloned()
            .unwrap_or_default())
    }
    fn replace_folio_rates(&self, folio_num: f64, rates: &[FolioRate]) -> Result<()> {
        self.st()
            .replaced_rates
            .insert(folio_num as i64, rates.to_vec());
        Ok(())
    }
    fn update_additional_name(&self, folio_num: f64, last_name: &str, email: &str) -> Result<()> {
        self.st()
            .updated_names
            .push((folio_num, last_name.into(), email.into()));
        Ok(())
    }
    fn get_global_param_int(&self, name: &str) -> Result<i64> {
        Ok(self.st().global_params.get(name).copied().unwrap_or(0))
    }
    fn seed_global_param_if_absent(&self, name: &str, value: &str) -> Result<()> {
        self.st()
            .global_params
            .entry(name.into())
            .or_insert(scan_int(value).unwrap_or(0));
        Ok(())
    }
    fn get_client_by_name(&self, name: &str) -> Result<Option<SmileClient>> {
        Ok(self
            .st()
            .clients
            .iter()
            .find(|c| c.client_name == name)
            .cloned())
    }
    fn get_client_by_folio_num(&self, num: f64) -> Result<Option<SmileClient>> {
        Ok(self
            .st()
            .clients
            .iter()
            .find(|c| c.client_folio_num == num)
            .cloned())
    }
    fn get_client_contact_by_name(&self, name: &str) -> Result<Option<ClientContact>> {
        Ok(self.st().contacts.get(name).cloned())
    }
    fn get_client_sts(&self, code: f64) -> Result<Option<ClientSts>> {
        Ok(self.st().client_sts.get(&(code as i64)).cloned())
    }
    fn get_hotel_date(&self) -> Result<DateTime> {
        Err(Error::other("fakeRepo: HotelDate không cấu hình"))
    }
}

/// A date at midnight.
pub(super) fn day(y: i32, m: u8, d: u8) -> DateTime {
    Date::new(y, m, d).expect("valid date").midnight()
}

pub(super) fn rn(date: &str, rate: f64) -> RateNight {
    RateNight {
        date: date.into(),
        rate,
        ..RateNight::default()
    }
}

/// `saveJob`.
pub(super) fn save_job() -> Job {
    Job {
        job_id: "0000000000001000-123456".into(),
        hotel_id: "h1".into(),
        action: ACTION_SAVE.into(),
        booking_id: "123456".into(),
        booker: "EXT-1".into(),
        source_name: "Booking.com".into(),
        hl_status: "Confirmed".into(),
        check_in: "2026-06-12".into(),
        check_out: "2026-06-14".into(),
        guest_name: "NGUYEN VAN DUC".into(),
        guest_phone: "0901".into(),
        guest_email: "a@b.c".into(),
        book_status: 1,
        rooms: Some(vec![Room {
            room_type_code: "DLX01".into(),
            quantity: 1,
            adults: 3,
            children: 1,
            include_bf: true,
            breakfast_code: "ABU".into(),
            rates: Some(vec![rn("2026-06-12", 1500000.0), rn("2026-06-13", 1600000.0)]),
            ..Room::default()
        }]),
        ..Job::default()
    }
}

/// `jobWithItem` (same as `saveJob`).
pub(super) fn job_with_item() -> Job {
    save_job()
}

/// The first room of a job, mutably.
pub(super) fn room0(job: &mut Job) -> &mut Room {
    &mut job.rooms.as_mut().expect("rooms")[0]
}

/// The first room's rates, mutably.
pub(super) fn rates0(job: &mut Job) -> &mut Vec<RateNight> {
    room0(job).rates.as_mut().expect("rates")
}

/// `ratesAsJob`.
pub(super) fn rates_as_job() -> Vec<RateNight> {
    vec![rn("2026-06-12", 1500000.0), rn("2026-06-13", 1600000.0)]
}

/// The fixed clock: 2026-06-10 08:00 UTC.
pub(super) fn fixed_now() -> OffsetDateTime {
    let t = dl_time::Time::from_hms(8, 0, 0).expect("valid");
    Date::new(2026, 6, 10).expect("valid").at(t).assume_utc()
}

/// `newReconciler`.
pub(super) fn new_reconciler(repo: &Arc<FakeRepo>) -> Reconciler {
    Reconciler::with_clock(repo.clone(), Arc::new(fixed_now))
}

/// `existingFolio`: matches `saveJob()` (item DLX01, one room), room 101 assigned.
pub(super) fn existing_folio() -> Folio {
    Folio {
        id: 5,
        folio_num: 100000.0,
        confirm_num: 499000.0,
        book_status: 1,
        folio_status: 1,
        user_info1: "rt:DLX01#0".into(),
        room_type_code: "DLX01".into(),
        room_code: "101".into(),
        arrival_date: parse_day("2026-06-12").expect("day"),
        departure_date: parse_day("2026-06-14").expect("day"),
        num_adult: 3,
        num_child: 1,
        rate_amount: 1500000.0,
        include_bf: true,
        breakfast_code: "ABU".into(),
        num_guest_bf: 3,
        num_child_bf: 1,
        last_name: "NGUYEN VAN DUC".into(),
        tel_num_m: "0901".into(),
        mail_m: "a@b.c".into(),
        ..Folio::default()
    }
}

/// `ratesFor`.
pub(super) fn rates_for(folio_num: f64, check_in: DateTime) -> Vec<FolioRate> {
    let d1 = check_in.date().add_days(1).midnight();
    vec![
        FolioRate {
            folio_num,
            h_date: check_in,
            to_date: check_in,
            rate_amount: 1500000.0,
            rate_override: 1,
            ..FolioRate::default()
        },
        FolioRate {
            folio_num,
            h_date: d1,
            to_date: d1,
            rate_amount: 1600000.0,
            rate_override: 1,
            ..FolioRate::default()
        },
    ]
}

/// A repo holding `folios` with `rates_for` each one.
pub(super) fn repo_with(folios: Vec<Folio>) -> Arc<FakeRepo> {
    let repo = FakeRepo::new();
    {
        let mut s = repo.st();
        for f in &folios {
            s.folio_rates
                .insert(f.folio_num as i64, rates_for(f.folio_num, f.arrival_date));
        }
        s.folios = folios;
    }
    repo
}

/// `(updates, inserted, cancelled, replacedRates)` counts.
pub(super) fn counts(repo: &FakeRepo) -> (usize, usize, usize, usize) {
    let s = repo.st();
    (
        s.updates.len(),
        s.inserted.len(),
        s.cancelled.len(),
        s.replaced_rates.len(),
    )
}
