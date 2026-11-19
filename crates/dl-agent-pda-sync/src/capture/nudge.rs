//! Read-your-writes nudges (port of the agent's `Capture/CaptureNudge.cs`, ADR 0104 §4).

use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::util::ctx::Ctx;

/// Asks capture to read the open checks now instead of at its next tick: an applied order
/// verb calls it so the cloud mirror catches up within the cloud's short wait
/// (`ICaptureNudge`).
pub trait CaptureNudge: Send + Sync {
    /// Makes the open-check job due now.
    fn nudge_open_checks(&self);
}

/// Wakes the uploader out of its idle wait once capture has queued the rows a nudge asked
/// for. Pulses coalesce: a wake nobody waits for yet is kept (one), never counted
/// (`UploadWake`, a `SemaphoreSlim(0, 1)`).
#[derive(Debug, Default)]
pub struct UploadWake {
    signal: Mutex<bool>,
    cv: Condvar,
}

/// How often a wait re-checks its context for cancellation.
const CANCEL_POLL: Duration = Duration::from_millis(50);

impl UploadWake {
    /// A wake with no pulse pending.
    pub fn new() -> UploadWake {
        UploadWake::default()
    }

    /// Records one pulse (kept until a wait takes it).
    pub fn pulse(&self) {
        *self.signal.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.cv.notify_one();
    }

    /// Waits up to `max`; true when a pulse ended the wait early. A cancelled `ctx` ends the
    /// wait with false (C#: `OperationCanceledException`).
    pub fn wait(&self, max: Duration, ctx: &Ctx) -> bool {
        let end = Instant::now().checked_add(max);
        let mut set = self.signal.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if *set {
                *set = false;
                return true;
            }
            if ctx.is_done() {
                return false;
            }
            let now = Instant::now();
            let left = match end {
                Some(e) if now >= e => return false,
                Some(e) => e - now,
                None => CANCEL_POLL,
            };
            set = self
                .cv
                .wait_timeout(set, left.min(CANCEL_POLL))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}
