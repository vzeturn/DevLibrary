//! The hub hosts an agent calls, primary first (PROTOCOL.md §2.1). The hub hands an ordered
//! list out in the enrol and heartbeat answers; the agent keeps a checked copy in
//! `identity.json` so a later outage of the primary host falls back without a reinstall.

use dl_json::Value;

/// Most hosts kept (the hub applies the same cap).
pub const MAX_HUB_URLS: usize = 5;

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
    let loopback = matches!(host, "localhost" | "127.0.0.1" | "[::1]");
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

/// The list to keep from a hub's `hub_urls` answer: the valid, distinct entries in the hub's
/// order, capped at [`MAX_HUB_URLS`], always including `answered` (the host that just
/// worked) so a misconfigured list can never cut the agent off. `None` when the answer has
/// no usable list.
pub fn accept(offered: &Value, answered: &str) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for u in offered.as_array()?.iter().filter_map(Value::as_str) {
        if let Some(n) = normalize(u)
            && !out.iter().any(|o| o.eq_ignore_ascii_case(&n))
        {
            out.push(n);
        }
    }
    if out.is_empty() {
        return None;
    }
    let answered = answered.trim_end_matches('/');
    if !out.iter().any(|o| o.eq_ignore_ascii_case(answered)) {
        out.truncate(MAX_HUB_URLS - 1);
        out.push(answered.to_string());
    }
    out.truncate(MAX_HUB_URLS);
    Some(out)
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
            accept(&offered, "https://agent.pms.id.vn").unwrap(),
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
        let got = accept(&only_new, "https://old.test/").unwrap();
        assert_eq!(got.len(), MAX_HUB_URLS);
        assert_eq!(got.last().map(String::as_str), Some("https://old.test"));
        assert!(accept(&json!(["http://x.test"]), "https://a.test").is_none());
        assert!(accept(&json!(null), "https://a.test").is_none());
    }
}
