//! `EscPosStream`: send and status query over an open byte link (TCP socket or serial port).

use std::io::{self, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use super::failures;
use crate::adapters::Cx;
use crate::contracts::{AdapterError, PosTarget};

/// An open byte link to a POS device. Reads may stop early on a timeout
/// ([`ErrorKind::TimedOut`] or [`ErrorKind::WouldBlock`]).
pub trait ByteLink: Read + Write + Send {
    /// Bounds how long the next read waits. Links without a timeout ignore it.
    fn set_read_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        let _ = timeout;
        Ok(())
    }
}

impl ByteLink for TcpStream {
    fn set_read_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        TcpStream::set_read_timeout(self, Some(timeout.max(Duration::from_millis(1))))
    }
}

/// Why a link operation failed: a write or read I/O error (each transport words it like
/// .NET) or an adapter refusal already worded.
#[derive(Debug)]
pub(super) enum LinkError {
    Write(io::Error),
    Read(io::Error),
    Refused(AdapterError),
}

impl From<AdapterError> for LinkError {
    fn from(e: AdapterError) -> LinkError {
        LinkError::Refused(e)
    }
}

/// Send and query over a [`ByteLink`] (C# `EscPosStream`).
pub struct EscPosStream;

impl EscPosStream {
    /// How long a DLE EOT reply may take; the command timeout still bounds the whole verb.
    pub const REPLY_TIMEOUT: Duration = Duration::from_secs(3);

    /// Writes and flushes `bytes`.
    pub(super) fn send(link: &mut dyn ByteLink, bytes: &[u8]) -> io::Result<()> {
        link.write_all(bytes)?;
        link.flush()
    }

    /// Sends `request` and reads exactly `reply_length` bytes within
    /// [`REPLY_TIMEOUT`](Self::REPLY_TIMEOUT) (DEVICE_UNAVAILABLE after it, or when the
    /// link closes first); a cancelled call fails with [`Cx::cancelled`].
    pub(super) fn query(
        link: &mut dyn ByteLink,
        request: &[u8],
        reply_length: usize,
        target: &PosTarget,
        cx: &Cx,
    ) -> Result<Vec<u8>, LinkError> {
        Self::send(link, request).map_err(LinkError::Write)?;
        let until = Instant::now() + Self::REPLY_TIMEOUT;
        let mut reply = vec![0u8; reply_length];
        let mut got = 0;
        while got < reply_length {
            cx.check()?;
            let now = Instant::now();
            if now >= until {
                return Err(failures::unavailable(
                    target,
                    &format!("no status reply within {} s", Self::REPLY_TIMEOUT.as_secs()),
                )
                .into());
            }
            link.set_read_timeout((until - now).min(cx.remaining()))
                .map_err(LinkError::Read)?;
            match link.read(&mut reply[got..]) {
                Ok(0) => {
                    return Err(failures::unavailable(
                        target,
                        "the device closed the link before answering",
                    )
                    .into());
                }
                Ok(n) => got += n,
                Err(e)
                    if matches!(
                        e.kind(),
                        ErrorKind::TimedOut | ErrorKind::WouldBlock | ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(LinkError::Read(e)),
            }
        }
        Ok(reply)
    }
}
