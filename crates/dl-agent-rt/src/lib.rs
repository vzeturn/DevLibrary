#![doc = include_str!("../README.md")]
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

mod codec;
mod error;
mod frame;
mod receiver;
mod sender;
mod session;

#[cfg(test)]
mod tests;

pub use codec::{FLAG_EXT, Limits, TAG_T_DETECTED, TAG_T_READ};
pub use error::Error;
pub use frame::{
    Channel, CloseCode, DEFAULT_MAX_FRAME, EXT_ENTRY_LEN, Ext, Frame, HEADER_LEN, Kind,
    MAX_CLOSE_REASON, VERSION,
};
pub use receiver::{ChannelReceiver, Delivery};
pub use sender::{Blocked, ChannelSender, Outcome, Resend, SenderState, WouldBlock};
pub use session::{Config, Event, Session, SessionState};
