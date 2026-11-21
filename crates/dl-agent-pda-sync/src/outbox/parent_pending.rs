//! The uploader's parent-pending episodes (port of `Outbox/ParentPendingTracker.cs`).

use std::collections::HashMap;
use std::time::Duration;

use crate::contracts::time::DateTimeOffset;

fn ticks(d: Duration) -> i64 {
    i64::try_from(d.as_nanos() / 100).unwrap_or(i64::MAX)
}

fn plus(t: DateTimeOffset, d: Duration) -> DateTimeOffset {
    t.checked_add(d).unwrap_or(t)
}

/// One stream's wait for parent rows of another stream (HTTP 424 `SYNC_PARENT_PENDING`,
/// ADR 0102), from its first refusal until a batch of the stream applies.
#[derive(Debug, Clone, PartialEq)]
pub struct ParentPendingEpisode {
    /// The waiting stream.
    pub stream: String,
    /// The stream whose rows are missing (latest answer).
    pub parent_stream: String,
    /// First refusal.
    pub first_seen: DateTimeOffset,
    /// Refusals so far.
    pub attempts: i32,
    /// When the stream is offered again.
    pub next_attempt: DateTimeOffset,
    /// When the uploader asked capture to resend the parent stream and the stream.
    pub escalated_at: Option<DateTimeOffset>,
    /// The episode's one site event was written (at escalation, or when it resolved first).
    pub reported: bool,
}

impl ParentPendingEpisode {
    /// `now - first_seen` in ticks.
    pub fn waited_ticks(&self, now: DateTimeOffset) -> i64 {
        now.ticks_since(self.first_seen)
    }

    /// `now - first_seen` (zero when the clock went back).
    pub fn waited(&self, now: DateTimeOffset) -> Duration {
        now.duration_since(self.first_seen).unwrap_or_default()
    }
}

/// The uploader's parent-pending episodes, one per stream. A refused stream is parked: its
/// batches stay queued, untouched, and are not offered again until its backoff (10 s
/// doubling to 2 min) has passed, while every other stream keeps flowing. Kept in memory:
/// after a restart an episode starts over. Used by the single uploader loop only.
#[derive(Debug, Clone)]
pub struct ParentPendingTracker {
    episodes: HashMap<String, ParentPendingEpisode>,
    orphans_skipped: HashMap<String, DateTimeOffset>,
    escalate_after: Duration,
}

impl ParentPendingTracker {
    /// Backoff after the first refusal.
    pub const FIRST_BACKOFF: Duration = Duration::from_secs(10);
    /// Longest backoff.
    pub const MAX_BACKOFF: Duration = Duration::from_secs(120);
    /// How long a stream whose orphan rows were left out (ADR 0109) skips them again
    /// without the long wait.
    pub const ORPHANS_REMEMBERED: Duration = Duration::from_secs(24 * 3600);

    /// A tracker escalating after `escalate_after` (zero: 15 minutes).
    pub fn new(escalate_after: Duration) -> ParentPendingTracker {
        ParentPendingTracker {
            episodes: HashMap::new(),
            orphans_skipped: HashMap::new(),
            escalate_after: if escalate_after > Duration::ZERO {
                escalate_after
            } else {
                Duration::from_secs(15 * 60)
            },
        }
    }

    /// How long an episode waits before it asks for a resnapshot.
    pub fn escalate_after(&self) -> Duration {
        self.escalate_after
    }

    /// Streams still in their backoff: left out of the next peek.
    pub fn parked(&self, now: DateTimeOffset) -> Vec<String> {
        self.episodes
            .values()
            .filter(|e| e.next_attempt > now)
            .map(|e| e.stream.clone())
            .collect()
    }

    /// The stream's episode.
    pub fn find(&self, stream: &str) -> Option<&ParentPendingEpisode> {
        self.episodes.get(stream)
    }

    /// The stream's episode, mutable (to record escalation and reporting).
    pub fn find_mut(&mut self, stream: &str) -> Option<&mut ParentPendingEpisode> {
        self.episodes.get_mut(stream)
    }

    /// Counts one more refusal of `stream` and sets its next attempt.
    pub fn park(
        &mut self,
        stream: &str,
        parent_stream: &str,
        now: DateTimeOffset,
    ) -> &mut ParentPendingEpisode {
        let episode =
            self.episodes
                .entry(stream.to_owned())
                .or_insert_with(|| ParentPendingEpisode {
                    stream: stream.to_owned(),
                    parent_stream: parent_stream.to_owned(),
                    first_seen: now,
                    attempts: 0,
                    next_attempt: DateTimeOffset::MIN,
                    escalated_at: None,
                    reported: false,
                });
        parent_stream.clone_into(&mut episode.parent_stream);
        episode.attempts = episode.attempts.saturating_add(1);
        episode.next_attempt = plus(now, Self::backoff(episode.attempts));
        episode
    }

    /// The episode has waited [`ParentPendingTracker::escalate_after`] and has not asked
    /// for a resnapshot yet.
    pub fn escalation_due(&self, episode: &ParentPendingEpisode, now: DateTimeOffset) -> bool {
        episode.escalated_at.is_none() && episode.waited_ticks(now) >= ticks(self.escalate_after)
    }

    /// The parent stream was resent in full after the escalation and the rows are still
    /// missing [`ParentPendingTracker::escalate_after`] later: they do not exist at the
    /// store, so the refusal is a data rejection, not a wait.
    pub fn parent_gone(
        &self,
        episode: &ParentPendingEpisode,
        parent_resent_at: Option<DateTimeOffset>,
        now: DateTimeOffset,
    ) -> bool {
        match (episode.escalated_at, parent_resent_at) {
            (Some(escalated), Some(resent)) => {
                resent >= escalated && now.ticks_since(resent) >= ticks(self.escalate_after)
            }
            _ => false,
        }
    }

    /// Capture cannot resend the parent stream, so waiting for its resnapshot would
    /// never end: after the escalation the outbox is full (capture is refused and keeps
    /// its changes) or twice [`ParentPendingTracker::escalate_after`] has passed since the
    /// escalation. Returns the reason (`outbox_full` or `resend_overdue`), `None` while the
    /// wait is still sensible. The caller has already ruled out
    /// [`ParentPendingTracker::parent_gone`] (resnapshot finished).
    pub fn parent_resend_stall(
        &self,
        episode: &ParentPendingEpisode,
        outbox_full: bool,
        now: DateTimeOffset,
    ) -> Option<&'static str> {
        let escalated = episode.escalated_at?;
        if outbox_full {
            Some("outbox_full")
        } else if now.ticks_since(escalated) >= ticks(self.escalate_after) * 2 {
            Some("resend_overdue")
        } else {
            None
        }
    }

    /// The cloud left out the stream's orphan rows (ADR 0109): remembered for
    /// [`ParentPendingTracker::ORPHANS_REMEMBERED`].
    pub fn orphans_skipped(&mut self, stream: &str, now: DateTimeOffset) {
        self.orphans_skipped.insert(stream.to_owned(), now);
    }

    /// A stream whose orphans were left out lately waits only
    /// [`ParentPendingTracker::MAX_BACKOFF`] (for parent batches still in flight) before it
    /// skips them again.
    pub fn skip_orphans_early(&self, episode: &ParentPendingEpisode, now: DateTimeOffset) -> bool {
        self.orphans_skipped.get(&episode.stream).is_some_and(|at| {
            now.ticks_since(*at) < ticks(Self::ORPHANS_REMEMBERED)
                && episode.waited_ticks(now) >= ticks(Self::MAX_BACKOFF)
        })
    }

    /// Ends the stream's episode (a batch applied, or it was given up), or `None` when it
    /// had none.
    pub fn resolve(&mut self, stream: &str) -> Option<ParentPendingEpisode> {
        self.episodes.remove(stream)
    }

    /// The backoff after `attempts` refusals: 10 s doubling, capped at 2 min.
    pub fn backoff(attempts: i32) -> Duration {
        let seconds = 10.0 * 2f64.powi(attempts.clamp(1, 16) - 1);
        if seconds >= Self::MAX_BACKOFF.as_secs_f64() {
            Self::MAX_BACKOFF
        } else {
            Duration::from_secs_f64(seconds)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: i64) -> DateTimeOffset {
        DateTimeOffset::from_unix_seconds(1_800_000_000 + s).unwrap()
    }

    #[test]
    fn backoff_doubles_from_ten_seconds_to_two_minutes() {
        let secs: Vec<u64> = [0, 1, 2, 3, 4, 5, 6, 40]
            .iter()
            .map(|a| ParentPendingTracker::backoff(*a).as_secs())
            .collect();
        assert_eq!(secs, [10, 10, 20, 40, 80, 120, 120, 120]);
    }

    #[test]
    fn a_parked_stream_waits_out_its_backoff_and_resolves() {
        let mut t = ParentPendingTracker::new(Duration::ZERO);
        assert_eq!(t.escalate_after(), Duration::from_secs(900));
        t.park("Trn", "Invoice", at(0));
        assert_eq!(t.parked(at(5)), ["Trn"]);
        assert!(t.parked(at(10)).is_empty());
        let e = t.park("Trn", "Other", at(10));
        assert_eq!((e.attempts, e.parent_stream.as_str()), (2, "Other"));
        assert_eq!(e.next_attempt, at(30));
        assert_eq!(e.first_seen, at(0));
        let e = t.find("Trn").unwrap().clone();
        assert!(!t.escalation_due(&e, at(899)));
        assert!(t.escalation_due(&e, at(900)));
        assert_eq!(t.resolve("Trn").map(|e| e.attempts), Some(2));
        assert!(t.resolve("Trn").is_none());
    }

    #[test]
    fn parent_gone_needs_a_resend_after_the_escalation_and_the_bound() {
        let mut t = ParentPendingTracker::new(Duration::from_secs(60));
        t.park("Trn", "Invoice", at(0));
        let mut e = t.find("Trn").unwrap().clone();
        assert!(!t.parent_gone(&e, Some(at(70)), at(200)), "not escalated");
        e.escalated_at = Some(at(60));
        assert!(
            !t.parent_gone(&e, Some(at(50)), at(200)),
            "resent before the escalation"
        );
        assert!(!t.parent_gone(&e, Some(at(70)), at(129)));
        assert!(t.parent_gone(&e, Some(at(70)), at(130)));
        assert!(!t.parent_gone(&e, None, at(130)));
    }

    #[test]
    fn orphans_skipped_lately_are_skipped_after_the_short_wait() {
        let mut t = ParentPendingTracker::new(Duration::from_secs(900));
        t.park("Trn", "Invoice", at(0));
        let e = t.find("Trn").unwrap().clone();
        assert!(!t.skip_orphans_early(&e, at(200)));
        t.orphans_skipped("Trn", at(-100));
        assert!(!t.skip_orphans_early(&e, at(119)));
        assert!(t.skip_orphans_early(&e, at(120)));
        assert!(!t.skip_orphans_early(&e, at(24 * 3600 - 100)));
    }

    #[test]
    fn a_stalled_parent_resend_needs_an_escalation_and_a_full_outbox_or_the_bound() {
        let mut t = ParentPendingTracker::new(Duration::from_secs(60));
        t.park("Trn", "Invoice", at(0));
        let mut e = t.find("Trn").unwrap().clone();
        assert_eq!(
            t.parent_resend_stall(&e, true, at(500)),
            None,
            "not escalated"
        );
        e.escalated_at = Some(at(60));
        assert_eq!(t.parent_resend_stall(&e, true, at(61)), Some("outbox_full"));
        assert_eq!(t.parent_resend_stall(&e, false, at(179)), None);
        assert_eq!(
            t.parent_resend_stall(&e, false, at(180)),
            Some("resend_overdue")
        );
    }
}
