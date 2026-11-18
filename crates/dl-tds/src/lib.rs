#![doc = include_str!("../README.md")]

mod browser;
mod client;
mod codepages;
mod collation;
mod config;
mod error;
mod login;
mod packet;
mod params;
mod prelogin;
mod results;
mod sspi;
#[cfg(windows)]
mod sspi_windows;
mod tls;
mod token;
mod types;
mod value;
mod wire;

pub use client::Client;
pub use collation::Collation;
pub use config::{Config, Encrypt};
/// TLS protocol version, for [`Config::with_tls_min`] (re-exported from `dl-tls`).
pub use dl_tls::Version as TlsVersion;
pub use error::Error;
pub use params::{IntoParam, NotRef, Param, ParamValue};
pub use results::{Column, ColumnIndex, Message, MessageKind, ResultSet, Results, Row};
pub use value::{FromValue, Value};

#[doc(hidden)]
pub mod __fuzz {
    //! Entry points for robustness tests; not part of the public API.

    /// Parses `msg` as a response token stream; returns the error, if any.
    pub fn parse_tokens(msg: &[u8]) -> Result<usize, crate::Error> {
        let mut s = crate::token::Session::default();
        crate::token::parse(msg, &mut s).map(|r| r.results.sets().len())
    }

    /// Parses `msg` as a PRELOGIN response.
    pub fn parse_prelogin(msg: &[u8]) -> Result<u8, crate::Error> {
        crate::prelogin::decode(msg).map(|r| r.encryption)
    }

    /// Feeds raw bytes to the packet reassembler.
    pub fn reassemble(raw: &[u8]) -> Result<Vec<Vec<u8>>, crate::Error> {
        let mut r = crate::packet::MessageReader::new();
        r.push_raw(raw);
        let mut out = Vec::new();
        while let Some((_, m)) = r.poll()? {
            out.push(m);
        }
        Ok(out)
    }
}
