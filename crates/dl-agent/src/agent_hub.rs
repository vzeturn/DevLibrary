//! The hub host list (PROTOCOL.md §2.1): a heartbeat answer may carry `hub_urls`; the agent
//! switches to it at once and keeps it in `identity.json` for the next start, so an outage
//! of the primary host falls back to the next one without a reinstall.

use dl_json::Value;

use super::Agent;
use crate::report::Severity;

impl Agent {
    /// Adopts the hub's host list from a heartbeat answer when it is usable and new.
    pub(super) fn adopt_hub_urls(&mut self, offered: Option<&Value>) {
        let (Some(offered), Some(answered)) = (offered, self.hub.current_url()) else {
            return;
        };
        let accepted = crate::hub_urls::accept(offered, &answered);
        self.note_ignored_hub_urls(accepted.ignored);
        let Some(urls) = accepted.urls else {
            dl_log::warn!("the hub sent no usable host list"; hub_urls = %dl_json::to_string(offered));
            return;
        };
        if !self.hub.set_hub_urls(urls.clone()) {
            return;
        }
        let mut id = self.hub.identity().clone();
        id.hub_urls = urls;
        match id.save_meta(&self.opts.data_dir) {
            Ok(()) => dl_log::info!("hub host list updated"; hub_urls = %id.hub_urls.join(", ")),
            Err(e) => {
                dl_log::error!("cannot save the hub host list"; error = %e);
                self.report(Severity::Warning, "hub_urls_not_saved", &e.to_string());
            }
        }
    }

    /// Hosts in the hub's list that are off the build's allowlist: a warning report each time
    /// that set changes (not on every heartbeat), and once more after a restart.
    fn note_ignored_hub_urls(&mut self, ignored: Vec<String>) {
        if ignored == self.ignored_hub_urls {
            return;
        }
        if !ignored.is_empty() {
            let list = ignored.join(", ");
            dl_log::warn!("hub hosts outside the allowlist ignored"; hub_urls = %list);
            self.report(
                Severity::Warning,
                "hub_urls_ignored",
                &format!(
                    "the hub listed hosts this build does not trust, ignored: {list} (allowed: {})",
                    crate::hub_urls::ALLOWED_HOSTS.join(", ")
                ),
            );
        }
        self.ignored_hub_urls = ignored;
    }
}

#[cfg(test)]
#[path = "agent_hub_tests.rs"]
mod tests;
