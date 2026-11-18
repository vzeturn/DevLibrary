//! RFC 6455 section 5 framing: frame headers, masking and an incremental decoder.

use crate::Error;

/// Frame opcodes (RFC 6455 section 5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpCode {
    /// Continuation of a fragmented message.
    Continuation,
    /// UTF-8 text.
    Text,
    /// Binary data.
    Binary,
    /// Close.
    Close,
    /// Ping.
    Ping,
    /// Pong.
    Pong,
}

impl OpCode {
    /// The 4-bit wire value.
    pub fn as_u8(self) -> u8 {
        match self {
            OpCode::Continuation => 0x0,
            OpCode::Text => 0x1,
            OpCode::Binary => 0x2,
            OpCode::Close => 0x8,
            OpCode::Ping => 0x9,
            OpCode::Pong => 0xA,
        }
    }

    /// The opcode for a wire value; `None` for the reserved values 3-7 and B-F.
    pub fn from_u8(v: u8) -> Option<OpCode> {
        Some(match v {
            0x0 => OpCode::Continuation,
            0x1 => OpCode::Text,
            0x2 => OpCode::Binary,
            0x8 => OpCode::Close,
            0x9 => OpCode::Ping,
            0xA => OpCode::Pong,
            _ => return None,
        })
    }

    /// Close, Ping and Pong.
    pub fn is_control(self) -> bool {
        matches!(self, OpCode::Close | OpCode::Ping | OpCode::Pong)
    }
}

/// Largest payload of a control frame.
pub const MAX_CONTROL_PAYLOAD: usize = 125;

/// XORs `data` with the 4-byte `key`, as if `data` started at payload offset `offset`.
pub fn apply_mask(data: &mut [u8], key: [u8; 4], offset: usize) {
    let mut k = key;
    k.rotate_left(offset % 4);
    let wide = u64::from_ne_bytes([k[0], k[1], k[2], k[3], k[0], k[1], k[2], k[3]]);
    let (chunks, rest) = data.as_chunks_mut::<8>();
    for c in chunks {
        *c = (u64::from_ne_bytes(*c) ^ wide).to_ne_bytes();
    }
    for (i, b) in rest.iter_mut().enumerate() {
        *b ^= k[i % 4];
    }
}

/// Appends one frame to `out`. With `mask`, the payload is masked with that key (client
/// to server frames must be masked, server to client frames must not).
pub fn encode(out: &mut Vec<u8>, fin: bool, opcode: OpCode, payload: &[u8], mask: Option<[u8; 4]>) {
    let b0 = (if fin { 0x80 } else { 0 }) | opcode.as_u8();
    let m = if mask.is_some() { 0x80 } else { 0 };
    let len = payload.len();
    out.reserve(14 + len);
    out.push(b0);
    if len < 126 {
        out.push(m | len as u8);
    } else if let Ok(l) = u16::try_from(len) {
        out.push(m | 126);
        out.extend_from_slice(&l.to_be_bytes());
    } else {
        out.push(m | 127);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }
    match mask {
        Some(key) => {
            out.extend_from_slice(&key);
            let start = out.len();
            out.extend_from_slice(payload);
            apply_mask(&mut out[start..], key, 0);
        }
        None => out.extend_from_slice(payload),
    }
}

/// One decoded frame with its payload unmasked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Final fragment of a message.
    pub fin: bool,
    /// The opcode.
    pub opcode: OpCode,
    /// Whether the frame was masked on the wire.
    pub masked: bool,
    /// The payload, unmasked.
    pub payload: Vec<u8>,
}

/// Incremental frame decoder: feed it bytes with [`Decoder::buffer`] and take whole frames
/// with [`Decoder::next_frame`]. Partial frames stay buffered.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
    start: usize,
    max_frame: usize,
}

impl Decoder {
    /// A decoder that rejects frames with a payload larger than `max_frame` bytes.
    pub fn new(max_frame: usize) -> Decoder {
        Decoder {
            buf: Vec::new(),
            start: 0,
            max_frame,
        }
    }

    /// Appends received bytes.
    pub fn extend(&mut self, data: &[u8]) {
        self.compact();
        self.buf.extend_from_slice(data);
    }

    /// Exposes `n` writable bytes at the end of the buffer for a socket read; call
    /// [`Decoder::commit`] with the count actually read.
    pub fn buffer(&mut self, n: usize) -> &mut [u8] {
        self.compact();
        let old = self.buf.len();
        self.buf.resize(old + n, 0);
        &mut self.buf[old..]
    }

    /// Keeps `read` of the `n` bytes exposed by the last [`Decoder::buffer`] call.
    pub fn commit(&mut self, n: usize, read: usize) {
        let keep = self.buf.len() - n + read.min(n);
        self.buf.truncate(keep);
    }

    /// Bytes buffered and not yet returned as frames.
    pub fn pending(&self) -> usize {
        self.buf.len() - self.start
    }

    fn compact(&mut self) {
        if self.start > 0 && (self.start == self.buf.len() || self.start >= 64 * 1024) {
            self.buf.drain(..self.start);
            self.start = 0;
        }
    }

    /// The next whole frame, `None` if more bytes are needed.
    ///
    /// # Errors
    /// [`Error::Protocol`] for reserved bits or opcodes, a non-minimal or 64-bit length
    /// with the top bit set, or a control frame that is fragmented or longer than 125
    /// bytes; [`Error::MessageTooBig`] for a payload over the frame limit.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, Error> {
        let b = &self.buf[self.start..];
        if b.len() < 2 {
            return Ok(None);
        }
        let fin = b[0] & 0x80 != 0;
        if b[0] & 0x70 != 0 {
            return Err(Error::Protocol(
                "reserved bits set without a negotiated extension",
            ));
        }
        let opcode = OpCode::from_u8(b[0] & 0x0F).ok_or(Error::Protocol("reserved opcode"))?;
        let masked = b[1] & 0x80 != 0;
        let (len, mut at) = match b[1] & 0x7F {
            126 => {
                if b.len() < 4 {
                    return Ok(None);
                }
                let l = u64::from(u16::from_be_bytes([b[2], b[3]]));
                if l < 126 {
                    return Err(Error::Protocol("payload length not minimally encoded"));
                }
                (l, 4)
            }
            127 => {
                if b.len() < 10 {
                    return Ok(None);
                }
                let mut l8 = [0u8; 8];
                l8.copy_from_slice(&b[2..10]);
                let l = u64::from_be_bytes(l8);
                if l >> 63 != 0 {
                    return Err(Error::Protocol("payload length has the top bit set"));
                }
                if l <= u64::from(u16::MAX) {
                    return Err(Error::Protocol("payload length not minimally encoded"));
                }
                (l, 10)
            }
            l => (u64::from(l), 2),
        };
        if opcode.is_control() {
            if !fin {
                return Err(Error::Protocol("fragmented control frame"));
            }
            if len > MAX_CONTROL_PAYLOAD as u64 {
                return Err(Error::Protocol("control frame payload over 125 bytes"));
            }
        }
        if len > self.max_frame as u64 {
            return Err(Error::MessageTooBig {
                limit: self.max_frame,
            });
        }
        // len <= max_frame, which is a usize.
        let len = len as usize;
        let key = if masked {
            if b.len() < at + 4 {
                return Ok(None);
            }
            let k = [b[at], b[at + 1], b[at + 2], b[at + 3]];
            at += 4;
            Some(k)
        } else {
            None
        };
        if b.len() < at + len {
            return Ok(None);
        }
        let mut payload = b[at..at + len].to_vec();
        if let Some(k) = key {
            apply_mask(&mut payload, k, 0);
        }
        self.start += at + len;
        Ok(Some(Frame {
            fin,
            opcode,
            masked,
            payload,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_all(bytes: &[u8]) -> Result<Vec<Frame>, Error> {
        let mut d = Decoder::new(1 << 20);
        d.extend(bytes);
        let mut out = Vec::new();
        while let Some(f) = d.next_frame()? {
            out.push(f);
        }
        assert_eq!(d.pending(), 0, "trailing bytes");
        Ok(out)
    }

    /// RFC 6455 section 5.7 examples.
    #[test]
    fn rfc6455_examples() {
        // A single-frame unmasked text message "Hello".
        let unmasked = [0x81, 0x05, 0x48, 0x65, 0x6c, 0x6c, 0x6f];
        let f = decode_all(&unmasked).unwrap();
        assert_eq!(f.len(), 1);
        assert!(f[0].fin && !f[0].masked);
        assert_eq!(f[0].opcode, OpCode::Text);
        assert_eq!(f[0].payload, b"Hello");
        let mut enc = Vec::new();
        encode(&mut enc, true, OpCode::Text, b"Hello", None);
        assert_eq!(enc, unmasked);

        // A single-frame masked text message "Hello".
        let masked = [
            0x81, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58,
        ];
        let f = decode_all(&masked).unwrap();
        assert!(f[0].masked);
        assert_eq!(f[0].payload, b"Hello");
        let mut enc = Vec::new();
        encode(
            &mut enc,
            true,
            OpCode::Text,
            b"Hello",
            Some([0x37, 0xfa, 0x21, 0x3d]),
        );
        assert_eq!(enc, masked);

        // A fragmented unmasked text message "Hel" + "lo".
        let frag = [0x01, 0x03, 0x48, 0x65, 0x6c, 0x80, 0x02, 0x6c, 0x6f];
        let f = decode_all(&frag).unwrap();
        assert_eq!(f.len(), 2);
        assert!(!f[0].fin && f[0].opcode == OpCode::Text && f[0].payload == b"Hel");
        assert!(f[1].fin && f[1].opcode == OpCode::Continuation && f[1].payload == b"lo");

        // Unmasked ping "Hello" and masked pong "Hello".
        let ping = [0x89, 0x05, 0x48, 0x65, 0x6c, 0x6c, 0x6f];
        assert_eq!(decode_all(&ping).unwrap()[0].opcode, OpCode::Ping);
        let pong = [
            0x8a, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58,
        ];
        let f = decode_all(&pong).unwrap();
        assert_eq!(
            (f[0].opcode, f[0].payload.as_slice()),
            (OpCode::Pong, &b"Hello"[..])
        );

        // 256 bytes binary in a single unmasked frame: 0x82 0x7E 0x0100.
        let mut enc = Vec::new();
        encode(&mut enc, true, OpCode::Binary, &[7u8; 256], None);
        assert_eq!(&enc[..4], &[0x82, 0x7e, 0x01, 0x00]);
        assert_eq!(decode_all(&enc).unwrap()[0].payload, vec![7u8; 256]);

        // 64 KiB binary in a single unmasked frame: 0x82 0x7F 0x0000000000010000.
        let mut enc = Vec::new();
        encode(&mut enc, true, OpCode::Binary, &vec![9u8; 65536], None);
        assert_eq!(&enc[..10], &[0x82, 0x7f, 0, 0, 0, 0, 0, 0x01, 0x00, 0x00]);
        assert_eq!(decode_all(&enc).unwrap()[0].payload.len(), 65536);
    }

    #[test]
    fn decodes_byte_by_byte() {
        let mut wire = Vec::new();
        encode(
            &mut wire,
            true,
            OpCode::Text,
            &[b'x'; 300],
            Some([1, 2, 3, 4]),
        );
        encode(&mut wire, true, OpCode::Ping, b"p", Some([9, 8, 7, 6]));
        let mut d = Decoder::new(1 << 20);
        let mut frames = Vec::new();
        for &b in &wire {
            let buf = d.buffer(16);
            buf[0] = b;
            d.commit(16, 1);
            while let Some(f) = d.next_frame().unwrap() {
                frames.push(f);
            }
        }
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].payload, vec![b'x'; 300]);
        assert_eq!(frames[1].payload, b"p");
        assert_eq!(d.pending(), 0);
    }

    #[test]
    fn mask_matches_bytewise_reference() {
        let key = [0xde, 0xad, 0xbe, 0xef];
        for len in 0..40 {
            for offset in 0..4 {
                let data: Vec<u8> = (0..len as u8).collect();
                let mut fast = data.clone();
                apply_mask(&mut fast, key, offset);
                let slow: Vec<u8> = data
                    .iter()
                    .enumerate()
                    .map(|(i, b)| b ^ key[(i + offset) % 4])
                    .collect();
                assert_eq!(fast, slow, "len {len} offset {offset}");
            }
        }
    }

    #[test]
    fn rejects_bad_frames() {
        let err = |b: &[u8]| decode_all(b).unwrap_err();
        assert!(matches!(err(&[0xC1, 0x00]), Error::Protocol(_)), "RSV1");
        assert!(matches!(err(&[0x83, 0x00]), Error::Protocol(_)), "opcode 3");
        assert!(matches!(err(&[0x8B, 0x00]), Error::Protocol(_)), "opcode B");
        assert!(
            matches!(err(&[0x09, 0x00]), Error::Protocol(_)),
            "fragmented ping"
        );
        assert!(
            matches!(err(&[0x89, 0x7E, 0x00, 0x7E]), Error::Protocol(_)),
            "long ping"
        );
        assert!(
            matches!(err(&[0x82, 0x7E, 0x00, 0x05]), Error::Protocol(_)),
            "non-minimal 16"
        );
        assert!(
            matches!(
                err(&[0x82, 0x7F, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF]),
                Error::Protocol(_)
            ),
            "non-minimal 64"
        );
        assert!(
            matches!(
                err(&[0x82, 0x7F, 0x80, 0, 0, 0, 0, 1, 0, 0]),
                Error::Protocol(_)
            ),
            "top bit"
        );
        let mut d = Decoder::new(10);
        d.extend(&[0x82, 0x0B]);
        assert!(matches!(
            d.next_frame(),
            Err(Error::MessageTooBig { limit: 10 })
        ));
        // A huge declared length is rejected before any payload arrives.
        let mut d = Decoder::new(1 << 20);
        d.extend(&[0x82, 0x7F, 0, 0, 0, 1, 0, 0, 0, 0]);
        assert!(matches!(d.next_frame(), Err(Error::MessageTooBig { .. })));
    }

    #[test]
    fn opcodes() {
        for v in 0..16u8 {
            if let Some(op) = OpCode::from_u8(v) {
                assert_eq!(op.as_u8(), v);
                assert_eq!(op.is_control(), v >= 8);
            } else {
                assert!((3..=7).contains(&v) || v >= 0xB);
            }
        }
    }
}
