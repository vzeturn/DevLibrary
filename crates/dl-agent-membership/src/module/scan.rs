//! The scan thread: the availability probe, the physical rooms and the folio observations,
//! each on its own interval, on a SMILE connection of their own. Nothing is scanned before
//! the first sync-config, nor while SMILE shows another database than the pinned one.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dl_agent::report::Severity;
use dl_winsvc::StopSignal;

use crate::availability::{self, AvailabilityError, AvailabilityScanner, TickOutcome};
use crate::folio::scanner::{FolioError, FolioScanner};
use crate::hub::{HubApi, HubError};
use crate::identity::Tracker;
use crate::log::{self, V};
use crate::module::state::{Shared, codes};
use crate::rooms::{RoomError, RoomOutcome, RoomScanner};
use crate::settings::Settings;
use crate::smile::{AvailabilitySource, FolioSource, SmileError, hotel_now};
use crate::wire::parse::SyncConfig;

/// `ErrorBackoff` of the availability scanner.
pub const ERROR_BACKOFF: Duration = Duration::from_secs(30);
/// The wait while there is nothing to do (no sync-config yet, or scans paused).
pub const IDLE: Duration = Duration::from_secs(1);

/// Every SMILE read the scanners need.
pub trait Sources: AvailabilitySource + FolioSource {}

impl<T: AvailabilitySource + FolioSource> Sources for T {}

/// What the scan thread works with.
#[derive(Clone)]
pub struct ScanDeps {
    /// SMILE.
    pub src: Arc<dyn Sources>,
    /// The hub.
    pub hub: Arc<dyn HubApi>,
    /// The identity tracker (shared with the sync thread).
    pub tracker: Arc<Tracker>,
    /// The shared state.
    pub shared: Arc<Shared>,
}

/// The scanners and when each is due.
pub struct ScanLoop {
    deps: ScanDeps,
    availability: AvailabilityScanner,
    rooms: RoomScanner,
    folio: Option<FolioScanner>,
    probe_every: Duration,
    rooms_every: Duration,
    folio_every: Duration,
    next_availability: Option<Instant>,
    next_rooms: Option<Instant>,
    next_folio: Option<Instant>,
    paused_logged: bool,
}

impl std::fmt::Debug for ScanLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScanLoop")
            .field("folio", &self.folio.is_some())
            .finish_non_exhaustive()
    }
}

impl ScanLoop {
    /// The scanners for `hotel_id`, with the folio cursor in `settings.program_data_dir`.
    pub fn new(
        deps: ScanDeps,
        settings: &Settings,
        hotel_id: &str,
        agent_version: &str,
    ) -> ScanLoop {
        let folio = settings.folio.enabled.then(|| {
            FolioScanner::new(
                hotel_id,
                agent_version,
                settings.folio.clone(),
                settings.smile.clone(),
                &settings.program_data_dir,
                deps.shared.folio.clone(),
            )
        });
        ScanLoop {
            availability: AvailabilityScanner::new(
                settings.availability.clone(),
                &settings.smile.extra_bed_special_code,
            ),
            rooms: RoomScanner::new(settings.rooms.full_push_every),
            folio,
            probe_every: settings
                .availability
                .probe_interval
                .max(Duration::from_secs(1)),
            rooms_every: settings.rooms.interval.max(Duration::from_secs(1)),
            folio_every: settings.folio.interval.max(Duration::from_secs(1)),
            next_availability: None,
            next_rooms: None,
            next_folio: None,
            paused_logged: false,
            deps,
        }
    }

    /// Runs until `stop` fires.
    pub fn run(&mut self, stop: &StopSignal) {
        while !stop.is_stopped() {
            let wait = self.turn(Instant::now());
            if stop.wait_timeout(wait) {
                break;
            }
        }
    }

    /// Runs every scanner that is due at `now`; returns the wait until the next one.
    pub fn turn(&mut self, now: Instant) -> Duration {
        if self.deps.shared.scans_paused() {
            if !self.paused_logged {
                self.paused_logged = true;
                log::error(
                    "scans paused: SMILE shows another database than the pinned one",
                    &[],
                );
            }
            return IDLE;
        }
        self.paused_logged = false;
        let Some(sync) = self.deps.shared.sync_config() else {
            return IDLE;
        };
        let due = |next: Option<Instant>| next.is_none_or(|t| now >= t);
        if due(self.next_availability) {
            self.next_availability = Some(now + self.availability_pass(&sync, now));
        }
        if due(self.next_rooms) {
            self.rooms_pass(&sync);
            self.next_rooms = Some(now + self.rooms_every);
        }
        if self.folio.is_some() && due(self.next_folio) {
            let more = self.folio_pass(&sync);
            self.next_folio = Some(if more { now } else { now + self.folio_every });
        }
        [self.next_availability, self.next_rooms, self.next_folio]
            .into_iter()
            .flatten()
            .map(|t| t.saturating_duration_since(now))
            .min()
            .unwrap_or(IDLE)
            .min(IDLE.max(self.probe_every))
    }

    fn availability_pass(&mut self, sync: &SyncConfig, now: Instant) -> Duration {
        let d = &self.deps;
        let today = hotel_now().date();
        match self.availability.tick(
            d.src.as_ref(),
            d.hub.as_ref(),
            Some(sync),
            &d.tracker,
            today,
            now,
        ) {
            Ok(TickOutcome::Unchanged) => self.probe_every,
            Ok(TickOutcome::Scanned(o)) => {
                let (rows, what) = match o {
                    availability::ScanOutcome::SyncOff => {
                        (0, "availability sync off in the hub".into())
                    }
                    availability::ScanOutcome::GateClosed => (0, "write gate closed".into()),
                    availability::ScanOutcome::OverCap => {
                        (0, "dropped: folio read hit max_folios".into())
                    }
                    availability::ScanOutcome::NoRooms => {
                        (0, "dropped: SMILE returned no room".into())
                    }
                    availability::ScanOutcome::Pushed { lines, stays, full } => (
                        lines as u64,
                        format!("pushed {lines} lines from {stays} stays (full: {full})"),
                    ),
                };
                d.shared.record_scan("availability", Ok((rows, what)));
                self.probe_every
            }
            Err(e) => {
                let (smile, hub) = match &e {
                    AvailabilityError::Smile(s) => (Some(s), None),
                    AvailabilityError::Hub(h) => (None, Some(h)),
                };
                self.failed("availability", &e.to_string(), smile, hub);
                ERROR_BACKOFF
            }
        }
    }

    fn rooms_pass(&mut self, sync: &SyncConfig) {
        let d = &self.deps;
        match self
            .rooms
            .scan_once(d.src.as_ref(), d.hub.as_ref(), Some(sync), &d.tracker)
        {
            Ok(o) => {
                let (rows, what) = match o {
                    RoomOutcome::SyncOff => (0, "room sync off in the hub".into()),
                    RoomOutcome::GateClosed => (0, "write gate closed".into()),
                    RoomOutcome::OverCap(n) => (0, format!("dropped: {n} rooms is over the cap")),
                    RoomOutcome::Pushed {
                        rooms,
                        room_types,
                        full,
                    } => (
                        rooms as u64,
                        format!("pushed {rooms} rooms, {room_types} room types (full: {full})"),
                    ),
                };
                d.shared.record_scan("rooms", Ok((rows, what)));
            }
            Err(e) => {
                let (smile, hub) = match &e {
                    RoomError::Smile(s) => (Some(s), None),
                    RoomError::Hub(h) => (None, Some(h)),
                };
                self.failed("rooms", &e.to_string(), smile, hub);
            }
        }
    }

    /// Returns whether the cursor moved a full batch (scan again at once).
    fn folio_pass(&mut self, sync: &SyncConfig) -> bool {
        let Some(scanner) = self.folio.as_mut() else {
            return false;
        };
        let d = &self.deps;
        match scanner.scan_once(d.src.as_ref(), d.hub.as_ref(), &d.tracker, Some(sync)) {
            Ok(o) => {
                let p = o.posted;
                d.shared.record_scan(
                    "folio",
                    Ok((
                        o.rows as u64,
                        format!(
                            "read {} folios; sent {}, skipped {}, accepted {}",
                            o.rows, p.sent, p.skipped, p.accepted
                        ),
                    )),
                );
                o.more
            }
            Err(e) => {
                let msg = e.to_string();
                let (smile, hub) = match &e {
                    FolioError::Smile(s) => (Some(s), None),
                    FolioError::Hub(h) => (None, Some(h)),
                    FolioError::Disabled(_) | FolioError::Ambiguous(_) => (None, None),
                };
                self.failed("folio", &msg, smile, hub);
                false
            }
        }
    }

    /// Records a failed pass; reports it unless it is the token gate (the sync thread
    /// reports that one).
    fn failed(
        &self,
        scanner: &'static str,
        msg: &str,
        smile: Option<&SmileError>,
        hub: Option<&HubError>,
    ) {
        let shared = &self.deps.shared;
        shared.record_scan(scanner, Err(msg.to_owned()));
        if hub.is_some_and(HubError::is_auth) {
            return;
        }
        log::warn(
            "scan failed",
            &[("scanner", V::S(scanner)), ("error", V::S(msg))],
        );
        if let Some(SmileError::Unavailable(m)) = smile {
            let m = m.clone();
            shared.update_health(|h| h.smile_error = Some(m));
        }
        shared.report(
            Severity::Warning,
            codes::SCAN_FAILED,
            msg,
            vec![("scanner", scanner.to_owned())],
        );
    }
}
