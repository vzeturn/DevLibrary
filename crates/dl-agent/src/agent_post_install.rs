//! The self-test after an install or an upgrade (RT-1104): once the hub has answered the first
//! heartbeat of a version, the agent runs its own self-test in the background and uploads it,
//! so the hub holds the evidence (version, signature, service, SQL, heartbeat, permissions)
//! that the hotel's IT would otherwise confirm by hand.

use dl_json::json;

use super::Agent;
use crate::selftest::post_install;

impl Agent {
    /// Starts the post-install self-test when this version has none on the hub yet. Called
    /// after a heartbeat the hub accepted; it starts at most once per process and never blocks
    /// the agent loop (the run takes a while and the hook has its own time limits).
    pub(super) fn post_install_selftest(&mut self) {
        if self.post_selftest {
            return;
        }
        self.post_selftest = true;
        let Some(hook) = self.opts.selftest.clone() else {
            return;
        };
        let version = self.opts.version.to_string();
        let Some(trigger) = post_install::due(&self.opts.data_dir, &version) else {
            return;
        };
        let spawned = std::thread::Builder::new()
            .name("post-install-selftest".into())
            .spawn(move || {
                let (ok, _) = (hook.0)(&json!({"trigger": trigger.as_str()}));
                dl_log::info!("post-install self-test finished"; trigger = trigger.as_str(), passed = ok);
            });
        if let Err(e) = spawned {
            dl_log::warn!("cannot start the post-install self-test"; error = %e);
        }
    }
}
