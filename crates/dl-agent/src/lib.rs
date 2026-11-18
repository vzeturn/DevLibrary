#![doc = include_str!("../README.md")]

pub mod agent;
pub mod channel;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub mod cli;
pub mod command;
pub mod config;
mod error;
pub mod hub;
pub mod identity;
pub mod manifest;
pub mod report;
pub mod store;
pub mod supervisor;
pub mod sys_module;
pub mod update;
pub mod version;

pub use error::Error;
pub use version::Version;

/// The version of this build, from `Cargo.toml`.
pub fn agent_version() -> Version {
    // The package version is a valid SemVer string; fall back to 0.0.0 rather than panic.
    env!("CARGO_PKG_VERSION")
        .parse()
        .unwrap_or_else(|_| Version::new(0, 0, 0))
}

/// The release target of this build: `"<os>-<arch>"`, e.g. `"windows-x86_64"`.
pub fn current_target() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// Current Unix time in seconds (0 if the clock is before 1970).
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
