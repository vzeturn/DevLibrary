//! The uplink queues of `CloudLinkWorker`: answers and change notices wait here for the next
//! POST to `api/agent/results`.
//!
//! The first item after a quiet spell goes out at once; items that arrive while a POST is in
//! flight, or within the window after it, ride the next one together ([`Pacer`]). Answers
//! are never dropped by the queue; change notices are capped and the oldest give way, since a
//! newer notice for the same watch supersedes them.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use dl_winsvc::StopSignal;

/// How often a waiting uplink re-checks the stop signal.
const STOP_CHECK: Duration = Duration::from_millis(200);

/// Answers per POST and the coalescing window (`ResultBatch*` of the .NET agent).
pub const RESULTS: Batching = Batching {
    max: 50,
    window: Duration::from_millis(50),
};
/// Change notices per POST and their window.
pub const NOTICES: Batching = Batching {
    max: 200,
    window: Duration::from_millis(250),
};
/// Change notices kept while the cloud is unreachable.
pub const NOTICE_CAPACITY: usize = 500;

/// Batch size and spacing of one uplink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Batching {
    /// Items per POST.
    pub max: usize,
    /// Least time between two POSTs.
    pub window: Duration,
}

/// A FIFO queue the uplink thread blocks on.
#[derive(Debug)]
pub struct Outbox<T> {
    items: Mutex<VecDeque<T>>,
    ready: Condvar,
    capacity: Option<usize>,
}

impl<T> Outbox<T> {
    /// A queue; `capacity` bounds it by dropping the oldest item.
    pub fn new(capacity: Option<usize>) -> Outbox<T> {
        Outbox {
            items: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
            capacity,
        }
    }

    /// Queues `item`; `true` when an older item was dropped to make room.
    pub fn push(&self, item: T) -> bool {
        let mut q = self.items.lock().unwrap_or_else(PoisonError::into_inner);
        let dropped = match self.capacity {
            Some(cap) if q.len() >= cap => q.pop_front().is_some(),
            _ => false,
        };
        q.push_back(item);
        drop(q);
        self.ready.notify_one();
        dropped
    }

    /// Items waiting.
    pub fn len(&self) -> usize {
        self.items
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Whether nothing waits.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Blocks until an item waits (`true`) or `stop` is set while the queue is empty
    /// (`false`).
    pub fn wait(&self, stop: &StopSignal) -> bool {
        let mut q = self.items.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if !q.is_empty() {
                return true;
            }
            if stop.is_stopped() {
                return false;
            }
            q = self
                .ready
                .wait_timeout(q, STOP_CHECK)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Takes up to `max` items, oldest first.
    pub fn drain(&self, max: usize) -> Vec<T> {
        let mut q = self.items.lock().unwrap_or_else(PoisonError::into_inner);
        let n = max.min(q.len());
        q.drain(..n).collect()
    }
}

/// Spaces POSTs of one uplink by its window.
#[derive(Debug)]
pub struct Pacer {
    window: Duration,
    last_post: Option<Instant>,
}

impl Pacer {
    /// A pacer that has not posted yet.
    pub fn new(window: Duration) -> Pacer {
        Pacer {
            window,
            last_post: None,
        }
    }

    /// How long to wait before the next POST (zero after a quiet spell).
    pub fn wait_before_post(&self, now: Instant) -> Duration {
        match self.last_post {
            Some(at) => self
                .window
                .saturating_sub(now.saturating_duration_since(at)),
            None => Duration::ZERO,
        }
    }

    /// Records a POST that ended at `now`.
    pub fn posted(&mut self, now: Instant) {
        self.last_post = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_drop_the_oldest_when_full_and_answers_never_drop() {
        let notices = Outbox::new(Some(2));
        assert!(!notices.push(1));
        assert!(!notices.push(2));
        assert!(notices.push(3));
        assert_eq!(notices.drain(10), vec![2, 3]);

        let answers = Outbox::new(None);
        for i in 0..1000 {
            assert!(!answers.push(i));
        }
        assert_eq!(answers.drain(50).len(), 50);
        assert_eq!(answers.len(), 950);
    }

    #[test]
    fn wait_returns_on_an_item_or_on_stop() {
        let q: Outbox<i32> = Outbox::new(None);
        let stop = StopSignal::new();
        stop.stop();
        assert!(!q.wait(&stop));
        q.push(1);
        assert!(q.wait(&stop));
    }

    #[test]
    fn the_first_post_after_a_quiet_spell_goes_at_once() {
        let mut p = Pacer::new(Duration::from_millis(250));
        let t0 = Instant::now();
        assert_eq!(p.wait_before_post(t0), Duration::ZERO);
        p.posted(t0);
        assert_eq!(
            p.wait_before_post(t0 + Duration::from_millis(100)),
            Duration::from_millis(150)
        );
        assert_eq!(
            p.wait_before_post(t0 + Duration::from_secs(1)),
            Duration::ZERO
        );
    }
}
