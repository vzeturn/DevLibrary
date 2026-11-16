//! TDS packet framing: splitting messages into packets and reassembling them.
//!
//! A packet is an 8-byte header (type, status, big-endian length including the header,
//! SPID, packet id, window) followed by payload. A message is a run of packets whose last
//! one has the EOM status bit.

use std::io::{self, Read};

use crate::Error;

pub(crate) const SQL_BATCH: u8 = 0x01;
pub(crate) const RPC: u8 = 0x03;
pub(crate) const REPLY: u8 = 0x04;
pub(crate) const ATTENTION: u8 = 0x06;
pub(crate) const LOGIN7: u8 = 0x10;
pub(crate) const PRELOGIN: u8 = 0x12;

pub(crate) const HEADER_LEN: usize = 8;
const STATUS_EOM: u8 = 0x01;

/// Default negotiated packet size.
pub(crate) const DEFAULT_PACKET_SIZE: usize = 4096;
/// Smallest and largest packet sizes SQL Server accepts.
pub(crate) const MIN_PACKET_SIZE: usize = 512;
pub(crate) const MAX_PACKET_SIZE: usize = 32767;

/// Appends `payload` to `out` framed as packets of at most `packet_size` bytes. An empty
/// payload produces one header-only packet (used by ATTENTION).
pub(crate) fn frame(ptype: u8, payload: &[u8], packet_size: usize, out: &mut Vec<u8>) {
    let chunk = packet_size.clamp(MIN_PACKET_SIZE, MAX_PACKET_SIZE) - HEADER_LEN;
    let count = payload.len().div_ceil(chunk).max(1);
    out.reserve(payload.len() + count * HEADER_LEN);
    let mut id: u8 = 1;
    let mut rest = payload;
    loop {
        let n = rest.len().min(chunk);
        let last = n == rest.len();
        let len = (n + HEADER_LEN) as u16;
        out.push(ptype);
        out.push(if last { STATUS_EOM } else { 0 });
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&[0, 0, id, 0]);
        out.extend_from_slice(&rest[..n]);
        rest = &rest[n..];
        id = id.wrapping_add(1);
        if last {
            break;
        }
    }
}

/// Length of the first packet `frame` produces for a payload of `payload_len` bytes.
pub(crate) fn first_packet_len(payload_len: usize, packet_size: usize) -> usize {
    let chunk = packet_size.clamp(MIN_PACKET_SIZE, MAX_PACKET_SIZE) - HEADER_LEN;
    payload_len.min(chunk) + HEADER_LEN
}

/// Reassembles messages from a byte stream. Keeps partial state between calls, so a read
/// timeout in the middle of a message loses nothing: the next call continues.
#[derive(Default)]
pub(crate) struct MessageReader {
    raw: Vec<u8>,
    start: usize,
    msg: Vec<u8>,
    msg_type: Option<u8>,
}

impl MessageReader {
    pub(crate) fn new() -> MessageReader {
        MessageReader::default()
    }

    /// Adds bytes already received from the socket (e.g. TLS leftovers).
    pub(crate) fn push_raw(&mut self, bytes: &[u8]) {
        self.raw.extend_from_slice(bytes);
    }

    /// `true` if a message is partially received.
    #[cfg(test)]
    pub(crate) fn in_message(&self) -> bool {
        self.msg_type.is_some() || self.start < self.raw.len()
    }

    /// Extracts complete packets from the buffered bytes; returns a message once its EOM
    /// packet has arrived.
    pub(crate) fn poll(&mut self) -> Result<Option<(u8, Vec<u8>)>, Error> {
        loop {
            let avail = &self.raw[self.start..];
            if avail.len() < HEADER_LEN {
                break;
            }
            let ptype = avail[0];
            let status = avail[1];
            let len = u16::from_be_bytes([avail[2], avail[3]]) as usize;
            if len < HEADER_LEN {
                return Err(Error::Malformed {
                    token: 0,
                    offset: self.msg.len(),
                    what: "packet length is shorter than the packet header",
                });
            }
            if avail.len() < len {
                break;
            }
            match self.msg_type {
                None => self.msg_type = Some(ptype),
                Some(t) if t != ptype => {
                    return Err(Error::Malformed {
                        token: 0,
                        offset: self.msg.len(),
                        what: "packet type changed within a message",
                    });
                }
                Some(_) => {}
            }
            self.msg.extend_from_slice(&avail[HEADER_LEN..len]);
            self.start += len;
            if status & STATUS_EOM != 0 {
                self.compact();
                let t = self.msg_type.take().unwrap_or(ptype);
                return Ok(Some((t, std::mem::take(&mut self.msg))));
            }
        }
        self.compact();
        Ok(None)
    }

    fn compact(&mut self) {
        if self.start == self.raw.len() {
            self.raw.clear();
            self.start = 0;
        } else if self.start > 64 * 1024 {
            self.raw.drain(..self.start);
            self.start = 0;
        }
    }

    /// Reads from `r` until one complete message is available.
    pub(crate) fn read_message<R: Read>(&mut self, r: &mut R) -> Result<(u8, Vec<u8>), Error> {
        let mut tmp = [0u8; 16 * 1024];
        loop {
            if let Some(m) = self.poll()? {
                return Ok(m);
            }
            let n = match r.read(&mut tmp) {
                Ok(0) => {
                    return Err(Error::Io(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "server closed the connection",
                    )));
                }
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(Error::Io(e)),
            };
            self.raw.extend_from_slice(&tmp[..n]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader that returns at most `step` bytes per call.
    struct Trickle<'a> {
        data: &'a [u8],
        step: usize,
    }

    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.data.len().min(self.step).min(buf.len());
            buf[..n].copy_from_slice(&self.data[..n]);
            self.data = &self.data[n..];
            Ok(n)
        }
    }

    #[test]
    fn frame_splits_and_numbers_packets() {
        let payload: Vec<u8> = (0..1200u32).map(|i| i as u8).collect();
        let mut out = Vec::new();
        frame(RPC, &payload, 512, &mut out);
        // 504 payload bytes per packet: 504 + 504 + 192.
        assert_eq!(out.len(), 1200 + 3 * 8);
        assert_eq!(&out[..8], &[3, 0, 0x02, 0x00, 0, 0, 1, 0]);
        assert_eq!(&out[512..520], &[3, 0, 0x02, 0x00, 0, 0, 2, 0]);
        assert_eq!(&out[1024..1032], &[3, 1, 0x00, 200, 0, 0, 3, 0]);
        assert_eq!(first_packet_len(1200, 512), 512);
        assert_eq!(first_packet_len(10, 4096), 18);
    }

    #[test]
    fn attention_is_header_only() {
        let mut out = Vec::new();
        frame(ATTENTION, &[], 4096, &mut out);
        assert_eq!(out, [6, 1, 0, 8, 0, 0, 1, 0]);
    }

    #[test]
    fn reassembly_across_reads() {
        let payload: Vec<u8> = (0..5000u32).map(|i| (i * 7) as u8).collect();
        let mut wire = Vec::new();
        frame(REPLY, &payload, 512, &mut wire);
        frame(REPLY, b"second", 512, &mut wire);
        for step in [1, 3, 7, 8, 511, 512, 513, 4096, 100_000] {
            let mut r = Trickle { data: &wire, step };
            let mut mr = MessageReader::new();
            let (t, m) = mr.read_message(&mut r).unwrap();
            assert_eq!(t, REPLY);
            assert_eq!(m, payload, "step {step}");
            let (_, m2) = mr.read_message(&mut r).unwrap();
            assert_eq!(m2, b"second");
            assert!(!mr.in_message());
            assert!(matches!(mr.read_message(&mut r), Err(Error::Io(_))));
        }
    }

    #[test]
    fn bad_headers_are_errors() {
        let mut mr = MessageReader::new();
        mr.push_raw(&[4, 1, 0, 3, 0, 0, 1, 0]);
        assert!(matches!(mr.poll(), Err(Error::Malformed { .. })));

        let mut mr = MessageReader::new();
        mr.push_raw(&[4, 0, 0, 9, 0, 0, 1, 0, 0xAA, 0x12, 1, 0, 8, 0, 0, 2, 0]);
        assert!(matches!(mr.poll(), Err(Error::Malformed { .. })));
    }

    #[test]
    fn partial_message_survives_timeout() {
        struct Flaky<'a> {
            data: &'a [u8],
            fail_at: usize,
            failed: bool,
        }
        impl Read for Flaky<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if !self.failed && self.fail_at == 0 {
                    self.failed = true;
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "t"));
                }
                let n = self.data.len().min(buf.len()).min(if self.failed {
                    usize::MAX
                } else {
                    self.fail_at
                });
                buf[..n].copy_from_slice(&self.data[..n]);
                self.data = &self.data[n..];
                self.fail_at -= n.min(self.fail_at);
                Ok(n)
            }
        }
        let payload = vec![0x5Au8; 3000];
        let mut wire = Vec::new();
        frame(REPLY, &payload, 1024, &mut wire);
        let mut r = Flaky {
            data: &wire,
            fail_at: 1500,
            failed: false,
        };
        let mut mr = MessageReader::new();
        assert!(matches!(mr.read_message(&mut r), Err(Error::Io(_))));
        assert!(mr.in_message());
        assert_eq!(mr.read_message(&mut r).unwrap().1, payload);
    }
}
