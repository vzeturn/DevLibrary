//! The availability and heartbeat loops and the hub policy (Go `availabilityLoop`,
//! `heartbeatLoop`, `fetchConfigBlocking`, `applyPolicy`, `seedPolicyFromCache`,
//! `pushAvailabilityOnce`).

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use dl_time::{Date, Offset, OffsetDateTime};

use super::Runner;
use crate::hubclient::{Heartbeat, HubConfig};
use crate::jobmodel::AvailRow;
use crate::log::{self, V};
use crate::policycache;

/// `startOfLocalDay`: today's date on the hotel's clock (Vietnam, UTC+7). Truncating the
/// UTC instant would give yesterday before 07:00 and push a past date to HL (error 123).
pub fn start_of_local_day(now: OffsetDateTime) -> Date {
    now.to_offset(Offset::VIETNAM).date()
}

/// `dropPastRows`: drops the rows dated before `today`; rows without a date are kept.
pub fn drop_past_rows(rows: Vec<AvailRow>, today: Date) -> Vec<AvailRow> {
    let cutoff = today.to_string();
    rows.into_iter()
        .filter(|r| r.date.is_empty() || r.date.as_str() >= cutoff.as_str())
        .collect()
}

impl Runner {
    /// `availabilityLoop`.
    pub(super) fn availability_loop(&self) {
        let Some(mut cfg) = self.fetch_config_blocking() else {
            return;
        };
        self.avail_interval_min
            .store(cfg.availability_interval_min, Ordering::SeqCst);
        let mut last_fetch = Instant::now();
        while !self.stop.is_stopped() {
            self.beats.beat("availability", Instant::now());
            self.push_availability_once(&cfg);
            if self
                .stop
                .sleep((self.t.avail_interval)(cfg.availability_interval_min))
            {
                return;
            }
            if last_fetch.elapsed() >= self.t.cfg_refresh {
                match self.hub.get_config() {
                    Ok(fresh) => {
                        self.avail_interval_min
                            .store(fresh.availability_interval_min, Ordering::SeqCst);
                        self.apply_policy(&fresh);
                        cfg = fresh;
                    }
                    Err(e) => {
                        if self.superseded(&e) {
                            return;
                        }
                        self.note_hub_error(&e);
                        log::warn("config refresh failed", &[("error", V::S(e.message()))]);
                    }
                }
                last_fetch = Instant::now();
            }
        }
    }

    /// `fetchConfigBlocking`: `None` when stopped or superseded.
    fn fetch_config_blocking(&self) -> Option<HubConfig> {
        let mut backoff = self.t.backoff_start;
        while !self.stop.is_stopped() {
            match self.hub.get_config() {
                Ok(cfg) => {
                    self.apply_policy(&cfg);
                    return Some(cfg);
                }
                Err(e) => {
                    if self.superseded(&e) {
                        return None;
                    }
                    self.note_hub_error(&e);
                    log::warn(
                        "hub config fetch failed",
                        &[
                            ("error", V::S(e.message())),
                            ("retryInMs", V::I(backoff.as_millis() as i64)),
                        ],
                    );
                }
            }
            if self.stop.sleep(backoff) {
                return None;
            }
            backoff = (backoff * 2).min(self.t.backoff_max);
        }
        None
    }

    /// `applyPolicy`: the hub's policy wins and is cached as last-known-good.
    fn apply_policy(&self, cfg: &HubConfig) {
        let Some(p) = &cfg.policy else {
            return;
        };
        self.rec.set_policy(*p, true);
        if let Err(e) = policycache::write(&self.state_dir, p) {
            log::warn(
                "writing policy-cache failed (continuing)",
                &[("error", V::S(&e.to_string()))],
            );
        }
    }

    /// `seedPolicyFromCache`: the module's cache first, then the Go agent's.
    pub(super) fn seed_policy_from_cache(&self) {
        let cached = policycache::read(&self.state_dir).or_else(|| {
            self.legacy_policy_dir
                .as_deref()
                .and_then(policycache::read)
        });
        if let Some(p) = cached {
            self.rec.set_policy(p, true);
            log::info(
                "policy applied from the last-known-good cache (until the hub answers)",
                &[],
            );
        }
    }

    /// `pushAvailabilityOnce`.
    pub(super) fn push_availability_once(&self, cfg: &HubConfig) {
        if !cfg.enabled {
            return;
        }
        if !self.rec.policy_from_hub() {
            let flag = match self.repo.get_global_param_int("HLInventorySync") {
                Ok(f) => f,
                Err(e) => {
                    log::error(
                        "reading HLInventorySync failed",
                        &[("error", V::S(e.message()))],
                    );
                    self.record_error(&format!("availability: {}", e.message()));
                    return;
                }
            };
            if flag == 0 {
                log::info(
                    "HLInventorySync is off, telling the hub (X-Inventory-Sync: 0)",
                    &[],
                );
                if let Err(e) = self.hub.push_availability_disabled() {
                    self.note_hub_error(&e);
                    log::warn(
                        "telling the hub that sync is off failed",
                        &[("error", V::S(e.message()))],
                    );
                }
                return;
            }
        }
        let today = start_of_local_day((self.now)());
        let rows = match self.repo.get_availability(today, cfg.availability_days) {
            Ok(r) => r,
            Err(e) => {
                log::error("AvailabilityTable failed", &[("error", V::S(e.message()))]);
                self.record_error(&format!("availability: {}", e.message()));
                return;
            }
        };
        let rows = drop_past_rows(rows, today);
        if let Err(e) = self.hub.push_availability(&rows) {
            self.note_hub_error(&e);
            log::error("availability push failed", &[("error", V::S(e.message()))]);
            self.record_error(&format!("availability: {}", e.message()));
            return;
        }
        self.stats
            .availability_pushes
            .fetch_add(1, Ordering::SeqCst);
        log::info(
            "availability pushed",
            &[
                ("rows", V::I(rows.len() as i64)),
                ("days", V::I(cfg.availability_days)),
            ],
        );
    }

    /// `heartbeatLoop` (without the Go self-update: dl-agent updates itself).
    pub(super) fn heartbeat_loop(&self) {
        while !self.stop.is_stopped() {
            self.beats.beat("heartbeat", Instant::now());
            self.send_heartbeat();
            if self.stop.sleep(self.t.heartbeat) {
                return;
            }
        }
    }

    fn avail_interval(&self) -> Duration {
        (self.t.avail_interval)(self.avail_interval_min.load(Ordering::SeqCst))
    }

    /// `sendHeartbeat`.
    fn send_heartbeat(&self) {
        let stalled = self
            .beats
            .stalled_loops(Instant::now(), self.avail_interval());
        if !stalled.is_empty() {
            log::error(
                "loop silent past its threshold (may be dead)",
                &[("loops", V::S(&stalled.join(",")))],
            );
        }
        self.stats.with_status(|s| s.stalled = stalled.clone());
        let hb = Heartbeat {
            pms_ok: !self.stats.pms_bad.load(Ordering::SeqCst),
            breaker_state: self.breaker.state().into(),
            queue_depth: self.queue.depth() as i64,
            recent_errors: self.drain_errors(),
            loops_stalled: stalled,
        };
        if let Err(e) = self.hub.heartbeat(&hb) {
            if self.superseded(&e) {
                return;
            }
            self.note_hub_error(&e);
            log::debug(
                "heartbeat failed (retried later)",
                &[("error", V::S(e.message()))],
            );
        }
    }
}
