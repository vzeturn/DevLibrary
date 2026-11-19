//! The PDA cloud link as the dl-agent watchdog sees it (B5).
//!
//! The core watchdog restarts the service when a module's declared cloud link stays
//! silent for 20 minutes while that cloud answers (`dl_agent::watchdog`), counting from
//! `max(ok_at, since)`. A wrong `ok_at` therefore restarts a healthy service, so the
//! module declares the link only from what proves it works: the last heartbeat the PDA
//! cloud accepted (2xx), in unix seconds of the system clock
//! ([`crate::config::AgentHealth::last_heartbeat_ok`]). [`CloudLinkPublisher`] turns that
//! value into `ModuleContext::set_cloud_link` / `clear_cloud_link` calls:
//!
//! - nothing is declared before the first accepted heartbeat of the run, so a site that
//!   never reaches the cloud (no internet, revoked agent) is never restarted for it;
//! - `ok_at` only moves forward;
//! - `reenroll_required` withdraws the link: a restart cannot fix a revoked agent.

/// What the module does with its cloud link after one look at the heartbeat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkUpdate {
    /// Leave the declared link (or its absence) as it is.
    Keep,
    /// `set_cloud_link(url, ok_at)`.
    Set {
        /// The PDA cloud URL from `agent.json`.
        url: String,
        /// The last accepted heartbeat, unix seconds.
        ok_at: u64,
    },
    /// `clear_cloud_link()`.
    Clear,
}

/// Decides the cloud link updates from successive heartbeat readings.
#[derive(Debug, Clone)]
pub struct CloudLinkPublisher {
    url: String,
    published: u64,
    declared: bool,
}

impl CloudLinkPublisher {
    /// A publisher for the enrolled cloud `url`; nothing declared yet.
    pub fn new(url: &str) -> CloudLinkPublisher {
        CloudLinkPublisher {
            url: url.trim().to_owned(),
            published: 0,
            declared: false,
        }
    }

    /// The update for `heartbeat_ok_at` (unix seconds, 0 before the first accepted
    /// heartbeat) while the agent is `reenroll_required` or not.
    pub fn next(&mut self, heartbeat_ok_at: u64, reenroll_required: bool) -> LinkUpdate {
        if reenroll_required || self.url.is_empty() {
            return if std::mem::take(&mut self.declared) {
                LinkUpdate::Clear
            } else {
                LinkUpdate::Keep
            };
        }
        if heartbeat_ok_at == 0 || (self.declared && heartbeat_ok_at <= self.published) {
            return LinkUpdate::Keep;
        }
        self.published = self.published.max(heartbeat_ok_at);
        self.declared = true;
        LinkUpdate::Set {
            url: self.url.clone(),
            ok_at: self.published,
        }
    }

    /// Whether a link is declared now.
    pub fn declared(&self) -> bool {
        self.declared
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dl_agent::watchdog::{Action, Beat, Link, STALE_SECONDS, State, decide};

    const URL: &str = "https://pda.example.test";
    const T0: u64 = 1_794_729_600;

    fn set(ok_at: u64) -> LinkUpdate {
        LinkUpdate::Set {
            url: URL.to_owned(),
            ok_at,
        }
    }

    /// What the watchdog decides at `now` for a service whose link was declared at
    /// `since` with `ok_at`, the PDA cloud answering and everything else healthy.
    fn watchdog(now: u64, since: u64, ok_at: Option<u64>) -> Action {
        let links = ok_at
            .map(|ok_at| Link {
                module: "pda-sync".into(),
                url: URL.into(),
                ok_at,
                since,
            })
            .into_iter()
            .collect();
        let beat = Beat {
            at: now,
            hub_at: now,
            version: "1.0.0".into(),
            pid: 42,
            channel: String::new(),
            skew: 0,
            links,
        };
        decide(
            &mut State::default(),
            now,
            dl_agent::watchdog::Service::Running,
            Some(&beat),
            false,
            || true,
            |_| true,
        )
    }

    #[test]
    fn nothing_is_declared_before_the_first_accepted_heartbeat() {
        let mut p = CloudLinkPublisher::new(URL);
        assert_eq!(p.next(0, false), LinkUpdate::Keep);
        assert_eq!(p.next(0, false), LinkUpdate::Keep);
        assert!(!p.declared());
        // Without a declared link the watchdog has nothing to restart for, however long
        // the cloud stays out of reach.
        assert_eq!(
            watchdog(T0 + 10 * STALE_SECONDS, T0, None),
            Action::Nothing("healthy".into())
        );
    }

    #[test]
    fn ok_at_only_moves_forward() {
        let mut p = CloudLinkPublisher::new(&format!("  {URL} "));
        assert_eq!(p.next(T0, false), set(T0));
        assert_eq!(p.next(T0, false), LinkUpdate::Keep);
        assert_eq!(p.next(T0 - 30, false), LinkUpdate::Keep, "never backwards");
        assert_eq!(
            p.next(0, false),
            LinkUpdate::Keep,
            "a reset reading is ignored"
        );
        assert_eq!(p.next(T0 + 15, false), set(T0 + 15));
    }

    #[test]
    fn reenroll_required_withdraws_the_link_once() {
        let mut p = CloudLinkPublisher::new(URL);
        assert_eq!(p.next(0, true), LinkUpdate::Keep, "nothing to withdraw");
        assert_eq!(p.next(T0, false), set(T0));
        assert_eq!(p.next(T0 + 15, true), LinkUpdate::Clear);
        assert_eq!(p.next(T0 + 30, true), LinkUpdate::Keep);
        assert!(!p.declared());
        assert_eq!(
            p.next(T0 + 45, false),
            set(T0 + 45),
            "declared again once it works"
        );
    }

    #[test]
    fn a_declared_link_restarts_only_after_twenty_silent_minutes() {
        let mut p = CloudLinkPublisher::new(URL);
        let LinkUpdate::Set { ok_at, .. } = p.next(T0, false) else {
            panic!("declared")
        };
        // Heartbeats every 15 s keep it fresh; a short outage does not restart.
        assert_eq!(
            watchdog(T0 + STALE_SECONDS - 1, T0, Some(ok_at)),
            Action::Nothing("healthy".into())
        );
        assert!(matches!(
            watchdog(T0 + STALE_SECONDS, T0, Some(ok_at)),
            Action::Restart(_)
        ));
    }

    #[test]
    fn an_empty_cloud_url_declares_nothing() {
        let mut p = CloudLinkPublisher::new(" ");
        assert_eq!(p.next(T0, false), LinkUpdate::Keep);
    }
}
