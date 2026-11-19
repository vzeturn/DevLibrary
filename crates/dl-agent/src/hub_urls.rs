//! The hub hosts an agent calls, primary first (PROTOCOL.md §2.1). The hub hands an ordered
//! list out in the enrol and heartbeat answers; the agent keeps a checked copy in
//! `identity.json` so a later outage of the primary host falls back without a reinstall.
//! Only hosts on the build's allowlist ([`ALLOWED_HOSTS`]) are taken from a hub's list.

use dl_json::Value;

/// Most hosts kept (the hub applies the same cap).
pub const MAX_HUB_URLS: usize = 5;

/// Hosts a hub's list may name, fixed at build time: an exact name, or `*.` and a domain for
/// any host under it (not the domain itself). Loopback (`localhost`, `127.0.0.1`, `[::1]`,
/// dev hubs) is always allowed. A host the hub lists outside these is ignored and reported.
pub const ALLOWED_HOSTS: [&str; 3] = [
    "agent.pms.id.vn",
    "*.pms.id.vn",
    "agent-hub.vutq-sense.workers.dev",
];

/// Test hubs (`https://hub`, `https://<name>.test`), in unit tests only.
#[cfg(test)]
const TEST_HOSTS: [&str; 2] = ["hub", "*.test"];
#[cfg(not(test))]
const TEST_HOSTS: [&str; 0] = [];

fn is_loopback(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

/// The host of a [`normalize`]d URL, lower case, without the port.
fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split('/').next().unwrap_or_default();
    let host = match authority.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => h,
        _ => authority,
    };
    host.to_ascii_lowercase()
}

/// Whether the host of `url` (a [`normalize`]d URL) is on [`ALLOWED_HOSTS`] or loopback.
pub fn host_allowed(url: &str) -> bool {
    let host = host_of(url);
    is_loopback(&host)
        || ALLOWED_HOSTS
            .iter()
            .chain(TEST_HOSTS.iter())
            .any(|pattern| match pattern.strip_prefix("*.") {
                Some(domain) => host
                    .strip_suffix(domain)
                    .is_some_and(|sub| sub.len() > 1 && sub.ends_with('.')),
                None => host == *pattern,
            })
}

/// `raw` as a base URL the agent may sign requests to: `https://`, or `http://` on loopback
/// only; no credentials, query or fragment; no trailing `/`. `None` otherwise.
pub fn normalize(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let (scheme, rest) = raw.split_once("://")?;
    let authority = rest.split('/').next().unwrap_or_default();
    let host = authority.rsplit_once(':').map_or(authority, |(h, p)| {
        if p.chars().all(|c| c.is_ascii_digit()) && !h.is_empty() {
            h
        } else {
            authority
        }
    });
    let loopback = is_loopback(host);
    let scheme_ok =
        scheme.eq_ignore_ascii_case("https") || (scheme.eq_ignore_ascii_case("http") && loopback);
    if !scheme_ok
        || authority.is_empty()
        || authority.contains('@')
        || raw.contains('?')
        || raw.contains('#')
        || raw.chars().any(char::is_whitespace)
    {
        return None;
    }
    Some(raw.trim_end_matches('/').to_string())
}

/// What [`accept`] made of a hub's `hub_urls` answer.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Accepted {
    /// The list to keep; `None` when the answer has no usable entry.
    pub urls: Option<Vec<String>>,
    /// Valid URLs whose host is not on [`ALLOWED_HOSTS`]: ignored, to be reported.
    pub ignored: Vec<String>,
}

/// The list to keep from a hub's `hub_urls` answer: the valid, distinct entries on an
/// allowed host in the hub's order, capped at [`MAX_HUB_URLS`], always including `answered`
/// (the host that just worked) so a misconfigured list can never cut the agent off.
pub fn accept(offered: &Value, answered: &str) -> Accepted {
    let mut out: Vec<String> = Vec::new();
    let mut ignored: Vec<String> = Vec::new();
    for n in offered
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_str)
        .filter_map(normalize)
    {
        if !host_allowed(&n) {
            if !ignored.iter().any(|o| o.eq_ignore_ascii_case(&n)) {
                ignored.push(n);
            }
        } else if !out.iter().any(|o| o.eq_ignore_ascii_case(&n)) {
            out.push(n);
        }
    }
    if out.is_empty() {
        return Accepted {
            urls: None,
            ignored,
        };
    }
    let answered = answered.trim_end_matches('/');
    if !out.iter().any(|o| o.eq_ignore_ascii_case(answered)) {
        out.truncate(MAX_HUB_URLS - 1);
        out.push(answered.to_string());
    }
    out.truncate(MAX_HUB_URLS);
    Accepted {
        urls: Some(out),
        ignored,
    }
}

#[cfg(test)]
mod tests {
    use dl_json::json;

    use super::*;

    #[test]
    fn only_https_or_loopback_http_without_credentials() {
        assert_eq!(
            normalize(" https://agent.pms.id.vn/ ").as_deref(),
            Some("https://agent.pms.id.vn")
        );
        assert_eq!(
            normalize("http://127.0.0.1:8787").as_deref(),
            Some("http://127.0.0.1:8787")
        );
        for bad in [
            "http://agent.pms.id.vn",
            "https://u:p@agent.pms.id.vn",
            "https://agent.pms.id.vn/?x=1",
            "ftp://x",
            "agent.pms.id.vn",
            "https://",
            "http://localhost.evil.test",
        ] {
            assert_eq!(normalize(bad), None, "{bad}");
        }
    }

    #[test]
    fn keeps_hub_order_dedupes_caps_and_keeps_the_host_that_answered() {
        let offered = json!([
            "https://agent.pms.id.vn",
            "https://AGENT.pms.id.vn/",
            "http://evil.test",
            "https://agent-hub.vutq-sense.workers.dev"
        ]);
        assert_eq!(
            accept(&offered, "https://agent.pms.id.vn").urls.unwrap(),
            [
                "https://agent.pms.id.vn",
                "https://agent-hub.vutq-sense.workers.dev"
            ]
        );
        // A list without the host that answered keeps that host last.
        let only_new = json!([
            "https://a.test",
            "https://b.test",
            "https://c.test",
            "https://d.test",
            "https://e.test"
        ]);
        let got = accept(&only_new, "https://old.test/").urls.unwrap();
        assert_eq!(got.len(), MAX_HUB_URLS);
        assert_eq!(got.last().map(String::as_str), Some("https://old.test"));
        assert!(
            accept(&json!(["http://x.test"]), "https://a.test")
                .urls
                .is_none()
        );
        assert!(accept(&json!(null), "https://a.test").urls.is_none());
    }

    #[test]
    fn hosts_off_the_build_allowlist_are_ignored_and_named() {
        for ok in [
            "https://agent.pms.id.vn",
            "https://bongspa-ops.pms.id.vn",
            "https://a.b.PMS.id.vn:8443",
            "https://agent-hub.vutq-sense.workers.dev",
            "http://127.0.0.1:8787",
            "https://localhost",
        ] {
            assert!(host_allowed(ok), "{ok}");
        }
        for bad in [
            "https://pms.id.vn",
            "https://evilpms.id.vn",
            "https://agent.pms.id.vn.evil.com",
            "https://agent-hub.other.workers.dev",
            "https://hub.example.com",
        ] {
            assert!(!host_allowed(bad), "{bad}");
        }
        let got = accept(
            &json!([
                "https://hub.example.com",
                "https://agent.pms.id.vn",
                "https://hub.example.com/"
            ]),
            "https://agent.pms.id.vn",
        );
        assert_eq!(got.urls.unwrap(), ["https://agent.pms.id.vn"]);
        assert_eq!(got.ignored, ["https://hub.example.com"]);
        // Nothing allowed: no list, the ignored ones still named.
        let none = accept(&json!(["https://evil.example"]), "https://agent.pms.id.vn");
        assert_eq!(none.urls, None);
        assert_eq!(none.ignored, ["https://evil.example"]);
    }
}
