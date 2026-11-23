//! The booking loop (Go `bookingLoop`, `processJob`, the queue drains and the remote
//! mapping commands).

use std::sync::atomic::Ordering;

use dl_time::{Date, DateTime};

use super::{Runner, rfc3339_now};
use crate::error::{Error, Kind, Result};
use crate::gofmt::quote;
use crate::jobmodel::{
    ACTION_SAVE, Job, MAPPING_DELETE, MAPPING_INSERT, MAPPING_UPDATE, MappingCmd, PmsResult,
    mapping_hash,
};
use crate::log::{self, V};

/// What the poll answer is worth to the loop.
enum Next {
    Continue,
    /// Jobs came back while the breaker is open: they stay held, so poll again later.
    Held,
    Stop,
}

impl Runner {
    /// `bookingLoop`.
    pub(super) fn booking_loop(&self) {
        let mut backoff = self.t.backoff_start;
        while !self.stop.is_stopped() {
            self.beats.beat("booking", std::time::Instant::now());
            self.drain_unacked();
            self.drain_pending();
            match self.poll_once() {
                Ok(Next::Continue) => backoff = self.t.backoff_start,
                Ok(Next::Held) => {
                    backoff = self.t.backoff_start;
                    if self.stop.sleep(self.t.breaker_poll_wait) {
                        return;
                    }
                }
                Ok(Next::Stop) => return,
                Err(e) => {
                    if self.stop.is_stopped() || self.superseded(&e) {
                        return;
                    }
                    self.note_hub_error(&e);
                    let streak = self.stats.hub_fail_streak.fetch_add(1, Ordering::SeqCst) + 1;
                    if streak == 1 {
                        self.stats
                            .event("hl_hub_failed", format!("hub poll failed ({:?})", e.kind()));
                    }
                    log::warn(
                        "hub poll failed",
                        &[
                            ("error", V::S(e.message())),
                            ("retryInMs", V::I(backoff.as_millis() as i64)),
                        ],
                    );
                    if self.stop.sleep(backoff) {
                        return;
                    }
                    backoff = (backoff * 2).min(self.t.backoff_max);
                }
            }
        }
    }

    fn poll_once(&self) -> Result<Next> {
        let poll = self.hub.poll_jobs(self.t.poll_wait_sec)?;
        self.stats.hub_fail_streak.store(0, Ordering::SeqCst);
        self.stats.unauthorized.store(false, Ordering::SeqCst);
        self.stats.polls_ok.fetch_add(1, Ordering::SeqCst);
        if self.stop.is_stopped() {
            // The jobs stay pending on the hub, which hands them out again.
            return Ok(Next::Stop);
        }
        self.stats
            .jobs_polled
            .fetch_add(poll.jobs.len() as u64, Ordering::SeqCst);
        if !poll.mapping_cmds.is_empty() {
            self.apply_mapping_cmds(&poll.mapping_cmds);
        } else if poll.want_mapping
            && let Err(e) = self.sync_mapping()
        {
            log::warn(
                "mapping push requested by the hub failed",
                &[("error", V::S(e.message()))],
            );
        }
        let hotel_date = self.hotel_date();
        for job in &poll.jobs {
            if let Err(e) = self.queue.enqueue(job) {
                log::warn(
                    "local enqueue failed (processing anyway)",
                    &[("jobId", V::S(&job.job_id)), ("error", V::S(e.message()))],
                );
            }
            self.process_job(job, hotel_date);
        }
        // The hub hands pending jobs out again on every poll without waiting; with the PMS
        // down they would come straight back (Go polls again at once here).
        if !poll.jobs.is_empty() && self.breaker.state() == "open" {
            return Ok(Next::Held);
        }
        Ok(Next::Continue)
    }

    /// `GetHotelDate`, fail-open: `None` processes every booking.
    fn hotel_date(&self) -> Option<DateTime> {
        match self.repo.get_hotel_date() {
            Ok(d) => Some(d),
            Err(e) => {
                log::warn(
                    "GetHotelDate failed, fail-open (processing every booking)",
                    &[("error", V::S(e.message()))],
                );
                None
            }
        }
    }

    /// `pastModifyHasFolio`: the booking's arrival is before the hotel date but the PMS
    /// still holds a live folio of it. That is a CHANGE of an existing booking (stay
    /// extended, late arrival, new rate), not a back-dated create, so it is reconciled as
    /// usual (the modify policy's check-in / room-assigned guards still apply in the
    /// reconciler). Before RT-0651 every such change was skipped silently. A PMS read error
    /// goes back to the caller: the job stays pending, neither skipped (would hide the
    /// change) nor applied (a later empty read would book a back-dated folio).
    fn past_modify_has_folio(&self, job: &Job) -> Result<bool> {
        if job.action != ACTION_SAVE {
            return Ok(false);
        }
        let folios = self.rec.find_existing_folios(job)?;
        if folios.is_empty() {
            return Ok(false);
        }
        log::info(
            "past-arrival booking still has folios: reconciling the change",
            &[
                ("jobId", V::S(&job.job_id)),
                ("folios", V::I(folios.len() as i64)),
            ],
        );
        Ok(true)
    }

    /// `processJob`: applies one job when the breaker allows, then acks it. Breaker open
    /// keeps the job pending in the queue (not applied, not dropped).
    pub(super) fn process_job(&self, job: &Job, hotel_date: Option<DateTime>) {
        self.stats
            .with_status(|s| s.last_job_at = rfc3339_now(&self.now));
        if !self.breaker.allow() {
            self.set_pms_ok(false);
            log::warn(
                "breaker open: job held in the local queue until the PMS recovers",
                &[("jobId", V::S(&job.job_id))],
            );
            return;
        }

        // Past booking: skipped, UNLESS the PMS still has a folio of it (RT-0651).
        if let Some(hd) = hotel_date
            && !job.check_in.is_empty()
            && let Ok(ci) = Date::parse_iso(&job.check_in)
            && ci < hd.date()
        {
            match self.past_modify_has_folio(job) {
                Err(e) => {
                    log::warn(
                        "reading folios of a past-arrival booking failed: kept pending, retried later",
                        &[("jobId", V::S(&job.job_id)), ("error", V::S(e.message()))],
                    );
                    if e.is_pms_down() {
                        self.breaker.on_failure();
                        self.set_pms_ok(false);
                    }
                    return; // no ack: the job stays pending locally and on the hub
                }
                Ok(false) => {
                    let skip = PmsResult {
                        action: "skip".into(),
                        completed: false,
                        reason: "arrival < HotelDate (booking quá khứ)".into(),
                        folios: Vec::new(),
                        synced_at: rfc3339_now(&self.now),
                        ..PmsResult::default()
                    };
                    log::info(
                        "past booking skipped",
                        &[
                            ("jobId", V::S(&job.job_id)),
                            ("checkIn", V::S(&job.check_in)),
                        ],
                    );
                    self.mark_applied(&job.job_id, true, "", Some(&skip));
                    self.ack_job(&job.job_id, true, "", Some(&skip));
                    return;
                }
                Ok(true) => {}
            }
        }

        let res = self.rec.apply(job);
        if let Err(e) = &res
            && e.kind() == Kind::PmsDown
        {
            self.breaker.on_failure();
            self.set_pms_ok(false);
            self.stats.jobs_failed.fetch_add(1, Ordering::SeqCst);
            self.stats.event(
                "hl_pms_failed",
                format!("job {}: PMS unreachable, job held", job.job_id),
            );
            self.record_error(&format!("job {}: PMS down: {}", job.job_id, e.message()));
            log::error(
                "apply job failed, PMS down (kept pending)",
                &[("jobId", V::S(&job.job_id)), ("error", V::S(e.message()))],
            );
            return;
        }
        self.breaker.on_success();
        self.set_pms_ok(true);

        match res {
            Ok(r) if r.needs_manual => {
                self.stats.manual_conflicts.fetch_add(1, Ordering::SeqCst);
                self.stats.event(
                    "hl_manual_conflict",
                    format!("job {}: booking routed to manual handling", job.job_id),
                );
                log::warn(
                    "job needs manual handling (policy blocked)",
                    &[("jobId", V::S(&job.job_id)), ("reason", V::S(&r.reason))],
                );
                self.mark_applied(&job.job_id, true, "", Some(&r));
                self.ack_job(&job.job_id, true, "", Some(&r));
            }
            Ok(r) => {
                self.mark_applied(&job.job_id, true, "", Some(&r));
                self.ack_job(&job.job_id, true, "", Some(&r));
            }
            Err(e) => {
                self.stats.jobs_failed.fetch_add(1, Ordering::SeqCst);
                self.stats.event(
                    "hl_reconcile_failed",
                    format!("job {}: apply failed ({:?})", job.job_id, e.kind()),
                );
                self.record_error(&format!("job {}: {}", job.job_id, e.message()));
                log::error(
                    "apply job failed",
                    &[("jobId", V::S(&job.job_id)), ("error", V::S(e.message()))],
                );
                self.mark_applied(&job.job_id, false, e.message(), None);
                self.ack_job(&job.job_id, false, e.message(), None);
            }
        }
    }

    fn mark_applied(&self, job_id: &str, ok: bool, err: &str, res: Option<&PmsResult>) {
        if ok {
            self.stats.jobs_applied.fetch_add(1, Ordering::SeqCst);
        }
        if let Err(e) = self.queue.mark_applied(job_id, ok, err, res) {
            log::warn(
                "local queue: mark applied failed",
                &[("jobId", V::S(job_id)), ("error", V::S(e.message()))],
            );
        }
    }

    /// `ackJob`: a failed ack stays `applied` and `drain_unacked` sends it again.
    fn ack_job(&self, job_id: &str, ok: bool, err: &str, res: Option<&PmsResult>) {
        let recent = self.drain_errors();
        match self.hub.ack(job_id, ok, err, &recent, res) {
            Ok(()) => self.acked(job_id),
            Err(e) => {
                self.note_hub_error(&e);
                log::warn(
                    "ack failed, retried on the next round",
                    &[("jobId", V::S(job_id)), ("error", V::S(e.message()))],
                );
            }
        }
    }

    fn acked(&self, job_id: &str) {
        self.stats.jobs_acked.fetch_add(1, Ordering::SeqCst);
        if let Err(e) = self.queue.mark_acked(job_id) {
            log::warn(
                "local queue: mark acked failed",
                &[("jobId", V::S(job_id)), ("error", V::S(e.message()))],
            );
        }
    }

    /// `drainUnacked`: acks again the jobs whose ack was lost.
    pub(super) fn drain_unacked(&self) {
        let entries = self.queue.unacked();
        if entries.is_empty() {
            return;
        }
        for e in &entries {
            log::info(
                "re-acking a job applied but not acked",
                &[("jobId", V::S(&e.job_id))],
            );
            match self
                .hub
                .ack(&e.job_id, e.ok, &e.last_error, &[], e.pms_result.as_ref())
            {
                Ok(()) => self.acked(&e.job_id),
                Err(err) => {
                    self.note_hub_error(&err);
                    log::warn(
                        "re-ack failed",
                        &[("jobId", V::S(&e.job_id)), ("error", V::S(err.message()))],
                    );
                }
            }
        }
        if let Err(e) = self.queue.compact() {
            log::warn(
                "local queue compaction failed",
                &[("error", V::S(e.message()))],
            );
        }
    }

    /// `drainPending`: applies the jobs held while the PMS was down.
    fn drain_pending(&self) {
        if !self.breaker.allow() {
            return;
        }
        let entries = self.queue.pending();
        if entries.is_empty() {
            return;
        }
        let hotel_date = self.repo.get_hotel_date().ok();
        for e in &entries {
            if self.stop.is_stopped() {
                return;
            }
            self.process_job(&e.job, hotel_date);
        }
    }

    /// `syncMapping`: reads `RoomTypeMapped` and pushes it to the hub.
    pub fn sync_mapping(&self) -> Result<()> {
        let rows = self.repo.get_room_type_mapped().inspect_err(|e| {
            log::error(
                "reading RoomTypeMapped failed",
                &[("error", V::S(e.message()))],
            );
            self.record_error(&format!("mapping: {}", e.message()));
        })?;
        let hash = mapping_hash(&rows);
        self.hub.push_mapping(&rows, &hash).inspect_err(|e| {
            self.note_hub_error(e);
            log::error("mapping push failed", &[("error", V::S(e.message()))]);
        })?;
        self.hub.set_mapping_hash(&hash);
        let at = rfc3339_now(&self.now);
        self.stats.with_status(|s| {
            s.mapping_hash = hash.clone();
            s.mapping_count = rows.len() as i64;
            s.last_sync_at = at;
        });
        log::info(
            "mapping pushed to the hub",
            &[
                ("rows", V::I(rows.len() as i64)),
                ("hash", V::S(&hash[..12.min(hash.len())])),
            ],
        );
        Ok(())
    }

    /// `applyMappingCmds`: PMS down holds the rest of the batch unacked; other errors are
    /// acked as failed; any applied command triggers a mapping push.
    pub(super) fn apply_mapping_cmds(&self, cmds: &[MappingCmd]) {
        if !self.breaker.allow() {
            self.set_pms_ok(false);
            log::warn(
                "breaker open: mapping commands deferred until the PMS recovers",
                &[("count", V::I(cmds.len() as i64))],
            );
            return;
        }
        let mut applied = false;
        for cmd in cmds {
            let res = self.apply_one_mapping_cmd(cmd);
            if let Err(e) = &res
                && e.kind() == Kind::PmsDown
            {
                self.breaker.on_failure();
                self.set_pms_ok(false);
                self.stats.event(
                    "hl_pms_failed",
                    format!("mapping-cmd {}: PMS unreachable, held", cmd.cmd_id),
                );
                self.record_error(&format!(
                    "mapping-cmd {}: PMS down: {}",
                    cmd.cmd_id,
                    e.message()
                ));
                log::error(
                    "mapping command failed, PMS down (held)",
                    &[("cmdId", V::S(&cmd.cmd_id)), ("error", V::S(e.message()))],
                );
                return;
            }
            self.breaker.on_success();
            self.set_pms_ok(true);
            applied = true;
            let err = match &res {
                Ok(()) => {
                    log::info(
                        "mapping command applied",
                        &[("cmdId", V::S(&cmd.cmd_id)), ("op", V::S(&cmd.op))],
                    );
                    String::new()
                }
                Err(e) => {
                    self.record_error(&format!("mapping-cmd {}: {}", cmd.cmd_id, e.message()));
                    log::error(
                        "mapping command failed (acked as failed)",
                        &[
                            ("cmdId", V::S(&cmd.cmd_id)),
                            ("op", V::S(&cmd.op)),
                            ("error", V::S(e.message())),
                        ],
                    );
                    e.message().to_owned()
                }
            };
            if let Err(e) = self.hub.ack_mapping_cmd(&cmd.cmd_id, res.is_ok(), &err) {
                self.note_hub_error(&e);
                log::warn(
                    "mapping command ack failed (the hub resends; re-apply is idempotent)",
                    &[("cmdId", V::S(&cmd.cmd_id)), ("error", V::S(e.message()))],
                );
            }
        }
        if applied && let Err(e) = self.sync_mapping() {
            log::warn(
                "mapping push after commands failed",
                &[("error", V::S(e.message()))],
            );
        }
    }

    fn apply_one_mapping_cmd(&self, cmd: &MappingCmd) -> Result<()> {
        let no_orig = || Error::other("lệnh update/delete thiếu orig");
        match cmd.op.as_str() {
            MAPPING_INSERT => self.repo.insert_mapping(&cmd.row),
            MAPPING_UPDATE => self
                .repo
                .update_mapping(cmd.orig.as_ref().ok_or_else(no_orig)?, &cmd.row),
            MAPPING_DELETE => self
                .repo
                .delete_mapping(cmd.orig.as_ref().ok_or_else(no_orig)?),
            op => Err(Error::other(format!(
                "op mapping không hợp lệ: {}",
                quote(op)
            ))),
        }
    }
}
