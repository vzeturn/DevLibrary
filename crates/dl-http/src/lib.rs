#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

mod body;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod client;
mod error;
mod headers;
mod message;
mod proxy;
mod proxy_rules;
mod url;

pub use body::ChunkedDecoder;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub use client::{Client, ClientOptions, ShutdownHandle, Upgraded};
pub use error::Error;
pub use headers::Headers;
pub use message::{
    BodyFraming, MAX_HEAD_SIZE, MAX_HEADERS, Method, Request, Response, StatusCode, Version,
    parse_response_head, response_framing,
};
pub use proxy::Proxy;
pub use proxy_rules::ProxyRules;
pub use url::{Scheme, Url, encode_path_segment, encode_query_component, percent_decode};
