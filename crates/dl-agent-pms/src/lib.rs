//! SMILE PMS site agent as a dl-agent module (`modules.pms-site`).
//!
//! The outbound-only bridge between a site's SMILE databases (SMILE_FO, SMILE_POS,
//! SMILE_BO) and SMILE PMS cloud, on the SMILE-only contract (SMILE-PMS ADR 2026-11-19):
//! ops run vendor procedures on the database the op frame names, MM_PMS lives in the cloud,
//! nothing is created on a customer database and replays are answered from a local
//! journal in the state directory. It keeps the agent's identity and state where
//! `SmilePmsAgent` left them (config file, state directory, `agent.key`, `identity.json`,
//! `databases.json`), so a site switches over without enrolling again. dl-agent owns the
//! service, self-update, log files and the shared SQL settings (`sql.json`).
//!
//! Register it with [`pms_site_module`]; it runs only where the remote config sets
//! `modules.pms-site.enabled = true`. The `pms-enroll` sub-command is [`pms_enroll_tool`];
//! [`pms_migrate_tool`] is the retired `pms-migrate`.

pub mod acl;
pub mod agent;
pub mod config;
pub mod conn;
pub mod ctx;
pub mod db;
pub mod enroll;
pub mod exec;
pub mod journal;
pub mod json;
pub mod keystore;
pub mod legacy_sql;
pub mod log;
pub mod minire;
pub mod module;
pub mod ops;
pub mod pat;
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
