//! The runner's side of [`CaptureNudge`]: an order command committed, so the scheduled
//! session reads the open checks now (ADR 0104 §4) and, for a known check, re-reads that
//! check whatever its signature says (RT-1309).

use std::sync::atomic::Ordering;

use super::{SqlCaptureRunner, lock};
use crate::capture::nudge::CaptureNudge;
use crate::capture::scheduler::CaptureScheduler;

impl SqlCaptureRunner {
    /// Two open-job runs pulse the uploader: the run in flight may have read before the
    /// commit. Outside a scheduled session there is nothing to hurry.
    fn nudge_with(&self, nudge: impl FnOnce(&CaptureScheduler)) {
        self.0.open_nudges.store(2, Ordering::SeqCst);
        let scheduler = lock(&self.0.scheduler).clone();
        if let Some(s) = scheduler {
            nudge(&s);
        }
    }
}

impl CaptureNudge for SqlCaptureRunner {
    fn nudge_open_checks(&self) {
        self.nudge_with(CaptureScheduler::nudge_open_checks);
    }

    fn nudge_open_check(&self, check_key: &str) {
        self.nudge_with(|s| s.nudge_open_check(check_key));
    }
}
