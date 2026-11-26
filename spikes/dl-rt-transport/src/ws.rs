//! WebSocket over TLS 1.3 over TCP adapter: one socket, both channels multiplexed by the
//! frame's `chan` byte. The writer prefers control frames (biased select) so a ping does
//! not wait behind queued bulk frames in the app; bytes already in the TCP stream still
//! block it (head-of-line).

use crate::frame::Frame;
use crate::link::{BULK, CONTROL, Half, IN_DEPTH, Link, OUT_DEPTH, TaskGuard, echo};
use crate::tlsconf::{SERVER_NAME, TestPki};
use crate::{Result, spawn_logged};
use futures_util::{SinkExt, StreamExt};
use rustls::pki_types::ServerName;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::error::{Error as WsError, ProtocolError};
use tokio_tungstenite::{WebSocketStream, accept_async, client_async};

/// A running echo server.
#[derive(Debug)]
pub struct Server {
    /// TCP address.
    pub addr: SocketAddr,
    _accept: TaskGuard,
}

/// Starts an echo server on 127.0.0.1.
pub async fn serve(pki: &TestPki) -> Result<Server> {
    let acceptor = TlsAcceptor::from(Arc::new(pki.server(b"http/1.1")?));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let accept = spawn_logged("ws accept", async move {
        loop {
            let (tcp, _) = listener.accept().await?;
            tcp.set_nodelay(true)?;
            let acceptor = acceptor.clone();
            spawn_logged("ws conn", async move {
                let tls = acceptor.accept(tcp).await?;
                let ws = accept_async(tls).await?;
                echo(spawn_link(ws)).await
            });
        }
    });
    Ok(Server {
        addr,
        _accept: TaskGuard(vec![accept]),
    })
}

/// Client side TLS settings.
#[derive(Clone)]
pub struct Client {
    connector: TlsConnector,
}

impl Client {
    /// Client trusting the test certificate.
    pub fn new(pki: &TestPki) -> Result<Self> {
        Ok(Self {
            connector: TlsConnector::from(Arc::new(pki.client(b"http/1.1")?)),
        })
    }

    /// TCP + TLS + WebSocket upgrade (3 RTT before the first frame).
    pub async fn connect(&self, addr: SocketAddr) -> Result<Link> {
        let tcp = TcpStream::connect(addr).await?;
        tcp.set_nodelay(true)?;
        let tls = self
            .connector
            .connect(ServerName::try_from(SERVER_NAME)?, tcp)
            .await?;
        let (ws, _resp) = client_async("wss://localhost/sync/v1/session", tls).await?;
        Ok(spawn_link(ws))
    }
}

fn peer_closed(e: &WsError) -> bool {
    matches!(
        e,
        WsError::ConnectionClosed
            | WsError::AlreadyClosed
            | WsError::Protocol(ProtocolError::SendAfterClosing)
    )
}

fn spawn_link<S>(ws: WebSocketStream<S>) -> Link
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut sink, mut stream) = ws.split();
    let (ctrl_tx, mut ctrl_out) = mpsc::channel::<Frame>(OUT_DEPTH);
    let (bulk_tx, mut bulk_out) = mpsc::channel::<Frame>(OUT_DEPTH);
    let (ctrl_in_tx, ctrl_in) = mpsc::channel(IN_DEPTH);
    let (bulk_in_tx, bulk_in) = mpsc::channel(IN_DEPTH);
    let writer = spawn_logged("ws writer", async move {
        let (mut ctrl_open, mut bulk_open) = (true, true);
        while ctrl_open || bulk_open {
            let next = tokio::select! {
                biased;
                f = ctrl_out.recv(), if ctrl_open => { if f.is_none() { ctrl_open = false; } f }
                f = bulk_out.recv(), if bulk_open => { if f.is_none() { bulk_open = false; } f }
            };
            if let Some(f) = next {
                match sink.send(Message::Binary(f.encode().into())).await {
                    Ok(()) => {}
                    // The peer closed first: frames still queued have nowhere to go.
                    Err(e) if peer_closed(&e) => return Ok(()),
                    Err(e) => return Err(e.into()),
                }
            }
        }
        match sink.close().await {
            Err(e) if !peer_closed(&e) => Err(e.into()),
            _ => Ok(()),
        }
    });
    let reader = spawn_logged("ws reader", async move {
        while let Some(msg) = stream.next().await {
            let bytes = match msg {
                Ok(Message::Binary(b)) => b,
                Ok(Message::Close(_)) => break,
                Ok(_) => continue,
                Err(
                    tokio_tungstenite::tungstenite::Error::ConnectionClosed
                    | tokio_tungstenite::tungstenite::Error::AlreadyClosed,
                ) => break,
                Err(tokio_tungstenite::tungstenite::Error::Protocol(p))
                    if p.to_string().contains("without closing handshake") =>
                {
                    break;
                }
                Err(e) => return Err(e.into()),
            };
            let f = Frame::decode(&bytes)?;
            let q = match f.chan {
                CONTROL => &ctrl_in_tx,
                BULK => &bulk_in_tx,
                other => return Err(format!("unknown channel {other}").into()),
            };
            if q.send(f).await.is_err() {
                break;
            }
        }
        Ok(())
    });
    Link {
        control: Half {
            tx: ctrl_tx,
            rx: ctrl_in,
        },
        bulk: Half {
            tx: bulk_tx,
            rx: bulk_in,
        },
        tasks: TaskGuard(vec![writer, reader]),
    }
}
