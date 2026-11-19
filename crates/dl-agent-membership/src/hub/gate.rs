//! `HubTokenRejectionGate`: once the hub answers 401 with
//! `X-MM-Agent-Auth-Error: AGENT_TOKEN_REJECTED` (the token was replaced by a newer
//! enrolment or revoked), every call fails fast without touching the network, except one
//! probe per retry interval. The first 2xx afterwards opens the gate again.
//!
//! A 401 without the header (older hub) does not close the gate; the caller sees it as a
//! plain status error, as in .NET.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The default probe interval (`AgentUpdate`-independent, ADR-0037 addendum).
pub const DEFAULT_RETRY: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Default)]
struct State {
    rejected_since: Option<Instant>,
    next_probe_at: Option<Instant>,
}

/// The token gate, shared by every call of one hub client.
#[derive(Debug)]
pub struct TokenGate {
    retry: Duration,
    state: Mutex<State>,
}

impl Default for TokenGate {
    fn default() -> TokenGate {
        TokenGate::new(DEFAULT_RETRY)
    }
}

impl TokenGate {
    /// A gate that probes every `retry` while closed (zero means the default).
    pub fn new(retry: Duration) -> TokenGate {
        TokenGate {
            retry: if retry.is_zero() {
                DEFAULT_RETRY
            } else {
                retry
            },
            state: Mutex::new(State::default()),
        }
    }

    /// The probe interval.
    pub fn retry_interval(&self) -> Duration {
        self.retry
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether the hub rejected the token and no call has succeeded since.
    pub fn is_rejected(&self) -> bool {
        self.lock().rejected_since.is_some()
    }

    /// Whether a call may go out now. While closed, one call per interval is let through.
    pub fn try_enter(&self) -> bool {
        let mut s = self.lock();
        if s.rejected_since.is_none() {
            return true;
        }
        let now = Instant::now();
        if s.next_probe_at.is_some_and(|at| now < at) {
            return false;
        }
        s.next_probe_at = Some(now + self.retry);
        true
    }

    /// Records a token rejection. Returns `true` on the first one (worth one report).
    pub fn record_rejected(&self) -> bool {
        let mut s = self.lock();
        let now = Instant::now();
        s.next_probe_at = Some(now + self.retry);
        if s.rejected_since.is_some() {
            return false;
        }
        s.rejected_since = Some(now);
        true
    }

    /// Records a 2xx. Returns `true` when this re-opened a closed gate.
    pub fn record_accepted(&self) -> bool {
        self.lock().rejected_since.take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_gate_lets_one_probe_through_per_interval() {
        let g = TokenGate::new(Duration::from_millis(40));
        assert!(g.try_enter() && g.try_enter(), "open gate never blocks");
        assert!(g.record_rejected());
        assert!(!g.record_rejected(), "second rejection stays quiet");
        assert!(g.is_rejected());
        assert!(!g.try_enter(), "blocked until the interval passes");
        std::thread::sleep(Duration::from_millis(50));
        assert!(g.try_enter(), "one probe");
        assert!(!g.try_enter(), "only one");
        assert!(g.record_accepted());
        assert!(!g.is_rejected());
        assert!(g.try_enter());
        assert!(!g.record_accepted());
    }
}
