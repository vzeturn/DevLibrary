//! SMILE PMS site agent as a dl-agent module (`modules.pms-site`).
//!
//! The outbound-only bridge between a site's MM_PMS database and SMILE PMS cloud: a port of
//! the Go site agent (`apps/site-agent`) through its Rust port, on DevLibrary crates only.
//! It keeps the agent's identity and state where `SmilePmsAgent` left them (config file,
//! state directory, `agent.key`, `identity.json`, `databases.json`), so a site switches
//! over without enrolling again. dl-agent owns the service, self-update and log files.
//!
//! Register it with [`pms_site_module`]; it runs only where the remote config sets
//! `modules.pms-site.enabled = true`. The `pms-migrate` and `pms-enroll` sub-commands are
//! [`pms_migrate_tool`] and [`pms_enroll_tool`].

pub mod acl;
pub mod agent;
pub mod bundle;
pub mod config;
pub mod conn;
pub mod ctx;
pub mod db;
pub mod enroll;
pub mod exec;
pub mod json;
pub mod keystore;
pub mod log;
pub mod migrate;
pub mod minire;
pub mod module;
pub mod ops;
pub mod pat;
pub mod remotemigrate;
pub mod retention;
pub mod telemetry;
pub mod tools;
pub mod wire;

pub use module::selftest::pms_site_selftest;
pub use module::{PmsSiteModule, pms_site_module};
pub use tools::{pms_enroll_tool, pms_migrate_tool};

/// Lower-case hex SHA-256.
pub fn sha256_hex(data: &[u8]) -> String {
    use dl_hash::{Digest, Sha256};
    dl_encoding::hex::encode(Sha256::digest(data))
}
