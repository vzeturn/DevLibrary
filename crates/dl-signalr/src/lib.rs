#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

mod connection;
mod error;
pub mod negotiate;
mod options;
pub mod protocol;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod transport;

#[cfg(all(test, not(all(target_arch = "wasm32", target_os = "unknown"))))]
mod tests;

pub use connection::HubConnection;
pub use error::Error;
pub use options::{CloseReason, ConnectOptions, Handler};
