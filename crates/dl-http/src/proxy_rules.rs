//! Proxy settings read once: the Windows Internet Options / `netsh winhttp` form
//! (`ProxyServer`, `ProxyOverride`) or a snapshot of the `HTTP(S)_PROXY` environment, with
//! optional Basic credentials for the proxy.

use crate::proxy::{first_set, is_always_direct, no_proxy_matches};
use crate::url::Scheme;
use crate::{Error, Url, encode_query_component};

/// Which hosts go direct.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Bypass {
    /// `ProxyOverride` entries (lowercase, scheme and port kept apart) and `<local>`.
    Windows { entries: Vec<Entry>, local: bool },
    /// A `NO_PROXY` list, matched like [`crate::Proxy::FromEnv`].
    NoProxy(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    host: String,
    port: Option<u16>,
}

/// A fixed proxy per protocol and its exceptions; build with
/// [`ProxyRules::from_windows`] or [`ProxyRules::from_env`], use as
/// [`crate::Proxy::Rules`]. `localhost` and loopback addresses are always direct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyRules {
    http: Option<Url>,
    https: Option<Url>,
    bypass: Bypass,
}

fn bracketed(u: &Url) -> String {
    if u.is_ipv6() {
        format!("[{}]", u.host())
    } else {
        u.host().to_string()
    }
}

fn proxy_url(value: &str) -> Result<Url, Error> {
    let value = value.trim();
    let url = if value.contains("://") {
        Url::parse(value)?
    } else {
        Url::parse(&format!("http://{value}"))?
    };
    if url.scheme() != Scheme::Http {
        return Err(Error::InvalidUrl {
            reason: "only http:// proxies are supported",
        });
    }
    Ok(url)
}

/// `*` matches any run of characters; everything else literally (both lowercase).
fn glob(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&b| b == b'*')
}

fn parse_entry(raw: &str) -> Option<Entry> {
    let e = raw.trim().to_ascii_lowercase();
    let e = e.split_once("://").map_or(e.as_str(), |(_, rest)| rest);
    let e = e.trim_end_matches('/');
    if e.is_empty() {
        return None;
    }
    if let Some(rest) = e.strip_prefix('[') {
        let (h, tail) = rest.split_once(']')?;
        let port = tail.strip_prefix(':').and_then(|p| p.parse().ok());
        return Some(Entry {
            host: h.to_string(),
            port,
        });
    }
    match e.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') && p.bytes().all(|b| b.is_ascii_digit()) => Some(Entry {
            host: h.to_string(),
            port: p.parse().ok(),
        }),
        _ => Some(Entry {
            host: e.to_string(),
            port: None,
        }),
    }
}

impl ProxyRules {
    /// Parses the Windows form: `server` is `host:port` for every protocol or
    /// `http=host:port;https=host:port` per protocol (other protocols such as `ftp=` and
    /// `socks=` are ignored; with a per-protocol list a missing protocol goes direct);
    /// `bypass` is `ProxyOverride` (`;`-separated; `netsh winhttp` also accepts `,` and
    /// spaces): host names or addresses with `*` wildcards (`*.corp.local`, `10.*`),
    /// optionally with `:port`, and `<local>` for every host name without a dot.
    ///
    /// # Errors
    /// [`Error::InvalidUrl`] if a proxy is not a valid `http://` address, or if `server`
    /// names no proxy for HTTP or HTTPS.
    ///
    /// ```
    /// use dl_http::{ProxyRules, Url};
    /// let rules = ProxyRules::from_windows("http=p1:8080;https=p2:3128", "<local>;*.corp.local")?;
    /// let to = |u: &str| rules.proxy_for(&Url::parse(u).unwrap()).map(|p| p.port());
    /// assert_eq!(to("https://hub.example.com/"), Some(3128));
    /// assert_eq!(to("http://hub.example.com/"), Some(8080));
    /// assert_eq!(to("https://db.corp.local/"), None);
    /// assert_eq!(to("https://intranet/"), None);
    /// # Ok::<(), dl_http::Error>(())
    /// ```
    pub fn from_windows(server: &str, bypass: &str) -> Result<ProxyRules, Error> {
        let (mut http, mut https) = (None, None);
        let server = server.trim();
        if server.contains('=') {
            for part in server.split(';') {
                let Some((proto, value)) = part.split_once('=') else {
                    continue;
                };
                let slot = match proto.trim().to_ascii_lowercase().as_str() {
                    "http" => &mut http,
                    "https" => &mut https,
                    _ => continue,
                };
                if !value.trim().is_empty() {
                    *slot = Some(proxy_url(value)?);
                }
            }
        } else if !server.is_empty() {
            let url = proxy_url(server)?;
            http = Some(url.clone());
            https = Some(url);
        }
        if http.is_none() && https.is_none() {
            return Err(Error::InvalidUrl {
                reason: "the proxy setting names no HTTP or HTTPS proxy",
            });
        }
        let mut entries = Vec::new();
        let mut local = false;
        for raw in bypass.split([';', ',', ' ', '\t', '\r', '\n']) {
            if raw.trim().eq_ignore_ascii_case("<local>") {
                local = true;
            } else if let Some(e) = parse_entry(raw) {
                entries.push(e);
            }
        }
        Ok(ProxyRules {
            http,
            https,
            bypass: Bypass::Windows { entries, local },
        })
    }

    /// A snapshot of [`crate::Proxy::FromEnv`]: `HTTPS_PROXY` / `HTTP_PROXY` (either case;
    /// `HTTP_PROXY` ignored under CGI) and `NO_PROXY`, read once through `get`. `Ok(None)`
    /// when neither proxy is set.
    ///
    /// # Errors
    /// [`Error::InvalidUrl`] if a proxy is not a valid `http://` address.
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Result<Option<ProxyRules>, Error> {
        let https = first_set(&get, &["HTTPS_PROXY", "https_proxy"]);
        let http = if get("REQUEST_METHOD").is_some() {
            None
        } else {
            first_set(&get, &["HTTP_PROXY", "http_proxy"])
        };
        if http.is_none() && https.is_none() {
            return Ok(None);
        }
        Ok(Some(ProxyRules {
            http: http.as_deref().map(proxy_url).transpose()?,
            https: https.as_deref().map(proxy_url).transpose()?,
            bypass: Bypass::NoProxy(first_set(&get, &["NO_PROXY", "no_proxy"]).unwrap_or_default()),
        }))
    }

    /// Adds Basic credentials to every proxy that has none in its URL (sent as
    /// `Proxy-Authorization`). An empty `user` changes nothing.
    ///
    /// # Errors
    /// [`Error::InvalidUrl`] if the rebuilt proxy address does not parse.
    pub fn with_credentials(mut self, user: &str, password: &str) -> Result<ProxyRules, Error> {
        if user.is_empty() {
            return Ok(self);
        }
        for url in [&mut self.http, &mut self.https].into_iter().flatten() {
            if !url.username().is_empty() || url.password().is_some() {
                continue;
            }
            let host = bracketed(url);
            *url = Url::parse(&format!(
                "http://{}:{}@{host}:{}",
                encode_query_component(user),
                encode_query_component(password),
                url.port()
            ))?;
        }
        Ok(self)
    }

    /// Whether any proxy carries credentials.
    pub fn has_credentials(&self) -> bool {
        [&self.http, &self.https]
            .into_iter()
            .flatten()
            .any(|u| !u.username().is_empty() || u.password().is_some())
    }

    /// The proxy for `target`, or `None` to connect directly.
    pub fn proxy_for(&self, target: &Url) -> Option<&Url> {
        if is_always_direct(target.host()) {
            return None;
        }
        let proxy = match target.scheme() {
            Scheme::Https => self.https.as_ref(),
            Scheme::Http => self.http.as_ref(),
        }?;
        let bypassed = match &self.bypass {
            Bypass::NoProxy(list) => no_proxy_matches(list, target.host(), target.port()),
            Bypass::Windows { entries, local } => {
                let host = target.host().trim_end_matches('.').to_ascii_lowercase();
                (*local && !host.contains('.') && !host.contains(':'))
                    || entries
                        .iter()
                        .any(|e| e.port.is_none_or(|p| p == target.port()) && glob(&e.host, &host))
            }
        };
        (!bypassed).then_some(proxy)
    }

    /// The settings without credentials, for logs and the self-test: `http=host:port
    /// https=host:port; 3 bypass entries + <local>; credentials yes`.
    pub fn describe(&self) -> String {
        let at = |u: &Option<Url>| u.as_ref().map(|u| format!("{}:{}", bracketed(u), u.port()));
        let mut s = match (at(&self.http), at(&self.https)) {
            (Some(a), Some(b)) if a == b => a,
            (a, b) => format!(
                "http={} https={}",
                a.as_deref().unwrap_or("direct"),
                b.as_deref().unwrap_or("direct")
            ),
        };
        match &self.bypass {
            Bypass::Windows { entries, local } => {
                s.push_str(&format!("; {} bypass entr", entries.len()));
                s.push_str(if entries.len() == 1 { "y" } else { "ies" });
                if *local {
                    s.push_str(" + <local>");
                }
            }
            Bypass::NoProxy(list) if !list.trim().is_empty() => s.push_str("; NO_PROXY set"),
            Bypass::NoProxy(_) => {}
        }
        s.push_str(if self.has_credentials() {
            "; credentials yes"
        } else {
            "; credentials no"
        });
        s
    }
}

#[cfg(test)]
#[path = "proxy_rules_tests.rs"]
mod tests;
