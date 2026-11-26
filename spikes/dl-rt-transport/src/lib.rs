//! RT-1318 spike: one long-lived duplex session between the store agent and the cloud,
//! measured over QUIC (quinn) and over WebSocket + TLS 1.3 (tokio-tungstenite + rustls),
//! through an in-process delay / loss / bandwidth shim (macOS has no `tc netem`).
//!
//! Not production code and not a DevLibrary workspace member: see `README.md`.

pub mod frame;
pub mod link;
pub mod quic;
pub mod rng;
pub mod shim;
pub mod stats;
pub mod tlsconf;
pub mod workload;
pub mod ws;

/// Error type of the spike: every failure carries its message up to `main`.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;
/// Result with [`BoxError`].
pub type Result<T> = std::result::Result<T, BoxError>;

/// Runs a background task and reports its error on stderr instead of dropping it.
pub fn spawn_logged<F>(name: &'static str, fut: F) -> tokio::task::JoinHandle<()>
where
    F: std::future::Future<Output = Result<()>> + Send + 'static,
{
    tokio::spawn(async move {
        if let Err(e) = fut.await {
            eprintln!("[{name}] task ended with error: {e}");
        }
    })
}
