//! A bounded worker pool that keeps one key's jobs in order (RT-1318): commands on
//! different aggregates (`check:{rvc}:{check}`, `fo-inhouse:...`) run side by side on at
//! most `workers` threads, while the commands of one aggregate run strictly one after the
//! other, in the order they were submitted. A busy kitchen sending many lines of many
//! checks is no longer held behind one check's SQL round trip.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

/// Jobs waiting per key, the keys whose next job may start, the keys running now.
struct State<J> {
    queues: HashMap<String, VecDeque<J>>,
    runnable: VecDeque<String>,
    busy: HashSet<String>,
    closed: bool,
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
                closed: false,
            }),
            wake: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State<J>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues `job` behind the earlier jobs of `key`.
    pub fn submit(&self, key: &str, job: J) {
        let mut s = self.lock();
        let queue = s.queues.entry(key.to_owned()).or_default();
        queue.push_back(job);
        // A key gets one runnable slot while it has jobs and none of them runs.
        if queue.len() == 1 && !s.busy.contains(key) {
            s.runnable.push_back(key.to_owned());
            self.wake.notify_one();
        }
    }

    /// Jobs queued or running.
    pub fn in_flight(&self) -> usize {
        let s = self.lock();
        s.busy.len() + s.queues.values().map(VecDeque::len).sum::<usize>()
    }

    /// The next job and its key; `None` once closed and drained.
    fn take(&self) -> Option<(String, J)> {
        let mut s = self.lock();
        loop {
            if let Some(key) = s.runnable.pop_front() {
                let job = s.queues.get_mut(&key).and_then(VecDeque::pop_front);
                if let Some(job) = job {
                    s.busy.insert(key.clone());
                    return Some((key, job));
                }
                s.queues.remove(&key);
                continue;
            }
            if s.closed && s.busy.is_empty() {
                return None;
            }
            s = self.wake.wait(s).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// `key`'s job ended: its next job, if any, may start.
    fn done(&self, key: String) {
        let mut s = self.lock();
        s.busy.remove(&key);
        let more = s.queues.get(&key).is_some_and(|q| !q.is_empty());
        if more {
            s.runnable.push_back(key);
        } else {
            s.queues.remove(&key);
        }
        // Wake a worker for the key's next job, and the idle ones to see a drained close.
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
pub fn run<J, R>(
    workers: usize,
    run: impl Fn(J) + Sync,
    body: impl FnOnce(&KeyedQueue<J>) -> R,
) -> R
where
    J: Send,
{
    let queue = KeyedQueue::new();
    std::thread::scope(|scope| {
        for _ in 0..workers.max(1) {
            scope.spawn(|| {
                while let Some((key, job)) = queue.take() {
                    run(job);
                    queue.done(key);
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn one_key_runs_in_order_and_keys_run_side_by_side() {
        let log = Mutex::new(Vec::new());
        let running = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        run(
            4,
            |(key, n): (String, usize)| {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(5));
                log.lock().unwrap().push((key, n));
                running.fetch_sub(1, Ordering::SeqCst);
            },
            |q| {
                for n in 0..10 {
                    for key in ["a", "b", "c", "d", "e"] {
                        q.submit(key, (key.to_owned(), n));
                    }
                }
            },
        );
        let log = log.into_inner().unwrap();
        assert_eq!(log.len(), 50, "every queued job ran before run returned");
        for key in ["a", "b", "c", "d", "e"] {
            let order: Vec<usize> = log
                .iter()
                .filter(|(k, _)| k == key)
                .map(|(_, n)| *n)
                .collect();
            assert_eq!(
                order,
                (0..10).collect::<Vec<_>>(),
                "key {key} kept its order"
            );
        }
        let peak = peak.load(Ordering::SeqCst);
        assert!(peak > 1 && peak <= 4, "bounded parallelism, got {peak}");
    }

    #[test]
    fn one_key_never_runs_twice_at_once() {
        let running = AtomicUsize::new(0);
        run(
            8,
            |()| {
                assert_eq!(running.fetch_add(1, Ordering::SeqCst), 0);
                std::thread::sleep(Duration::from_millis(1));
                running.fetch_sub(1, Ordering::SeqCst);
            },
            |q| {
                for _ in 0..20 {
                    q.submit("check:1:7", ());
                }
            },
        );
    }
}
