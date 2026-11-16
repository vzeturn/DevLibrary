//! A WebSocket connection over any blocking byte stream.

use std::io::{self, Read, Write};

use crate::Error;
use crate::frame::{self, Decoder, Frame, MAX_CONTROL_PAYLOAD, OpCode};

/// Bytes requested from the stream per read.
const READ_CHUNK: usize = 16 * 1024;

/// Which end of the connection this is; decides the masking rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Sends masked frames, expects unmasked frames.
    Client,
    /// Sends unmasked frames, expects masked frames.
    Server,
}

/// Limits for one connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Largest message accepted, after reassembling fragments. Default 16 MiB.
    pub max_message_size: usize,
    /// Largest single frame accepted. Default 16 MiB.
    pub max_frame_size: usize,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            max_message_size: 16 << 20,
            max_frame_size: 16 << 20,
        }
    }
}

/// The payload of a close frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseFrame {
    /// Status code (RFC 6455 section 7.4), e.g. 1000 for a normal closure.
    pub code: u16,
    /// Reason text, at most 123 bytes of UTF-8.
    pub reason: String,
}

impl CloseFrame {
    /// A close frame with `code` and `reason`.
    pub fn new(code: u16, reason: impl Into<String>) -> CloseFrame {
        CloseFrame {
            code,
            reason: reason.into(),
        }
    }

    /// 1000, normal closure, without a reason.
    pub fn normal() -> CloseFrame {
        CloseFrame::new(1000, "")
    }
}

/// A message received or to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// A UTF-8 text message.
    Text(String),
    /// A binary message.
    Binary(Vec<u8>),
    /// A ping. [`WebSocket::read`] has already answered it with a pong.
    Ping(Vec<u8>),
    /// A pong.
    Pong(Vec<u8>),
    /// A close frame; `None` when the peer sent no status code.
    Close(Option<CloseFrame>),
}

/// Where the connection is in its life cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Messages flow both ways.
    Open,
    /// This end sent a close frame and waits for the peer's.
    CloseSent,
    /// The close handshake has completed.
    Closed,
    /// An error ended the connection.
    Failed,
}

/// Status codes a peer may send (RFC 6455 section 7.4 and the IANA registry).
fn is_valid_received_code(code: u16) -> bool {
    matches!(code, 1000..=1003 | 1007..=1014 | 3000..=4999)
}

/// Status codes this end may send.
fn is_valid_sent_code(code: u16) -> bool {
    matches!(code, 1000..=1003 | 1007..=1011 | 3000..=4999)
}

/// A WebSocket connection over a blocking stream `S`.
///
/// The connection is not split into halves: use it from one thread at a time. To send
/// while waiting for messages, set a short read timeout on the stream and treat
/// [`Error::is_timeout`] errors from [`WebSocket::read`] as "nothing yet"; a timeout
/// loses no data.
#[derive(Debug)]
pub struct WebSocket<S> {
    stream: S,
    role: Role,
    config: Config,
    decoder: Decoder,
    /// Opcode and payload of a fragmented message being reassembled.
    partial: Option<(OpCode, Vec<u8>)>,
    state: State,
    protocol: Option<String>,
    out: Vec<u8>,
}

impl<S: Read + Write> WebSocket<S> {
    /// Wraps a stream on which the opening handshake has already completed.
    pub fn from_raw(stream: S, role: Role, config: Config) -> WebSocket<S> {
        WebSocket {
            stream,
            role,
            config,
            decoder: Decoder::new(config.max_frame_size.min(config.max_message_size)),
            partial: None,
            state: State::Open,
            protocol: None,
            out: Vec::new(),
        }
    }

    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    pub(crate) fn set_protocol(&mut self, protocol: Option<String>) {
        self.protocol = protocol;
    }

    /// The subprotocol the server selected, if any.
    pub fn protocol(&self) -> Option<&str> {
        self.protocol.as_deref()
    }

    /// This end's role.
    pub fn role(&self) -> Role {
        self.role
    }

    /// The limits in use.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The connection state.
    pub fn state(&self) -> State {
        self.state
    }

    /// Whether messages can still be sent.
    pub fn is_open(&self) -> bool {
        self.state == State::Open
    }

    /// The underlying stream.
    pub fn get_ref(&self) -> &S {
        &self.stream
    }

    /// The underlying stream, mutably. Reading or writing it directly corrupts the
    /// framing.
    pub fn get_mut(&mut self) -> &mut S {
        &mut self.stream
    }

    /// Returns the stream.
    pub fn into_inner(self) -> S {
        self.stream
    }

    /// Reads the next message.
    ///
    /// Pings are answered with a pong before they are returned. A close frame from the
    /// peer is answered (when this end has not sent one yet) and returned as
    /// [`Message::Close`]; after that every call returns [`Error::Closed`].
    ///
    /// # Errors
    /// [`Error::Io`] for stream errors (a timeout leaves the connection usable),
    /// [`Error::ConnectionReset`] when the stream ends without a close frame, and
    /// [`Error::Protocol`], [`Error::InvalidUtf8`] or [`Error::MessageTooBig`] when the
    /// peer misbehaves; those also send the matching close frame and end the connection.
    pub fn read(&mut self) -> Result<Message, Error> {
        loop {
            if matches!(self.state, State::Closed | State::Failed) {
                return Err(Error::Closed);
            }
            match self.decoder.next_frame() {
                Ok(Some(f)) => match self.handle(f) {
                    Ok(Some(m)) => return Ok(m),
                    Ok(None) => continue,
                    Err(e) => return Err(self.fail(e)),
                },
                Ok(None) => {}
                Err(e) => return Err(self.fail(e)),
            }
            let buf = self.decoder.buffer(READ_CHUNK);
            match self.stream.read(buf) {
                Ok(0) => {
                    self.decoder.commit(READ_CHUNK, 0);
                    self.state = State::Failed;
                    return Err(Error::ConnectionReset);
                }
                Ok(n) => self.decoder.commit(READ_CHUNK, n),
                Err(e) => {
                    self.decoder.commit(READ_CHUNK, 0);
                    if e.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    if !matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) {
                        self.state = State::Failed;
                    }
                    return Err(Error::Io(e));
                }
            }
        }
    }

    /// Ends the connection after a peer error, sending the matching close code.
    fn fail(&mut self, e: Error) -> Error {
        if let Some(code) = e.close_code()
            && self.state == State::Open
        {
            let _ = self.write_frame(OpCode::Close, &code.to_be_bytes());
        }
        self.state = State::Failed;
        e
    }

    fn handle(&mut self, f: Frame) -> Result<Option<Message>, Error> {
        match (self.role, f.masked) {
            (Role::Client, true) => return Err(Error::Protocol("masked frame from the server")),
            (Role::Server, false) => {
                return Err(Error::Protocol("unmasked frame from the client"));
            }
            _ => {}
        }
        match f.opcode {
            OpCode::Ping => {
                if self.state == State::Open {
                    self.write_frame(OpCode::Pong, &f.payload)?;
                }
                Ok(Some(Message::Ping(f.payload)))
            }
            OpCode::Pong => Ok(Some(Message::Pong(f.payload))),
            OpCode::Close => {
                let frame = match f.payload.len() {
                    0 => None,
                    1 => return Err(Error::Protocol("close payload of one byte")),
                    _ => {
                        let code = u16::from_be_bytes([f.payload[0], f.payload[1]]);
                        if !is_valid_received_code(code) {
                            return Err(Error::Protocol("invalid close code"));
                        }
                        let reason = String::from_utf8(f.payload[2..].to_vec())
                            .map_err(|_| Error::InvalidUtf8)?;
                        Some(CloseFrame { code, reason })
                    }
                };
                if self.state == State::Open {
                    // Echo the status code (RFC 6455 section 5.5.1).
                    let echo: Vec<u8> = frame
                        .as_ref()
                        .map(|c| c.code.to_be_bytes().to_vec())
                        .unwrap_or_default();
                    let _ = self.write_frame(OpCode::Close, &echo);
                }
                self.state = State::Closed;
                Ok(Some(Message::Close(frame)))
            }
            OpCode::Text | OpCode::Binary => {
                if self.partial.is_some() {
                    return Err(Error::Protocol(
                        "new message before the fragmented one finished",
                    ));
                }
                if f.fin {
                    return finish(f.opcode, f.payload).map(Some);
                }
                self.partial = Some((f.opcode, f.payload));
                Ok(None)
            }
            OpCode::Continuation => {
                let Some((op, mut data)) = self.partial.take() else {
                    return Err(Error::Protocol("continuation frame without a message"));
                };
                if data.len() + f.payload.len() > self.config.max_message_size {
                    return Err(Error::MessageTooBig {
                        limit: self.config.max_message_size,
                    });
                }
                data.extend_from_slice(&f.payload);
                if f.fin {
                    return finish(op, data).map(Some);
                }
                self.partial = Some((op, data));
                Ok(None)
            }
        }
    }

    /// Writes one frame (masked for a client) with a single `write_all` and a flush.
    fn write_frame(&mut self, opcode: OpCode, payload: &[u8]) -> Result<(), Error> {
        let mask = match self.role {
            Role::Client => {
                let r = dl_rand::u64().to_le_bytes();
                Some([r[0], r[1], r[2], r[3]])
            }
            Role::Server => None,
        };
        self.out.clear();
        frame::encode(&mut self.out, true, opcode, payload, mask);
        let r = self
            .stream
            .write_all(&self.out)
            .and_then(|()| self.stream.flush());
        if self.out.capacity() > 64 * 1024 {
            self.out = Vec::new();
        }
        r.map_err(|e| {
            // A partly written frame cannot be resumed.
            self.state = State::Failed;
            Error::Io(e)
        })
    }

    fn send_data(&mut self, opcode: OpCode, payload: &[u8]) -> Result<(), Error> {
        match self.state {
            State::Open => self.write_frame(opcode, payload),
            State::CloseSent => Err(Error::InvalidInput("send after close")),
            State::Closed | State::Failed => Err(Error::Closed),
        }
    }

    /// Sends a text message in one frame.
    ///
    /// # Errors
    /// [`Error::Io`] (the connection is then unusable), [`Error::Closed`], or
    /// [`Error::InvalidInput`] after [`WebSocket::close`].
    pub fn send_text(&mut self, text: &str) -> Result<(), Error> {
        self.send_data(OpCode::Text, text.as_bytes())
    }

    /// Sends a binary message in one frame.
    ///
    /// # Errors
    /// As [`WebSocket::send_text`].
    pub fn send_binary(&mut self, data: &[u8]) -> Result<(), Error> {
        self.send_data(OpCode::Binary, data)
    }

    /// Sends a ping.
    ///
    /// # Errors
    /// [`Error::InvalidInput`] for a payload over 125 bytes, otherwise as
    /// [`WebSocket::send_text`].
    pub fn ping(&mut self, payload: &[u8]) -> Result<(), Error> {
        if payload.len() > MAX_CONTROL_PAYLOAD {
            return Err(Error::InvalidInput("ping payload over 125 bytes"));
        }
        self.send_data(OpCode::Ping, payload)
    }

    /// Sends a pong (an unsolicited pong works as a one-way heartbeat).
    ///
    /// # Errors
    /// As [`WebSocket::ping`].
    pub fn pong(&mut self, payload: &[u8]) -> Result<(), Error> {
        if payload.len() > MAX_CONTROL_PAYLOAD {
            return Err(Error::InvalidInput("pong payload over 125 bytes"));
        }
        self.send_data(OpCode::Pong, payload)
    }

    /// Sends `message`; [`Message::Close`] calls [`WebSocket::close`].
    ///
    /// # Errors
    /// As the specific send method.
    pub fn send(&mut self, message: &Message) -> Result<(), Error> {
        match message {
            Message::Text(t) => self.send_text(t),
            Message::Binary(b) => self.send_binary(b),
            Message::Ping(p) => self.ping(p),
            Message::Pong(p) => self.pong(p),
            Message::Close(c) => self.close(c.clone()),
        }
    }

    /// Starts the close handshake. Keep calling [`WebSocket::read`] until it returns
    /// [`Message::Close`] (or an error) to complete it. Calling it again is a no-op.
    ///
    /// # Errors
    /// [`Error::InvalidInput`] for a code this end may not send (1004-1006, 1015, below
    /// 1000, 1016-2999, 5000 and up) or a reason over 123 bytes; [`Error::Io`].
    pub fn close(&mut self, frame: Option<CloseFrame>) -> Result<(), Error> {
        let mut payload = Vec::new();
        if let Some(c) = &frame {
            if !is_valid_sent_code(c.code) {
                return Err(Error::InvalidInput("close code may not be sent"));
            }
            if c.reason.len() > MAX_CONTROL_PAYLOAD - 2 {
                return Err(Error::InvalidInput("close reason over 123 bytes"));
            }
            payload.extend_from_slice(&c.code.to_be_bytes());
            payload.extend_from_slice(c.reason.as_bytes());
        }
        match self.state {
            State::Open => {
                self.write_frame(OpCode::Close, &payload)?;
                self.state = State::CloseSent;
                Ok(())
            }
            State::CloseSent | State::Closed => Ok(()),
            State::Failed => Err(Error::Closed),
        }
    }
}

fn finish(opcode: OpCode, data: Vec<u8>) -> Result<Message, Error> {
    if opcode == OpCode::Text {
        String::from_utf8(data)
            .map(Message::Text)
            .map_err(|_| Error::InvalidUtf8)
    } else {
        Ok(Message::Binary(data))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// An in-memory stream: reads come from `input` (`None` entries simulate timeouts),
    /// writes collect in `output`.
    #[derive(Default)]
    struct Mem {
        input: VecDeque<Option<Vec<u8>>>,
        output: Vec<u8>,
    }

    impl Read for Mem {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.input.pop_front() {
                None => Ok(0),
                Some(None) => Err(io::ErrorKind::WouldBlock.into()),
                Some(Some(mut chunk)) => {
                    let n = buf.len().min(chunk.len());
                    buf[..n].copy_from_slice(&chunk[..n]);
                    if n < chunk.len() {
                        chunk.drain(..n);
                        self.input.push_front(Some(chunk));
                    }
                    Ok(n)
                }
            }
        }
    }

    impl Write for Mem {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn server_frame(fin: bool, op: OpCode, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        frame::encode(&mut v, fin, op, payload, None);
        v
    }

    fn client(chunks: Vec<Option<Vec<u8>>>) -> WebSocket<Mem> {
        WebSocket::from_raw(
            Mem {
                input: chunks.into(),
                output: Vec::new(),
            },
            Role::Client,
            Config::default(),
        )
    }

    /// Frames this client wrote, decoded with the server's view (masked).
    fn sent(ws: &WebSocket<Mem>) -> Vec<Frame> {
        let mut d = Decoder::new(1 << 20);
        d.extend(&ws.get_ref().output);
        let mut out = Vec::new();
        while let Some(f) = d.next_frame().unwrap() {
            assert!(f.masked, "client frames must be masked");
            out.push(f);
        }
        out
    }

    #[test]
    fn text_binary_and_fragments() {
        let mut wire = server_frame(true, OpCode::Text, "xin chào".as_bytes());
        wire.extend(server_frame(false, OpCode::Binary, &[1, 2]));
        wire.extend(server_frame(true, OpCode::Ping, b"hb")); // interleaved control frame
        wire.extend(server_frame(false, OpCode::Continuation, &[3]));
        wire.extend(server_frame(true, OpCode::Continuation, &[4]));
        let mut ws = client(vec![Some(wire)]);
        assert_eq!(ws.read().unwrap(), Message::Text("xin chào".into()));
        assert_eq!(ws.read().unwrap(), Message::Ping(b"hb".to_vec()));
        assert_eq!(ws.read().unwrap(), Message::Binary(vec![1, 2, 3, 4]));
        let f = sent(&ws);
        assert_eq!(f.len(), 1);
        assert_eq!(
            (f[0].opcode, f[0].payload.as_slice()),
            (OpCode::Pong, &b"hb"[..])
        );
        assert!(matches!(ws.read(), Err(Error::ConnectionReset)));
        assert!(matches!(ws.read(), Err(Error::Closed)));
    }

    #[test]
    fn timeouts_lose_nothing() {
        let wire = server_frame(true, OpCode::Text, b"split across reads");
        let (a, b) = wire.split_at(5);
        let mut ws = client(vec![Some(a.to_vec()), None, Some(b.to_vec())]);
        let e = ws.read().unwrap_err();
        assert!(e.is_timeout());
        assert!(ws.is_open());
        assert_eq!(
            ws.read().unwrap(),
            Message::Text("split across reads".into())
        );
    }

    #[test]
    fn close_handshake_initiated_by_peer() {
        let mut payload = 1001u16.to_be_bytes().to_vec();
        payload.extend_from_slice(b"going away");
        let mut ws = client(vec![Some(server_frame(true, OpCode::Close, &payload))]);
        assert_eq!(
            ws.read().unwrap(),
            Message::Close(Some(CloseFrame::new(1001, "going away")))
        );
        assert_eq!(ws.state(), State::Closed);
        let f = sent(&ws);
        assert_eq!(
            (f[0].opcode, f[0].payload.as_slice()),
            (OpCode::Close, &[0x03, 0xE9][..])
        );
        assert!(matches!(ws.send_text("x"), Err(Error::Closed)));
        assert!(matches!(ws.read(), Err(Error::Closed)));
    }

    #[test]
    fn close_handshake_initiated_here() {
        let mut ws = client(vec![
            Some(server_frame(true, OpCode::Text, b"late")),
            Some(server_frame(true, OpCode::Close, &1000u16.to_be_bytes())),
        ]);
        ws.close(Some(CloseFrame::new(1000, "bye"))).unwrap();
        assert_eq!(ws.state(), State::CloseSent);
        assert!(matches!(ws.send_text("x"), Err(Error::InvalidInput(_))));
        assert_eq!(ws.read().unwrap(), Message::Text("late".into()));
        assert_eq!(
            ws.read().unwrap(),
            Message::Close(Some(CloseFrame::new(1000, "")))
        );
        assert_eq!(ws.state(), State::Closed);
        // Only our one close frame went out (no echo of the reply).
        let f = sent(&ws);
        assert_eq!(f.len(), 1);
        assert_eq!(&f[0].payload[2..], b"bye");
        ws.close(None).unwrap();
    }

    #[test]
    fn peer_errors_send_the_matching_close_code() {
        let cases: Vec<(Vec<u8>, u16)> = vec![
            (server_frame(true, OpCode::Text, &[0xff, 0xfe]), 1007),
            (server_frame(true, OpCode::Continuation, b"x"), 1002),
            (server_frame(true, OpCode::Close, &[0x03]), 1002),
            (
                server_frame(true, OpCode::Close, &1005u16.to_be_bytes()),
                1002,
            ),
            (
                {
                    let mut v = Vec::new();
                    frame::encode(&mut v, true, OpCode::Text, b"m", Some([1, 2, 3, 4]));
                    v
                },
                1002,
            ),
            (
                {
                    let mut v = server_frame(false, OpCode::Text, b"a");
                    v.extend(server_frame(true, OpCode::Binary, b"b"));
                    v
                },
                1002,
            ),
        ];
        for (wire, code) in cases {
            let mut ws = client(vec![Some(wire)]);
            let e = ws.read().unwrap_err();
            assert_eq!(e.close_code(), Some(code), "{e}");
            assert_eq!(ws.state(), State::Failed);
            let f = sent(&ws);
            assert_eq!(f.last().unwrap().opcode, OpCode::Close);
            assert_eq!(f.last().unwrap().payload, code.to_be_bytes());
        }
    }

    #[test]
    fn message_limit_applies_to_reassembled_messages() {
        let mut wire = server_frame(false, OpCode::Binary, &[0; 6]);
        wire.extend(server_frame(true, OpCode::Continuation, &[0; 6]));
        let mut ws = WebSocket::from_raw(
            Mem {
                input: vec![Some(wire)].into(),
                output: Vec::new(),
            },
            Role::Client,
            Config {
                max_message_size: 10,
                max_frame_size: 10,
            },
        );
        assert!(matches!(ws.read(), Err(Error::MessageTooBig { limit: 10 })));
    }

    #[test]
    fn send_side_rules() {
        let mut ws = client(vec![]);
        ws.send_text("a").unwrap();
        ws.send_binary(&[1]).unwrap();
        ws.ping(b"p").unwrap();
        assert!(ws.ping(&[0; 126]).is_err());
        assert!(ws.close(Some(CloseFrame::new(1005, ""))).is_err());
        assert!(
            ws.close(Some(CloseFrame::new(1000, "x".repeat(124))))
                .is_err()
        );
        let f = sent(&ws);
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].payload, b"a");

        let mut srv = WebSocket::from_raw(Mem::default(), Role::Server, Config::default());
        srv.send_text("hi").unwrap();
        assert_eq!(srv.get_ref().output, [0x81, 0x02, b'h', b'i']);
    }

    #[test]
    fn server_role_requires_masked_frames() {
        let mut srv = WebSocket::from_raw(
            Mem {
                input: vec![Some(server_frame(true, OpCode::Text, b"x"))].into(),
                output: Vec::new(),
            },
            Role::Server,
            Config::default(),
        );
        assert!(matches!(srv.read(), Err(Error::Protocol(_))));
    }
}
