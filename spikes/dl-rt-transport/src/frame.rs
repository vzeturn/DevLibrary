//! Wire frame shared by both transports: `chan | kind | seq | sent_us | payload`.
//! On a byte stream (QUIC) each frame is prefixed with its length (u32, big endian);
//! on WebSocket one binary message carries one frame.

use std::fmt;
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Bytes before the payload.
pub const HEADER_LEN: usize = 18;
/// Largest frame accepted (header + payload).
pub const MAX_FRAME: usize = 1 << 20;

/// What a frame asks of the peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    /// Latency probe; the peer answers with an [`Kind::Ack`] at once.
    Ping = 1,
    /// Bulk data (a capture batch); the peer acknowledges it.
    Data = 2,
    /// Acknowledgement of `seq`.
    Ack = 3,
}

impl Kind {
    fn from_u8(b: u8) -> Option<Self> {
        match b {
            1 => Some(Self::Ping),
            2 => Some(Self::Data),
            3 => Some(Self::Ack),
            _ => None,
        }
    }
}

/// One frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Logical channel (see [`crate::link`]).
    pub chan: u8,
    /// Frame kind.
    pub kind: Kind,
    /// Sequence number within the channel.
    pub seq: u64,
    /// Sender clock, microseconds; echoed back in the ack.
    pub sent_us: u64,
    /// Opaque payload.
    pub payload: Vec<u8>,
}

/// A frame that cannot be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// Fewer bytes than the header.
    Short(usize),
    /// Unknown kind byte.
    Kind(u8),
    /// Larger than [`MAX_FRAME`].
    TooLarge(usize),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Short(n) => write!(
                f,
                "frame of {n} bytes is shorter than the {HEADER_LEN}-byte header"
            ),
            Self::Kind(k) => write!(f, "unknown frame kind {k}"),
            Self::TooLarge(n) => write!(f, "frame of {n} bytes exceeds {MAX_FRAME}"),
        }
    }
}

impl std::error::Error for FrameError {}

impl From<FrameError> for io::Error {
    fn from(e: FrameError) -> Self {
        io::Error::new(io::ErrorKind::InvalidData, e)
    }
}

fn be_u64(b: &[u8]) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[..8]);
    u64::from_be_bytes(a)
}

impl Frame {
    /// The acknowledgement of this frame (same channel, seq and timestamp, no payload).
    pub fn ack_of(&self) -> Frame {
        Frame {
            chan: self.chan,
            kind: Kind::Ack,
            seq: self.seq,
            sent_us: self.sent_us,
            payload: Vec::new(),
        }
    }

    /// Encoded size without the stream length prefix.
    pub fn encoded_len(&self) -> usize {
        HEADER_LEN + self.payload.len()
    }

    /// Appends the encoding (no length prefix).
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        out.push(self.chan);
        out.push(self.kind as u8);
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.extend_from_slice(&self.sent_us.to_be_bytes());
        out.extend_from_slice(&self.payload);
    }

    /// The encoding (no length prefix).
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.encoded_len());
        self.encode_into(&mut v);
        v
    }

    /// Decodes one frame (no length prefix).
    pub fn decode(buf: &[u8]) -> Result<Frame, FrameError> {
        if buf.len() < HEADER_LEN {
            return Err(FrameError::Short(buf.len()));
        }
        if buf.len() > MAX_FRAME {
            return Err(FrameError::TooLarge(buf.len()));
        }
        let kind = Kind::from_u8(buf[1]).ok_or(FrameError::Kind(buf[1]))?;
        Ok(Frame {
            chan: buf[0],
            kind,
            seq: be_u64(&buf[2..10]),
            sent_us: be_u64(&buf[10..18]),
            payload: buf[HEADER_LEN..].to_vec(),
        })
    }
}

/// Writes one length-prefixed frame.
pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, f: &Frame) -> io::Result<()> {
    let len = f.encoded_len();
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len).into());
    }
    let mut buf = Vec::with_capacity(4 + len);
    buf.extend_from_slice(&(len as u32).to_be_bytes());
    f.encode_into(&mut buf);
    w.write_all(&buf).await
}

/// Reads one length-prefixed frame; `Ok(None)` on a clean end of stream between frames.
/// An end of stream inside a frame is an error.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Option<Frame>> {
    let mut lenb = [0u8; 4];
    let first = r.read(&mut lenb[..1]).await?;
    if first == 0 {
        return Ok(None);
    }
    r.read_exact(&mut lenb[1..]).await?;
    let len = u32::from_be_bytes(lenb) as usize;
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len).into());
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(Some(Frame::decode(&buf)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Frame {
        Frame {
            chan: 1,
            kind: Kind::Data,
            seq: 42,
            sent_us: 7,
            payload: vec![1, 2, 3],
        }
    }

    #[test]
    fn roundtrip() {
        let f = sample();
        assert_eq!(Frame::decode(&f.encode()), Ok(f));
    }

    #[test]
    fn rejects_short_and_unknown_kind() {
        assert_eq!(Frame::decode(&[0; 5]), Err(FrameError::Short(5)));
        let mut b = sample().encode();
        b[1] = 9;
        assert_eq!(Frame::decode(&b), Err(FrameError::Kind(9)));
    }

    #[test]
    fn ack_keeps_identity_and_drops_payload() {
        let a = sample().ack_of();
        assert_eq!(
            (a.chan, a.kind, a.seq, a.sent_us, a.payload.len()),
            (1, Kind::Ack, 42, 7, 0)
        );
    }

    #[tokio::test]
    async fn stream_roundtrip_and_truncation() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &sample()).await.unwrap();
        let mut r = &buf[..];
        assert_eq!(read_frame(&mut r).await.unwrap(), Some(sample()));
        assert_eq!(read_frame(&mut r).await.unwrap(), None);
        let mut cut = &buf[..buf.len() - 1];
        assert!(read_frame(&mut cut).await.is_err());
    }
}
