//! Sub-commands the module adds to `dl-agent`: `arrival-enroll` (enrolls the site the way
//! Install.ps1 does, for an install without the .NET agent).

pub mod enroll;

pub use enroll::arrival_enroll_tool;
