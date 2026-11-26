//! The upload rounds of [`BatchUploader`] (RT-1318). Each round posts the next batch of
//! up to `parallel` streams at the same time over the client's pooled keep-alive
//! connections, then reads the answers in order. A stream never has two batches in flight:
//! its next batch is posted only after the cloud answered the one before, so a 409 or a 424
//! `SYNC_PARENT_PENDING` answer can never let a later seq of the stream overtake it.

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use super::{BatchUploader, Outcome, UploadCycleResult, parent_stream_of};
use crate::contracts::wire::sync_streams;
use crate::outbox::batches::OutboxItem;
use crate::outbox::error::UploadError;
use crate::reporting::Value;
use crate::transport::SyncResponse;
use crate::util::ctx::Ctx;

type Sent = Result<(SyncResponse, Duration), UploadError>;

/// The parent stream named by a 424 `SYNC_PARENT_PENDING` answer when that parent is posted
/// in the same round: the child may have been read before its parent landed.
fn parent_in_round(sent: &Sent, round: &[OutboxItem]) -> Option<String> {
    let (resp, _) = sent.as_ref().ok()?;
    if resp.status != 424 {
        return None;
    }
    parent_stream_of(resp).filter(|p| round.iter().any(|i| &i.stream == p))
}

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
                // A child answered 424 for a parent posted beside it is read after the
                // round's other answers: when the parent landed, the child goes again next
                // round instead of parking (RT-1318 review).
                let (children, others): (Vec<_>, Vec<_>) = round
                    .iter()
                    .zip(sent)
                    .map(|(item, sent)| (parent_in_round(&sent, &round), item, sent))
                    .partition(|(parent, _, _)| parent.is_some());
                let mut landed: HashSet<String> = HashSet::new();
                let mut again: Vec<OutboxItem> = Vec::new();
                // Every answer is read, even after a failure: a batch the cloud applied is
                // acked here, not posted again.
                for (parent, item, sent) in others.into_iter().chain(children) {
                    if let Some(parent) = parent.filter(|p| landed.contains(p)) {
                        self.log.debug(
                            "Batch {Stream}#{Seq} waited for {Parent}, which landed beside it; it is sent again",
                            &[
                                Value::from(&item.stream),
                                Value::from(item.seq),
                                Value::from(parent),
                            ],
                        );
                        again.push(item.clone());
                        continue;
                    }
                    let outcome = sent.and_then(|(resp, took)| self.answer(item, &resp, took, ctx));
                    match outcome {
                        Ok(Outcome::Done) => {
                            progressed = true;
                            landed.insert(item.stream.clone());
                        }
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
                for item in again.into_iter().rev() {
                    match streams
                        .iter_mut()
                        .find(|q| q.front().is_some_and(|i| i.stream == item.stream))
                    {
                        Some(q) => q.push_front(item),
                        None => streams.insert(0, VecDeque::from([item])),
                    }
                }
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
    fn post_round(&self, round: &[OutboxItem], ctx: &Ctx) -> Result<Vec<Sent>, UploadError> {
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
