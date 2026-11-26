//! A bounded worker pool that keeps one key's jobs in order (RT-1318): commands on
//! different aggregates (`check:{rvc}:{check}`, `fo-inhouse:...`) run side by side on at
//! most `workers` threads, while the commands of one aggregate run strictly one after the
//! other, in the order they were submitted. A busy kitchen sending many lines of many
//! checks is no longer held behind one check's SQL round trip.
//!
//! A job that touches several keys (a check merge or item move, an `rvc:*` command) is
//! submitted with [`KeyedQueue::submit_alone`]: it waits until every job submitted before
//! it ended, runs with nothing beside it, and every job submitted after it waits for it,
//! so the arrival order of the socket holds across the keys it touches.

use std::any::Any;
use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

/// Jobs the socket loop may hold per worker before it stops reading the socket: a hot
/// check cannot pile up a backlog longer than the cloud's lease (RT-1318 review).
pub const INTAKE_PER_WORKER: usize = 4;

/// Jobs queued or running at which the submit side stops taking more.
pub fn intake_limit(workers: usize) -> usize {
    workers.max(1) * INTAKE_PER_WORKER
}

/// A job submitted behind a job that runs alone, in arrival order.
enum Held<J> {
    Keyed(String, J),
    Alone(J),
}

/// What a taken job holds until it ends.
enum Slot {
    Key(String),
    Alone,
}

/// Jobs waiting per key, the keys whose next job may start, the keys running now, and the
/// jobs held behind a job that runs alone.
struct State<J> {
    queues: HashMap<String, VecDeque<J>>,
    runnable: VecDeque<String>,
    busy: HashSet<String>,
    /// Empty, or starts with an `Alone` job (while none runs).
    held: VecDeque<Held<J>>,
    alone_running: bool,
    closed: bool,
}

impl<J> State<J> {
    fn queue_keyed(&mut self, key: String, job: J) -> bool {
        let len = {
            let queue = self.queues.entry(key.clone()).or_default();
            queue.push_back(job);
            queue.len()
        };
        // A key gets one runnable slot while it has jobs and none of them runs.
        let runnable = len == 1 && !self.busy.contains(&key);
        if runnable {
            self.runnable.push_back(key);
        }
        runnable
    }
}

/// The submit side of [`run`]: jobs queued here are run by the pool's workers.
pub struct KeyedQueue<J> {
    state: Mutex<State<J>>,
    wake: Condvar,
}

impl<J> KeyedQueue<J> {
    fn new() -> KeyedQueue<J> {
        KeyedQueue {
            state: Mutex::new(State {
                queues: HashMap::new(),
                runnable: VecDeque::new(),
                busy: HashSet::new(),
                held: VecDeque::new(),
                alone_running: false,
                closed: false,
            }),
            wake: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State<J>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues `job` behind the earlier jobs of `key` (and behind a job running alone).
    pub fn submit(&self, key: &str, job: J) {
        let mut s = self.lock();
        if s.alone_running || !s.held.is_empty() {
            s.held.push_back(Held::Keyed(key.to_owned(), job));
            return;
        }
        if s.queue_keyed(key.to_owned(), job) {
            self.wake.notify_one();
        }
    }

    /// Queues `job` to run alone: after every job submitted before it, with no other job
    /// beside it, and before every job submitted after it.
    pub fn submit_alone(&self, job: J) {
        self.lock().held.push_back(Held::Alone(job));
        self.wake.notify_all();
    }

    /// Jobs queued or running.
    pub fn in_flight(&self) -> usize {
        let s = self.lock();
        s.busy.len()
            + s.queues.values().map(VecDeque::len).sum::<usize>()
            + s.held.len()
            + usize::from(s.alone_running)
    }

    /// The next job and what it holds; `None` once closed and drained.
    fn take(&self) -> Option<(Slot, J)> {
        let mut s = self.lock();
        loop {
            if !s.alone_running {
                if let Some(key) = s.runnable.pop_front() {
                    let job = s.queues.get_mut(&key).and_then(VecDeque::pop_front);
                    if let Some(job) = job {
                        s.busy.insert(key.clone());
                        return Some((Slot::Key(key), job));
                    }
                    s.queues.remove(&key);
                    continue;
                }
                // Keyed jobs left are only those submitted before the alone job.
                if s.busy.is_empty()
                    && s.queues.is_empty()
                    && let Some(Held::Alone(_)) = s.held.front()
                    && let Some(Held::Alone(job)) = s.held.pop_front()
                {
                    s.alone_running = true;
                    return Some((Slot::Alone, job));
                }
            }
            if s.closed && s.busy.is_empty() && !s.alone_running && s.held.is_empty() {
                return None;
            }
            s = self.wake.wait(s).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// A job ended: its key's next job, or the jobs held behind a job that ran alone, may start.
    fn done(&self, slot: Slot) {
        let mut s = self.lock();
        match slot {
            Slot::Key(key) => {
                s.busy.remove(&key);
                let more = s.queues.get(&key).is_some_and(|q| !q.is_empty());
                if more {
                    s.runnable.push_back(key);
                } else {
                    s.queues.remove(&key);
                }
            }
            Slot::Alone => {
                s.alone_running = false;
                // Release the keyed jobs up to the next alone job, in arrival order.
                while let Some(Held::Keyed(..)) = s.held.front() {
                    if let Some(Held::Keyed(key, job)) = s.held.pop_front() {
                        s.queue_keyed(key, job);
                    }
                }
            }
        }
        // Wake workers for the next jobs, and the idle ones to see a drained close.
        self.wake.notify_all();
    }

    fn close(&self) {
        self.lock().closed = true;
        self.wake.notify_all();
    }
}

/// Runs `body` with a queue whose jobs `workers` threads (at least 1) pass to `run`.
/// When `body` returns, the jobs already queued still run to the end before this returns,
/// so no received command is dropped (its result is kept by the result outbox).
///
/// A job that panics still frees its key (its next jobs run) and its worker goes on with
/// the next job; the first such panic is raised again once the pool has drained.
pub fn run<J, R>(
    workers: usize,
    run: impl Fn(J) + Sync,
    body: impl FnOnce(&KeyedQueue<J>) -> R,
) -> R
where
    J: Send,
{
    let queue = KeyedQueue::new();
    let panicked: Mutex<Option<Box<dyn Any + Send>>> = Mutex::new(None);
    let out = std::thread::scope(|scope| {
        for _ in 0..workers.max(1) {
            scope.spawn(|| {
                while let Some((slot, job)) = queue.take() {
                    let outcome = catch_unwind(AssertUnwindSafe(|| run(job)));
                    queue.done(slot);
                    if let Err(panic) = outcome {
                        panicked
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .get_or_insert(panic);
                    }
                }
            });
        }
        // Closed even when `body` panics, so the scope's workers end and the panic surfaces.
        struct CloseOnDrop<'q, J>(&'q KeyedQueue<J>);
        impl<J> Drop for CloseOnDrop<'_, J> {
            fn drop(&mut self) {
                self.0.close();
            }
        }
        let _close = CloseOnDrop(&queue);
        body(&queue)
    });
    if let Some(panic) = panicked
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner)
    {
        resume_unwind(panic);
    }
    out
}

#[cfg(test)]
mod tests;
