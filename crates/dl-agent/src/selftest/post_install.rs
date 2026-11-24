//! The self-test that runs by itself after an install or an upgrade (RT-1104).
//!
//! The agent keeps the version it last reported a self-test for in [`VERSION_MARKER`]. On the
//! first heartbeat the hub answers after a start, a version that differs from the marker (or no
//! marker at all) means this binary was just installed or upgraded: the agent runs the
//! self-test once, uploads it with trigger `update`, and only then writes the
//! marker. A run that could not be uploaded leaves the marker alone, so the next start tries
//! again; a run with failed checks is still uploaded and marked (the hub shows the failure).

use std::path::Path;

use super::Trigger;

/// The version a post-install self-test was last uploaded for, in the data directory.
pub const VERSION_MARKER: &str = "selftest.version";

/// The trigger a post-install run should use for `version`, or `None` when it already ran.
pub fn due(dir: &Path, version: &str) -> Option<Trigger> {
    match std::fs::read_to_string(dir.join(VERSION_MARKER)) {
        Ok(seen) if seen.trim() == version => None,
        Ok(_) => Some(Trigger::Update),
        // No marker: a fresh install (the installer's own run was `install`; this one comes
        // from the running service) or an agent from before this feature.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(Trigger::Update),
        Err(e) => {
            // An unreadable marker would repeat the run on every start; say so and run once.
            dl_log::warn!("selftest: cannot read the version marker"; error = %e);
            Some(Trigger::Update)
        }
    }
}

/// Records that the post-install run for `version` reached the hub.
pub fn mark_done(dir: &Path, version: &str) {
    if let Err(e) = std::fs::write(dir.join(VERSION_MARKER), version) {
        dl_log::warn!("selftest: cannot write the version marker"; error = %e);
    }
}

/// Whether `trigger` is the one a post-install run uses.
pub fn is_post_install(trigger: Trigger) -> bool {
    trigger == Trigger::Update
}
