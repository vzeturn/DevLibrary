//! SMILE Hotel Link module for dl-agent (port of the Go `smile-hl-agent`).

pub mod breaker;
pub mod config;
#[cfg(test)]
mod e2e;
pub mod enroll;
pub mod error;
mod gofmt;
mod gojson;
pub mod hubclient;
pub mod jobmodel;
pub mod jobqueue;
pub mod legacy_sql;
pub mod liveness;
mod log;
pub mod module;
pub mod pms;
pub mod policycache;
mod reconcile;
pub mod run;
#[cfg(test)]
mod testhub;

pub use enroll::hl_enroll_tool;
pub use module::selftest::hotellink_selftest;
pub use module::{HotellinkModule, hotellink_module};
