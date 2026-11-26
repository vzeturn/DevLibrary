//! The uploader's backlog report (RT-1309): when the outbox stays deep, a warning names
//! the streams that fill it and the ones parked on a parent, so a site whose live checks
//! reach the cloud late shows what it is queued behind.

use std::time::Duration;

use super::batches::SqliteOutbox;
use super::error::UploadError;
use crate::config::codes;
use crate::contracts::time::DateTimeOffset;
use crate::reporting::{Logger, Value};

/// Depth from which a backlog is reported.
pub const BACKLOG_REPORT_DEPTH: i64 = 100;

/// Least time between two backlog reports.
pub const BACKLOG_REPORT_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Streams named in one report.
const STREAMS_NAMED: i32 = 8;

/// When the backlog was last looked at.
#[derive(Debug, Default)]
pub(super) struct BacklogReporter {
    last_check: Option<DateTimeOffset>,
}

impl BacklogReporter {
    /// Looks at the outbox at most once per [`BACKLOG_REPORT_INTERVAL`]; warns when
    /// [`BACKLOG_REPORT_DEPTH`] or more batches wait.
    pub(super) fn check(
        &mut self,
        outbox: &SqliteOutbox,
        parked: &[String],
        now: DateTimeOffset,
        log: &Logger,
    ) -> Result<(), UploadError> {
        if let Some(last) = self.last_check
            && now.duration_since(last).unwrap_or_default() < BACKLOG_REPORT_INTERVAL
        {
            return Ok(());
        }
        self.last_check = Some(now);
        let depth = outbox.depth()?;
        if depth < BACKLOG_REPORT_DEPTH {
            return Ok(());
        }
        let streams = outbox
            .depth_by_stream(STREAMS_NAMED)?
            .iter()
            .map(|(s, n)| format!("{s}={n}"))
            .collect::<Vec<_>>()
            .join(", ");
        let parked = if parked.is_empty() {
            "none".to_owned()
        } else {
            parked.join(", ")
        };
        log.warn(
            "Outbox backlog of {Depth} batches ({Code}): {Streams}; parked on a parent: {Parked}",
            &[
                Value::from(depth),
                Value::from(codes::SYNC_OUTBOX_BACKLOG),
                Value::from(streams),
                Value::from(parked),
            ],
        );
        Ok(())
    }
}
