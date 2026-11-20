#![doc = include_str!("../README.md")]

pub mod agent;
pub mod channel;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub mod cli;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub mod cli_attach;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub mod cli_install;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub mod cli_install_sql;
pub mod cli_install_sql_auto;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub mod cli_install_win;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub mod cli_sql;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub mod cli_uninstall;
pub mod command;
pub mod config;
mod error;
pub mod fault;
pub mod host;
pub mod hub;
pub mod hub_urls;
pub mod identity;
pub mod manifest;
pub mod optout;
pub mod proxy;
pub mod report;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub mod selftest;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub mod service_watch;
pub mod sql;
pub mod store;
pub mod supervisor;
pub mod sys_module;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub mod tool;
pub mod update;
pub mod version;
pub mod watchdog;

pub use error::Error;
pub use version::Version;

/// The version of this build: `DL_AGENT_VERSION` at compile time (release builds stamp it),
/// otherwise the crate version from `Cargo.toml`.
pub fn agent_version() -> Version {
    // Both are SemVer strings; fall back to 0.0.0 rather than panic.
    option_env!("DL_AGENT_VERSION")
        .unwrap_or(env!("CARGO_PKG_VERSION"))
        .parse()
        .unwrap_or_else(|_| Version::new(0, 0, 0))
}

/// The release target of this build: `"<os>-<arch>"`, e.g. `"windows-x86_64"`.
///
/// Builds for the Windows 7 / Server 2008 R2 targets (`*-win7-windows-*`) report
/// `"windows7-<arch>"`, so the hub serves them the legacy artifact and never a build that
/// needs Windows 10. 32-bit builds report `"x86"`.
pub fn current_target() -> String {
    target_name(
        std::env::consts::OS,
        std::env::consts::ARCH,
        cfg!(target_vendor = "win7"),
    )
}

fn target_name(os: &str, arch: &str, win7: bool) -> String {
    let os = if win7 && os == "windows" {
        "windows7"
    } else {
        os
    };
    format!("{os}-{arch}")
}

/// Current Unix time in seconds (0 if the clock is before 1970).
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod target_tests {
    use super::target_name;

    #[test]
    fn win7_builds_get_their_own_target() {
        assert_eq!(target_name("windows", "x86_64", false), "windows-x86_64");
        assert_eq!(target_name("windows", "x86", true), "windows7-x86");
        assert_eq!(target_name("linux", "x86_64", true), "linux-x86_64");
    }
}
