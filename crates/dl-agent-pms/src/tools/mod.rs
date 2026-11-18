//! Sub-commands the module adds to `dl-agent`: `pms-migrate` (the port of
//! `pms-agent migrate`) and `pms-enroll` (writes the cloud URL and a one-time code into the
//! SmilePmsAgent config file for the module to enroll with).

pub mod enroll;
pub mod migrate;

pub use enroll::pms_enroll_tool;
pub use migrate::pms_migrate_tool;
