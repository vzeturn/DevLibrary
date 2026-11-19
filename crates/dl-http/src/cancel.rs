//! Cancelling a blocking request from another thread.

use std::fmt;
use std::io;
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::Error;

#[derive(Default)]
struct State {
    cancelled: bool,
    next_id: u64,
    /// Clones of the sockets of the requests in flight.
    streams: Vec<(u64, TcpStream)>,
}

/// A cancel handle for [`Client::request_with_cancel`](crate::Client::request_with_cancel).
///
/// Clone it, give one copy to the thread that sends requests and keep the other. After
/// [`cancel`](Cancel::cancel), a request in flight returns [`Error::Cancelled`] as soon as
/// its socket wakes up (the socket is shut down, which ends a blocking read at once), and
/// every later request with this handle fails with [`Error::Cancelled`] before connecting.
/// A DNS lookup or a TCP connect already started is not interrupted: it ends within
/// [`ClientOptions::connect_timeout`](crate::ClientOptions::connect_timeout).
///
/// ```
/// let cancel = dl_http::Cancel::new();
/// cancel.cancel()?;
/// let client = dl_http::Client::new();
/// let req = dl_http::Request::get(dl_http::Url::parse("http://127.0.0.1:9/")?);
/// assert!(matches!(
///     client.request_with_cancel(&req, &cancel),
///     Err(dl_http::Error::Cancelled)
/// ));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Default)]
pub struct Cancel(Arc<Mutex<State>>);

impl fmt::Debug for Cancel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let st = self.lock();
        f.debug_struct("Cancel")
            .field("cancelled", &st.cancelled)
            .field("in_flight", &st.streams.len())
            .finish()
    }
}

impl Cancel {
    /// A handle that is not cancelled.
    pub fn new() -> Cancel {
        Cancel::default()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Cancels every request in flight with this handle and every later one.
    ///
    /// # Errors
    /// The first error shutting a socket down, other than "not connected" (a socket the
    /// peer already closed). Every socket is still shut down and the handle stays
    /// cancelled.
    pub fn cancel(&self) -> io::Result<()> {
        let mut st = self.lock();
        st.cancelled = true;
        let mut first = None;
        for (_, s) in st.streams.drain(..) {
            match s.shutdown(Shutdown::Both) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotConnected => {}
                Err(e) => {
                    first.get_or_insert(e);
                }
            }
        }
        first.map_or(Ok(()), Err)
    }

    /// Whether [`cancel`](Cancel::cancel) was called.
    pub fn is_cancelled(&self) -> bool {
        self.lock().cancelled
    }

    /// [`Error::Cancelled`] once cancelled.
    pub(crate) fn check(&self) -> Result<(), Error> {
        if self.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(())
    }

    /// Registers the socket of an exchange; it is shut down by `cancel` until the returned
    /// guard is dropped.
    pub(crate) fn watch(&self, tcp: &TcpStream) -> Result<Watch, Error> {
        let clone = tcp.try_clone().map_err(Error::Io)?;
        let mut st = self.lock();
        if st.cancelled {
            return Err(Error::Cancelled);
        }
        let id = st.next_id;
        st.next_id += 1;
        st.streams.push((id, clone));
        Ok(Watch {
            cancel: self.clone(),
            id,
        })
    }
}

/// Unregisters a socket from its [`Cancel`] when dropped.
pub(crate) struct Watch {
    cancel: Cancel,
    id: u64,
}

impl Drop for Watch {
    fn drop(&mut self) {
        let id = self.id;
        self.cancel.lock().streams.retain(|(i, _)| *i != id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Client, Request, Url};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    /// A server that reads the request and never answers.
    fn silent_server() -> (String, std::thread::JoinHandle<()>) {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}/poll", l.local_addr().expect("addr"));
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().expect("accept");
            let mut buf = [0u8; 1024];
            // Ends when the client shuts its side down.
            while matches!(s.read(&mut buf), Ok(n) if n > 0) {}
        });
        (url, h)
    }

    #[test]
    fn cancel_ends_a_request_in_flight_promptly() {
        let (url, server) = silent_server();
        let cancel = Cancel::new();
        let c2 = cancel.clone();
        let req =
            Request::get(Url::parse(&url).expect("url")).with_timeout(Duration::from_secs(30));
        let t = std::thread::spawn(move || {
            let started = Instant::now();
            let r = Client::new().request_with_cancel(&req, &c2);
            (r, started.elapsed())
        });
        std::thread::sleep(Duration::from_millis(200));
        cancel.cancel().expect("cancel");
        let (r, took) = t.join().expect("client thread");
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
        assert!(took < Duration::from_secs(2), "took {took:?}");
        server.join().expect("server thread");
        assert!(cancel.lock().streams.is_empty());
    }

    #[test]
    fn a_cancelled_handle_fails_before_connecting() {
        let cancel = Cancel::new();
        cancel.cancel().expect("cancel");
        assert!(cancel.is_cancelled());
        // Nothing listens on port 9 of this test; the error must still be Cancelled.
        let req = Request::get(Url::parse("http://127.0.0.1:9/").expect("url"));
        let r = Client::new().request_with_cancel(&req, &cancel);
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    }

    #[test]
    fn finished_requests_unregister_and_stay_pooled() {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}/", l.local_addr().expect("addr"));
        let server = std::thread::spawn(move || {
            let (mut s, _) = l.accept().expect("accept");
            let mut buf = [0u8; 4096];
            for _ in 0..2 {
                let mut got = Vec::new();
                while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = s.read(&mut buf).expect("read");
                    assert!(n > 0, "closed early");
                    got.extend_from_slice(&buf[..n]);
                }
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .expect("write");
            }
        });
        let client = Client::new();
        let cancel = Cancel::new();
        let req = Request::get(Url::parse(&url).expect("url"));
        for _ in 0..2 {
            let r = client.request_with_cancel(&req, &cancel).expect("request");
            assert_eq!(r.body, b"ok");
            assert!(cancel.lock().streams.is_empty());
        }
        // Both requests used the one accepted connection.
        server.join().expect("server thread");
        assert!(!cancel.is_cancelled());
    }
}
