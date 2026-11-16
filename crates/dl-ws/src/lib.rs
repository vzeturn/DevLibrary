#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod client;
mod error;
pub mod frame;
mod handshake;
mod socket;

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub use client::{ConnectOptions, connect, connect_with};
pub use error::Error;
pub use handshake::{accept_key, check_request, generate_key};
pub use socket::{CloseFrame, Config, Message, Role, State, WebSocket};
