//! Per-hop latency stamps (RT-1318, ADR 0011 "Each hop is measured"): unix epoch
//! microseconds the agent adds to a capture batch and to a command result, so the cloud
//! can time every hop. Optional and additive (`SyncBatch.TDetected`..., `Wire/Envelope.cs`):
//! an absent stamp is not written, an older cloud ignores the fields and an older agent
//! simply sends none. The stamps come from [`crate::util::hop_clock`].

use std::borrow::Cow;

use crate::contracts::json;
use crate::util::stj::{Obj, ReadError, Writer};

/// The stamps of a capture batch, written after `watermark`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BatchHops {
    /// `t_detected`: the capture pass that found the change started.
    pub t_detected: Option<i64>,
    /// `t_read`: the changed rows were read and mapped (the batch writer got them).
    pub t_read: Option<i64>,
    /// `t_sent`: the batch was handed to the transport. Batches wait in the outbox, so
    /// this one is added to the stored payload at upload time ([`with_t_sent`]).
    pub t_sent: Option<i64>,
}

impl BatchHops {
    pub(super) fn write(&self, w: &mut Writer) {
        json::opt_int(w, "t_detected", self.t_detected);
        json::opt_int(w, "t_read", self.t_read);
        json::opt_int(w, "t_sent", self.t_sent);
    }

    pub(super) fn read(o: &Obj<'_>) -> Result<BatchHops, ReadError> {
        Ok(BatchHops {
            t_detected: o.opt_int("t_detected")?,
            t_read: o.opt_int("t_read")?,
            t_sent: o.opt_int("t_sent")?,
        })
    }
}

/// The stamps of a command result, written after `exec_ms`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResultHops {
    /// `t_received`: the agent got the command (socket frame read, poll answered).
    pub t_received: Option<i64>,
    /// `t_started`: the command started on its worker.
    pub t_started: Option<i64>,
    /// `t_finished`: its result was ready.
    pub t_finished: Option<i64>,
}

impl ResultHops {
    pub(super) fn write(&self, w: &mut Writer) {
        json::opt_int(w, "t_received", self.t_received);
        json::opt_int(w, "t_started", self.t_started);
        json::opt_int(w, "t_finished", self.t_finished);
    }

    pub(super) fn read(o: &Obj<'_>) -> Result<ResultHops, ReadError> {
        Ok(ResultHops {
            t_received: o.opt_int("t_received")?,
            t_started: o.opt_int("t_started")?,
            t_finished: o.opt_int("t_finished")?,
        })
    }
}

/// `payload` (a serialized [`SyncBatch`](super::SyncBatch)) with `"t_sent":at` added as
/// its last member. A payload that is not a non-empty JSON object, or already carries a
/// `t_sent`, is returned unchanged, as is every payload when `at` is `None`.
pub fn with_t_sent(payload: &[u8], at: Option<i64>) -> Cow<'_, [u8]> {
    let Some(at) = at else {
        return Cow::Borrowed(payload);
    };
    let Some(end) = payload.iter().rposition(|b| !b.is_ascii_whitespace()) else {
        return Cow::Borrowed(payload);
    };
    let before = payload[..end]
        .iter()
        .rposition(|b| !b.is_ascii_whitespace());
    let object = payload[end] == b'}'
        && payload.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{')
        && before.is_some_and(|i| payload[i] != b'{');
    if !object || contains(payload, b"\"t_sent\":") {
        return Cow::Borrowed(payload);
    }
    let member = format!(",\"t_sent\":{at}");
    let mut out = Vec::with_capacity(payload.len() + member.len());
    out.extend_from_slice(&payload[..end]);
    out.extend_from_slice(member.as_bytes());
    out.extend_from_slice(&payload[end..]);
    Cow::Owned(out)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
