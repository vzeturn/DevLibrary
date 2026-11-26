//! The upload rounds of [`BatchUploader`] (RT-1318). Each round posts the next batch of
//! up to `parallel` streams at the same time over the client's pooled keep-alive
//! connections, then reads the answers in order. A stream never has two batches in flight:
//! its next batch is posted only after the cloud answered the one before, so a 409 or a 424
//! `SYNC_PARENT_PENDING` answer can never let a later seq of the stream overtake it.

use std::collections::VecDeque;
use std::time::Duration;

use super::{BatchUploader, Outcome, UploadCycleResult};
use crate::contracts::wire::sync_streams;
use crate::outbox::batches::OutboxItem;
use crate::outbox::error::UploadError;
use crate::transport::SyncResponse;
use crate::util::ctx::Ctx;

/// The peeked batches per stream, streams in the order the peek met them (live first).
fn by_stream(items: Vec<OutboxItem>, skip: &[String]) -> Vec<VecDeque<OutboxItem>> {
    let mut streams: Vec<VecDeque<OutboxItem>> = Vec::new();
    for item in items {
        // A parked stream keeps its order: none of its later batches goes before the one that waits.
        if skip.contains(&item.stream) {
            continue;
        }
        match streams.iter_mut().find(|q| q[0].stream == item.stream) {
            Some(q) => q.push_back(item),
            None => streams.push(VecDeque::from([item])),
        }
    }
    streams
}

/// How a round ends the cycle.
enum Stop {
    /// Keep going.
    No,
    /// Back off (`Outcome::Retry`).
    Retry,
    /// A stream was renumbered: the next cycle peeks again.
    Repeek,
}

impl BatchUploader {
    pub(super) fn drain(
        &mut self,
        max_batches: i32,
        skip: &mut Vec<String>,
        ctx: &Ctx,
    ) -> Result<UploadCycleResult, UploadError> {
        let mut progressed = false;
        loop {
            let items = self
                .outbox
                .peek_live_first(max_batches, skip, &[sync_streams::OPEN])?;
            let mut streams = by_stream(items, skip);
            let mut newly_parked = false;
            while !streams.is_empty() {
                let round: Vec<OutboxItem> = streams
                    .iter_mut()
                    .take(self.parallel)
                    .filter_map(VecDeque::pop_front)
                    .collect();
                let sent = self.post_round(&round, ctx)?;
                let mut stop = Stop::No;
                let mut failed = None;
                // Every answer is read, even after a failure: a batch the cloud applied is
                // acked here, not posted again.
                for (item, sent) in round.iter().zip(sent) {
                    let outcome = sent.and_then(|(resp, took)| self.answer(item, &resp, took, ctx));
                    match outcome {
                        Ok(Outcome::Done) => progressed = true,
                        Ok(Outcome::Parked) => {
                            skip.push(item.stream.clone());
                            streams.retain(|q| q.front().is_none_or(|i| i.stream != item.stream));
                            newly_parked = true;
                        }
                        Ok(Outcome::Retry) => stop = Stop::Retry,
                        Ok(Outcome::Repeek) => {
                            if !matches!(stop, Stop::Retry) {
                                stop = Stop::Repeek;
                            }
                        }
                        Err(e) => {
                            failed.get_or_insert(e);
                        }
                    }
                }
                if let Some(e) = failed {
                    return Err(e);
                }
                match stop {
                    Stop::Retry => {
                        return Ok(if progressed {
                            UploadCycleResult::Progress
                        } else {
                            UploadCycleResult::Blocked
                        });
                    }
                    // Renumbered: the rest of this peek carries stale seqs.
                    Stop::Repeek => return Ok(UploadCycleResult::Progress),
                    Stop::No => {}
                }
                streams.retain(|q| !q.is_empty());
            }
            // Progress: the loop calls again at once. Every stream skipped grows `skip`, so
            // the re-peek ends; with only parked streams left the loop idles, not backs off.
            if progressed || !newly_parked {
                return Ok(if progressed {
                    UploadCycleResult::Progress
                } else {
                    UploadCycleResult::Idle
                });
            }
        }
    }

    /// Posts the round's batches side by side (one alone is posted on this thread).
    #[allow(clippy::type_complexity)]
    fn post_round(
        &self,
        round: &[OutboxItem],
        ctx: &Ctx,
    ) -> Result<Vec<Result<(SyncResponse, Duration), UploadError>>, UploadError> {
        let bearer = self.tokens.get_access_token(ctx)?;
        if let [item] = round {
            return Ok(vec![self.post(item, &bearer, ctx)]);
        }
        Ok(std::thread::scope(|scope| {
            let posts: Vec<_> = round
                .iter()
                .map(|item| scope.spawn(|| self.post(item, &bearer, ctx)))
                .collect();
            posts
                .into_iter()
                .map(|p| match p.join() {
                    Ok(r) => r,
                    Err(panic) => std::panic::resume_unwind(panic),
                })
                .collect()
        }))
    }
}
