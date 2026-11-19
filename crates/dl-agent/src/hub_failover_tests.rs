//! Hub failover over real HTTP (PROTOCOL.md §2.1): two loopback hubs, the primary down, the
//! agent moves to the fallback, stays on it while it answers, and comes back to the primary
//! when the fallback fails.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use super::*;
use crate::hub::tests_support::identity;

/// A loopback hub answering `{}` to every request, recording the paths it served.
struct LoopbackHub {
    port: u16,
    paths: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// Reads one request (head and `Content-Length` body); returns its path.
fn read_request(stream: &TcpStream) -> std::io::Result<String> {
    let mut r = BufReader::new(stream);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let path = line.split(' ').nth(1).unwrap_or_default().to_string();
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 || h == "\r\n" {
            break;
        }
        if let Some((k, v)) = h.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body)?;
    Ok(path)
}

impl LoopbackHub {
    /// Serves on `port` (0: any free port).
    fn start(port: u16) -> LoopbackHub {
        let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let paths: Arc<Mutex<Vec<String>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (p, s) = (Arc::clone(&paths), Arc::clone(&stop));
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if s.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(mut stream) = stream else { continue };
                if let Ok(path) = read_request(&stream) {
                    p.lock().unwrap().push(path);
                    let answer = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
                    stream.write_all(answer.as_bytes()).unwrap();
                }
            }
        });
        LoopbackHub {
            port,
            paths,
            stop,
            thread: Some(thread),
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn served(&self) -> usize {
        self.paths.lock().unwrap().len()
    }

    /// Stops serving: the port refuses connections afterwards.
    fn down(mut self) -> u16 {
        self.stop.store(true, Ordering::SeqCst);
        // Wakes the accept loop so it sees the flag and drops the listener.
        let _wake = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(t) = self.thread.take() {
            t.join().unwrap();
        }
        self.port
    }
}

fn result(c: &HubClient, id: &str) -> Result<(), Error> {
    c.command_result(&CommandResult::new(id, true, "ok", 1))
}

#[test]
fn primary_down_moves_to_the_fallback_sticks_and_returns_when_the_fallback_fails() {
    let primary_port = LoopbackHub::start(0).down();
    let fallback = LoopbackHub::start(0);
    let primary_url = format!("http://127.0.0.1:{primary_port}");
    let c = HubClient::new(
        Box::new(HttpTransport::with_timeout(std::time::Duration::from_secs(
            5,
        ))),
        identity(&[&primary_url, &fallback.url()]),
    );

    // The primary refuses connections: the call lands on the fallback, which is kept.
    result(&c, "c1").unwrap();
    assert_eq!(fallback.served(), 1);
    assert_eq!(c.current_url(), Some(fallback.url()));

    // The primary is back, yet calls stay on the fallback while it answers.
    let primary = LoopbackHub::start(primary_port);
    result(&c, "c2").unwrap();
    result(&c, "c3").unwrap();
    assert_eq!((primary.served(), fallback.served()), (0, 3));

    // The fallback fails: the next call goes round to the primary and stays there.
    let fallback_url = fallback.url();
    fallback.down();
    result(&c, "c4").unwrap();
    result(&c, "c5").unwrap();
    assert_eq!(c.current_url(), Some(primary_url.clone()));
    assert_eq!(
        *primary.paths.lock().unwrap(),
        ["/v1/commands/c4/result", "/v1/commands/c5/result"]
    );

    // Both down: the call fails with the transport error, nothing is remembered wrongly.
    primary.down();
    let err = result(&c, "c6").unwrap_err();
    assert!(matches!(err, Error::Transport(_)), "{err}");
    assert_eq!(c.hub_urls(), [primary_url, fallback_url]);
}
