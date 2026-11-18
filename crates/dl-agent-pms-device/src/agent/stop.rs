//! The service stop signal (the C# `stoppingToken`), shared by the worker, the reporter and
//! every adapter call ([`crate::adapters::Cx`]).

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// A cloneable stop flag that sleeping threads can wait on. Once set it stays set.
#[derive(Debug, Clone, Default)]
pub struct StopToken {
    inner: Arc<(Mutex<bool>, Condvar)>,
}

impl StopToken {
    /// A token that is not stopped.
    pub fn new() -> StopToken {
        StopToken::default()
    }

    /// Requests the stop and wakes every [`StopToken::wait`].
    pub fn stop(&self) {
        let (flag, cv) = &*self.inner;
        *flag.lock().unwrap_or_else(PoisonError::into_inner) = true;
        cv.notify_all();
    }

    /// True once [`StopToken::stop`] was called.
    pub fn is_stopped(&self) -> bool {
        *self.inner.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits up to `timeout` for the stop; true when stopped.
    pub fn wait(&self, timeout: Duration) -> bool {
        let (flag, cv) = &*self.inner;
        let mut stopped = flag.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(until) = Instant::now().checked_add(timeout) else {
            // An "infinite" wait: wait until stopped.
            while !*stopped {
                stopped = cv.wait(stopped).unwrap_or_else(PoisonError::into_inner);
            }
            return true;
        };
        while !*stopped {
            let now = Instant::now();
            if now >= until {
                break;
            }
            stopped = cv
                .wait_timeout(stopped, until - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        *stopped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_returns_early_on_stop() {
        let t = StopToken::new();
        assert!(!t.wait(Duration::from_millis(5)));
        let t2 = t.clone();
        let h = std::thread::spawn(move || t2.wait(Duration::from_secs(30)));
        std::thread::sleep(Duration::from_millis(20));
        t.stop();
        assert!(h.join().unwrap());
        assert!(t.is_stopped() && t.wait(Duration::ZERO));
    }
}
