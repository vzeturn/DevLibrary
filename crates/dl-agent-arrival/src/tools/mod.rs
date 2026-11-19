//! Sub-commands the module adds to `dl-agent`: `arrival-enroll` (the fresh enrol of a site,
//! keeping the removed .NET agent's tenant key and PMS connection).

pub mod enroll;
pub mod files;

pub use enroll::arrival_enroll_tool;
