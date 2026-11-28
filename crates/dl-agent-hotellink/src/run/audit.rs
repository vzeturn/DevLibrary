//! The PMS self-check (Go `internal/agent/audit.go`, RT-0702): on hub request the agent
//! reads the live Hotel Link folios of a window (or every folio of a few PMS keys) with
//! their last change, FolioLog rows and FolioRate rows, and sends the snapshot back.
//! Read-only; the hub decides what is a duplicate or missing.

use super::Runner;
use crate::jobmodel::{AuditRequest, AuditSnapshot};
use crate::log::{self, V};
use crate::pms::{AuditQuery, normalize_audit_keys};

const DEFAULT_WINDOW_DAYS: i64 = 7;
const MAX_WINDOW_DAYS: i64 = 60;
const DEFAULT_ROWS: i64 = 2000;

impl Runner {
    /// `runPMSAudits`: answers each request; a snapshot that cannot be posted is logged and
    /// the hub re-sends the request later.
    pub(super) fn run_pms_audits(&self, reqs: &[AuditRequest]) {
        for req in reqs {
            let snap = self.build_pms_audit(req);
            if !snap.error.is_empty() {
                self.record_error(&format!("pms-audit {}: {}", req.id, snap.error));
            }
            if let Err(e) = self.hub.post_pms_audit(&snap) {
                if self.superseded(&e) {
                    return;
                }
                self.note_hub_error(&e);
                log::warn(
                    "posting the PMS audit failed (the hub will ask again)",
                    &[("requestId", V::S(&req.id)), ("error", V::S(e.message()))],
                );
                continue;
            }
            log::info(
                "PMS audit sent",
                &[
                    ("requestId", V::S(&req.id)),
                    ("folios", V::I(snap.folios.len() as i64)),
                    (
                        "truncated",
                        V::S(if snap.truncated { "true" } else { "false" }),
                    ),
                    ("error", V::S(&snap.error)),
                    ("logError", V::S(&snap.log_error)),
                ],
            );
        }
    }

    /// `buildPMSAudit`: every failure ends up in the snapshot so the hub shows it.
    pub(super) fn build_pms_audit(&self, req: &AuditRequest) -> AuditSnapshot {
        let window = if req.window_days <= 0 {
            DEFAULT_WINDOW_DAYS
        } else {
            req.window_days
        }
        .min(MAX_WINDOW_DAYS);
        let rows = if req.max_rows <= 0 {
            DEFAULT_ROWS
        } else {
            req.max_rows
        };
        let keys = normalize_audit_keys(&req.keys);
        let now = (self.now)();
        let mut snap = AuditSnapshot {
            request_id: req.id.clone(),
            taken_at: super::rfc3339_now(&self.now),
            window_days: window,
            keys: keys.clone(),
            ..AuditSnapshot::default()
        };
        if !self.breaker.allow() {
            snap.error = "PMS unavailable (circuit breaker open)".into();
            return snap;
        }
        let hotel_date = match self.repo.get_hotel_date() {
            Ok(d) => d,
            Err(e) => {
                snap.warning = format!(
                    "hotel date unavailable, window counted from the agent clock: {}",
                    e.message()
                );
                now.to_offset(dl_time::Offset::VIETNAM).datetime()
            }
        };
        let day = hotel_date.start_of_day();
        let from = day.date().add_days(-window);
        snap.hotel_date = crate::pms::ymd(day);
        snap.from = from.to_string();
        let q = AuditQuery {
            from: day.with_date(from),
            max_rows: rows,
            log_rows: req.log_rows,
            keys,
        };
        match self.repo.audit_folios(&q) {
            Err(e) => {
                snap.error = e.message().to_owned();
                if e.is_pms_down() {
                    self.breaker.on_failure();
                    self.set_pms_ok(false);
                }
            }
            Ok(res) => {
                self.breaker.on_success();
                snap.truncated = res.truncated;
                if res.truncated
                    && let Some(last) = res.folios.last()
                {
                    snap.last_arrival = last.arrival_date.clone();
                }
                snap.folios = res.folios;
                snap.log_error = res.log_err.unwrap_or_default();
            }
        }
        snap
    }
}
