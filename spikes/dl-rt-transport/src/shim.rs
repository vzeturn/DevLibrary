//! In-process network impairment between client and server: one-way delay, a bandwidth
//! cap (one FIFO queue per direction, like a store uplink) and random loss.
//!
//! - UDP (QUIC): a lost datagram is dropped; quinn's own loss recovery runs for real.
//! - TCP (WebSocket): bytes cannot be dropped above the kernel, so a lost segment is
//!   modelled as its recovery delay (`tcp_loss_penalty`, default 2 x RTT, about Linux's
//!   tail-loss probe for sparse traffic) and every later byte waits behind it (head-of-line
//!   blocking). TCP congestion control is NOT modelled, so WebSocket throughput under loss
//!   is optimistic.

use crate::rng::XorShift;
use crate::{Result, spawn_logged};
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};

/// Typical TCP payload per segment, to turn a chunk into a packet count for loss.
const MSS: usize = 1448;

/// What the shim does to traffic.
#[derive(Debug, Clone, Copy)]
pub struct Impairment {
    /// Delay added in each direction (RTT = 2 x this).
    pub one_way: Duration,
    /// Probability that a packet is lost, per direction.
    pub loss: f64,
    /// Bottleneck rate in bits per second, per direction; `None` = unlimited.
    pub rate_bps: Option<u64>,
    /// Extra delay of a lost TCP segment (stream mode only).
    pub tcp_loss_penalty: Duration,
    /// PRNG seed.
    pub seed: u64,
}

impl Impairment {
    /// A WAN link: `rtt_ms` round trip, `loss` per packet, `mbit` per direction.
    pub fn wan(rtt_ms: u64, loss: f64, mbit: u64) -> Self {
        Self {
            one_way: Duration::from_millis(rtt_ms / 2),
            loss,
            rate_bps: Some(mbit * 1_000_000),
            tcp_loss_penalty: Duration::from_millis(rtt_ms * 2),
            seed: 0x5EED,
        }
    }
}

/// Schedules packets of one direction.
#[derive(Debug)]
pub struct Pacer {
    imp: Impairment,
    rng: XorShift,
    last_dep: Instant,
    last_at: Instant,
}

impl Pacer {
    /// New pacer; `salt` separates the two directions' loss patterns.
    pub fn new(imp: Impairment, salt: u64) -> Self {
        let now = Instant::now();
        Self {
            imp,
            rng: XorShift::new(imp.seed ^ salt),
            last_dep: now,
            last_at: now,
        }
    }

    /// Delivery time of `len` bytes sent now made of `packets` packets, or `None` when a
    /// datagram is dropped (`datagram` = true). Delivery order is kept.
    pub fn schedule(&mut self, len: usize, packets: usize, datagram: bool) -> Option<Instant> {
        let now = Instant::now();
        let lost = (0..packets.max(1)).any(|_| self.rng.chance(self.imp.loss));
        if lost && datagram {
            return None;
        }
        let mut dep = now;
        if let Some(rate) = self.imp.rate_bps {
            let start = dep.max(self.last_dep);
            dep = start + Duration::from_secs_f64(len as f64 * 8.0 / rate as f64);
            self.last_dep = dep;
        }
        let mut at = dep + self.imp.one_way;
        if lost {
            at += self.imp.tcp_loss_penalty;
        }
        at = at.max(self.last_at);
        self.last_at = at;
        Some(at)
    }
}

/// A running shim; its tasks stop when it is dropped.
#[derive(Debug)]
pub struct Shim {
    /// Address clients connect to instead of the server.
    pub addr: SocketAddr,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Shim {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

type Queue = mpsc::UnboundedSender<(Instant, Vec<u8>)>;

/// UDP shim in front of `upstream` (one client at a time).
pub async fn udp(upstream: SocketAddr, imp: Impairment) -> Result<Shim> {
    let front = Arc::new(UdpSocket::bind("127.0.0.1:0").await?);
    let back = Arc::new(UdpSocket::bind("127.0.0.1:0").await?);
    back.connect(upstream).await?;
    let addr = front.local_addr()?;
    let client: Arc<Mutex<Option<SocketAddr>>> = Arc::new(Mutex::new(None));
    let (up_tx, mut up_rx) = mpsc::unbounded_channel::<(Instant, Vec<u8>)>();
    let (down_tx, mut down_rx) = mpsc::unbounded_channel::<(Instant, Vec<u8>)>();
    let mut tasks = Vec::new();

    let (f, c) = (front.clone(), client.clone());
    tasks.push(spawn_logged("udp-shim up-read", async move {
        let mut pacer = Pacer::new(imp, 1);
        let mut buf = vec![0u8; 65_536];
        loop {
            let (n, from) = f.recv_from(&mut buf).await?;
            *c.lock().map_err(|_| "client lock poisoned")? = Some(from);
            enqueue(&up_tx, pacer.schedule(n, 1, true), &buf[..n])?;
        }
    }));
    let b = back.clone();
    tasks.push(spawn_logged("udp-shim up-send", async move {
        while let Some((at, pkt)) = up_rx.recv().await {
            sleep_until(at).await;
            send_ignoring_refused(b.send(&pkt).await)?;
        }
        Ok(())
    }));
    tasks.push(spawn_logged("udp-shim down-read", async move {
        let mut pacer = Pacer::new(imp, 2);
        let mut buf = vec![0u8; 65_536];
        loop {
            let n = match back.recv(&mut buf).await {
                Ok(n) => n,
                // ICMP port unreachable from a closed server endpoint: not fatal for the shim.
                Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => continue,
                Err(e) => return Err(e.into()),
            };
            enqueue(&down_tx, pacer.schedule(n, 1, true), &buf[..n])?;
        }
    }));
    tasks.push(spawn_logged("udp-shim down-send", async move {
        while let Some((at, pkt)) = down_rx.recv().await {
            sleep_until(at).await;
            let dst = *client.lock().map_err(|_| "client lock poisoned")?;
            if let Some(dst) = dst {
                front.send_to(&pkt, dst).await?;
            }
        }
        Ok(())
    }));
    Ok(Shim { addr, tasks })
}

fn enqueue(q: &Queue, at: Option<Instant>, pkt: &[u8]) -> Result<()> {
    if let Some(at) = at {
        q.send((at, pkt.to_vec()))
            .map_err(|_| "shim sender task ended")?;
    }
    Ok(())
}

fn send_ignoring_refused(r: io::Result<usize>) -> io::Result<()> {
    match r {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => Ok(()),
        Err(e) => Err(e),
    }
}

/// TCP shim in front of `upstream` (any number of connections).
pub async fn tcp(upstream: SocketAddr, imp: Impairment) -> Result<Shim> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let accept = spawn_logged("tcp-shim accept", async move {
        let mut salt = 0u64;
        loop {
            let (inbound, _) = listener.accept().await?;
            let outbound = TcpStream::connect(upstream).await?;
            inbound.set_nodelay(true)?;
            outbound.set_nodelay(true)?;
            let (ir, iw) = inbound.into_split();
            let (or, ow) = outbound.into_split();
            salt += 2;
            spawn_logged("tcp-shim up", pipe(ir, ow, Pacer::new(imp, salt)));
            spawn_logged("tcp-shim down", pipe(or, iw, Pacer::new(imp, salt + 1)));
        }
    });
    Ok(Shim {
        addr,
        tasks: vec![accept],
    })
}

async fn pipe(mut r: OwnedReadHalf, mut w: OwnedWriteHalf, mut pacer: Pacer) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<(Instant, Vec<u8>)>();
    let writer = tokio::spawn(async move {
        while let Some((at, buf)) = rx.recv().await {
            sleep_until(at).await;
            w.write_all(&buf).await?;
        }
        w.shutdown().await
    });
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match r.read(&mut buf).await {
            Ok(n) => n,
            // The peer reset at the end of a run: end this direction like a close.
            Err(e) if e.kind() == io::ErrorKind::ConnectionReset => 0,
            Err(e) => return Err(e.into()),
        };
        if n == 0 {
            break;
        }
        let at = pacer
            .schedule(n, n.div_ceil(MSS), false)
            .ok_or("stream pacer dropped a chunk")?;
        tx.send((at, buf[..n].to_vec()))
            .map_err(|_| "tcp-shim writer ended")?;
    }
    drop(tx);
    match writer.await? {
        Ok(()) => Ok(()),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::NotConnected
            ) =>
        {
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn imp(loss: f64) -> Impairment {
        Impairment {
            one_way: Duration::from_millis(10),
            loss,
            rate_bps: Some(8_000_000),
            tcp_loss_penalty: Duration::from_millis(40),
            seed: 3,
        }
    }

    #[tokio::test]
    async fn delay_rate_and_order() {
        let mut p = Pacer::new(imp(0.0), 0);
        let t0 = Instant::now();
        let a = p.schedule(1_000_000, 1, true).unwrap(); // 1 MB at 8 Mbit/s = 1 s
        let b = p.schedule(10, 1, true).unwrap();
        assert!(a - t0 >= Duration::from_millis(1010));
        assert!(b >= a);
    }

    #[tokio::test]
    async fn loss_drops_datagrams_and_delays_streams() {
        let mut p = Pacer::new(imp(1.0), 0);
        assert_eq!(p.schedule(100, 1, true), None);
        let t0 = Instant::now();
        let at = p.schedule(100, 1, false).unwrap();
        assert!(at - t0 >= Duration::from_millis(50));
    }
}
