#![doc = include_str!("../README.md")]
#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod convert;
mod error;
mod macros;
mod map;
mod number;
mod parse;
mod ser;
mod value;

pub use convert::{FromJson, ToJson, from_str, to_string_of};
pub use error::{Error, ErrorKind, Position};
pub use map::{IntoIter, Iter, IterMut, Keys, Map, Values, ValuesMut};
pub use number::Number;
pub use parse::{ParseOptions, parse, parse_slice, parse_slice_with, parse_with};
pub use ser::{to_string, to_string_pretty, to_string_pretty_with, write_pretty_to, write_to};
pub use value::Value;

/// Re-exports used by the `json!` macro. Not part of the public API.
#[doc(hidden)]
pub mod __private {
    pub use alloc::vec;
    pub use alloc::vec::Vec;
}
