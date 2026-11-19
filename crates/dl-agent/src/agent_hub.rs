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
        let Some(urls) = crate::hub_urls::accept(offered, &answered) else {
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
}

#[cfg(test)]
#[path = "agent_hub_tests.rs"]
mod tests;
