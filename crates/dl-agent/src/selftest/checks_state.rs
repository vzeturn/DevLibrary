//! Core rows read from local state, for the hub to see how this agent reaches it and
//! updates: the hub host list (fallback), the proxy in use, and the update state
//! (probation, failed versions, a fault build).

use std::path::Path;

use super::{CORE, Check, Outcome};
use crate::identity::Identity;
use crate::update::{MAX_FAILURES_LIFETIME, MAX_FAILURES_PER_WINDOW, UpdateState};
use crate::{Version, now};

/// The hub hosts this agent knows, in order: a single host has no fallback.
pub(crate) fn hub_urls(id: Option<&Identity>) -> Check {
    let title = "Hub hosts (fallback)";
    let Some(id) = id else {
        return Check::skip(CORE, "hub_urls", title, "not enrolled");
    };
    Check::timed(CORE, "hub_urls", title, || match id.hub_urls.as_slice() {
        [] => Outcome::fail("no hub address in identity.json")
            .remedy("enroll again with `enroll --force`"),
        [one] => Outcome::warn(format!("{one} only: no fallback host"))
            .remedy("the hub sends its host list on the next heartbeat (HUB_URLS on the hub)"),
        [first, rest @ ..] => Outcome::pass(format!("{first}, fallback {}", rest.join(", "))),
    })
}

/// The proxy every hub call goes through (never its password).
pub(crate) fn proxy() -> Check {
    Check::timed(CORE, "proxy", "Proxy to the hub", || {
        let c = crate::proxy::current();
        if c.problems.is_empty() {
            Outcome::pass(c.describe())
        } else {
            Outcome::warn(format!(
                "{}; ignored: {}",
                c.describe(),
                c.problems.join("; ")
            ))
            .remedy("fix the proxy setting (HTTPS_PROXY, netsh winhttp or Internet Options)")
        }
    })
}

fn failures(state: &UpdateState, at: u64) -> String {
    let mut versions: Vec<&Version> = Vec::new();
    for (v, _) in &state.failures {
        if !versions.contains(&v) {
            versions.push(v);
        }
    }
    versions
        .iter()
        .map(|v| {
            let (recent, ever) = state.failures_of(v, at);
            let held = if ever >= MAX_FAILURES_LIFETIME {
                " (given up: send retry_update)"
            } else if recent >= MAX_FAILURES_PER_WINDOW {
                " (held for a day)"
            } else {
                ""
            };
            format!("{v} x{ever}{held}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `update.json` (probation, failed versions) and whether this binary is a fault build.
pub(crate) fn update_state(dir: &Path, version: &Version, fault: Option<&str>) -> Check {
    Check::timed(CORE, "update_state", "Update state", || {
        let state = match UpdateState::load(dir) {
            Ok(s) => s,
            Err(e) => {
                return Outcome::warn(format!("update.json unreadable: {e}"))
                    .remedy("the service starts a fresh update state; send retry_update");
            }
        };
        let mut parts = vec![format!("running {version}")];
        if let Some(p) = &state.pending {
            parts.push(format!(
                "on probation since the update from {} (until {}, boot {})",
                p.previous_version, p.probation_until, p.boots
            ));
        }
        let failed = failures(&state, now());
        if failed.is_empty() {
            parts.push("no failed version".into());
        } else {
            parts.push(format!("failed: {failed}"));
        }
        if let Some(why) = crate::fault::probation_failure(fault) {
            parts.push(why.into());
            return Outcome::warn(parts.join("; "))
                .remedy("a test build: publish the fixed version, never this one to stable");
        }
        Outcome::pass(parts.join("; "))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selftest::Status;
    use crate::store::testdir::TestDir;
    use crate::update::Pending;

    fn identity(urls: &[&str]) -> Identity {
        crate::hub::tests_support::identity(urls)
    }

    #[test]
    fn hub_hosts_warn_without_a_fallback() {
        let two = hub_urls(Some(&identity(&["https://a.test", "https://b.test"])));
        assert_eq!(two.status, Status::Pass);
        assert_eq!(two.detail, "https://a.test, fallback https://b.test");
        let one = hub_urls(Some(&identity(&["https://a.test"])));
        assert_eq!(one.status, Status::Warn);
        assert_eq!(hub_urls(None).status, Status::Skip);
    }

    #[test]
    fn update_state_lists_failed_versions_and_fault_builds() {
        let d = TestDir::new("selftest-update-state");
        let v: Version = "1.3.9".parse().unwrap();
        let fresh = update_state(&d.0, &v, None);
        assert_eq!(fresh.status, Status::Pass);
        assert_eq!(fresh.detail, "running 1.3.9; no failed version");
        let mut s = UpdateState::default();
        let bad: Version = "1.3.10".parse().unwrap();
        for _ in 0..3 {
            s.record_failure(&bad, now());
        }
        s.pending = Some(Pending {
            version: v.clone(),
            previous_version: "1.3.8".parse().unwrap(),
            probation_until: 500,
            boots: 1,
            unhealthy_before: Vec::new(),
        });
        s.save(&d.0).unwrap();
        let c = update_state(&d.0, &v, None);
        assert_eq!(c.status, Status::Pass);
        assert!(
            c.detail
                .contains("on probation since the update from 1.3.8"),
            "{}",
            c.detail
        );
        assert!(
            c.detail.contains("failed: 1.3.10 x3 (held for a day)"),
            "{}",
            c.detail
        );
        let f = update_state(&d.0, &v, Some("probation"));
        assert_eq!(f.status, Status::Warn);
        assert!(
            f.detail.contains("DL_AGENT_FAULT=probation"),
            "{}",
            f.detail
        );
    }

    #[test]
    fn proxy_row_names_the_source() {
        let c = proxy();
        assert!(matches!(c.status, Status::Pass | Status::Warn));
        assert!(!c.detail.is_empty());
    }
}
