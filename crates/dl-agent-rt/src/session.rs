//! One end of a dl-rt/1 session: a sender and a receiver per channel, the connection
//! handshake and the writer's priority order.

use alloc::collections::VecDeque;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::receiver::{ChannelReceiver, Delivery};
use crate::sender::{Blocked, ChannelSender, Outcome, Resend, WouldBlock};
use crate::{Channel, CloseCode, Error, Ext, Frame, Kind, Limits};

/// Settings of a [`Session`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Codec limits.
    pub limits: Limits,
    /// Credit granted to the peer per channel (indexed by [`Channel::index`]) when it resumes
    /// on a new connection. Grant at least one largest payload, or the peer cannot send a
    /// large frame. 0 grants nothing; the caller then grants with [`Session::grant`].
    pub initial_credit: [u64; 5],
}

impl Default for Config {
    /// Default limits and one largest payload (about 1 MiB) of initial credit per channel.
    fn default() -> Config {
        let limits = Limits::default();
        Config {
            limits,
            initial_credit: [limits.max_payload() as u64; 5],
        }
    }
}

/// Connection state of a [`Session`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// No connection.
    Disconnected,
    /// Connected (channels may still be resuming).
    Open,
    /// A close frame was sent or received; drop the socket and call `disconnect`.
    Closing,
}

/// What an incoming frame meant. Frames to answer are queued for [`Session::poll_outgoing`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// New data in order: apply it, then call [`Session::commit`].
    Data(Frame),
    /// A resend of data already received; not to be applied again.
    Duplicate {
        /// Channel.
        chan: Channel,
        /// Its sequence number.
        seq: u64,
    },
    /// Data out of order: frames are missing. Close with `PROTOCOL_ERROR` and reconnect.
    Gap {
        /// Channel.
        chan: Channel,
        /// The sequence number that was due.
        expected: u64,
        /// The sequence number that came.
        got: u64,
    },
    /// The peer acknowledged frames.
    Acked {
        /// Channel.
        chan: Channel,
        /// Highest acknowledged seq so far.
        through: u64,
        /// Frames released by this ack (their outbox rows may be deleted).
        released: usize,
    },
    /// The peer granted credit.
    Credit {
        /// Channel.
        chan: Channel,
        /// Bytes granted.
        bytes: u64,
    },
    /// The peer resumed one of its channels; `resume_ok` (and initial credit) are queued.
    PeerResumed {
        /// Channel.
        chan: Channel,
        /// The last ack the peer saw.
        last_acked: u64,
        /// Our high-water mark sent back.
        hwm: u64,
    },
    /// One of our channels is resumed.
    Resumed {
        /// Channel.
        chan: Channel,
        /// The peer's high-water mark.
        hwm: u64,
        /// Frames after it that will be resent.
        resending: u64,
    },
    /// A ping arrived; its pong is queued.
    Ping {
        /// The ping's seq.
        seq: u64,
    },
    /// Our ping came back.
    Pong {
        /// The ping's seq.
        seq: u64,
        /// Round trip in microseconds (`now_us - sent_us`).
        rtt_us: u64,
    },
    /// The peer closes the connection.
    Closed {
        /// Reason code.
        code: CloseCode,
        /// Reason text.
        reason: String,
    },
}

/// Writer priority class: control first, then capture/commands/results, then bulk.
fn class(f: &Frame) -> usize {
    match (f.kind, f.chan) {
        (Kind::Data, Channel::Bulk) => 2,
        (Kind::Data, Channel::Control) => 0,
        (Kind::Data, _) => 1,
        _ => 0,
    }
}

/// One end of a session (agent or cloud). Pure state: the caller moves bytes and time.
///
/// Every connection starts with [`connect`](Self::connect), which queues a `resume` per
/// channel; the peer answers each with `resume_ok` and initial credit. Frames to write
/// come from [`poll_outgoing`](Self::poll_outgoing) in priority order; frames read go to
/// [`handle`](Self::handle).
#[derive(Debug, Clone)]
pub struct Session {
    config: Config,
    senders: [ChannelSender; 5],
    receivers: [ChannelReceiver; 5],
    queues: [VecDeque<Frame>; 3],
    state: SessionState,
    next_ping: u64,
}

impl Session {
    /// A session with nothing sent or received.
    pub fn new(config: Config) -> Session {
        Session {
            config,
            senders: Channel::ALL.map(ChannelSender::new),
            receivers: Channel::ALL.map(ChannelReceiver::new),
            queues: Default::default(),
            state: SessionState::Disconnected,
            next_ping: 1,
        }
    }

    /// A session rebuilt from persisted senders and receivers, in [`Channel::ALL`] order.
    pub fn from_parts(
        config: Config,
        senders: [ChannelSender; 5],
        receivers: [ChannelReceiver; 5],
    ) -> Result<Session, Error> {
        for (i, chan) in Channel::ALL.into_iter().enumerate() {
            for got in [senders[i].chan(), receivers[i].chan()] {
                if got != chan {
                    return Err(Error::WrongChannel {
                        expected: chan,
                        got,
                    });
                }
            }
        }
        let mut s = Session::new(config);
        s.senders = senders;
        s.receivers = receivers;
        Ok(s)
    }

    /// Settings.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Connection state.
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// The sender of a channel.
    pub fn sender(&self, chan: Channel) -> &ChannelSender {
        &self.senders[chan.index()]
    }

    /// The receiver of a channel.
    pub fn receiver(&self, chan: Channel) -> &ChannelReceiver {
        &self.receivers[chan.index()]
    }

    /// A new connection is up: queues one `resume` per channel.
    pub fn connect(&mut self, now_us: u64) {
        self.disconnect();
        self.state = SessionState::Open;
        for s in &mut self.senders {
            self.queues[0].push_back(s.connect(now_us));
        }
    }

    /// The connection is gone: queued frames and credit are dropped; unacknowledged data
    /// stays for the next resume.
    pub fn disconnect(&mut self) {
        self.state = SessionState::Disconnected;
        self.senders.iter_mut().for_each(ChannelSender::disconnect);
        self.receivers
            .iter_mut()
            .for_each(ChannelReceiver::disconnect);
        self.queues.iter_mut().for_each(VecDeque::clear);
    }

    /// Queues a data frame; returns its seq, or the payload back when it has to wait.
    pub fn send(
        &mut self,
        chan: Channel,
        payload: Vec<u8>,
        ext: Ext,
        now_us: u64,
    ) -> Result<Outcome<u64>, Error> {
        let reason = match self.state {
            SessionState::Open => None,
            SessionState::Disconnected => Some(Blocked::Disconnected),
            SessionState::Closing => Some(Blocked::Closing),
        };
        if let Some(reason) = reason {
            return Ok(Outcome::WouldBlock(WouldBlock { payload, reason }));
        }
        let limits = self.config.limits;
        Ok(
            match self.senders[chan.index()].try_send(payload, ext, now_us, &limits)? {
                Outcome::Done(frame) => {
                    let seq = frame.seq;
                    self.queues[class(&frame)].push_back(frame);
                    Outcome::Done(seq)
                }
                Outcome::WouldBlock(w) => Outcome::WouldBlock(w),
            },
        )
    }

    /// Every received frame of `chan` up to `seq` is durably stored: queues the ack.
    pub fn commit(&mut self, chan: Channel, seq: u64, now_us: u64) -> Result<(), Error> {
        let ack = self.receivers[chan.index()].commit(seq, now_us)?;
        if let (Some(ack), SessionState::Open) = (ack, self.state) {
            self.queues[0].push_back(ack);
        }
        Ok(())
    }

    /// Frames received but not committed were lost (see [`ChannelReceiver::rewind`]).
    pub fn rewind(&mut self, chan: Channel) {
        self.receivers[chan.index()].rewind();
    }

    /// Grants the peer `bytes` more payload bytes on `chan`.
    pub fn grant(&mut self, chan: Channel, bytes: u64, now_us: u64) -> Result<(), Error> {
        if self.state != SessionState::Open {
            return Err(Error::NotConnected);
        }
        let f = self.receivers[chan.index()].grant(bytes, now_us);
        self.queues[0].push_back(f);
        Ok(())
    }

    /// Queues a ping; returns its seq.
    pub fn ping(&mut self, payload: Vec<u8>, now_us: u64) -> Result<u64, Error> {
        if self.state != SessionState::Open {
            return Err(Error::NotConnected);
        }
        let seq = self.next_ping;
        self.next_ping = self.next_ping.wrapping_add(1);
        self.queues[0].push_back(Frame::ping(seq, now_us, payload));
        Ok(seq)
    }

    /// Queues a close frame; data sends stop. Write the queue out, then drop the socket.
    pub fn close(&mut self, code: CloseCode, reason: &str, now_us: u64) -> Result<(), Error> {
        if self.state != SessionState::Open {
            return Err(Error::NotConnected);
        }
        let f = Frame::close(code, reason, now_us)?;
        self.queues[0].push_back(f);
        self.state = SessionState::Closing;
        Ok(())
    }

    /// The next frame to write: control first, then capture/commands/results, then bulk;
    /// first in, first out within a class (so seq order holds within each channel).
    pub fn poll_outgoing(&mut self) -> Option<Frame> {
        self.queues.iter_mut().find_map(VecDeque::pop_front)
    }

    /// Frames waiting in the write queue.
    pub fn pending_outgoing(&self) -> usize {
        self.queues.iter().map(VecDeque::len).sum()
    }

    fn pull_resends(&mut self, chan: Channel, now_us: u64) {
        while let Resend::Frame(f) = self.senders[chan.index()].poll_resend(now_us) {
            self.queues[class(&f)].push_back(f);
        }
    }

    /// Processes one frame read from the connection. Errors are protocol violations by
    /// the peer: close the connection with a matching [`CloseCode`] and reconnect.
    pub fn handle(&mut self, frame: Frame, now_us: u64) -> Result<Event, Error> {
        if self.state == SessionState::Disconnected {
            return Err(Error::NotConnected);
        }
        frame.validate()?;
        let chan = frame.chan;
        let i = chan.index();
        Ok(match frame.kind {
            Kind::Data => match self.receivers[i].on_data(&frame)? {
                Delivery::Fresh => Event::Data(frame),
                Delivery::Duplicate => Event::Duplicate {
                    chan,
                    seq: frame.seq,
                },
                Delivery::Gap { expected } => Event::Gap {
                    chan,
                    expected,
                    got: frame.seq,
                },
            },
            Kind::Ack => {
                let released = self.senders[i].on_ack(frame.seq)?;
                self.pull_resends(chan, now_us);
                Event::Acked {
                    chan,
                    through: self.senders[i].acked(),
                    released,
                }
            }
            Kind::Credit => {
                // validate() proved the 8-byte payload.
                let bytes = frame.credit_bytes().unwrap_or(0);
                self.senders[i].grant(bytes);
                self.pull_resends(chan, now_us);
                Event::Credit { chan, bytes }
            }
            Kind::Resume => {
                let ok = self.receivers[i].on_resume(frame.seq, now_us)?;
                let hwm = ok.seq;
                self.queues[0].push_back(ok);
                let initial = self.config.initial_credit[i];
                if initial > 0 {
                    let credit = self.receivers[i].grant(initial, now_us);
                    self.queues[0].push_back(credit);
                }
                Event::PeerResumed {
                    chan,
                    last_acked: frame.seq,
                    hwm,
                }
            }
            Kind::ResumeOk => {
                self.senders[i].on_resume_ok(frame.seq)?;
                self.pull_resends(chan, now_us);
                Event::Resumed {
                    chan,
                    hwm: frame.seq,
                    resending: self.senders[i].last_sent() - frame.seq,
                }
            }
            Kind::Ping => {
                self.queues[0].push_back(frame.pong_of());
                Event::Ping { seq: frame.seq }
            }
            Kind::Pong => Event::Pong {
                seq: frame.seq,
                rtt_us: now_us.saturating_sub(frame.sent_us),
            },
            Kind::Close => {
                self.state = SessionState::Closing;
                Event::Closed {
                    code: frame.close_code().unwrap_or(CloseCode::PROTOCOL_ERROR),
                    reason: frame.close_reason().unwrap_or_default().to_string(),
                }
            }
        })
    }
}
