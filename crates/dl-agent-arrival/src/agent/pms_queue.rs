//! The PMS lane's queue (RT-0755): interactive requests (reception, guests) always go first, and
//! background requests (cache warming) hold at most [`background_slots`] of the
//! `MaxConcurrency` PMS threads, so an interactive request never waits behind the warmer.
//!
//! The .NET agent does the same with a second semaphore taken before the PMS one; here the
//! lane threads pick from this queue, which only hands out a background request while fewer
//! than [`background_slots`] of them run.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use dl_winsvc::StopSignal;

use crate::contracts::PmsRequest;

/// How often a waiting lane re-checks the stop signal.
const STOP_CHECK: Duration = Duration::from_millis(200);

/// A request and when the agent received it (the start of its queue time and deadline).
#[derive(Debug, Clone)]
pub struct Queued {
    /// The cloud's request.
    pub request: PmsRequest,
    /// When the poll answer carrying it arrived.
    pub received: Instant,
}

impl Queued {
    /// `request`, received now.
    pub fn now(request: PmsRequest) -> Queued {
        Queued {
            request,
            received: Instant::now(),
        }
    }
}

/// PMS slots background requests may hold at once: `min(2, max - 1)`, and 1 when there is only
/// one slot (then they share it, but an interactive request is still picked first).
pub fn background_slots(max_concurrency: usize) -> usize {
    max_concurrency.saturating_sub(1).clamp(1, 2)
}

#[derive(Debug, Default)]
struct State {
    interactive: VecDeque<Queued>,
    background: VecDeque<Queued>,
    background_running: usize,
}

/// The queue the PMS lane threads block on.
#[derive(Debug)]
pub struct PmsQueue {
    state: Mutex<State>,
    ready: Condvar,
    background_slots: usize,
}

/// A request taken from the queue; dropping it frees its background slot.
#[derive(Debug)]
pub struct Taken<'a> {
    /// The request.
    pub item: Queued,
    queue: &'a PmsQueue,
    background: bool,
}

impl Drop for Taken<'_> {
    fn drop(&mut self) {
        if self.background {
            self.queue.lock().background_running -= 1;
            self.queue.ready.notify_all();
        }
    }
}

impl PmsQueue {
    /// A queue for `max_concurrency` PMS threads.
    pub fn new(max_concurrency: usize) -> PmsQueue {
        PmsQueue {
            state: Mutex::new(State::default()),
            ready: Condvar::new(),
            background_slots: background_slots(max_concurrency),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues `item` behind the others of its kind.
    pub fn push(&self, item: Queued) {
        let mut s = self.lock();
        if item.request.background {
            s.background.push_back(item);
        } else {
            s.interactive.push_back(item);
        }
        drop(s);
        self.ready.notify_all();
    }

    /// Requests waiting.
    pub fn len(&self) -> usize {
        let s = self.lock();
        s.interactive.len() + s.background.len()
    }

    /// Whether nothing waits.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Removes every waiting request (the stop: the cloud asks again); returns how many.
    pub fn clear(&self) -> usize {
        let mut s = self.lock();
        let n = s.interactive.len() + s.background.len();
        s.interactive.clear();
        s.background.clear();
        n
    }

    /// Blocks until a request may run: the oldest interactive one, else the oldest background
    /// one while a background slot is free. `None` once `stop` is set.
    pub fn take(&self, stop: &StopSignal) -> Option<Taken<'_>> {
        let mut s = self.lock();
        loop {
            if stop.is_stopped() {
                return None;
            }
            if let Some(item) = s.interactive.pop_front() {
                return Some(Taken {
                    item,
                    queue: self,
                    background: false,
                });
            }
            if s.background_running < self.background_slots
                && let Some(item) = s.background.pop_front()
            {
                s.background_running += 1;
                return Some(Taken {
                    item,
                    queue: self,
                    background: true,
                });
            }
            s = self
                .ready
                .wait_timeout(s, STOP_CHECK)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(id: &str, background: bool) -> Queued {
        Queued::now(PmsRequest {
            request_id: id.into(),
            background,
            ..PmsRequest::default()
        })
    }

    #[test]
    fn background_never_takes_the_last_slot() {
        assert_eq!(background_slots(0), 1);
        assert_eq!(background_slots(1), 1);
        assert_eq!(background_slots(2), 1);
        assert_eq!(background_slots(3), 2);
        assert_eq!(background_slots(4), 2);
        assert_eq!(background_slots(16), 2);
    }

    #[test]
    fn interactive_requests_jump_the_background_queue() {
        let q = PmsQueue::new(4);
        let stop = StopSignal::new();
        q.push(req("b1", true));
        q.push(req("i1", false));
        q.push(req("b2", true));
        q.push(req("i2", false));
        let order: Vec<String> = (0..4)
            .map(|_| {
                let t = q.take(&stop).expect("item");
                let id = t.item.request.request_id.clone();
                drop(t);
                id
            })
            .collect();
        assert_eq!(order, ["i1", "i2", "b1", "b2"]);
        assert!(q.is_empty());
    }

    #[test]
    fn background_requests_wait_for_a_background_slot() {
        let q = PmsQueue::new(4);
        let stop = StopSignal::new();
        for i in 0..3 {
            q.push(req(&format!("b{i}"), true));
        }
        let first = q.take(&stop).expect("b0");
        let second = q.take(&stop).expect("b1");
        // Both background slots are busy: an interactive request still gets through at once…
        q.push(req("i", false));
        assert_eq!(q.take(&stop).expect("i").item.request.request_id, "i");
        // …while the third background request waits until one of them ends.
        std::thread::scope(|s| {
            let waiter = s.spawn(|| q.take(&stop).map(|t| t.item.request.request_id.clone()));
            std::thread::sleep(Duration::from_millis(100));
            assert!(!waiter.is_finished(), "a third background request ran");
            drop(first);
            assert_eq!(waiter.join().expect("join").as_deref(), Some("b2"));
        });
        drop(second);
    }

    #[test]
    fn a_stop_ends_the_wait() {
        let q = PmsQueue::new(1);
        let stop = StopSignal::new();
        std::thread::scope(|s| {
            let waiter = s.spawn(|| q.take(&stop).is_none());
            std::thread::sleep(Duration::from_millis(50));
            stop.stop();
            assert!(waiter.join().expect("join"));
        });
    }
}
