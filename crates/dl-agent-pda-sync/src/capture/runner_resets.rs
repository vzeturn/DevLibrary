//! What a capture session forgets before its initial load (part of `SqlCaptureRunner.cs`):
//! a requested resnapshot of everything, a targeted one of some streams (ADR 0102), and the
//! one-time resend of ledger history after the mapper's text form changed (ADR 0123).

use super::{Shared, distinct};
use crate::capture::engine::{CaptureEngine, TEXT_REVISION};
use crate::capture::error::Result;
use crate::capture::sinks_bridge::BatchStore;
use crate::config::codes;
use crate::reporting::log::Value;
use crate::util::ctx::Ctx;

impl Shared {
    /// Resets at session start. The text revision cursor is written after the reset that
    /// covers it, so a crash in between resets the ledger streams again next time (safe).
    pub(super) fn session_resets(
        &self,
        engine: &CaptureEngine,
        queue: &dyn BatchStore,
        ctx: &Ctx,
    ) -> Result<()> {
        let text_upgrade = engine.text_upgrade_streams(ctx)?;
        if queue.needs_resnapshot()? {
            self.log.warn(
                "Resnapshot requested: capture forgets what it sent and sends every row in scope again",
                &[],
            );
            engine.reset(ctx)?;
            queue.clear_needs_resnapshot()?;
            // Pending targeted ones are covered too, or they would restart the session forever.
            let streams = distinct(
                engine
                    .streams()
                    .into_iter()
                    .chain(queue.pending_stream_resnapshot()?),
            );
            queue.begin_stream_resnapshot(&streams)?;
        } else {
            // ADR 0123: a state built before text values lost their trailing spaces sends its
            // ledger history once more.
            let upgrade = text_upgrade.clone().unwrap_or_default();
            let streams = distinct(
                queue
                    .pending_stream_resnapshot()?
                    .into_iter()
                    .chain(upgrade.iter().cloned()),
            );
            if !upgrade.is_empty() {
                self.log.info(
                    "Text revision {Revision}: ledger streams {Streams} are sent again without trailing spaces",
                    &[Value::from(TEXT_REVISION), Value::from(upgrade.join(", "))],
                );
            }
            if !streams.is_empty() {
                self.reset_streams(engine, queue, &streams, ctx)?;
            }
        }
        if text_upgrade.is_some() {
            engine.mark_text_revision(ctx)?;
        }
        Ok(())
    }

    /// Targeted resnapshot (ADR 0102): capture forgets what it sent for the pending streams
    /// only. A stream the plan does not know turns it into a resnapshot of everything.
    fn reset_streams(
        &self,
        engine: &CaptureEngine,
        queue: &dyn BatchStore,
        streams: &[String],
        ctx: &Ctx,
    ) -> Result<()> {
        let joined = streams.join(", ");
        if engine.reset_streams(streams, ctx)? {
            self.log.info(
                "Stream resnapshot: capture forgets what it sent for {Streams} and sends their rows again",
                &[Value::from(joined)],
            );
            return queue.begin_stream_resnapshot(streams);
        }
        self.log.warn(
            "Stream resnapshot of {Streams} names a stream this capture plan does not have ({Code}); sending every row in scope again instead",
            &[Value::from(joined), Value::from(codes::SYNC_STREAM_RESNAPSHOT)],
        );
        engine.reset(ctx)?;
        queue.begin_stream_resnapshot(&distinct(
            engine.streams().into_iter().chain(streams.iter().cloned()),
        ))?;
        queue.clear_needs_resnapshot()
    }
}
