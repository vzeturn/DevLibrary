//! QUIC adapter (quinn): one connection, one bidirectional stream per channel. The control
//! stream gets a higher send priority so a ping is never queued behind bulk data, and a lost
//! bulk packet does not block the control stream (no head-of-line blocking across streams).

use crate::frame::{read_frame, write_frame};
use crate::link::{BULK, CONTROL, Half, IN_DEPTH, Link, OUT_DEPTH, TaskGuard, echo};
use crate::tlsconf::{SERVER_NAME, TestPki};
use crate::{Result, spawn_logged};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{Connection, ConnectionError, Endpoint, RecvStream, SendStream};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;

/// ALPN of the spike protocol.
pub const ALPN: &[u8] = b"dl-rt/1";

/// A running echo server.
#[derive(Debug)]
pub struct Server {
    /// UDP address.
    pub addr: SocketAddr,
    _accept: TaskGuard,
}

/// Starts an echo server on 127.0.0.1.
pub async fn serve(pki: &TestPki) -> Result<Server> {
    let crypto = QuicServerConfig::try_from(pki.server(ALPN)?)?;
    let endpoint = Endpoint::server(
        quinn::ServerConfig::with_crypto(Arc::new(crypto)),
        "127.0.0.1:0".parse()?,
    )?;
    let addr = endpoint.local_addr()?;
    let accept = spawn_logged("quic accept", async move {
        while let Some(incoming) = endpoint.accept().await {
            spawn_logged("quic conn", async move {
                let conn = match incoming.await {
                    Ok(c) => c,
                    // A connect-time probe may close before the server side finishes.
                    Err(ConnectionError::ApplicationClosed(_)) => return Ok(()),
                    Err(e) => return Err(e.into()),
                };
                let mut control = match conn.accept_bi().await {
                    Ok(s) => s,
                    // The connect-time probe closes without opening streams.
                    Err(ConnectionError::ApplicationClosed(_)) => return Ok(()),
                    Err(e) => return Err(e.into()),
                };
                let mut bulk = conn.accept_bi().await?;
                expect_channel(&mut control.1, CONTROL).await?;
                expect_channel(&mut bulk.1, BULK).await?;
                echo(spawn_link(conn, control, bulk)?).await
            });
        }
        Ok(())
    });
    Ok(Server {
        addr,
        _accept: TaskGuard(vec![accept]),
    })
}

/// Client endpoint.
#[derive(Debug)]
pub struct Client {
    endpoint: Endpoint,
}

impl Client {
    /// Client endpoint trusting the test certificate.
    pub fn new(pki: &TestPki) -> Result<Self> {
        let crypto = QuicClientConfig::try_from(pki.client(ALPN)?)?;
        let mut endpoint = Endpoint::client("127.0.0.1:0".parse()?)?;
        endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(crypto)));
        Ok(Self { endpoint })
    }

    /// Completes the handshake (1 RTT).
    pub async fn connect(&self, addr: SocketAddr) -> Result<Connection> {
        Ok(self.endpoint.connect(addr, SERVER_NAME)?.await?)
    }

    /// Opens the two channel streams on a connection.
    pub async fn open_link(&self, conn: Connection) -> Result<Link> {
        // A stream is announced to the peer only when data is written on it, so each one
        // starts with its channel id (the server waits for both before echoing).
        let mut control = conn.open_bi().await?;
        control.0.write_all(&[CONTROL]).await?;
        let mut bulk = conn.open_bi().await?;
        bulk.0.write_all(&[BULK]).await?;
        spawn_link(conn, control, bulk)
    }
}

async fn expect_channel(recv: &mut RecvStream, want: u8) -> Result<()> {
    let mut id = [0u8; 1];
    recv.read_exact(&mut id).await?;
    if id[0] != want {
        return Err(format!(
            "stream announced channel {} where {want} was expected",
            id[0]
        )
        .into());
    }
    Ok(())
}

fn spawn_link(
    conn: Connection,
    control: (SendStream, RecvStream),
    bulk: (SendStream, RecvStream),
) -> Result<Link> {
    control.0.set_priority(1)?;
    let mut tasks = Vec::new();
    let c = spawn_half(control, &mut tasks, Some(conn));
    let b = spawn_half(bulk, &mut tasks, None);
    Ok(Link {
        control: c,
        bulk: b,
        tasks: TaskGuard(tasks),
    })
}

fn spawn_half(
    (mut send, mut recv): (SendStream, RecvStream),
    tasks: &mut Vec<tokio::task::JoinHandle<()>>,
    keep_alive: Option<Connection>,
) -> Half {
    let (out_tx, mut out_rx) = mpsc::channel(OUT_DEPTH);
    let (in_tx, in_rx) = mpsc::channel(IN_DEPTH);
    tasks.push(spawn_logged("quic writer", async move {
        while let Some(f) = out_rx.recv().await {
            write_frame(&mut send, &f).await?;
        }
        send.finish()?;
        Ok(())
    }));
    tasks.push(spawn_logged("quic reader", async move {
        let _conn = keep_alive;
        loop {
            match read_frame(&mut recv).await {
                Ok(Some(f)) => {
                    if in_tx.send(f).await.is_err() {
                        return Ok(());
                    }
                }
                Ok(None) => return Ok(()),
                // The peer closed the connection at the end of a run.
                Err(e)
                    if e.kind() == std::io::ErrorKind::ConnectionAborted
                        || e.kind() == std::io::ErrorKind::NotConnected =>
                {
                    return Ok(());
                }
                Err(e) => return Err(e.into()),
            }
        }
    }));
    Half {
        tx: out_tx,
        rx: in_rx,
    }
}
