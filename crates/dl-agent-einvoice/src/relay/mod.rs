//! The provider egress relay (ADR 0016; Go `internal/agentcollector/relay.go`).
//!
//! The hub hands VNPT calls to the tenant's agent because the portal answers only from
//! Vietnamese addresses. The worker long-polls `/relay/next`, checks the call again
//! against the allowlist, performs it and posts the answer to `/relay/{id}/result`, one
//! call at a time. Bodies carry the tenant's credential: they are never logged or stored;
//! log lines carry the request id, host, status and duration only.

pub mod allow;
pub mod transport;
pub mod wire;

use std::sync::Arc;
use std::time::{Duration, Instant};

use dl_agent::report::Severity;
use dl_agent::supervisor::ModuleContext;
use dl_http::Method;
use dl_json::Map;
use dl_winsvc::StopSignal;

use crate::log::{self, V};
use crate::platform::{ApiError, Platform};
use crate::stats::Stats;
use transport::{CallError, ProviderCall, Transport};
use wire::{Poll, RelayRequest, RelayResult};

/// First wait after a failed poll.
pub const BACKOFF_MIN: Duration = Duration::from_secs(5);
/// The longest wait between failed polls.
pub const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// Wait when the hub has no relay route (a hub older than ADR 0016).
pub const UNSUPPORTED_WAIT: Duration = Duration::from_secs(5 * 60);

/// The hub side of the relay.
pub trait Hub: Send {
    /// One long poll of `/relay/next`.
    fn next(&self) -> Result<Poll, ApiError>;
    /// Posts the answer of call `id`.
    fn result(&self, id: &str, res: &RelayResult) -> Result<(), ApiError>;
}

impl Hub for Platform {
    fn next(&self) -> Result<Poll, ApiError> {
        self.relay_next()
    }

    fn result(&self, id: &str, res: &RelayResult) -> Result<(), ApiError> {
        self.relay_result(id, res)
    }
}

/// Why the worker returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The stop signal was raised.
    Stopped,
    /// The platform says this install was replaced by an agent on another machine.
    Replaced,
}

/// Waits between polls; returns early when `stop` is raised.
pub type Sleep = Box<dyn FnMut(&StopSignal, Duration) + Send>;

/// Serves relayed provider calls until stopped or replaced (`RelayWorker`).
pub struct Worker<H: Hub, T: Transport> {
    hub: H,
    transport: T,
    stop: StopSignal,
    stats: Arc<Stats>,
    ctx: ModuleContext,
    sleep: Sleep,
}

impl<H: Hub, T: Transport> Worker<H, T> {
    /// A worker that reports problems through `ctx` and counts into `stats`.
    pub fn new(
        hub: H,
        transport: T,
        stop: StopSignal,
        stats: Arc<Stats>,
        ctx: ModuleContext,
    ) -> Worker<H, T> {
        Worker {
            hub,
            transport,
            stop,
            stats,
            ctx,
            sleep: Box::new(|stop, d| {
                stop.wait_timeout(d);
            }),
        }
    }

    /// Replaces the wait between polls (tests).
    pub fn with_sleep(mut self, sleep: Sleep) -> Worker<H, T> {
        self.sleep = sleep;
        self
    }

    /// `Run`: polls and serves until stopped or replaced.
    pub fn run(&mut self) -> Exit {
        let mut backoff = BACKOFF_MIN;
        while !self.stop.is_stopped() {
            let req = match self.hub.next() {
                Err(ApiError::DeviceReplaced) => return Exit::Replaced,
                Ok(Poll::Unsupported) => {
                    (self.sleep)(&self.stop, UNSUPPORTED_WAIT);
                    continue;
                }
                Err(e) => {
                    if !self.stop.is_stopped() {
                        self.poll_failed(&e, backoff);
                    }
                    (self.sleep)(&self.stop, backoff);
                    backoff = (backoff * 2).min(BACKOFF_MAX);
                    continue;
                }
                Ok(Poll::Empty) => {
                    backoff = BACKOFF_MIN;
                    continue;
                }
                Ok(Poll::Call(req)) => req,
            };
            backoff = BACKOFF_MIN;
            if self.stop.is_stopped() {
                break;
            }
            Stats::bump(&self.stats.relay_calls, 1);
            let res = self.perform(&req);
            if !res.error.is_empty() {
                Stats::bump(&self.stats.relay_failures, 1);
            }
            if let Err(e) = self.hub.result(&req.id, &res)
                && !self.stop.is_stopped()
            {
                let err = e.to_string();
                log::warn(
                    "relay: post result failed",
                    &[("request_id", V::S(&req.id)), ("error", V::S(&err))],
                );
                self.report(
                    Severity::Warning,
                    "einvoice_relay_result_failed",
                    &err,
                    &req.id,
                );
            }
        }
        Exit::Stopped
    }

    fn poll_failed(&self, e: &ApiError, backoff: Duration) {
        Stats::bump(&self.stats.relay_poll_failures, 1);
        let err = e.to_string();
        log::warn(
            "relay: poll failed",
            &[
                ("error", V::S(&err)),
                ("retry_in", V::S(&format!("{}s", backoff.as_secs()))),
            ],
        );
        let mut m = Map::new();
        m.insert("retry_in_seconds", backoff.as_secs());
        self.ctx
            .report(Severity::Warning, "einvoice_relay_poll_failed", &err, m);
    }

    fn report(&self, severity: Severity, code: &str, msg: &str, request_id: &str) {
        let mut m = Map::new();
        m.insert("request_id", request_id);
        self.ctx.report(severity, code, msg, m);
    }

    fn refuse(&self, req: &RelayRequest, reason: &str) -> RelayResult {
        Stats::bump(&self.stats.relay_refused, 1);
        log::warn(
            "relay: refused",
            &[("request_id", V::S(&req.id)), ("reason", V::S(reason))],
        );
        self.report(Severity::Warning, "einvoice_relay_refused", reason, &req.id);
        RelayResult::failed(format!("agent từ chối: {reason}"))
    }

    /// `perform`: checks the call again, then makes it.
    pub fn perform(&self, req: &RelayRequest) -> RelayResult {
        if let Err(reason) = allow::check_method(&req.method) {
            return self.refuse(req, reason);
        }
        let url = match allow::check_url(&req.url) {
            Ok(u) => u,
            Err(reason) => return self.refuse(req, reason),
        };
        let call = ProviderCall {
            method: if req.method == "GET" {
                Method::Get
            } else {
                Method::Post
            },
            headers: req
                .header
                .iter()
                .filter(|(name, _)| allow::forward_header(name))
                .cloned()
                .collect(),
            body: req.body.clone(),
            timeout: allow::clamp_timeout(req.timeout_ms),
            url,
        };
        let host = call.url.host().to_owned();
        let start = Instant::now();
        let answer = self.transport.call(&call);
        let ms = i64::try_from(start.elapsed().as_millis()).unwrap_or(i64::MAX);
        match answer {
            Err(CallError::Failed(reason)) => {
                log::warn(
                    "relay: provider call failed",
                    &[
                        ("request_id", V::S(&req.id)),
                        ("host", V::S(&host)),
                        ("duration_ms", V::I(ms)),
                        ("error", V::S(&reason)),
                    ],
                );
                let mut m = Map::new();
                m.insert("request_id", req.id.as_str());
                m.insert("host", host.as_str());
                m.insert("duration_ms", ms);
                self.ctx
                    .report(Severity::Warning, "einvoice_relay_call_failed", &reason, m);
                RelayResult::failed(reason)
            }
            Err(CallError::TooLarge) => {
                RelayResult::failed("trả lời của cổng nhà cung cấp quá lớn để chuyển tiếp")
            }
            Ok(a) => {
                log::info(
                    "relay: provider call done",
                    &[
                        ("request_id", V::S(&req.id)),
                        ("host", V::S(&host)),
                        ("status", V::I(i64::from(a.status))),
                        ("duration_ms", V::I(ms)),
                    ],
                );
                RelayResult {
                    status: a.status,
                    content_type: a.content_type,
                    body: a.body,
                    error: String::new(),
                }
            }
        }
    }
}

#[cfg(test)]
mod worker_tests;
