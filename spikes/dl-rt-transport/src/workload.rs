//! Client workloads: sequential ping-pong (latency of one small batch), windowed bulk
//! transfer (throughput with backpressure), and pings while bulk runs (control latency
//! under load: head-of-line blocking shows here).

use crate::Result;
use crate::frame::{Frame, Kind};
use crate::link::{BULK, CONTROL, Half};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::time::Instant;

fn data(chan: u8, kind: Kind, seq: u64, clock: Instant, size: usize) -> Frame {
    Frame {
        chan,
        kind,
        seq,
        sent_us: clock.elapsed().as_micros() as u64,
        payload: vec![0xA5; size],
    }
}

/// `n` sequential pings of `size` bytes on the control channel; returns each RTT.
pub async fn ping(control: &mut Half, n: u64, size: usize) -> Result<Vec<Duration>> {
    let clock = Instant::now();
    let mut out = Vec::with_capacity(n as usize);
    for seq in 0..n {
        let start = Instant::now();
        control
            .send(data(CONTROL, Kind::Ping, seq, clock, size))
            .await?;
        loop {
            let f = control.recv().await?;
            if f.kind == Kind::Ack && f.seq == seq {
                break;
            }
        }
        out.push(start.elapsed());
    }
    Ok(out)
}

/// Sends `n` bulk frames of `size` bytes with at most `window` unacknowledged; returns the
/// time until the last ack.
pub async fn throughput(bulk: &mut Half, n: u64, size: usize, window: u64) -> Result<Duration> {
    let clock = Instant::now();
    let (mut sent, mut acked) = (0u64, 0u64);
    while acked < n {
        while sent < n && sent - acked < window {
            bulk.send(data(BULK, Kind::Data, sent, clock, size)).await?;
            sent += 1;
        }
        if bulk.recv().await?.kind == Kind::Ack {
            acked += 1;
        }
    }
    Ok(clock.elapsed())
}

/// Pings on the control channel while the bulk channel streams `bulk_size` frames with
/// `window` in flight; returns the ping RTTs, the bulk bytes acknowledged meanwhile and the
/// bulk channel (so the caller can close the link cleanly).
pub async fn ping_under_bulk(
    control: &mut Half,
    mut bulk: Half,
    n: u64,
    size: usize,
    bulk_size: usize,
    window: u64,
) -> Result<(Vec<Duration>, u64, Half)> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let bulk_task = tokio::spawn(async move {
        let clock = Instant::now();
        let (mut sent, mut acked) = (0u64, 0u64);
        while !stop2.load(Ordering::Relaxed) {
            while sent - acked < window {
                bulk.send(data(BULK, Kind::Data, sent, clock, bulk_size))
                    .await?;
                sent += 1;
            }
            if bulk.recv().await?.kind == Kind::Ack {
                acked += 1;
            }
        }
        Ok::<(u64, Half), crate::BoxError>((acked * bulk_size as u64, bulk))
    });
    // Let the bulk stream fill the path before measuring.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let pings = ping(control, n, size).await;
    stop.store(true, Ordering::Relaxed);
    let (bytes, bulk) = bulk_task.await??;
    Ok((pings?, bytes, bulk))
}
