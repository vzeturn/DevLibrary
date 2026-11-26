//! The one error type of this crate.

use core::fmt;

use crate::frame::{Channel, Kind};

/// Everything that can go wrong while decoding a frame or driving the state machines.
///
/// Decode errors carry the facts a log line needs (lengths, the offending byte). The
/// [`name`](Self::name) of each variant is stable and is what the golden vectors in
/// `testdata/frames.txt` and the .NET port compare against.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The buffer ends before the frame does: `need` bytes are required, `have` are present.
    Truncated {
        /// Bytes the frame needs, as far as the decoder could tell.
        need: u64,
        /// Bytes present.
        have: usize,
    },
    /// The version byte is not [`VERSION`](crate::VERSION).
    Version(u8),
    /// The frame is larger than the configured [`Limits::max_frame`](crate::Limits::max_frame).
    Oversize {
        /// Size of the frame in bytes.
        len: u64,
        /// The limit in bytes.
        max: usize,
    },
    /// Unknown channel byte.
    Channel(u8),
    /// Unknown kind byte.
    Kind(u8),
    /// A reserved flag bit is set (the byte as received).
    Flags(u8),
    /// Malformed header extension at this byte offset of the frame.
    Ext {
        /// Byte offset of the bad length byte or entry.
        offset: usize,
    },
    /// Bytes follow the payload that its declared length does not cover.
    Trailing {
        /// Number of extra bytes.
        extra: usize,
    },
    /// A field breaks the rule of its frame kind (for example a credit frame whose payload is
    /// not 8 bytes, or a ping outside the control channel).
    Field {
        /// The frame kind.
        kind: Kind,
        /// The field: `chan`, `seq`, `payload`, `reason` or `ext`.
        field: &'static str,
    },
    /// The peer acknowledged a sequence number this side never sent.
    AckAhead {
        /// Channel.
        chan: Channel,
        /// The acknowledged sequence number.
        ack: u64,
        /// The last sequence number sent.
        last_sent: u64,
    },
    /// `resume_ok` names a high-water mark beyond the last sequence number sent.
    ResumeAhead {
        /// Channel.
        chan: Channel,
        /// The peer's high-water mark.
        hwm: u64,
        /// The last sequence number sent.
        last_sent: u64,
    },
    /// `resume_ok` names a high-water mark below an acknowledgement already received:
    /// the peer lost data it had confirmed as durable.
    ResumeBehind {
        /// Channel.
        chan: Channel,
        /// The peer's high-water mark.
        hwm: u64,
        /// The highest acknowledgement received earlier.
        acked: u64,
    },
    /// A `resume` claims acknowledgements beyond what this side committed.
    ResumeConflict {
        /// Channel.
        chan: Channel,
        /// What the peer says was acknowledged.
        last_acked: u64,
        /// What this side committed.
        hwm: u64,
    },
    /// `commit` names a sequence number that has not been received yet.
    CommitAhead {
        /// Channel.
        chan: Channel,
        /// The sequence number passed to `commit`.
        seq: u64,
        /// The highest sequence number received in order.
        received: u64,
    },
    /// A data frame is larger than the credit granted for its channel.
    CreditExceeded {
        /// Channel.
        chan: Channel,
        /// Payload bytes of the frame.
        need: u64,
        /// Credit left.
        available: u64,
    },
    /// A frame or a restored outbox entry belongs to another channel.
    WrongChannel {
        /// The channel of the state machine.
        expected: Channel,
        /// The channel of the frame.
        got: Channel,
    },
    /// A frame of this kind is not valid here (for example `resume_ok` without a `resume`).
    Unexpected {
        /// Channel.
        chan: Channel,
        /// Frame kind.
        kind: Kind,
    },
    /// Frames passed to restore a sender are not data frames numbered `acked + 1, acked + 2, ...`.
    Restore {
        /// Channel.
        chan: Channel,
        /// The sequence number expected at the first bad entry.
        expected_seq: u64,
    },
    /// The 64-bit sequence space of a channel is used up.
    SeqExhausted(Channel),
    /// The session has no open connection.
    NotConnected,
}

impl Error {
    /// Stable, lowercase name of the variant, as used in the golden vectors.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Truncated { .. } => "truncated",
            Self::Version(_) => "version",
            Self::Oversize { .. } => "oversize",
            Self::Channel(_) => "channel",
            Self::Kind(_) => "kind",
            Self::Flags(_) => "flags",
            Self::Ext { .. } => "ext",
            Self::Trailing { .. } => "trailing",
            Self::Field { .. } => "field",
            Self::AckAhead { .. } => "ack_ahead",
            Self::ResumeAhead { .. } => "resume_ahead",
            Self::ResumeBehind { .. } => "resume_behind",
            Self::ResumeConflict { .. } => "resume_conflict",
            Self::CommitAhead { .. } => "commit_ahead",
            Self::CreditExceeded { .. } => "credit_exceeded",
            Self::WrongChannel { .. } => "wrong_channel",
            Self::Unexpected { .. } => "unexpected",
            Self::Restore { .. } => "restore",
            Self::SeqExhausted(_) => "seq_exhausted",
            Self::NotConnected => "not_connected",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { need, have } => {
                write!(f, "frame truncated: need {need} bytes, have {have}")
            }
            Self::Version(v) => write!(f, "unsupported dl-rt version {v}"),
            Self::Oversize { len, max } => {
                write!(f, "frame of {len} bytes exceeds the {max}-byte limit")
            }
            Self::Channel(c) => write!(f, "unknown channel {c}"),
            Self::Kind(k) => write!(f, "unknown frame kind {k}"),
            Self::Flags(b) => write!(f, "reserved flag bits set in 0x{b:02x}"),
            Self::Ext { offset } => write!(f, "malformed header extension at byte {offset}"),
            Self::Trailing { extra } => write!(f, "{extra} bytes after the payload"),
            Self::Field { kind, field } => write!(f, "invalid {field} in a {kind} frame"),
            Self::AckAhead {
                chan,
                ack,
                last_sent,
            } => {
                write!(
                    f,
                    "{chan}: ack {ack} is beyond the last sent seq {last_sent}"
                )
            }
            Self::ResumeAhead {
                chan,
                hwm,
                last_sent,
            } => {
                write!(
                    f,
                    "{chan}: resume_ok hwm {hwm} is beyond the last sent seq {last_sent}"
                )
            }
            Self::ResumeBehind { chan, hwm, acked } => {
                write!(
                    f,
                    "{chan}: resume_ok hwm {hwm} is below the acknowledged seq {acked}"
                )
            }
            Self::ResumeConflict {
                chan,
                last_acked,
                hwm,
            } => write!(
                f,
                "{chan}: peer resumes from acked seq {last_acked} but only {hwm} was committed"
            ),
            Self::CommitAhead {
                chan,
                seq,
                received,
            } => {
                write!(
                    f,
                    "{chan}: commit {seq} is beyond the received seq {received}"
                )
            }
            Self::CreditExceeded {
                chan,
                need,
                available,
            } => write!(
                f,
                "{chan}: data frame of {need} payload bytes exceeds the {available}-byte credit"
            ),
            Self::WrongChannel { expected, got } => {
                write!(
                    f,
                    "frame of channel {got} given to the {expected} state machine"
                )
            }
            Self::Unexpected { chan, kind } => write!(f, "{chan}: unexpected {kind} frame"),
            Self::Restore { chan, expected_seq } => write!(
                f,
                "{chan}: restored frames must be data frames in order, expected seq {expected_seq}"
            ),
            Self::SeqExhausted(chan) => write!(f, "{chan}: sequence numbers exhausted"),
            Self::NotConnected => f.write_str("the session has no open connection"),
        }
    }
}

impl core::error::Error for Error {}
