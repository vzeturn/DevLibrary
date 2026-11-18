//! Cancellation and deadlines, the part of Go's `context.Context` the agent uses: a tree
//! of tokens where cancelling a parent cancels every child, an optional deadline per
//! token (never later than the parent's), interruptible sleeps and on-cancel hooks that
//! unblock I/O waiting in another thread.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

type Hook = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct State {
    cancelled: bool,
    children: Vec<Weak<Node>>,
    hooks: Vec<Hook>,
}

struct Node {
    state: Mutex<State>,
    cv: Condvar,
    deadline: Option<Instant>,
}

impl Node {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Why a context is done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Done {
    /// [`Ctx::cancel`] (here or on a parent).
    Cancelled,
    /// The deadline passed.
    DeadlineExceeded,
}

impl std::fmt::Display for Done {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Done::Cancelled => "context canceled",
            Done::DeadlineExceeded => "context deadline exceeded",
        })
    }
}

impl std::error::Error for Done {}

/// A cancellation token; clones share it.
#[derive(Clone)]
pub struct Ctx(Arc<Node>);

impl std::fmt::Debug for Ctx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ctx")
            .field("done", &self.err())
            .field("deadline", &self.0.deadline)
            .finish()
    }
}

impl Ctx {
    /// A root that is never done until cancelled.
    pub fn background() -> Ctx {
        Ctx(Arc::new(Node {
            state: Mutex::new(State::default()),
            cv: Condvar::new(),
            deadline: None,
        }))
    }

    fn derive(&self, deadline: Option<Instant>) -> Ctx {
        let deadline = match (self.0.deadline, deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let child = Ctx(Arc::new(Node {
            state: Mutex::new(State::default()),
            cv: Condvar::new(),
            deadline,
        }));
        let mut st = self.0.lock();
        if st.cancelled {
            drop(st);
            child.cancel();
        } else {
            st.children.retain(|w| w.strong_count() > 0);
            st.children.push(Arc::downgrade(&child.0));
        }
        child
    }

    /// A child cancelled with this one (or on its own).
    pub fn child(&self) -> Ctx {
        self.derive(None)
    }

    /// A child that is also done after `d`.
    pub fn with_timeout(&self, d: Duration) -> Ctx {
        self.derive(Instant::now().checked_add(d))
    }

    /// Cancels this context and every child; runs the on-cancel hooks once.
    pub fn cancel(&self) {
        let (children, hooks) = {
            let mut st = self.0.lock();
            if st.cancelled {
                return;
            }
            st.cancelled = true;
            (
                std::mem::take(&mut st.children),
                std::mem::take(&mut st.hooks),
            )
        };
        self.0.cv.notify_all();
        for h in hooks {
            h();
        }
        for c in children.iter().filter_map(Weak::upgrade) {
            Ctx(c).cancel();
        }
    }

    /// Runs `f` when the context is cancelled (at once if it already is). Deadlines do
    /// not run hooks: code that waits on a deadline polls [`Ctx::err`].
    pub fn on_cancel(&self, f: impl FnOnce() + Send + 'static) {
        let mut st = self.0.lock();
        if st.cancelled {
            drop(st);
            f();
        } else {
            st.hooks.push(Box::new(f));
        }
    }

    /// `None` while live, otherwise why it is done.
    pub fn err(&self) -> Option<Done> {
        if self.0.lock().cancelled {
            return Some(Done::Cancelled);
        }
        match self.0.deadline {
            Some(d) if Instant::now() >= d => Some(Done::DeadlineExceeded),
            _ => None,
        }
    }

    /// Whether the context is cancelled or past its deadline.
    pub fn is_done(&self) -> bool {
        self.err().is_some()
    }

    /// The deadline, if any.
    pub fn deadline(&self) -> Option<Instant> {
        self.0.deadline
    }

    /// Time left before the deadline (`None` without one).
    pub fn remaining(&self) -> Option<Duration> {
        self.0
            .deadline
            .map(|d| d.saturating_duration_since(Instant::now()))
    }

    /// Sleeps for `d` or until the context is done; `true` when the full time passed.
    pub fn sleep(&self, d: Duration) -> bool {
        let Some(until) = Instant::now().checked_add(d) else {
            self.wait_done();
            return false;
        };
        let until = self.0.deadline.map_or(until, |dl| dl.min(until));
        let mut st = self.0.lock();
        loop {
            if st.cancelled {
                return false;
            }
            let now = Instant::now();
            if now >= until {
                return self.0.deadline.is_none_or(|dl| now < dl);
            }
            st = self
                .0
                .cv
                .wait_timeout(st, until - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Blocks until the context is done.
    pub fn wait_done(&self) {
        let mut st = self.0.lock();
        loop {
            if st.cancelled {
                return;
            }
            match self.0.deadline {
                Some(dl) => {
                    let now = Instant::now();
                    if now >= dl {
                        return;
                    }
                    st = self
                        .0
                        .cv
                        .wait_timeout(st, dl - now)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
                None => st = self.0.cv.wait(st).unwrap_or_else(PoisonError::into_inner),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn cancel_propagates_and_wakes_sleepers() {
        let root = Ctx::background();
        let child = root.child();
        let grandchild = child.with_timeout(Duration::from_secs(60));
        let hook = Arc::new(AtomicBool::new(false));
        let h = hook.clone();
        grandchild.on_cancel(move || h.store(true, Ordering::SeqCst));
        let g = grandchild.clone();
        let t = std::thread::spawn(move || g.sleep(Duration::from_secs(60)));
        std::thread::sleep(Duration::from_millis(50));
        root.cancel();
        assert!(!t.join().unwrap());
        assert_eq!(grandchild.err(), Some(Done::Cancelled));
        assert!(hook.load(Ordering::SeqCst));
        // A child of a cancelled context is born cancelled.
        assert!(root.child().is_done());
    }

    #[test]
    fn deadlines_are_inherited_and_end_sleeps() {
        let c = Ctx::background().with_timeout(Duration::from_millis(30));
        let inner = c.with_timeout(Duration::from_secs(60));
        assert!(inner.remaining().unwrap() <= Duration::from_millis(30));
        assert!(!inner.sleep(Duration::from_secs(5)));
        assert_eq!(inner.err(), Some(Done::DeadlineExceeded));
        assert!(Ctx::background().sleep(Duration::from_millis(1)));
    }
}
