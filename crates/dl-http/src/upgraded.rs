//! The connection a [`Client::upgrade`](crate::Client::upgrade) hands over after
//! `101 Switching Protocols`.

use crate::client::Transport;
use std::fmt;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// A connection after `101 Switching Protocols`, returned by [`Client::upgrade`](crate::Client::upgrade).
///
/// Reads first return any bytes the server sent right after the response head, then
/// read from the socket (through TLS for `https`). It is `Send`; use one thread at a
/// time, and [`Upgraded::shutdown_handle`] to unblock a reader from another thread.
pub struct Upgraded {
    io: Transport,
    buf: Vec<u8>,
    pos: usize,
}

impl Upgraded {
    pub(crate) fn new(io: Transport, buf: Vec<u8>) -> Upgraded {
        Upgraded { io, buf, pos: 0 }
    }

    /// Sets the socket read timeout (`None` blocks). A read that times out fails with
    /// [`io::ErrorKind::WouldBlock`] or [`io::ErrorKind::TimedOut`] and loses no data:
    /// partial TLS records stay buffered for the next read.
    ///
    /// # Errors
    /// The OS error, e.g. for a zero duration.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.io.tcp().set_read_timeout(timeout)
    }

    /// Sets the socket write timeout (`None` blocks).
    ///
    /// # Errors
    /// The OS error, e.g. for a zero duration.
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.io.tcp().set_write_timeout(timeout)
    }

    /// Whether the connection runs over TLS.
    pub fn is_tls(&self) -> bool {
        matches!(self.io, Transport::Tls(_))
    }

    /// A handle that closes the socket from another thread; a blocked read then returns.
    ///
    /// # Errors
    /// The OS error from duplicating the socket handle.
    pub fn shutdown_handle(&self) -> io::Result<ShutdownHandle> {
        self.io.tcp().try_clone().map(ShutdownHandle)
    }

    /// Sends TLS close_notify (for `https`) and shuts the socket down in both directions.
    ///
    /// # Errors
    /// The OS error; a peer that already closed is not an error.
    pub fn shutdown(&mut self) -> io::Result<()> {
        if let Transport::Tls(t) = &mut self.io {
            let _ = t.shutdown();
        }
        match self.io.tcp().shutdown(std::net::Shutdown::Both) {
            Err(e) if e.kind() == io::ErrorKind::NotConnected => Ok(()),
            r => r,
        }
    }
}

impl Read for Upgraded {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos < self.buf.len() {
            let n = out.len().min(self.buf.len() - self.pos);
            out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
            self.pos += n;
            if self.pos == self.buf.len() {
                self.buf = Vec::new();
                self.pos = 0;
            }
            return Ok(n);
        }
        self.io.read(out)
    }
}

impl Write for Upgraded {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.io.write(data)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.io.flush()
    }
}

impl fmt::Debug for Upgraded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Upgraded")
            .field("tls", &self.is_tls())
            .field("buffered", &(self.buf.len() - self.pos))
            .finish_non_exhaustive()
    }
}

/// Closes an [`Upgraded`] connection's socket from another thread.
#[derive(Debug)]
pub struct ShutdownHandle(TcpStream);

impl ShutdownHandle {
    /// Shuts the socket down in both directions. Blocked reads and writes on the
    /// connection return; later ones fail.
    ///
    /// # Errors
    /// The OS error; a socket that is already closed is not an error.
    pub fn shutdown(&self) -> io::Result<()> {
        match self.0.shutdown(std::net::Shutdown::Both) {
            Err(e) if e.kind() == io::ErrorKind::NotConnected => Ok(()),
            r => r,
        }
    }
}
