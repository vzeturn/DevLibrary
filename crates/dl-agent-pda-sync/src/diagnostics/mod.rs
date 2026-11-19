//! The site event queue statistics `status` shows (port of `Diagnostics/`). The bundle
//! itself is dl-agent's `collect_diagnostics`.

pub mod queue_stats;

pub use queue_stats::SiteEventQueueStats;

/// `Environment.MachineName`: `COMPUTERNAME` on Windows, the short host name elsewhere.
pub fn machine_name() -> String {
    #[cfg(windows)]
    {
        std::env::var("COMPUTERNAME").unwrap_or_default()
    }
    #[cfg(not(windows))]
    {
        let host = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
        host.trim().split('.').next().unwrap_or("").to_owned()
    }
}
