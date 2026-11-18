//! The agent-side allowlist (`egressrelay.CheckURL`, `CheckMethod`, `ForwardHeader`) and
//! the timeout rule of `RelayWorker.perform` (ADR 0016 §2): https only, port 443, no user
//! info, host under an allowed suffix; only three headers travel. A hub that asks for a
//! LAN address is refused on the customer's machine: the agent is not a proxy into the
//! hotel network.

use std::time::Duration;

use dl_http::{Scheme, Url};

/// The only destinations a relayed call may have.
pub const ALLOWED_HOST_SUFFIXES: &[&str] = &[".vnpt-invoice.com.vn"];

/// Timeout when the hub asks for none.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// The longest timeout a relayed call gets.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(60);

/// The URL does not parse.
pub const INVALID_URL: &str = "địa chỉ không hợp lệ";
/// The URL is not https.
pub const NOT_HTTPS: &str = "chỉ chuyển tiếp https";
/// The URL carries user info.
pub const HAS_USERINFO: &str = "địa chỉ không được chứa tài khoản";
/// The URL names a port other than 443.
pub const NOT_443: &str = "chỉ chuyển tiếp cổng 443";
/// The host is not under an allowed suffix.
pub const HOST_NOT_ALLOWED: &str = "máy chủ đích không nằm trong danh sách cho phép";
/// The method is not `POST` or `GET`.
pub const METHOD_NOT_ALLOWED: &str = "phương thức không được chuyển tiếp";

/// The scheme before the first `:`, when `raw` starts with one.
fn scheme_of(raw: &str) -> Option<&str> {
    let (s, _) = raw.split_once(':')?;
    let ok = s.starts_with(|c: char| c.is_ascii_alphabetic())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    ok.then_some(s)
}

/// The authority of `raw` (between `//` and the first `/`, `?` or `#`).
fn authority_of(raw: &str) -> &str {
    let rest = raw.split_once("//").map_or("", |(_, r)| r);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    &rest[..end]
}

/// `CheckURL`: the parsed URL when `raw` may be relayed, else the reason it may not.
///
/// Go's `url.Parse` accepts any scheme, so a URL this parser rejects is reported as
/// "not https" unless it claims https (then it is malformed).
pub fn check_url(raw: &str) -> Result<Url, &'static str> {
    let url = match Url::parse(raw) {
        Ok(u) => u,
        Err(_) => {
            return Err(match scheme_of(raw) {
                Some(s) if s.eq_ignore_ascii_case("https") => INVALID_URL,
                _ => NOT_HTTPS,
            });
        }
    };
    if url.scheme() != Scheme::Https {
        return Err(NOT_HTTPS);
    }
    if authority_of(raw).contains('@') {
        return Err(HAS_USERINFO);
    }
    if url.port() != 443 {
        return Err(NOT_443);
    }
    let host = url.host();
    let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    if !url.is_ipv6()
        && ALLOWED_HOST_SUFFIXES
            .iter()
            .any(|s| host.ends_with(s) && host.len() > s.len())
    {
        return Ok(url);
    }
    Err(HOST_NOT_ALLOWED)
}

/// `CheckMethod`: only `POST` and `GET` (exact, as Go compares).
pub fn check_method(method: &str) -> Result<(), &'static str> {
    if method == "POST" || method == "GET" {
        Ok(())
    } else {
        Err(METHOD_NOT_ALLOWED)
    }
}

/// `ForwardHeader` on the canonical name: `Content-Type`, `SOAPAction` and `Accept` only.
pub fn forward_header(name: &str) -> bool {
    ["Content-Type", "SOAPAction", "Accept"]
        .iter()
        .any(|h| h.eq_ignore_ascii_case(name))
}

/// The call timeout: `timeout_ms <= 0` gets the default, anything above the cap is capped.
pub fn clamp_timeout(timeout_ms: i64) -> Duration {
    if timeout_ms <= 0 {
        return DEFAULT_TIMEOUT;
    }
    Duration::from_millis(u64::try_from(timeout_ms).unwrap_or(u64::MAX)).min(MAX_TIMEOUT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_vnpt_portals_only() {
        for ok in [
            "https://abc-tt78.vnpt-invoice.com.vn/PublishService.asmx",
            "https://ABC.VNPT-Invoice.com.vn:443/x?y=1",
            "https://a.vnpt-invoice.com.vn./x",
        ] {
            assert!(check_url(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn refuses_everything_else_with_gos_reason() {
        for (raw, why) in [
            ("http://abc.vnpt-invoice.com.vn/x", NOT_HTTPS),
            ("ftp://abc.vnpt-invoice.com.vn/x", NOT_HTTPS),
            ("abc.vnpt-invoice.com.vn/x", NOT_HTTPS),
            ("https://u:p@abc.vnpt-invoice.com.vn/x", HAS_USERINFO),
            ("https://@abc.vnpt-invoice.com.vn/x", HAS_USERINFO),
            ("https://abc.vnpt-invoice.com.vn:8443/x", NOT_443),
            ("https://abc.vnpt-invoice.com.vn:80/x", NOT_443),
            ("https://192.168.1.10/x", HOST_NOT_ALLOWED),
            ("https://10.0.0.5:443/SMILE_FO", HOST_NOT_ALLOWED),
            ("https://[fd00::1]/x", HOST_NOT_ALLOWED),
            ("https://localhost/x", HOST_NOT_ALLOWED),
            ("https://vnpt-invoice.com.vn/x", HOST_NOT_ALLOWED),
            ("https://.vnpt-invoice.com.vn/x", INVALID_URL),
            ("https://evil.com/a.vnpt-invoice.com.vn", HOST_NOT_ALLOWED),
            ("https://a.vnpt-invoice.com.vn.evil.com/x", HOST_NOT_ALLOWED),
            ("https://exa mple.vnpt-invoice.com.vn/x", INVALID_URL),
        ] {
            assert_eq!(check_url(raw).err(), Some(why), "{raw}");
        }
    }

    #[test]
    fn methods() {
        assert!(check_method("POST").is_ok());
        assert!(check_method("GET").is_ok());
        for m in ["PUT", "DELETE", "CONNECT", "post", ""] {
            assert_eq!(check_method(m), Err(METHOD_NOT_ALLOWED), "{m}");
        }
    }

    #[test]
    fn only_three_headers_travel() {
        for h in [
            "Content-Type",
            "content-type",
            "SOAPAction",
            "Soapaction",
            "ACCEPT",
        ] {
            assert!(forward_header(h), "{h}");
        }
        for h in [
            "Authorization",
            "Cookie",
            "Host",
            "X-Forwarded-For",
            "Accept-Encoding",
        ] {
            assert!(!forward_header(h), "{h}");
        }
    }

    #[test]
    fn timeout_defaults_and_cap() {
        assert_eq!(clamp_timeout(0), Duration::from_secs(30));
        assert_eq!(clamp_timeout(-5), Duration::from_secs(30));
        assert_eq!(clamp_timeout(1500), Duration::from_millis(1500));
        assert_eq!(clamp_timeout(60_000), Duration::from_secs(60));
        assert_eq!(clamp_timeout(600_000), Duration::from_secs(60));
        assert_eq!(clamp_timeout(i64::MAX), Duration::from_secs(60));
    }
}
