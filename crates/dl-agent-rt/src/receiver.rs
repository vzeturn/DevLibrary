//! Receiving side of one channel: dedupe, gaps, cumulative acks after commit, credit, resume.

use crate::{Channel, Error, Frame, Kind};

/// What an arriving data frame is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// The next frame in order: apply it, then [`ChannelReceiver::commit`] it.
    Fresh,
    /// Already received (a resend after reconnect): do not apply again.
    Duplicate,
    /// Frames are missing before it: `expected` was due. Not accepted; the connection
    /// should be closed with [`CloseCode::PROTOCOL_ERROR`](crate::CloseCode::PROTOCOL_ERROR)
    /// and resumed.
    Gap {
        /// The sequence number that was due.
        expected: u64,
    },
}

/// Receiver state machine of one channel.
///
/// `received` is the highest seq accepted in order; `committed` (the high-water mark) is
/// the highest seq the caller reported durably stored. Only committed frames are acked, and
/// `resume_ok` answers with the committed mark, so a frame is never lost by a crash between
/// receipt and commit. Persist the high-water mark with the data and rebuild with
/// [`with_hwm`](Self::with_hwm).
#[derive(Debug, Clone)]
pub struct ChannelReceiver {
    chan: Channel,
    received: u64,
    committed: u64,
    credit: u64,
}

impl ChannelReceiver {
    /// A fresh channel: nothing received.
    pub fn new(chan: Channel) -> ChannelReceiver {
        ChannelReceiver::with_hwm(chan, 0)
    }

    /// A channel whose frames up to `hwm` are already committed.
    pub fn with_hwm(chan: Channel, hwm: u64) -> ChannelReceiver {
        ChannelReceiver {
            chan,
            received: hwm,
            committed: hwm,
            credit: 0,
        }
    }

    /// The channel.
    pub fn chan(&self) -> Channel {
        self.chan
    }

    /// Highest seq accepted in order.
    pub fn received(&self) -> u64 {
        self.received
    }

    /// High-water mark: highest seq committed (and acknowledged).
    pub fn committed(&self) -> u64 {
        self.committed
    }

    /// Payload bytes granted to the peer and not used yet on this connection.
    pub fn credit(&self) -> u64 {
        self.credit
    }

    /// The connection is gone: outstanding credit is void.
    pub fn disconnect(&mut self) {
        self.credit = 0;
    }

    /// Answers the peer's `resume`: the `resume_ok` frame carrying the high-water mark.
    /// A peer that saw acks beyond it means committed data was lost here: an error.
    pub fn on_resume(&mut self, last_acked: u64, now_us: u64) -> Result<Frame, Error> {
        if last_acked > self.committed {
            return Err(Error::ResumeConflict {
                chan: self.chan,
                last_acked,
                hwm: self.committed,
            });
        }
        Ok(Frame::resume_ok(self.chan, self.committed, now_us))
    }

    /// Grants the peer `bytes` more payload bytes; returns the `credit` frame to send.
    pub fn grant(&mut self, bytes: u64, now_us: u64) -> Frame {
        self.credit = self.credit.saturating_add(bytes);
        Frame::credit(self.chan, bytes, now_us)
    }

    /// Classifies an arriving data frame. Every data frame, duplicate or not, uses its
    /// payload length of credit, exactly as the sender counted it; more than granted is an error.
    pub fn on_data(&mut self, frame: &Frame) -> Result<Delivery, Error> {
        if frame.chan != self.chan {
            return Err(Error::WrongChannel {
                expected: self.chan,
                got: frame.chan,
            });
        }
        if frame.kind != Kind::Data {
            return Err(Error::Unexpected {
                chan: self.chan,
                kind: frame.kind,
            });
        }
        let need = frame.payload.len() as u64;
        if need > self.credit {
            return Err(Error::CreditExceeded {
                chan: self.chan,
                need,
                available: self.credit,
            });
        }
        self.credit -= need;
        Ok(if frame.seq <= self.received {
            Delivery::Duplicate
        } else if frame.seq == self.received + 1 {
            self.received = frame.seq;
            Delivery::Fresh
        } else {
            Delivery::Gap {
                expected: self.received + 1,
            }
        })
    }

    /// The caller has durably stored every frame up to `seq`: returns the cumulative `ack`
    /// to send, or `None` when `seq` was committed before.
    pub fn commit(&mut self, seq: u64, now_us: u64) -> Result<Option<Frame>, Error> {
        if seq > self.received {
            return Err(Error::CommitAhead {
                chan: self.chan,
                seq,
                received: self.received,
            });
        }
        if seq <= self.committed {
            return Ok(None);
        }
        self.committed = seq;
        Ok(Some(Frame::ack(self.chan, seq, now_us)))
    }

    /// Forgets frames received but not committed (their store failed or was lost), so their
    /// resends count as fresh again.
    pub fn rewind(&mut self) {
        self.received = self.committed;
    }
}
