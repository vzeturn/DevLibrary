#![doc = include_str!("../README.md")]

mod arg;
mod command;
mod error;
mod help;
mod matches;
mod parser;
mod suggest;

pub use arg::Arg;
pub use command::Command;
pub use error::{Error, ErrorKind};
pub use matches::{Matches, ValueSource};
