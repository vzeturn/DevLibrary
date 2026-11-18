//! `EscPosTcpAdapter.cs`: network receipt / kitchen printer on raw TCP (port 9100). One
//! connection per verb, closed afterwards, like the VB6 Winsock send.

use std::io::ErrorKind;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use super::stream::{EscPosStream, LinkError};
use super::{EscPosTransport, failures, os_message};
use crate::adapters::Cx;
use crate::contracts::{AdapterError, PosTarget, transports};

/// ESC/POS over raw TCP. Connect, write and read are bounded by the command deadline.
#[derive(Debug, Default, Clone, Copy)]
pub struct EscPosTcpAdapter;

impl EscPosTcpAdapter {
    /// The raw printing port.
    pub const DEFAULT_PORT: u16 = 9100;

    /// `host` or `host:port`; a bare IPv6 address (several colons) takes the default port.
    pub fn parse_endpoint(endpoint: &str) -> Result<(String, u16), AdapterError> {
        let text = endpoint.trim();
        if let Some(colon) = text.rfind(':')
            && colon > 0
            && text.find(':') == Some(colon)
        {
            let host = &text[..colon];
            let port = parse_port(&text[colon + 1..]);
            return match port {
                Some(port) if !host.is_empty() => Ok((host.to_owned(), port)),
                _ => Err(failures::bad_payload(format!(
                    "tcp endpoint '{endpoint}' is not host:port"
                ))),
            };
        }
        if text.is_empty() {
            Err(failures::bad_payload("tcp endpoint is empty"))
        } else {
            Ok((text.to_owned(), Self::DEFAULT_PORT))
        }
    }

    fn connect(target: &PosTarget, cx: &Cx) -> Result<TcpStream, AdapterError> {
        let endpoint = target
            .endpoint
            .as_deref()
            .ok_or_else(failures::null_reference)?;
        let (host, port) = Self::parse_endpoint(endpoint)?;
        cx.check()?;
        let addrs: Vec<SocketAddr> = (host.as_str(), port)
            .to_socket_addrs()
            .map_err(|e| failures::unavailable(target, &os_message(&e)))?
            .collect();
        let mut last = None;
        for addr in addrs {
            cx.check()?;
            match TcpStream::connect_timeout(&addr, cx.remaining().max(Duration::from_millis(1))) {
                Ok(stream) => {
                    // Nagle off, like TcpClient { NoDelay = true }; a failure only costs latency.
                    let _ = stream.set_nodelay(true);
                    return Ok(stream);
                }
                Err(_) if cx.is_cancelled() => return Err(Cx::cancelled()),
                Err(e) => last = Some(e),
            }
        }
        let reason = last.map_or_else(|| "No such host is known.".to_owned(), |e| os_message(&e));
        Err(failures::unavailable(target, &reason))
    }

    /// The C# `Guard`: an I/O failure is DEVICE_UNAVAILABLE with the NetworkStream message;
    /// one caused by the deadline is the cancellation.
    fn guard(target: &PosTarget, cx: &Cx, e: LinkError) -> AdapterError {
        match e {
            LinkError::Refused(e) => e,
            LinkError::Write(_) | LinkError::Read(_) if cx.is_cancelled() => Cx::cancelled(),
            LinkError::Write(e) => failures::unavailable(
                target,
                &format!(
                    "Unable to write data to the transport connection: {}.",
                    os_message(&e)
                ),
            ),
            LinkError::Read(e) => failures::unavailable(
                target,
                &format!(
                    "Unable to read data from the transport connection: {}.",
                    os_message(&e)
                ),
            ),
        }
    }
}

/// `int.TryParse(s, NumberStyles.None, ...)` and `port is > 0 and <= 65535`.
fn parse_port(s: &str) -> Option<u16> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: i32 = s.parse().ok()?;
    u16::try_from(n).ok().filter(|&p| p > 0)
}

impl EscPosTransport for EscPosTcpAdapter {
    fn transport(&self) -> &str {
        transports::TCP
    }

    fn send(&self, target: &PosTarget, bytes: &[u8], cx: &Cx) -> Result<(), AdapterError> {
        let mut stream = Self::connect(target, cx)?;
        let timeout = cx.remaining().max(Duration::from_millis(1));
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|e| Self::guard(target, cx, LinkError::Write(e)))?;
        EscPosStream::send(&mut stream, bytes).map_err(|e| {
            let e = if e.kind() == ErrorKind::WouldBlock {
                std::io::Error::from(ErrorKind::TimedOut)
            } else {
                e
            };
            Self::guard(target, cx, LinkError::Write(e))
        })
    }

    fn query(
        &self,
        target: &PosTarget,
        request: &[u8],
        reply_length: usize,
        cx: &Cx,
    ) -> Result<Vec<u8>, AdapterError> {
        let mut stream = Self::connect(target, cx)?;
        let timeout = cx.remaining().max(Duration::from_millis(1));
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|e| Self::guard(target, cx, LinkError::Write(e)))?;
        EscPosStream::query(&mut stream, request, reply_length, target, cx)
            .map_err(|e| Self::guard(target, cx, e))
    }
}
