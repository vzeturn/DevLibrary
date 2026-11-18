//! Carries the TLS handshake inside TDS PRELOGIN packets (TDS 7.x), then gets out of the
//! way.
//!
//! In TDS 7.x the client and server exchange TLS handshake records as the payload of
//! PRELOGIN (0x12) packets. After the handshake, TLS records go directly on the socket.
//! [`TdsTlsTransport`] is the `Read + Write` stream handed to `dl_tls::TlsStream`: in
//! handshake mode it buffers what TLS writes and sends it as PRELOGIN packets on `flush`
//! (dl-tls flushes after each flight), and unwraps the payload of incoming packets;
//! in pass-through mode it forwards bytes unchanged.

use std::io::{self, Read, Write};

use crate::packet::{self, HEADER_LEN};

pub(crate) struct TdsTlsTransport<S> {
    inner: S,
    handshake: bool,
    packet_size: usize,
    /// Handshake bytes waiting to be sent.
    out: Vec<u8>,
    /// Payload of the current incoming handshake packet, and the read position.
    payload: Vec<u8>,
    pos: usize,
}

impl<S: Read + Write> TdsTlsTransport<S> {
    /// Wraps `inner`, starting in handshake mode when `handshake` is true (TDS 7.x) or in
    /// pass-through mode (TDS 8 strict, where TLS runs directly on the socket).
    pub(crate) fn new(inner: S, handshake: bool, packet_size: usize) -> Self {
        TdsTlsTransport {
            inner,
            handshake,
            packet_size,
            out: Vec::new(),
            payload: Vec::new(),
            pos: 0,
        }
    }

    /// Switches to pass-through after the handshake, sending anything still buffered.
    pub(crate) fn finish_handshake(&mut self) -> io::Result<()> {
        self.send_pending()?;
        self.handshake = false;
        Ok(())
    }

    pub(crate) fn get_ref(&self) -> &S {
        &self.inner
    }

    /// `true` while handshake bytes are wrapped in PRELOGIN packets.
    pub(crate) fn in_handshake(&self) -> bool {
        self.handshake
    }

    pub(crate) fn into_inner(self) -> S {
        self.inner
    }

    fn send_pending(&mut self) -> io::Result<()> {
        if self.out.is_empty() {
            return Ok(());
        }
        let mut framed = Vec::with_capacity(self.out.len() + 64);
        packet::frame(packet::PRELOGIN, &self.out, self.packet_size, &mut framed);
        self.out.clear();
        self.inner.write_all(&framed)?;
        self.inner.flush()
    }

    fn read_packet(&mut self) -> io::Result<()> {
        let mut h = [0u8; HEADER_LEN];
        self.inner.read_exact(&mut h)?;
        let len = u16::from_be_bytes([h[2], h[3]]) as usize;
        // Older servers answer with REPLY (4) instead of PRELOGIN (18).
        if (h[0] != packet::PRELOGIN && h[0] != packet::REPLY) || len < HEADER_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected packet during the TLS handshake",
            ));
        }
        self.payload.resize(len - HEADER_LEN, 0);
        self.inner.read_exact(&mut self.payload)?;
        self.pos = 0;
        Ok(())
    }
}

impl<S: Read + Write> Read for TdsTlsTransport<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.handshake {
            // A read means our flight is complete (go-mssqldb finishes the packet here too).
            self.send_pending()?;
            while self.pos >= self.payload.len() {
                self.read_packet()?;
            }
        }
        if self.pos < self.payload.len() {
            let n = buf.len().min(self.payload.len() - self.pos);
            buf[..n].copy_from_slice(&self.payload[self.pos..self.pos + n]);
            self.pos += n;
            return Ok(n);
        }
        self.inner.read(buf)
    }
}

impl<S: Read + Write> Write for TdsTlsTransport<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.handshake {
            self.out.extend_from_slice(buf);
            Ok(buf.len())
        } else {
            self.inner.write(buf)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.handshake {
            self.send_pending()
        } else {
            self.inner.flush()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory duplex: reads from `input`, records writes.
    struct Mem {
        input: io::Cursor<Vec<u8>>,
        written: Vec<u8>,
        flushes: usize,
    }

    impl Read for Mem {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.input.read(buf)
        }
    }
    impl Write for Mem {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    #[test]
    fn wraps_and_unwraps_handshake_then_passes_through() {
        let mut input = Vec::new();
        packet::frame(packet::REPLY, b"server-hello", 4096, &mut input);
        packet::frame(packet::PRELOGIN, b"finished", 4096, &mut input);
        input.extend_from_slice(b"RAW-RECORD");
        let mem = Mem {
            input: io::Cursor::new(input),
            written: Vec::new(),
            flushes: 0,
        };
        let mut t = TdsTlsTransport::new(mem, true, 512);
        t.write_all(b"client-hello").unwrap();
        t.flush().unwrap();
        let mut buf = [0u8; 6];
        t.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"server");
        let mut rest = [0u8; 6];
        t.read_exact(&mut rest).unwrap();
        assert_eq!(&rest, b"-hello");
        // A second flight: 600 bytes span two 512-byte PRELOGIN packets; the read sends it.
        t.write_all(&[7u8; 600]).unwrap();
        let mut fin = [0u8; 8];
        t.read_exact(&mut fin).unwrap();
        assert_eq!(&fin, b"finished");
        t.finish_handshake().unwrap();
        t.write_all(b"app").unwrap();
        let mut raw = [0u8; 10];
        t.read_exact(&mut raw).unwrap();
        assert_eq!(&raw, b"RAW-RECORD");

        let mem = t.into_inner();
        assert!(mem.flushes >= 2);
        let w = &mem.written;
        assert_eq!(&w[..8], &[0x12, 1, 0, 20, 0, 0, 1, 0]);
        assert_eq!(&w[8..20], b"client-hello");
        assert_eq!(&w[20..28], &[0x12, 0, 2, 0, 0, 0, 1, 0]);
        assert_eq!(&w[20 + 512..20 + 520], &[0x12, 1, 0, 104, 0, 0, 2, 0]);
        assert_eq!(&w[w.len() - 3..], b"app");
    }

    #[test]
    fn rejects_other_packet_types() {
        let mut input = Vec::new();
        packet::frame(packet::LOGIN7, b"x", 4096, &mut input);
        let mem = Mem {
            input: io::Cursor::new(input),
            written: Vec::new(),
            flushes: 0,
        };
        let mut t = TdsTlsTransport::new(mem, true, 4096);
        let mut b = [0u8; 1];
        assert!(t.read(&mut b).is_err());
    }
}
