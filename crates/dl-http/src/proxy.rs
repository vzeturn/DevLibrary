//! Proxy selection: explicit proxy URL or `HTTP(S)_PROXY` / `NO_PROXY` from the environment.

use crate::url::Scheme;
use crate::{Error, ProxyRules, Url};
use std::net::IpAddr;

/// Which proxy the client uses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Proxy {
    /// Always connect directly.
    None,
    /// Send every request through this `http://` proxy (also for `localhost`).
    /// User info in the URL is sent as `Proxy-Authorization: Basic`.
    Http(Url),
    /// Read the environment for every request, like Go's `http.ProxyFromEnvironment`:
    /// `HTTPS_PROXY` / `https_proxy` for `https` URLs, `HTTP_PROXY` / `http_proxy` for
    /// `http` URLs (ignored when `REQUEST_METHOD` is set, i.e. inside CGI), and
    /// `NO_PROXY` / `no_proxy` for exceptions. A value without a scheme means `http://`.
    /// `localhost` and loopback addresses are always direct.
    ///
    /// `NO_PROXY` is a comma-separated list of `*` (everything direct), host names
    /// (`example.com` matches it and its subdomains, `.example.com` only subdomains),
    /// optionally with `:port`, IP addresses, and CIDR ranges such as `10.0.0.0/8`.
    #[default]
    FromEnv,
    /// Fixed settings read once: Windows Internet Options, `netsh winhttp`, or an
    /// environment snapshot with credentials ([`ProxyRules`]).
    Rules(ProxyRules),
}

impl Proxy {
    /// The proxy to use for `target`, or `None` for a direct connection.
    ///
    /// # Errors
    /// [`Error::InvalidUrl`] if the configured proxy URL is invalid or is not `http://`.
    ///
    /// ```
    /// use dl_http::{Proxy, Url};
    /// let proxy = Proxy::Http(Url::parse("http://proxy.local:3128")?);
    /// let target = Url::parse("https://api.example.com/")?;
    /// assert_eq!(proxy.resolve(&target)?.unwrap().port(), 3128);
    /// assert!(Proxy::None.resolve(&target)?.is_none());
    /// # Ok::<(), dl_http::Error>(())
    /// ```
    pub fn resolve(&self, target: &Url) -> Result<Option<Url>, Error> {
        match self {
            Proxy::None => Ok(None),
            Proxy::Http(url) => check_proxy_url(url).map(|()| Some(url.clone())),
            Proxy::FromEnv => resolve_with(target, |name| std::env::var(name).ok()),
            Proxy::Rules(rules) => Ok(rules.proxy_for(target).cloned()),
        }
    }
}

fn check_proxy_url(url: &Url) -> Result<(), Error> {
    if url.scheme() != Scheme::Http {
        return Err(Error::InvalidUrl {
            reason: "only http:// proxies are supported",
        });
    }
    Ok(())
}

pub(crate) fn first_set(get: &impl Fn(&str) -> Option<String>, names: &[&str]) -> Option<String> {
    names
        .iter()
        .filter_map(|n| get(n))
        .find(|v| !v.trim().is_empty())
}

/// [`Proxy::FromEnv`] with an injectable environment (for tests).
pub(crate) fn resolve_with(
    target: &Url,
    get: impl Fn(&str) -> Option<String>,
) -> Result<Option<Url>, Error> {
    if is_always_direct(target.host()) {
        return Ok(None);
    }
    let value = match target.scheme() {
        Scheme::Https => first_set(&get, &["HTTPS_PROXY", "https_proxy"]),
        Scheme::Http if get("REQUEST_METHOD").is_some() => None,
        Scheme::Http => first_set(&get, &["HTTP_PROXY", "http_proxy"]),
    };
    let Some(value) = value else {
        return Ok(None);
    };
    if let Some(no_proxy) = first_set(&get, &["NO_PROXY", "no_proxy"])
        && no_proxy_matches(&no_proxy, target.host(), target.port())
    {
        return Ok(None);
    }
    let value = value.trim();
    let url = if value.contains("://") {
        Url::parse(value)?
    } else {
        Url::parse(&format!("http://{value}"))?
    };
    check_proxy_url(&url)?;
    Ok(Some(url))
}

/// `localhost`, `*.localhost` and loopback addresses never use a proxy.
pub(crate) fn is_always_direct(host: &str) -> bool {
    let h = host.strip_suffix('.').unwrap_or(host);
    if h == "localhost" || h.ends_with(".localhost") {
        return true;
    }
    h.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn in_cidr(ip: IpAddr, net: IpAddr, bits: u8) -> bool {
    match (ip, net) {
        (IpAddr::V4(a), IpAddr::V4(n)) if bits <= 32 => {
            let mask = u32::MAX.checked_shl(32 - u32::from(bits)).unwrap_or(0);
            u32::from(a) & mask == u32::from(n) & mask
        }
        (IpAddr::V6(a), IpAddr::V6(n)) if bits <= 128 => {
            let mask = u128::MAX.checked_shl(128 - u32::from(bits)).unwrap_or(0);
            u128::from(a) & mask == u128::from(n) & mask
        }
        _ => false,
    }
}

/// Splits a `NO_PROXY` entry into host and optional port.
fn split_entry(e: &str) -> (&str, Option<&str>) {
    if let Some(rest) = e.strip_prefix('[') {
        return match rest.split_once(']') {
            Some((h, tail)) => (h, tail.strip_prefix(':')),
            None => (e, None),
        };
    }
    if e.parse::<IpAddr>().is_ok() {
        return (e, None);
    }
    match e.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => (h, Some(p)),
        _ => (e, None),
    }
}

/// Whether `host:port` matches a `NO_PROXY` list.
pub(crate) fn no_proxy_matches(list: &str, host: &str, port: u16) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    let host_ip = host.parse::<IpAddr>().ok();
    for entry in list.split(',') {
        let entry = entry.trim().to_ascii_lowercase();
        if entry.is_empty() {
            continue;
        }
        if entry == "*" {
            return true;
        }
        if let Some((net, bits)) = entry.split_once('/') {
            if let (Some(ip), Ok(net), Ok(bits)) =
                (host_ip, net.parse::<IpAddr>(), bits.parse::<u8>())
                && in_cidr(ip, net, bits)
            {
                return true;
            }
            continue;
        }
        let (h, p) = split_entry(&entry);
        if let Some(p) = p
            && p.parse::<u16>().ok() != Some(port)
        {
            continue;
        }
        let h = h.strip_suffix('.').unwrap_or(h);
        if let (Some(a), Ok(b)) = (host_ip, h.parse::<IpAddr>()) {
            if a == b {
                return true;
            }
            continue;
        }
        let h = h.strip_prefix('*').unwrap_or(h);
        if h.is_empty() {
            continue;
        }
        if h.starts_with('.') {
            if host.ends_with(h) {
                return true;
            }
        } else if host == h || host.strip_suffix(h).is_some_and(|pre| pre.ends_with('.')) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    fn resolved(target: &str, vars: &[(&str, &str)]) -> Option<String> {
        resolve_with(&url(target), env(vars))
            .unwrap()
            .map(|u| u.to_string())
    }

    #[test]
    fn no_proxy_rules() {
        let list = "example.com, .internal.corp,*.wild.test, 10.0.0.0/8, 192.168.1.5, [::1]:8080, fd00::/8, api.local:8443, 2001:db8::1";
        let yes = [
            ("example.com", 80),
            ("EXAMPLE.com", 80),
            ("example.com.", 80),
            ("www.example.com", 443),
            ("a.b.example.com", 443),
            ("x.internal.corp", 80),
            ("x.wild.test", 80),
            ("10.1.2.3", 80),
            ("192.168.1.5", 80),
            ("fd12::1", 443),
            ("api.local", 8443),
            ("2001:db8::1", 80),
        ];
        for (h, p) in yes {
            assert!(no_proxy_matches(list, h, p), "{h}:{p} should be direct");
        }
        let no = [
            ("notexample.com", 80),
            ("example.com.evil", 80),
            ("internal.corp", 80),
            ("wild.test", 80),
            ("11.0.0.1", 80),
            ("192.168.1.6", 80),
            ("api.local", 443),
            ("fe80::1", 80),
            ("other.org", 80),
        ];
        for (h, p) in no {
            assert!(
                !no_proxy_matches(list, h, p),
                "{h}:{p} should use the proxy"
            );
        }
        assert!(no_proxy_matches("*", "anything", 1));
        assert!(no_proxy_matches(" , *", "anything", 1));
        assert!(!no_proxy_matches("", "anything", 1));
        assert!(!no_proxy_matches(
            "10.0.0.0/99,bogus/8,10.0.0.0/x",
            "10.0.0.1",
            80
        ));
        assert!(no_proxy_matches("0.0.0.0/0", "8.8.8.8", 80));
        assert!(no_proxy_matches("[::1]:8080", "::1", 8080));
        assert!(!no_proxy_matches("[::1]:8080", "::1", 8081));
    }

    #[test]
    fn env_selection() {
        let vars = [
            ("HTTPS_PROXY", "http://secure-proxy:3128"),
            ("http_proxy", "plain-proxy:8080"),
            ("NO_PROXY", "direct.example"),
        ];
        assert_eq!(
            resolved("https://api.example.com/", &vars).as_deref(),
            Some("http://secure-proxy:3128/")
        );
        assert_eq!(
            resolved("http://api.example.com/", &vars).as_deref(),
            Some("http://plain-proxy:8080/")
        );
        assert_eq!(resolved("https://direct.example/", &vars), None);
        assert_eq!(resolved("https://sub.direct.example/", &vars), None);
        // Loopback and localhost are always direct.
        for t in [
            "http://localhost:8080/",
            "http://127.0.0.1/",
            "http://[::1]/",
            "http://a.localhost/",
        ] {
            assert_eq!(resolved(t, &vars), None, "{t}");
        }
        // Upper case wins over lower case; empty values are ignored.
        let vars = [("HTTPS_PROXY", " "), ("https_proxy", "http://lower:1")];
        assert_eq!(
            resolved("https://x/", &vars).as_deref(),
            Some("http://lower:1/")
        );
        let vars = [
            ("HTTP_PROXY", "http://upper:1"),
            ("http_proxy", "http://lower:1"),
        ];
        assert_eq!(
            resolved("http://x/", &vars).as_deref(),
            Some("http://upper:1/")
        );
        // Nothing configured.
        assert_eq!(resolved("https://x/", &[]), None);
        // HTTPS_PROXY does not apply to http URLs.
        assert_eq!(
            resolved("http://x/", &[("HTTPS_PROXY", "http://p:1")]),
            None
        );
        // CGI: HTTP_PROXY may come from a request header ("httpoxy"), so it is ignored.
        let vars = [("HTTP_PROXY", "http://evil:1"), ("REQUEST_METHOD", "GET")];
        assert_eq!(resolved("http://x/", &vars), None);
        // Credentials in the proxy URL are kept.
        let vars = [("HTTPS_PROXY", "http://user:p%40ss@proxy:3128")];
        let p = resolve_with(&url("https://x/"), env(&vars))
            .unwrap()
            .unwrap();
        assert_eq!((p.username(), p.password()), ("user", Some("p%40ss")));
    }

    #[test]
    fn invalid_proxy_values() {
        let target = url("https://x/");
        for v in [
            "https://proxy:443",
            "socks5://proxy:1080",
            "http://bad host:1",
            "http://proxy:99999",
        ] {
            assert!(
                resolve_with(&target, env(&[("HTTPS_PROXY", v)])).is_err(),
                "{v}"
            );
        }
        assert!(Proxy::Http(url("https://proxy/")).resolve(&target).is_err());
        assert!(Proxy::None.resolve(&target).unwrap().is_none());
        assert_eq!(
            Proxy::Http(url("http://p:1/"))
                .resolve(&url("http://localhost/"))
                .unwrap(),
            Some(url("http://p:1/"))
        );
    }
}
