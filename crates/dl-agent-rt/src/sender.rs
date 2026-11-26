//! Sending side of one channel: sequence numbers, unacknowledged frames, credit and resume.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::{Channel, Error, Ext, Frame, Kind, Limits};

/// Connection state of a [`ChannelSender`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SenderState {
    /// No connection; nothing can be sent.
    Disconnected,
    /// `resume` sent, waiting for `resume_ok`.
    Resuming,
    /// Resumed; data may flow as credit allows.
    Open,
}

/// Why a send did not happen. Nothing is dropped: the payload comes back to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocked {
    /// No connection.
    Disconnected,
    /// The channel waits for `resume_ok`.
    Resuming,
    /// Earlier frames are still being resent; new data waits behind them to keep order.
    Resending,
    /// Not enough credit: `need` payload bytes, `available` granted.
    Credit {
        /// Payload bytes of the frame.
        need: u64,
        /// Credit left on the channel.
        available: u64,
    },
    /// The connection is closing.
    Closing,
}

/// A send that has to wait, with the payload handed back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WouldBlock {
    /// The payload, unchanged.
    pub payload: Vec<u8>,
    /// Why it has to wait.
    pub reason: Blocked,
}

/// Result of a send that may have to wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome<T> {
    /// Sent.
    Done(T),
    /// Not sent; try again after the reason has cleared (credit, resume, reconnect).
    WouldBlock(WouldBlock),
}

/// Result of [`ChannelSender::poll_resend`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resend {
    /// The next frame to write again (its `sent_us` is updated).
    Frame(Frame),
    /// The next resend needs more credit.
    Blocked {
        /// Payload bytes of the frame.
        need: u64,
        /// Credit left on the channel.
        available: u64,
    },
    /// Nothing to resend.
    Idle,
}

/// Sender state machine of one channel.
///
/// Persistence contract (outbox): store every frame [`try_send`](Self::try_send) returns
/// before writing it to the socket; delete stored frames up to [`acked`](Self::acked) after
/// each [`on_ack`](Self::on_ack) / [`on_resume_ok`](Self::on_resume_ok); after a restart
/// rebuild the sender with [`restore`](Self::restore). The in-memory copy here is the
/// resend source while the process lives.
#[derive(Debug, Clone)]
pub struct ChannelSender {
    chan: Channel,
    next_seq: u64,
    acked: u64,
    unacked: VecDeque<Frame>,
    credit: u64,
    resend_next: Option<u64>,
    state: SenderState,
}

impl ChannelSender {
    /// A fresh channel: nothing sent, the first data frame gets seq 1.
    pub fn new(chan: Channel) -> ChannelSender {
        ChannelSender {
            chan,
            next_seq: 1,
            acked: 0,
            unacked: VecDeque::new(),
            credit: 0,
            resend_next: None,
            state: SenderState::Disconnected,
        }
    }

    /// Rebuilds a sender from persisted state: the last acknowledgement and the frames sent
    /// after it, which must be data frames of `chan` numbered `acked + 1, acked + 2, ...`.
    pub fn restore(chan: Channel, acked: u64, unacked: Vec<Frame>) -> Result<ChannelSender, Error> {
        let mut expected = acked.checked_add(1).ok_or(Error::SeqExhausted(chan))?;
        for f in &unacked {
            if f.chan != chan || f.kind != Kind::Data || f.seq != expected {
                return Err(Error::Restore {
                    chan,
                    expected_seq: expected,
                });
            }
            expected = expected.checked_add(1).ok_or(Error::SeqExhausted(chan))?;
        }
        Ok(ChannelSender {
            next_seq: expected,
            acked,
            unacked: unacked.into(),
            ..ChannelSender::new(chan)
        })
    }

    /// The channel.
    pub fn chan(&self) -> Channel {
        self.chan
    }

    /// Connection state.
    pub fn state(&self) -> SenderState {
        self.state
    }

    /// Sequence number the next new data frame gets.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Last sequence number assigned (0 before the first frame).
    pub fn last_sent(&self) -> u64 {
        self.next_seq - 1
    }

    /// Highest cumulative acknowledgement received.
    pub fn acked(&self) -> u64 {
        self.acked
    }

    /// Payload bytes the peer still allows on this connection.
    pub fn credit(&self) -> u64 {
        self.credit
    }

    /// Frames sent and not yet acknowledged, in seq order.
    pub fn unacked(&self) -> impl ExactSizeIterator<Item = &Frame> {
        self.unacked.iter()
    }

    /// True while frames from before a reconnect are still to be written again.
    pub fn is_resending(&self) -> bool {
        self.resend_next.is_some()
    }

    /// A new connection is up: returns the `resume` frame to send. Credit starts at 0.
    pub fn connect(&mut self, now_us: u64) -> Frame {
        self.disconnect();
        self.state = SenderState::Resuming;
        Frame::resume(self.chan, self.acked, now_us)
    }

    /// The connection is gone: credit is void, unacknowledged frames are kept for resume.
    pub fn disconnect(&mut self) {
        self.state = SenderState::Disconnected;
        self.credit = 0;
        self.resend_next = None;
    }

    /// The peer granted `bytes` more payload bytes (a `credit` frame).
    pub fn grant(&mut self, bytes: u64) {
        self.credit = self.credit.saturating_add(bytes);
    }

    /// Sends a new data frame when the channel is open, nothing waits to be resent and the
    /// credit covers the payload. A payload that could never fit a frame is an error.
    pub fn try_send(
        &mut self,
        payload: Vec<u8>,
        ext: Ext,
        now_us: u64,
        limits: &Limits,
    ) -> Result<Outcome<Frame>, Error> {
        let mut frame = Frame::data(self.chan, self.next_seq, now_us, ext, Vec::new());
        let len = frame.encoded_len() as u64 + payload.len() as u64;
        if len > limits.max_frame() as u64 {
            return Err(Error::Oversize {
                len,
                max: limits.max_frame(),
            });
        }
        let need = payload.len() as u64;
        let reason = match self.state {
            SenderState::Disconnected => Some(Blocked::Disconnected),
            SenderState::Resuming => Some(Blocked::Resuming),
            SenderState::Open if self.resend_next.is_some() => Some(Blocked::Resending),
            SenderState::Open if need > self.credit => Some(Blocked::Credit {
                need,
                available: self.credit,
            }),
            SenderState::Open => None,
        };
        if let Some(reason) = reason {
            return Ok(Outcome::WouldBlock(WouldBlock { payload, reason }));
        }
        self.next_seq = self
            .next_seq
            .checked_add(1)
            .ok_or(Error::SeqExhausted(self.chan))?;
        self.credit -= need;
        frame.payload = payload;
        self.unacked.push_back(frame.clone());
        Ok(Outcome::Done(frame))
    }

    /// The next frame to write again after `resume_ok`, if the credit covers it.
    pub fn poll_resend(&mut self, now_us: u64) -> Resend {
        let Some(seq) = self.resend_next else {
            return Resend::Idle;
        };
        // resend_next is always in acked+1 ..= last_sent while set (see on_ack / on_resume_ok).
        let Some(stored) = self.unacked.get((seq - self.acked - 1) as usize) else {
            self.resend_next = None;
            return Resend::Idle;
        };
        let need = stored.payload.len() as u64;
        if need > self.credit {
            return Resend::Blocked {
                need,
                available: self.credit,
            };
        }
        self.credit -= need;
        let mut frame = stored.clone();
        frame.sent_us = now_us;
        self.resend_next = (seq < self.last_sent()).then_some(seq + 1);
        Resend::Frame(frame)
    }

    /// Applies a cumulative `ack`; returns how many frames it released. Old acks are ignored.
    pub fn on_ack(&mut self, through: u64) -> Result<usize, Error> {
        if through > self.last_sent() {
            return Err(Error::AckAhead {
                chan: self.chan,
                ack: through,
                last_sent: self.last_sent(),
            });
        }
        Ok(self.release_through(through))
    }

    /// Applies `resume_ok`: frames up to `hwm` are released, the rest will be resent from
    /// `hwm + 1` (see [`poll_resend`](Self::poll_resend)). Returns the number released.
    pub fn on_resume_ok(&mut self, hwm: u64) -> Result<usize, Error> {
        if self.state != SenderState::Resuming {
            return Err(Error::Unexpected {
                chan: self.chan,
                kind: Kind::ResumeOk,
            });
        }
        if hwm > self.last_sent() {
            return Err(Error::ResumeAhead {
                chan: self.chan,
                hwm,
                last_sent: self.last_sent(),
            });
        }
        if hwm < self.acked {
            return Err(Error::ResumeBehind {
                chan: self.chan,
                hwm,
                acked: self.acked,
            });
        }
        let released = self.release_through(hwm);
        self.state = SenderState::Open;
        self.resend_next = (hwm < self.last_sent()).then_some(hwm + 1);
        Ok(released)
    }

    fn release_through(&mut self, through: u64) -> usize {
        if through <= self.acked {
            return 0;
        }
        let n = (through - self.acked) as usize;
        self.unacked.drain(..n);
        self.acked = through;
        if let Some(next) = self.resend_next {
            self.resend_next = if through >= self.last_sent() {
                None
            } else {
                Some(next.max(through + 1))
            };
        }
        n
    }
}
