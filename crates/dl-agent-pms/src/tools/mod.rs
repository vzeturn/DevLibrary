//! Sub-commands the module adds to `dl-agent`: `pms-enroll` (writes the cloud URL and a
//! one-time code into the SmilePmsAgent config file for the module to enroll with) and the
//! retired `pms-migrate` (MM_PMS migrations run in the cloud now).

pub mod enroll;
pub mod migrate;

pub use enroll::pms_enroll_tool;
pub use migrate::pms_migrate_tool;
