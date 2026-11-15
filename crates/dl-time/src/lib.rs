#![doc = include_str!("../README.md")]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;
#[cfg(all(test, not(feature = "std")))]
extern crate std;

mod buf;
mod calendar;
mod date;
mod datetime;
mod duration;
mod error;
mod http;
mod offset;
mod offset_datetime;
mod parse;
mod pattern;
mod time;

#[cfg(test)]
mod tests;

pub use calendar::{Month, Weekday, is_leap_year};
pub use date::Date;
pub use datetime::DateTime;
pub use duration::Duration;
pub use error::Error;
pub use offset::Offset;
pub use offset_datetime::OffsetDateTime;
pub use pattern::Locale;
pub use time::Time;
