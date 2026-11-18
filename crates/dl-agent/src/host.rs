//! Facts about the machine the agent runs on, for the site survey (PROTOCOL.md §9.5).

use dl_json::{Value, json};

use crate::current_target;

/// `{family, target, version, os_arch, process_arch, win7_build, legacy_windows}`.
///
/// `legacy_windows` is `true` before Windows 10 / Server 2016 (NT < 10.0): Rust's standard
/// `*-windows-gnu` builds target Windows 10+, so these hosts need the `windows7-*` build;
/// `os_arch` differs from `process_arch` when a 32-bit build runs on 64-bit Windows.
pub fn info() -> Value {
    let version = os_version();
    let nt = version.as_deref().and_then(nt_version);
    json!({
        "family": std::env::consts::OS,
        "target": current_target(),
        "version": version,
        "nt_version": nt.map(|(a, b, c)| format!("{a}.{b}.{c}")),
        "legacy_windows": nt.map(|(a, _, _)| a < 10),
        "process_arch": std::env::consts::ARCH,
        "os_arch": os_arch(),
        "win7_build": cfg!(target_vendor = "win7"),
    })
}

/// `(major, minor, build)` from `Microsoft Windows [Version 6.1.7601]`.
pub fn nt_version(ver: &str) -> Option<(u32, u32, u32)> {
    let inner = ver.split("Version").nth(1)?;
    let digits: String = inner
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut it = digits.split('.').map(|p| p.parse::<u32>().ok());
    Some((it.next()??, it.next()??, it.next().flatten().unwrap_or(0)))
}

#[cfg(windows)]
fn os_version() -> Option<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("cmd")
        .args(["/d", "/c", "ver"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

#[cfg(not(windows))]
fn os_version() -> Option<String> {
    let release = std::fs::read_to_string("/etc/os-release").ok();
    let pretty = release.as_deref().and_then(|r| {
        r.lines()
            .find_map(|l| l.strip_prefix("PRETTY_NAME="))
            .map(|v| v.trim_matches('"').to_string())
    });
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|k| k.trim().to_string());
    match (pretty, kernel) {
        (Some(p), Some(k)) => Some(format!("{p} (kernel {k})")),
        (p, k) => p.or(k),
    }
}

#[cfg(windows)]
fn os_arch() -> Option<String> {
    // WOW64 sets PROCESSOR_ARCHITEW6432 to the real architecture for 32-bit processes.
    std::env::var("PROCESSOR_ARCHITEW6432")
        .or_else(|_| std::env::var("PROCESSOR_ARCHITECTURE"))
        .ok()
        .map(|a| a.to_ascii_lowercase())
}

#[cfg(not(windows))]
fn os_arch() -> Option<String> {
    Some(std::env::consts::ARCH.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ver_output() {
        assert_eq!(
            nt_version("Microsoft Windows [Version 6.1.7601]"),
            Some((6, 1, 7601))
        );
        assert_eq!(
            nt_version("Microsoft Windows [Version 10.0.19045.5131]"),
            Some((10, 0, 19045))
        );
        assert_eq!(nt_version("Linux"), None);
    }

    #[test]
    fn info_has_the_target_and_arch() {
        let i = info();
        assert_eq!(i["target"].as_str(), Some(current_target().as_str()));
        assert!(i["process_arch"].as_str().is_some());
    }
}
