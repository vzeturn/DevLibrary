//! One dedicated COM single-threaded-apartment thread. dl-winos COM objects are not `Send`:
//! each job creates, uses and drops its objects on this thread.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, PoisonError};

type Job = Box<dyn FnOnce() + Send>;

/// A lazily started STA thread that runs jobs one at a time.
pub(super) struct StaThread {
    name: &'static str,
    jobs: Mutex<Option<Sender<Job>>>,
}

impl StaThread {
    /// A thread named `name`, started on the first job.
    pub(super) fn new(name: &'static str) -> StaThread {
        StaThread {
            name,
            jobs: Mutex::new(None),
        }
    }

    /// Runs `job` on the thread and waits for its result; `None` when the thread could not
    /// start or the job panicked.
    pub(super) fn run<R: Send + 'static>(
        &self,
        job: impl FnOnce() -> R + Send + 'static,
    ) -> Option<R> {
        let (done, result) = mpsc::channel();
        let job: Job = Box::new(move || {
            let _ = done.send(job());
        });
        let mut pending = Some(job);
        {
            let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
            // A thread that ended (it never does, unless COM aborted it) is started again once.
            for _ in 0..2 {
                let Some(job) = pending.take() else { break };
                if jobs.is_none() {
                    *jobs = spawn(self.name);
                }
                let sender = jobs.as_ref()?;
                if let Err(mpsc::SendError(back)) = sender.send(job) {
                    pending = Some(back);
                    *jobs = None;
                }
            }
        }
        // Not sent: dropping it drops the result sender, so `recv` fails.
        drop(pending);
        result.recv().ok()
    }
}

fn spawn(name: &str) -> Option<Sender<Job>> {
    let (tx, rx) = mpsc::channel::<Job>();
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            // Kept for the thread's life; when it fails, Object::create reports the error.
            let _apartment = dl_winos::com::init();
            for job in rx {
                // A panicking job drops its result sender: the caller gets None.
                let _ = catch_unwind(AssertUnwindSafe(job));
            }
        })
        .ok()?;
    Some(tx)
}
