//! Purges the request-key log MM_OpLog (migration 531): rows older than the retention
//! window go, one committed batch per call of `MM_Op_DeleteExpiredLog`, repeated while a
//! call returns a full batch. Counts only reach the log; a failure is logged by kind and
//! retried on the next run.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dl_tds::Param;

use crate::attrs;
use crate::ctx::Ctx;
use crate::db::{DbError, Pool};
use crate::log::Logger;

/// The documented idempotency window.
pub const DEFAULT_RETAIN_DAYS: i64 = 30;
/// A batch below SQL Server lock escalation.
pub const DEFAULT_BATCH: i64 = 1000;
/// Batches per run.
pub const DEFAULT_MAX_BATCHES: u32 = 200;
/// Between runs.
pub const DEFAULT_EVERY: Duration = Duration::from_secs(6 * 3600);
/// Before the first run.
pub const DEFAULT_FIRST_AFTER: Duration = Duration::from_secs(300);

const PROC_DELETE_EXPIRED: &str = "MM_Op_DeleteExpiredLog";

/// Deletes at most `batch` expired rows and says how many went.
pub trait Store: Send + Sync {
    /// One batch.
    fn delete_expired(&self, ctx: &Ctx, retain_days: i64, batch: i64) -> Result<i64, DbError>;
}

/// Calls MM_Op_DeleteExpiredLog on the MM_PMS pool, outside any transaction.
pub struct SqlStore {
    /// The MM_PMS pool.
    pub pool: Arc<Pool>,
}

impl Store for SqlStore {
    fn delete_expired(&self, ctx: &Ctx, retain_days: i64, batch: i64) -> Result<i64, DbError> {
        let mut c = self.pool.get(ctx)?;
        let r = c.call(
            ctx,
            PROC_DELETE_EXPIRED,
            &[
                Param::new("RetainDays", retain_days),
                Param::new("BatchSize", batch),
            ],
        )?;
        let row = r
            .rows()
            .first()
            .ok_or(DbError::Tds(dl_tds::Error::ColumnNotFound(
                "deleted".into(),
            )))?;
        Ok(row.get::<i64>(0)?)
    }
}

/// Runs the purge now and then.
pub struct Purger {
    /// Where rows are deleted.
    pub store: Box<dyn Store>,
    /// Logger.
    pub log: Logger,
    /// Retention window.
    pub retain_days: i64,
    /// Rows per call.
    pub batch: i64,
    /// Calls per run.
    pub max_batches: u32,
    /// Between runs.
    pub every: Duration,
    /// Before the first run.
    pub first_after: Duration,
}

impl Purger {
    /// A purger with the defaults.
    pub fn new(store: Box<dyn Store>, log: Logger) -> Purger {
        Purger {
            store,
            log,
            retain_days: DEFAULT_RETAIN_DAYS,
            batch: DEFAULT_BATCH,
            max_batches: DEFAULT_MAX_BATCHES,
            every: DEFAULT_EVERY,
            first_after: DEFAULT_FIRST_AFTER,
        }
    }

    /// Deletes batches until one is short or `max_batches` ran; the rows deleted so far
    /// come back with the error.
    pub fn run_once(&self, ctx: &Ctx) -> (i64, Option<DbError>) {
        let mut total = 0;
        for _ in 0..self.max_batches {
            match self.store.delete_expired(ctx, self.retain_days, self.batch) {
                Ok(n) => {
                    total += n;
                    if n < self.batch {
                        break;
                    }
                }
                Err(e) => return (total, Some(e)),
            }
        }
        (total, None)
    }

    /// Blocks until `ctx` ends: first run after `first_after`, then every `every`.
    pub fn run(&self, ctx: &Ctx) {
        let mut wait = self.first_after;
        loop {
            if !ctx.sleep(wait) {
                return;
            }
            let started = Instant::now();
            let (n, err) = self.run_once(ctx);
            match err {
                Some(_) if ctx.is_done() => return,
                Some(e) => self.log.warn(
                    "agent.oplog.purge_failed",
                    attrs!["deleted" => n, "kind" => err_kind(&e)],
                ),
                None if n > 0 => self.log.info(
                    "agent.oplog.purged",
                    attrs![
                        "deleted" => n,
                        "retain_days" => self.retain_days,
                        "ms" => i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX)
                    ],
                ),
                None => {}
            }
            wait = self.every;
        }
    }
}

/// A failure named without its text (a driver message can carry SQL).
pub fn err_kind(err: &DbError) -> &'static str {
    match err {
        DbError::Tds(dl_tds::Error::Server { number: 1222, .. }) => "lock_timeout",
        DbError::Tds(dl_tds::Error::Server { number: 2812, .. }) => "proc_missing",
        DbError::Tds(dl_tds::Error::Server { .. }) => "sql_error",
        DbError::Tds(dl_tds::Error::Timeout)
        | DbError::Done(crate::ctx::Done::DeadlineExceeded) => "timeout",
        _ => "error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Fake(Mutex<Vec<Result<i64, DbError>>>);

    impl Store for Fake {
        fn delete_expired(&self, _: &Ctx, _: i64, _: i64) -> Result<i64, DbError> {
            self.0.lock().unwrap().remove(0)
        }
    }

    #[test]
    fn batches_until_short_and_reports_partial_counts() {
        let p = Purger {
            batch: 10,
            ..Purger::new(
                Box::new(Fake(Mutex::new(vec![Ok(10), Ok(10), Ok(3)]))),
                Logger::discard(),
            )
        };
        let (n, e) = p.run_once(&Ctx::background());
        assert_eq!((n, e.is_none()), (23, true));
        let failing = Purger {
            batch: 10,
            ..Purger::new(
                Box::new(Fake(Mutex::new(vec![
                    Ok(10),
                    Err(DbError::Tds(dl_tds::Error::Timeout)),
                ]))),
                Logger::discard(),
            )
        };
        let (n, e) = failing.run_once(&Ctx::background());
        assert_eq!((n, err_kind(&e.unwrap())), (10, "timeout"));
    }
}
