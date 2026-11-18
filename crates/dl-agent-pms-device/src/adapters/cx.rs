//! The call context of one adapter verb (the C# linked CancellationToken: CommandTimeout
//! plus the service stop).

use std::time::{Duration, Instant};

use crate::agent::StopToken;
use crate::contracts::AdapterError;

/// Code of [`Cx::cancelled`]: the C# `OperationCanceledException`. Never sent to the API:
/// the processor turns it into DEVICE_TIMEOUT, or into a service stop.
pub const CANCELLED: &str = "OPERATION_CANCELED";

/// Deadline and stop flag of one adapter call. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Cx {
    deadline: Instant,
    stop: StopToken,
}

impl Cx {
    /// A context that ends `timeout` from now or when `stop` is set.
    pub fn new(timeout: Duration, stop: StopToken) -> Cx {
        Cx::with_deadline(far_after(Instant::now(), timeout), stop)
    }

    /// A context that ends at `deadline` or when `stop` is set.
    pub fn with_deadline(deadline: Instant, stop: StopToken) -> Cx {
        Cx { deadline, stop }
    }

    /// A context without a practical deadline and never stopped (tests, tools).
    pub fn unbounded() -> Cx {
        Cx::new(Duration::from_secs(365 * 24 * 3600), StopToken::new())
    }

    /// When the call times out.
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// The service stop flag.
    pub fn stop_token(&self) -> &StopToken {
        &self.stop
    }

    /// True when the service is stopping.
    pub fn is_stopping(&self) -> bool {
        self.stop.is_stopped()
    }

    /// True when the call must end: deadline passed or service stopping
    /// (`ct.IsCancellationRequested`).
    pub fn is_cancelled(&self) -> bool {
        self.is_stopping() || Instant::now() >= self.deadline
    }

    /// Time left before the deadline; zero once cancelled. Use it to bound blocking I/O.
    pub fn remaining(&self) -> Duration {
        if self.is_stopping() {
            return Duration::ZERO;
        }
        self.deadline.saturating_duration_since(Instant::now())
    }

    /// `ct.ThrowIfCancellationRequested()`.
    pub fn check(&self) -> Result<(), AdapterError> {
        if self.is_cancelled() {
            Err(Cx::cancelled())
        } else {
            Ok(())
        }
    }

    /// `Task.Delay(duration, ct)`: waits `duration`, or fails with [`Cx::cancelled`] as soon
    /// as the call is cancelled.
    pub fn sleep(&self, duration: Duration) -> Result<(), AdapterError> {
        self.check()?;
        let left = self.remaining();
        if duration < left {
            if self.stop.wait(duration) {
                return Err(Cx::cancelled());
            }
            return Ok(());
        }
        self.stop.wait(left);
        // Instant granularity: make sure the deadline has really passed.
        while !self.is_cancelled() {
            self.stop.wait(Duration::from_millis(1));
        }
        Err(Cx::cancelled())
    }

    /// The cancellation refusal (code [`CANCELLED`], .NET message).
    pub fn cancelled() -> AdapterError {
        AdapterError::new(CANCELLED, "The operation was canceled.")
    }

    /// True for an error made by [`Cx::cancelled`].
    pub fn is_cancellation(error: &AdapterError) -> bool {
        error.code == CANCELLED
    }
}

/// `start + d`, or a day later when that overflows `Instant`.
fn far_after(start: Instant, d: Duration) -> Instant {
    start
        .checked_add(d)
        .or_else(|| start.checked_add(Duration::from_secs(24 * 3600)))
        .unwrap_or(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_and_stop_cancel() {
        let cx = Cx::new(Duration::from_millis(30), StopToken::new());
        assert!(!cx.is_cancelled() && cx.remaining() > Duration::ZERO);
        assert!(cx.check().is_ok());
        let e = cx.sleep(Duration::from_secs(5)).unwrap_err();
        assert!(Cx::is_cancellation(&e) && cx.is_cancelled());
        assert_eq!(cx.remaining(), Duration::ZERO);

        let stop = StopToken::new();
        let cx = Cx::new(Duration::from_secs(60), stop.clone());
        assert!(cx.sleep(Duration::from_millis(1)).is_ok());
        stop.stop();
        assert!(cx.is_stopping() && cx.check().is_err());
        assert_eq!(cx.remaining(), Duration::ZERO);
        assert!(!Cx::unbounded().is_cancelled());
    }
}
