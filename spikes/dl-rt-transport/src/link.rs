//! A session as the workloads see it: two logical channels, `CONTROL` (pings, commands,
//! small capture batches) and `BULK` (large batches, blobs), each a pair of mpsc queues
//! fed by the transport adapter ([`crate::quic`], [`crate::ws`]).

use crate::Result;
use crate::frame::{Frame, Kind};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Channel id of control traffic.
pub const CONTROL: u8 = 0;
/// Channel id of bulk traffic.
pub const BULK: u8 = 1;
/// Outbound queue depth per channel (frames): the adapter's backpressure point.
pub const OUT_DEPTH: usize = 64;
/// Inbound queue depth per channel (frames).
pub const IN_DEPTH: usize = 4096;

/// One channel of a link.
#[derive(Debug)]
pub struct Half {
    /// Frames to send.
    pub tx: mpsc::Sender<Frame>,
    /// Frames received.
    pub rx: mpsc::Receiver<Frame>,
}

impl Half {
    /// Sends a frame; an error means the link is gone.
    pub async fn send(&self, f: Frame) -> Result<()> {
        self.tx
            .send(f)
            .await
            .map_err(|_| "link writer ended".into())
    }

    /// Next received frame; an error means the link closed.
    pub async fn recv(&mut self) -> Result<Frame> {
        self.rx.recv().await.ok_or_else(|| "link closed".into())
    }
}

/// Both channels plus the adapter tasks (aborted on drop).
#[derive(Debug)]
pub struct Link {
    /// Control channel.
    pub control: Half,
    /// Bulk channel.
    pub bulk: Half,
    /// Adapter tasks.
    pub tasks: TaskGuard,
}

impl Link {
    /// Closes both channels and waits (at most 5 s per task) for the adapter to finish.
    pub async fn close(self) -> Result<()> {
        let Link {
            control,
            bulk,
            mut tasks,
        } = self;
        drop(control);
        drop(bulk);
        for t in std::mem::take(&mut tasks.0) {
            tokio::time::timeout(std::time::Duration::from_secs(5), t)
                .await
                .map_err(|_| "link task did not stop within 5 s")??;
        }
        Ok(())
    }
}

/// Aborts its tasks when dropped.
#[derive(Debug, Default)]
pub struct TaskGuard(pub Vec<JoinHandle<()>>);

impl Drop for TaskGuard {
    fn drop(&mut self) {
        for t in &self.0 {
            t.abort();
        }
    }
}

/// Server side: acknowledge every non-ack frame on both channels until the link closes.
pub async fn echo(link: Link) -> Result<()> {
    let Link {
        control,
        bulk,
        mut tasks,
    } = link;
    let a = tokio::spawn(echo_half(control));
    let b = tokio::spawn(echo_half(bulk));
    let (ra, rb) = (a.await?, b.await?);
    ra.and(rb)?;
    // Let the writers flush and close (TLS close_notify) instead of aborting them.
    for t in std::mem::take(&mut tasks.0) {
        tokio::time::timeout(std::time::Duration::from_secs(5), t)
            .await
            .map_err(|_| "server link task did not stop within 5 s")??;
    }
    Ok(())
}

async fn echo_half(mut h: Half) -> Result<()> {
    while let Some(f) = h.rx.recv().await {
        if f.kind != Kind::Ack && h.tx.send(f.ack_of()).await.is_err() {
            break;
        }
    }
    Ok(())
}
