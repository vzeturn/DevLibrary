//! Frame types of dl-rt/1. The byte layout is in `docs/protocol/dl-rt-1.md` and in
//! [`codec`](crate::Frame::encode).

use alloc::vec::Vec;
use core::fmt;

use crate::Error;

/// Protocol version carried in byte 0 of every frame.
pub const VERSION: u8 = 1;
/// Bytes of the fixed header: version, chan, kind, flags, seq, sent_us, payload_len.
pub const HEADER_LEN: usize = 24;
/// Bytes of one header-extension entry: tag (1) + value (8).
pub const EXT_ENTRY_LEN: usize = 9;
/// Default largest frame (header + extension + payload): 1 MiB.
pub const DEFAULT_MAX_FRAME: usize = 1 << 20;
/// Largest close reason text in bytes, the WebSocket limit for a close reason
/// (RFC 6455 section 5.5: 125-byte control payload minus the 2-byte code).
pub const MAX_CLOSE_REASON: usize = 123;

/// Logical channel of a frame. Each channel has its own sequence space per direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Channel {
    /// Session control: ping, pong, close, config pushes.
    Control = 0,
    /// Capture batches, agent to cloud.
    Capture = 1,
    /// Commands, cloud to agent.
    Commands = 2,
    /// Command results, agent to cloud.
    Results = 3,
    /// Blobs and resync segments; never takes the credit of another channel.
    Bulk = 4,
}

impl Channel {
    /// Every channel in wire order.
    pub const ALL: [Channel; 5] = [
        Channel::Control,
        Channel::Capture,
        Channel::Commands,
        Channel::Results,
        Channel::Bulk,
    ];

    /// The channel for a wire byte, if known.
    pub const fn from_u8(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::Control),
            1 => Some(Self::Capture),
            2 => Some(Self::Commands),
            3 => Some(Self::Results),
            4 => Some(Self::Bulk),
            _ => None,
        }
    }

    /// The wire byte.
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Index into per-channel arrays (`0..5`).
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Lowercase name, as in the golden vectors.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Capture => "capture",
            Self::Commands => "commands",
            Self::Results => "results",
            Self::Bulk => "bulk",
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What a frame asks of the peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Kind {
    /// Application data with a sequence number; the only kind that takes credit.
    Data = 1,
    /// Cumulative acknowledgement: every `seq` up to this one is durably committed.
    Ack = 2,
    /// Grants the peer more payload bytes on the channel (8-byte payload).
    Credit = 3,
    /// Sent on every new connection: `seq` is the last acknowledgement this side saw.
    Resume = 4,
    /// Answer to `resume`: `seq` is the receiver's high-water mark (last committed).
    ResumeOk = 5,
    /// Latency probe on the control channel.
    Ping = 6,
    /// Echo of a ping (same seq, sent_us and payload).
    Pong = 7,
    /// The sender closes the connection: 2-byte reason code + UTF-8 text.
    Close = 8,
}

impl Kind {
    /// The kind for a wire byte, if known.
    pub const fn from_u8(b: u8) -> Option<Self> {
        match b {
            1 => Some(Self::Data),
            2 => Some(Self::Ack),
            3 => Some(Self::Credit),
            4 => Some(Self::Resume),
            5 => Some(Self::ResumeOk),
            6 => Some(Self::Ping),
            7 => Some(Self::Pong),
            8 => Some(Self::Close),
            _ => None,
        }
    }

    /// The wire byte.
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Lowercase name, as in the golden vectors.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::Ack => "ack",
            Self::Credit => "credit",
            Self::Resume => "resume",
            Self::ResumeOk => "resume_ok",
            Self::Ping => "ping",
            Self::Pong => "pong",
            Self::Close => "close",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Optional per-hop timestamps of a data frame (ADR 0011, "Each hop is measured").
/// `t_sent` is the frame's own [`Frame::sent_us`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Ext {
    /// When the change was detected on site, microseconds (extension tag 1).
    pub t_detected_us: Option<u64>,
    /// When the changed rows were read, microseconds (extension tag 2).
    pub t_read_us: Option<u64>,
}

impl Ext {
    /// No extension.
    pub const NONE: Ext = Ext {
        t_detected_us: None,
        t_read_us: None,
    };

    /// True when no timestamp is set (the frame carries no extension area).
    pub const fn is_empty(&self) -> bool {
        self.t_detected_us.is_none() && self.t_read_us.is_none()
    }

    /// Bytes of the extension area on the wire, including its length byte (0 when empty).
    pub const fn encoded_len(&self) -> usize {
        let n = self.t_detected_us.is_some() as usize + self.t_read_us.is_some() as usize;
        if n == 0 { 0 } else { 1 + n * EXT_ENTRY_LEN }
    }
}

/// Reason code of a [`Kind::Close`] frame. Unknown codes are kept as they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CloseCode(pub u16);

impl CloseCode {
    /// Normal end of the connection.
    pub const NORMAL: CloseCode = CloseCode(0);
    /// The sender shuts down or restarts.
    pub const GOING_AWAY: CloseCode = CloseCode(1);
    /// The peer broke a rule of this protocol (bad frame, gap, ack ahead).
    pub const PROTOCOL_ERROR: CloseCode = CloseCode(2);
    /// A frame exceeded the size limit.
    pub const TOO_LARGE: CloseCode = CloseCode(3);
    /// A data frame exceeded the granted credit.
    pub const CREDIT_EXCEEDED: CloseCode = CloseCode(4);
    /// Resume state does not match (`resume_conflict`, `resume_behind`, `resume_ahead`).
    pub const RESUME_CONFLICT: CloseCode = CloseCode(5);
    /// Internal failure of the sender.
    pub const INTERNAL: CloseCode = CloseCode(6);
}

/// One dl-rt/1 frame.
///
/// The fields are public; [`validate`](Self::validate) (run by both encode and decode)
/// checks the rules of each kind.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Frame {
    /// Logical channel.
    pub chan: Channel,
    /// Frame kind.
    pub kind: Kind,
    /// Kind-specific sequence field (see the kind's documentation).
    pub seq: u64,
    /// Sender clock when the frame was written, microseconds.
    pub sent_us: u64,
    /// Per-hop timestamps; data frames only.
    pub ext: Ext,
    /// Payload bytes.
    pub payload: Vec<u8>,
}

impl Frame {
    fn bare(chan: Channel, kind: Kind, seq: u64, sent_us: u64) -> Frame {
        Frame {
            chan,
            kind,
            seq,
            sent_us,
            ext: Ext::NONE,
            payload: Vec::new(),
        }
    }

    /// A data frame.
    pub fn data(chan: Channel, seq: u64, sent_us: u64, ext: Ext, payload: Vec<u8>) -> Frame {
        Frame {
            chan,
            kind: Kind::Data,
            seq,
            sent_us,
            ext,
            payload,
        }
    }

    /// Cumulative acknowledgement of every `seq <= through` on `chan`.
    pub fn ack(chan: Channel, through: u64, sent_us: u64) -> Frame {
        Frame::bare(chan, Kind::Ack, through, sent_us)
    }

    /// Grants `bytes` more payload bytes on `chan`.
    pub fn credit(chan: Channel, bytes: u64, sent_us: u64) -> Frame {
        let mut f = Frame::bare(chan, Kind::Credit, 0, sent_us);
        f.payload = bytes.to_be_bytes().to_vec();
        f
    }

    /// Resume request: the last acknowledgement this side saw on `chan`.
    pub fn resume(chan: Channel, last_acked: u64, sent_us: u64) -> Frame {
        Frame::bare(chan, Kind::Resume, last_acked, sent_us)
    }

    /// Resume answer: the receiver's high-water mark on `chan`.
    pub fn resume_ok(chan: Channel, hwm: u64, sent_us: u64) -> Frame {
        Frame::bare(chan, Kind::ResumeOk, hwm, sent_us)
    }

    /// A ping on the control channel.
    pub fn ping(seq: u64, sent_us: u64, payload: Vec<u8>) -> Frame {
        let mut f = Frame::bare(Channel::Control, Kind::Ping, seq, sent_us);
        f.payload = payload;
        f
    }

    /// The pong answering this ping: same seq, sent_us and payload.
    pub fn pong_of(&self) -> Frame {
        let mut f = Frame::bare(Channel::Control, Kind::Pong, self.seq, self.sent_us);
        f.payload = self.payload.clone();
        f
    }

    /// A close frame; fails when `reason` is longer than [`MAX_CLOSE_REASON`] bytes.
    pub fn close(code: CloseCode, reason: &str, sent_us: u64) -> Result<Frame, Error> {
        if reason.len() > MAX_CLOSE_REASON {
            return Err(Error::Field {
                kind: Kind::Close,
                field: "reason",
            });
        }
        let mut f = Frame::bare(Channel::Control, Kind::Close, 0, sent_us);
        f.payload.extend_from_slice(&code.0.to_be_bytes());
        f.payload.extend_from_slice(reason.as_bytes());
        Ok(f)
    }

    /// Granted bytes of a valid credit frame.
    pub fn credit_bytes(&self) -> Option<u64> {
        if self.kind != Kind::Credit {
            return None;
        }
        let b: [u8; 8] = self.payload.as_slice().try_into().ok()?;
        Some(u64::from_be_bytes(b))
    }

    /// Reason code of a valid close frame.
    pub fn close_code(&self) -> Option<CloseCode> {
        match (self.kind, self.payload.as_slice()) {
            (Kind::Close, [a, b, ..]) => Some(CloseCode(u16::from_be_bytes([*a, *b]))),
            _ => None,
        }
    }

    /// Reason text of a valid close frame.
    pub fn close_reason(&self) -> Option<&str> {
        if self.kind != Kind::Close {
            return None;
        }
        core::str::from_utf8(self.payload.get(2..)?).ok()
    }

    /// Checks the rules of the frame's kind (protocol document, section "Frame kinds").
    /// Checked in this order: ext, chan, seq, payload, reason.
    pub fn validate(&self) -> Result<(), Error> {
        let bad = |field| {
            Err(Error::Field {
                kind: self.kind,
                field,
            })
        };
        if self.kind != Kind::Data && !self.ext.is_empty() {
            return bad("ext");
        }
        let control_only = matches!(self.kind, Kind::Ping | Kind::Pong | Kind::Close);
        if control_only && self.chan != Channel::Control {
            return bad("chan");
        }
        match self.kind {
            Kind::Data if self.seq == 0 => bad("seq"),
            Kind::Ack | Kind::Resume | Kind::ResumeOk if !self.payload.is_empty() => bad("payload"),
            Kind::Credit if self.seq != 0 => bad("seq"),
            Kind::Credit if self.payload.len() != 8 => bad("payload"),
            Kind::Close if self.seq != 0 => bad("seq"),
            Kind::Close if self.payload.len() < 2 || self.payload.len() > 2 + MAX_CLOSE_REASON => {
                bad("payload")
            }
            Kind::Close if self.close_reason().is_none() => bad("reason"),
            _ => Ok(()),
        }
    }
}
