//! Local opt-out of automatic updates (PROTOCOL.md §6.5): a `no-update` file in the data
//! directory stops the agent installing on the hub's `update_check`. An explicit `update_now`
//! command still runs, as the operator's override.

use std::path::Path;

/// File name inside the data directory.
pub const FILE: &str = "no-update";

/// Value of `update_blocked` in the heartbeat while the file exists.
pub const BLOCKED: &str = "local";

/// Whether the opt-out file is present. Any entry named `no-update` counts.
pub fn blocked(data_dir: &Path) -> bool {
    data_dir.join(FILE).exists()
}

/// Why the agent does not update by itself, when it does not.
pub fn reason(data_dir: &Path) -> Option<&'static str> {
    blocked(data_dir).then_some(BLOCKED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testdir::TestDir;

    #[test]
    fn the_file_turns_the_opt_out_on_and_removing_it_turns_it_off() {
        let d = TestDir::new("optout");
        assert_eq!(reason(&d.0), None);
        std::fs::write(d.0.join(FILE), b"").unwrap();
        assert!(blocked(&d.0));
        assert_eq!(reason(&d.0), Some("local"));
        std::fs::remove_file(d.0.join(FILE)).unwrap();
        assert!(!blocked(&d.0));
    }
}
