//! `SyncWorker`: the heartbeat, the shared sync-config and, when enabled, the job pull.
//! Read-only, pulled jobs are counted and left (the hub leases them again). With a
//! [`Dispatcher`] (not read-only) each job runs and is acknowledged with its lease
//! (`PumpCoreAsync`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use dl_agent::report::Severity;
use dl_time::OffsetDateTime;
use dl_winsvc::StopSignal;

use crate::hub::{HubApi, HubError};
use crate::identity::{self, Refresh, Tracker};
use crate::jobs::dispatch::{Dispatcher, Outcome};
use crate::log::{self, V};
use crate::module::state::{Shared, codes};
use crate::smile::{SmileHealth, hotel_offset};
use crate::terms::TermsSync;
use crate::wire::Heartbeat;

/// `IdlePollSeconds`: the pause after a quiet loop turn.
pub const IDLE: Duration = Duration::from_secs(5);
/// `MaxBackoffSeconds`.
pub const MAX_BACKOFF: Duration = Duration::from_secs(300);
/// How often the shared sync-config is read again.
pub const SYNC_CONFIG_EVERY: Duration = Duration::from_secs(60);

/// `BackoffFor(n)`: 5 s doubled per failure in a row, at most [`MAX_BACKOFF`].
pub fn backoff_for(failures: u32) -> Duration {
    let exp = failures.saturating_sub(1).min(16);
    Duration::from_secs(5u64 << exp).min(MAX_BACKOFF)
}

/// What the worker needs from the settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncSettings {
    /// Pull jobs (counted and left; off by default).
    pub pull_jobs: bool,
    /// `MaxJobsPerPull`.
    pub max_jobs_per_pull: u32,
    /// `HeartbeatSeconds`.
    pub heartbeat: Duration,
    /// Capabilities announced in the heartbeat.
    pub capabilities: Vec<String>,
}

/// The worker. One per enrolled run, on its own thread.
pub struct SyncWorker {
    hub: Arc<dyn HubApi>,
    smile: Arc<dyn SmileHealth>,
    tracker: Arc<Tracker>,
    shared: Arc<Shared>,
    settings: SyncSettings,
    agent_version: String,
    last_heartbeat: Option<Instant>,
    last_sync_config: Option<Instant>,
    smile_was_ok: bool,
    failures: u32,
    diagnostics_logged: bool,
    dispatcher: Option<Arc<Dispatcher>>,
    terms: Option<TermsSync>,
}

impl std::fmt::Debug for SyncWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncWorker")
            .field("settings", &self.settings)
            .field("failures", &self.failures)
            .finish_non_exhaustive()
    }
}

impl SyncWorker {
    /// A worker over `hub` and `smile`.
    pub fn new(
        hub: Arc<dyn HubApi>,
        smile: Arc<dyn SmileHealth>,
        tracker: Arc<Tracker>,
        shared: Arc<Shared>,
        settings: SyncSettings,
        agent_version: &str,
    ) -> SyncWorker {
        SyncWorker {
            hub,
            smile,
            tracker,
            shared,
            settings,
            agent_version: agent_version.to_owned(),
            last_heartbeat: None,
            last_sync_config: None,
            smile_was_ok: true,
            failures: 0,
            diagnostics_logged: false,
            dispatcher: None,
            terms: None,
        }
    }

    /// Executes pulled jobs through `dispatcher` instead of leaving them.
    pub fn with_dispatcher(mut self, dispatcher: Arc<Dispatcher>) -> SyncWorker {
        self.dispatcher = Some(dispatcher);
        self
    }

    /// Reports SMILE's extra-bed terms and special-service catalogue (`ExtraBedTermsSync`,
    /// every 300 s, after sync-config).
    pub fn with_terms(mut self, terms: TermsSync) -> SyncWorker {
        self.terms = Some(terms);
        self
    }

    /// Hub failures in a row.
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// Runs until `stop` fires.
    pub fn run(&mut self, stop: &StopSignal) {
        while !stop.is_stopped() {
            let wait = self.cycle(Instant::now());
            if stop.wait_timeout(wait) {
                break;
            }
        }
    }

    /// One loop turn (`RunLoopAsync` body); returns how long to wait before the next.
    pub fn cycle(&mut self, now: Instant) -> Duration {
        match self.step(now) {
            Ok(()) => {
                self.failures = 0;
                self.shared.update_health(|h| {
                    h.hub_failures = 0;
                    h.hub_error = None;
                    h.auth_rejected = false;
                });
                IDLE
            }
            Err(HubError::TokenRejected { retry_in }) => {
                self.failures = 0;
                let first = !self.shared.health().auth_rejected;
                self.shared.update_health(|h| {
                    h.auth_rejected = true;
                    h.hub_failures = 0;
                });
                if first {
                    log::error(
                        "hub rejected the agent token; pausing until the next probe",
                        &[("retry_minutes", V::I((retry_in.as_secs() / 60) as i64))],
                    );
                }
                self.shared.report(
                    Severity::Error,
                    codes::AUTH_FAILED,
                    "hub rejected the agent token (401); run membership-enroll on this machine",
                    Vec::new(),
                );
                retry_in.max(IDLE)
            }
            Err(e) => {
                self.failures = self.failures.saturating_add(1);
                let msg = e.to_string();
                log::warn(
                    "hub call failed; backing off",
                    &[
                        ("error", V::S(&msg)),
                        ("failures", V::I(i64::from(self.failures))),
                    ],
                );
                let failures = self.failures;
                self.shared.update_health(|h| {
                    h.hub_failures = failures;
                    h.hub_error = Some(msg.clone());
                    h.auth_rejected = false;
                });
                if failures >= 2 {
                    self.shared.report(
                        Severity::Error,
                        codes::HUB_FAILED,
                        format!("hub unreachable past one back-off: {msg}"),
                        vec![("failures", failures.to_string())],
                    );
                }
                backoff_for(failures)
            }
        }
    }

    /// The jobs (when enabled), the heartbeat when due, and sync-config when due.
    pub fn step(&mut self, now: Instant) -> Result<(), HubError> {
        if self.settings.pull_jobs {
            match self.dispatcher.clone() {
                Some(d) => self.pump(&d)?,
                None => self.pull_and_leave()?,
            }
        }
        let heartbeat_due = self
            .last_heartbeat
            .is_none_or(|t| now.saturating_duration_since(t) >= self.settings.heartbeat);
        if heartbeat_due {
            self.heartbeat()?;
            self.last_heartbeat = Some(now);
        }
        let config_due = self
            .last_sync_config
            .is_none_or(|t| now.saturating_duration_since(t) >= SYNC_CONFIG_EVERY);
        if config_due {
            let c = self.hub.sync_config()?;
            self.shared.set_sync_config(c);
            self.last_sync_config = Some(now);
        }
        if let Some(t) = self.terms.as_mut()
            && !self.shared.scans_paused()
        {
            t.run_if_due(now);
        }
        Ok(())
    }

    /// Pulls (and so leases) jobs, counts them and leaves them unacknowledged.
    fn pull_and_leave(&mut self) -> Result<(), HubError> {
        let jobs = self.hub.pull_jobs(self.settings.max_jobs_per_pull.max(1))?;
        let n = jobs.len() as u64;
        self.shared.update_stats(|s| {
            s.jobs_seen += n;
            s.jobs_left = n;
        });
        if n > 0 {
            log::warn(
                "jobs pulled and left unacknowledged (the writers are M2); the hub leases them again",
                &[("count", V::I(n as i64))],
            );
        }
        Ok(())
    }

    /// `PumpCoreAsync`: runs and acknowledges each pulled job. One failed ack never stops
    /// the batch; when every ack failed, the last error goes to the back-off.
    fn pump(&mut self, dispatcher: &Dispatcher) -> Result<(), HubError> {
        let jobs = self.hub.pull_jobs(self.settings.max_jobs_per_pull.max(1))?;
        let n = jobs.len() as u64;
        self.shared.update_stats(|s| s.jobs_seen += n);
        let (mut acked, mut left, mut ack_failures) = (0u64, 0u64, 0usize);
        let mut last_error = None;
        for job in &jobs {
            match dispatcher.process(job) {
                Outcome::Ack(ack) => match self.hub.ack_job(&ack) {
                    Ok(()) => acked += 1,
                    Err(e) => {
                        ack_failures += 1;
                        log::error(
                            "job ack failed; the hub delivers the job again",
                            &[("job", V::S(&job.job_id)), ("error", V::S(&e.to_string()))],
                        );
                        last_error = Some(e);
                    }
                },
                Outcome::Leave(_) => left += 1,
            }
        }
        self.shared.update_stats(|s| {
            s.jobs_executed += acked + ack_failures as u64;
            s.jobs_acked += acked;
            s.jobs_left = left;
        });
        match last_error {
            Some(e) if ack_failures == jobs.len() => Err(e),
            _ => Ok(()),
        }
    }

    /// `SendHeartbeatAsync`: SMILE probe, identity refresh, then the heartbeat.
    fn heartbeat(&mut self) -> Result<(), HubError> {
        let smile_ok = self.probe_smile();
        if smile_ok {
            let refresh = self.tracker.refresh_if_due(!self.smile_was_ok);
            self.check_identity(&refresh);
        }
        self.smile_was_ok = smile_ok;
        let hb = self.build_heartbeat(smile_ok);
        match self.hub.heartbeat(&hb) {
            Ok(diagnostics_requested) => {
                self.shared.update_stats(|s| {
                    s.heartbeats += 1;
                    s.heartbeat_last = Some(OffsetDateTime::now_utc());
                });
                self.shared.update_health(|h| h.heartbeat_ok = true);
                if diagnostics_requested && !self.diagnostics_logged {
                    self.diagnostics_logged = true;
                    log::info(
                        "hub asked for a SMILE diagnostics run; not available in this agent yet",
                        &[],
                    );
                }
                Ok(())
            }
            Err(e) => {
                self.shared.update_stats(|s| s.heartbeat_failures += 1);
                Err(e)
            }
        }
    }

    /// `ProbeSmileAsync`.
    fn probe_smile(&self) -> bool {
        match self.smile.ping() {
            Ok(()) => {
                if self.shared.health().smile_error.is_some() {
                    log::info("SMILE reachable again", &[]);
                }
                self.shared.update_health(|h| h.smile_error = None);
                true
            }
            Err(e) => {
                let msg = e.to_string();
                log::warn("SMILE probe failed", &[("error", V::S(&msg))]);
                self.shared
                    .update_health(|h| h.smile_error = Some(msg.clone()));
                self.shared.report(
                    Severity::Warning,
                    codes::SMILE_FAILED,
                    format!("SMILE probe failed: {msg}"),
                    Vec::new(),
                );
                false
            }
        }
    }

    /// Compares the pinned fingerprint with the latest reading (whichever thread read it).
    fn check_identity(&self, refresh: &Refresh) {
        if let Refresh::Failed(e) = refresh {
            log::warn(
                "SMILE identity read failed; previous reading kept",
                &[("error", V::S(e))],
            );
            return;
        }
        let (Some(pinned), Some(seen)) =
            (self.tracker.pinned(), self.tracker.current_fingerprint())
        else {
            return;
        };
        if identity::same_database(&pinned, &seen) {
            if self.shared.scans_paused() {
                log::info("SMILE shows the pinned database again; scans resume", &[]);
            }
            self.shared.update_health(|h| h.identity_changed = None);
            return;
        }
        let why = "SMILE now shows another database than the one pinned at start".to_owned();
        if !self.shared.scans_paused() {
            log::error(&why, &[("pinned", V::S(&pinned)), ("seen", V::S(&seen))]);
        }
        self.shared
            .update_health(|h| h.identity_changed = Some(why.clone()));
        self.shared.report(
            Severity::Critical,
            codes::DB_IDENTITY_CHANGED,
            format!("{why}; scans stopped until it matches again"),
            vec![("pinned", pinned), ("seen", seen)],
        );
    }

    fn build_heartbeat(&self, smile_ok: bool) -> Heartbeat {
        let scan = self.shared.folio.snapshot();
        Heartbeat {
            agent_version: self.agent_version.clone(),
            pending_jobs: 0,
            smile_ok,
            walk_in_earn_ok: self.shared.walk_in.is_healthy(),
            folio_scan_watermark: scan.map(|s| s.watermark),
            folio_scan_lag_seconds: scan.map(|s| s.lag_seconds),
            folio_scan_at: scan.map(|s| s.scan_at),
            db_identity: self.tracker.current().map(|s| s.identity),
            foreign_activity_age_minutes: self.tracker.foreign_age_minutes(),
            pms_now: self
                .tracker
                .pms_now()
                .map(|t| OffsetDateTime::new(t, hotel_offset())),
            capabilities: Some(self.settings.capabilities.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_from_five_seconds_up_to_five_minutes() {
        let s = |n| backoff_for(n).as_secs();
        assert_eq!(
            (s(0), s(1), s(2), s(3), s(6), s(7), s(40)),
            (5, 5, 10, 20, 160, 300, 300)
        );
    }

    mod pump {
        use std::sync::Arc;

        use super::*;
        use crate::jobs::beds::MODE_FOLD_LEGACY;
        use crate::jobs::dispatch::{DispatchSettings, Dispatcher};
        use crate::jobs::fakes::{self, Hub, Pms};
        use crate::smile::SmileError;
        use crate::wire::parse::Job;

        struct Up;
        impl SmileHealth for Up {
            fn ping(&self) -> Result<(), SmileError> {
                Ok(())
            }
        }

        fn job(id: &str, action: &str, attempts: i64) -> Job {
            Job {
                job_id: id.into(),
                action: action.into(),
                booking_id: None,
                payload_json: r#"{"bookingId":"6f1c2d3e-0000-4000-8000-000000000001"}"#.into(),
                attempts,
            }
        }

        fn worker(hub: &Arc<Hub>, pms: Option<&Arc<Pms>>) -> (SyncWorker, Arc<Shared>) {
            let shared = Arc::new(Shared::default());
            let w = SyncWorker::new(
                hub.clone(),
                Arc::new(Up),
                fakes::tracker("SMILE_DB"),
                shared.clone(),
                SyncSettings {
                    pull_jobs: true,
                    max_jobs_per_pull: 10,
                    heartbeat: Duration::from_secs(3600),
                    capabilities: Vec::new(),
                },
                "test",
            );
            let w = match pms {
                Some(p) => w.with_dispatcher(Arc::new(Dispatcher::new(
                    p.clone(),
                    None,
                    DispatchSettings {
                        origin_code: "AZ".into(),
                        member_discount_code: String::new(),
                        extra_bed_mode: MODE_FOLD_LEGACY.into(),
                    },
                ))),
                None => w,
            };
            (w, shared)
        }

        #[test]
        fn pulled_jobs_run_and_are_acked_with_their_lease() {
            let hub = Arc::new(Hub::default());
            *hub.jobs.lock().unwrap() = vec![job("a", "Cancel", 3), job("b", "Teleport", 1)];
            let pms = Arc::new(Pms::default());
            let (mut w, shared) = worker(&hub, Some(&pms));
            w.step(Instant::now()).unwrap();
            let acks = hub.acks.lock().unwrap().clone();
            assert_eq!(acks.len(), 2);
            assert!(acks[0].success && acks[0].attempts == Some(3));
            assert!(!acks[1].success && acks[1].attempts == Some(1));
            let s = shared.stats();
            assert_eq!(
                (s.jobs_seen, s.jobs_executed, s.jobs_acked, s.jobs_left),
                (2, 2, 2, 0)
            );
        }

        #[test]
        fn read_only_jobs_are_counted_and_never_acked() {
            let hub = Arc::new(Hub::default());
            *hub.jobs.lock().unwrap() = vec![job("a", "Cancel", 1)];
            let (mut w, shared) = worker(&hub, None);
            w.step(Instant::now()).unwrap();
            assert!(hub.acks.lock().unwrap().is_empty());
            assert_eq!(shared.stats().jobs_left, 1);
        }

        #[test]
        fn when_every_ack_fails_the_loop_backs_off() {
            let hub = Arc::new(Hub::default());
            *hub.jobs.lock().unwrap() = vec![job("a", "Cancel", 1)];
            *hub.fail_acks.lock().unwrap() = true;
            let pms = Arc::new(Pms::default());
            let (mut w, shared) = worker(&hub, Some(&pms));
            assert!(w.step(Instant::now()).is_err());
            assert_eq!(shared.stats().jobs_acked, 0);
        }

        #[test]
        fn unported_writes_are_left_unacked() {
            let hub = Arc::new(Hub::default());
            *hub.jobs.lock().unwrap() = vec![job("a", "Create", 1)];
            let pms = Arc::new(Pms::default());
            *pms.not_ported.lock().unwrap() = true;
            let (mut w, shared) = worker(&hub, Some(&pms));
            w.step(Instant::now()).unwrap();
            assert!(hub.acks.lock().unwrap().is_empty());
            assert_eq!(shared.stats().jobs_left, 1);
        }
    }
}
