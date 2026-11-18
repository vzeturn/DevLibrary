//! `http` / `https` URLs (an RFC 3986 subset) and percent-encoding helpers.

use crate::Error;
use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

/// Longest URL accepted by [`Url::parse`], in bytes.
const MAX_URL_LEN: usize = 64 * 1024;
/// Longest host name (DNS limit).
const MAX_HOST_LEN: usize = 253;

const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

/// The scheme of a [`Url`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scheme {
    /// Plain HTTP, default port 80.
    Http,
    /// HTTP over TLS, default port 443.
    Https,
}

impl Scheme {
    /// `"http"` or `"https"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Scheme::Http => "http",
            Scheme::Https => "https",
        }
    }

    /// 80 for HTTP, 443 for HTTPS.
    pub fn default_port(self) -> u16 {
        match self {
            Scheme::Http => 80,
            Scheme::Https => 443,
        }
    }
}

/// An absolute `http` or `https` URL.
///
/// Parsing normalises the URL: the scheme and host are lower-cased, a default port is
/// dropped, an empty path becomes `/`, dot segments are removed, percent-escapes are
/// upper-cased, and characters that are not allowed in a component (non-ASCII, `"`, `<`,
/// `>`, `\`, `^`, `` ` ``, `{`, `|`, `}`) are percent-encoded. Control characters and
/// spaces are rejected. The fragment is dropped because it is never sent to the server.
///
/// `Display` writes the normalised URL, including the password if there is one; `Debug`
/// hides the password.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Url {
    scheme: Scheme,
    username: String,
    password: Option<String>,
    /// Lower-case registered name, dotted IPv4, or IPv6 without brackets.
    host: String,
    ipv6: bool,
    port: u16,
    /// Always starts with `/`.
    path: String,
    query: Option<String>,
}

fn invalid(reason: &'static str) -> Error {
    Error::InvalidUrl { reason }
}

fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}

fn is_sub_delim(b: u8) -> bool {
    matches!(
        b,
        b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'='
    )
}

fn is_userinfo_char(b: u8) -> bool {
    is_unreserved(b) || is_sub_delim(b) || b == b':'
}

fn is_path_char(b: u8) -> bool {
    is_unreserved(b) || is_sub_delim(b) || matches!(b, b':' | b'@' | b'/')
}

fn is_query_char(b: u8) -> bool {
    is_path_char(b) || b == b'?'
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn push_pct(out: &mut String, b: u8) {
    out.push('%');
    out.push(char::from(HEX_UPPER[usize::from(b >> 4)]));
    out.push(char::from(HEX_UPPER[usize::from(b & 15)]));
}

/// Rejects control characters and spaces anywhere in a URL.
fn check_chars(s: &str) -> Result<(), Error> {
    if s.bytes().any(|b| b <= b' ' || b == 0x7f) {
        return Err(invalid("URL contains a space or control character"));
    }
    Ok(())
}

/// Validates percent-escapes and encodes every byte that `allowed` rejects.
fn normalize(input: &str, allowed: fn(u8) -> bool) -> Result<String, Error> {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' {
            match (bytes.get(i + 1).copied(), bytes.get(i + 2).copied()) {
                (Some(h), Some(l)) if hex_val(h).is_some() && hex_val(l).is_some() => {
                    out.push('%');
                    out.push(char::from(h.to_ascii_uppercase()));
                    out.push(char::from(l.to_ascii_uppercase()));
                    i += 3;
                    continue;
                }
                _ => return Err(invalid("invalid percent-encoding")),
            }
        }
        if allowed(b) {
            out.push(char::from(b));
        } else {
            push_pct(&mut out, b);
        }
        i += 1;
    }
    Ok(out)
}

/// Percent-encodes a string for use as a query key or value.
///
/// Every byte except the RFC 3986 unreserved characters (`A-Z a-z 0-9 - . _ ~`) is
/// encoded; a space becomes `%20`.
///
/// ```
/// assert_eq!(dl_http::encode_query_component("a b&c=d/é"), "a%20b%26c%3Dd%2F%C3%A9");
/// ```
pub fn encode_query_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if is_unreserved(b) {
            out.push(char::from(b));
        } else {
            push_pct(&mut out, b);
        }
    }
    out
}

/// Percent-encodes a string for use as one path segment (`/` is encoded too).
///
/// ```
/// assert_eq!(dl_http::encode_path_segment("a/b c"), "a%2Fb%20c");
/// ```
pub fn encode_path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if is_path_char(b) && b != b'/' {
            out.push(char::from(b));
        } else {
            push_pct(&mut out, b);
        }
    }
    out
}

/// Decodes `%XX` escapes. `+` is left as is.
///
/// ```
/// assert_eq!(dl_http::percent_decode("p%40ss%20word").unwrap(), b"p@ss word");
/// assert!(dl_http::percent_decode("%zz").is_err());
/// ```
pub fn percent_decode(s: &str) -> Result<Vec<u8>, Error> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let h = bytes.get(i + 1).copied().and_then(hex_val);
            let l = bytes.get(i + 2).copied().and_then(hex_val);
            match (h, l) {
                (Some(h), Some(l)) => out.push((h << 4) | l),
                _ => return Err(invalid("invalid percent-encoding")),
            }
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(out)
}

/// RFC 3986 section 5.2.4 for a path that starts with `/`.
fn remove_dot_segments(path: &str) -> String {
    let segments: Vec<&str> = path.split('/').skip(1).collect();
    let n = segments.len();
    let mut out: Vec<&str> = Vec::with_capacity(n);
    for (i, seg) in segments.into_iter().enumerate() {
        let last = i + 1 == n;
        match seg {
            "." => {
                if last {
                    out.push("");
                }
            }
            ".." => {
                out.pop();
                if last {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    let mut s = String::with_capacity(path.len());
    for seg in out {
        s.push('/');
        s.push_str(seg);
    }
    if s.is_empty() {
        s.push('/');
    }
    s
}

/// Index of the `:` ending a scheme, if `s` starts with one.
fn scheme_end(s: &str) -> Option<usize> {
    for (i, b) in s.bytes().enumerate() {
        match b {
            b':' => {
                return (i > 0 && s.as_bytes()[0].is_ascii_alphabetic()).then_some(i);
            }
            b if b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.') => {}
            _ => return None,
        }
    }
    None
}

fn parse_port(s: &str, scheme: Scheme) -> Result<u16, Error> {
    if s.is_empty() {
        return Ok(scheme.default_port());
    }
    if s.len() > 5 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid("invalid port"));
    }
    match s.parse::<u32>() {
        Ok(p @ 1..=65535) => u16::try_from(p).map_err(|_| invalid("port out of range")),
        _ => Err(invalid("port out of range")),
    }
}

fn parse_reg_name(h: &str) -> Result<String, Error> {
    if h.is_empty() {
        return Err(invalid("empty host"));
    }
    if h.len() > MAX_HOST_LEN + 1 {
        return Err(invalid("host name is too long"));
    }
    if !h
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
    {
        return Err(invalid("invalid character in host"));
    }
    let trimmed = h.strip_suffix('.').unwrap_or(h);
    if trimmed.is_empty() || trimmed.split('.').any(|l| l.is_empty() || l.len() > 63) {
        return Err(invalid("invalid host name label"));
    }
    if trimmed.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && trimmed.parse::<Ipv4Addr>().is_err()
    {
        return Err(invalid("invalid IPv4 address"));
    }
    Ok(h.to_ascii_lowercase())
}

/// Parses `host[:port]` or `[v6][:port]`. Returns (host, is_ipv6, port).
fn parse_host_port(s: &str, scheme: Scheme) -> Result<(String, bool, u16), Error> {
    if let Some(rest) = s.strip_prefix('[') {
        let close = rest.find(']').ok_or(invalid("unterminated IPv6 literal"))?;
        let addr: Ipv6Addr = rest[..close]
            .parse()
            .map_err(|_| invalid("invalid IPv6 address"))?;
        let after = &rest[close + 1..];
        let port = if after.is_empty() {
            scheme.default_port()
        } else {
            let p = after
                .strip_prefix(':')
                .ok_or(invalid("invalid character after IPv6 literal"))?;
            parse_port(p, scheme)?
        };
        return Ok((addr.to_string(), true, port));
    }
    let (h, p) = match s.split_once(':') {
        Some((h, p)) => (h, p),
        None => (s, ""),
    };
    let host = parse_reg_name(h)?;
    Ok((host, false, parse_port(p, scheme)?))
}

impl Url {
    /// Parses an absolute `http://` or `https://` URL.
    ///
    /// ```
    /// let url = dl_http::Url::parse("HTTPS://User:p%40ss@API.Example.com:443/v1/../v2/x?q=1#frag")?;
    /// assert_eq!(url.host(), "api.example.com");
    /// assert_eq!(url.port(), 443);
    /// assert_eq!(url.path(), "/v2/x");
    /// assert_eq!(url.query(), Some("q=1"));
    /// assert_eq!(url.to_string(), "https://User:p%40ss@api.example.com/v2/x?q=1");
    /// # Ok::<(), dl_http::Error>(())
    /// ```
    pub fn parse(input: &str) -> Result<Url, Error> {
        if input.len() > MAX_URL_LEN {
            return Err(invalid("URL is too long"));
        }
        check_chars(input)?;
        let colon = scheme_end(input).ok_or(invalid("missing scheme"))?;
        let scheme = match input[..colon].to_ascii_lowercase().as_str() {
            "http" => Scheme::Http,
            "https" => Scheme::Https,
            _ => return Err(invalid("unsupported scheme")),
        };
        let rest = input[colon + 1..]
            .strip_prefix("//")
            .ok_or(invalid("missing authority"))?;
        let rest = rest.split_once('#').map_or(rest, |(r, _)| r);
        let auth_end = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(auth_end);

        let (userinfo, hostport) = match authority.rfind('@') {
            Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
            None => (None, authority),
        };
        let (username, password) = match userinfo {
            None => (String::new(), None),
            Some(ui) => match ui.split_once(':') {
                Some((u, p)) => (
                    normalize(u, is_userinfo_char)?,
                    Some(normalize(p, |b| b != b':' && is_userinfo_char(b))?),
                ),
                None => (normalize(ui, is_userinfo_char)?, None),
            },
        };
        let (host, ipv6, port) = parse_host_port(hostport, scheme)?;

        let (path, query) = match tail.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (tail, None),
        };
        let path = if path.is_empty() {
            "/".to_owned()
        } else {
            remove_dot_segments(&normalize(path, is_path_char)?)
        };
        let query = query.map(|q| normalize(q, is_query_char)).transpose()?;
        Ok(Url {
            scheme,
            username,
            password,
            host,
            ipv6,
            port,
            path,
            query,
        })
    }

    /// Resolves a reference (absolute URL, `//host/...`, `/path`, `relative/path`, `?query`)
    /// against this URL, as described in RFC 3986 section 5.
    ///
    /// ```
    /// let base = dl_http::Url::parse("https://api.example.com/v1/devices/")?;
    /// assert_eq!(base.join("42/status?full=1")?.to_string(), "https://api.example.com/v1/devices/42/status?full=1");
    /// assert_eq!(base.join("../health")?.to_string(), "https://api.example.com/v1/health");
    /// assert_eq!(base.join("/login")?.to_string(), "https://api.example.com/login");
    /// # Ok::<(), dl_http::Error>(())
    /// ```
    pub fn join(&self, reference: &str) -> Result<Url, Error> {
        if reference.len() > MAX_URL_LEN {
            return Err(invalid("URL is too long"));
        }
        check_chars(reference)?;
        if scheme_end(reference).is_some() {
            return Url::parse(reference);
        }
        if reference.starts_with("//") {
            return Url::parse(&format!("{}:{reference}", self.scheme.as_str()));
        }
        let reference = reference.split_once('#').map_or(reference, |(r, _)| r);
        let (path, query) = match reference.split_once('?') {
            Some((p, q)) => (p, Some(normalize(q, is_query_char)?)),
            None => (reference, None),
        };
        let mut url = self.clone();
        if path.is_empty() {
            if query.is_some() {
                url.query = query;
            }
            return Ok(url);
        }
        let path = normalize(path, is_path_char)?;
        url.path = if path.starts_with('/') {
            remove_dot_segments(&path)
        } else {
            let dir_end = self.path.rfind('/').map_or(0, |i| i + 1);
            remove_dot_segments(&format!("{}{path}", &self.path[..dir_end]))
        };
        url.query = query;
        Ok(url)
    }

    /// The scheme.
    pub fn scheme(&self) -> Scheme {
        self.scheme
    }

    /// The user name, still percent-encoded (empty if there is none).
    pub fn username(&self) -> &str {
        &self.username
    }

    /// The password, still percent-encoded.
    pub fn password(&self) -> Option<&str> {
        self.password.as_deref()
    }

    /// The host: a lower-case name, a dotted IPv4 address, or an IPv6 address without
    /// brackets.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Whether the host is an IPv6 literal.
    pub fn is_ipv6(&self) -> bool {
        self.ipv6
    }

    /// The port, explicit or the scheme's default.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The path, starting with `/`.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The query without the leading `?`.
    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    /// Host and port as sent in the `Host` header: brackets around IPv6, port omitted
    /// when it is the default.
    pub fn authority(&self) -> String {
        let mut s = self.host_bracketed();
        if self.port != self.scheme.default_port() {
            s.push(':');
            s.push_str(&self.port.to_string());
        }
        s
    }

    /// Path and query as sent in the request line (`/a/b?x=1`).
    pub fn request_target(&self) -> String {
        match &self.query {
            Some(q) => format!("{}?{q}", self.path),
            None => self.path.clone(),
        }
    }

    /// `host:port` with the port always present (used for `CONNECT`).
    #[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), allow(dead_code))]
    pub(crate) fn host_port(&self) -> String {
        format!("{}:{}", self.host_bracketed(), self.port)
    }

    /// Whether two URLs have the same scheme, host and port.
    #[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), allow(dead_code))]
    pub(crate) fn same_origin(&self, other: &Url) -> bool {
        self.scheme == other.scheme && self.host == other.host && self.port == other.port
    }

    fn host_bracketed(&self) -> String {
        if self.ipv6 {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        }
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://", self.scheme.as_str())?;
        if !self.username.is_empty() || self.password.is_some() {
            f.write_str(&self.username)?;
            if let Some(p) = &self.password {
                write!(f, ":{p}")?;
            }
            f.write_str("@")?;
        }
        write!(f, "{}{}", self.authority(), self.path)?;
        if let Some(q) = &self.query {
            write!(f, "?{q}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut shown = self.clone();
        if shown.password.is_some() {
            shown.password = Some("***".to_owned());
        }
        write!(f, "Url({shown})")
    }
}

impl FromStr for Url {
    type Err = Error;

    fn from_str(s: &str) -> Result<Url, Error> {
        Url::parse(s)
    }
}

impl TryFrom<&str> for Url {
    type Error = Error;

    fn try_from(s: &str) -> Result<Url, Error> {
        Url::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Url {
        match Url::parse(s) {
            Ok(u) => u,
            Err(e) => panic!("{s}: {e}"),
        }
    }

    fn roundtrip(input: &str, expected: &str) {
        assert_eq!(p(input).to_string(), expected, "input {input}");
        // Serialisation is a fixed point.
        assert_eq!(p(expected).to_string(), expected);
    }

    #[test]
    fn parses_components() {
        let u = p("http://example.com");
        assert_eq!(u.scheme(), Scheme::Http);
        assert_eq!(u.host(), "example.com");
        assert_eq!(u.port(), 80);
        assert_eq!(u.path(), "/");
        assert_eq!(u.query(), None);
        assert_eq!(u.username(), "");
        assert_eq!(u.password(), None);

        let u = p("https://u%20x:pw@Host.Example:8443/a/b?x=1&y=2#top");
        assert_eq!(u.scheme(), Scheme::Https);
        assert_eq!(u.username(), "u%20x");
        assert_eq!(u.password(), Some("pw"));
        assert_eq!(u.host(), "host.example");
        assert_eq!(u.port(), 8443);
        assert_eq!(u.path(), "/a/b");
        assert_eq!(u.query(), Some("x=1&y=2"));
        assert_eq!(u.authority(), "host.example:8443");
        assert_eq!(u.request_target(), "/a/b?x=1&y=2");
        assert_eq!(u.host_port(), "host.example:8443");
    }

    #[test]
    fn normalises() {
        roundtrip("HTTP://EXAMPLE.COM:80/", "http://example.com/");
        roundtrip("https://example.com:443", "https://example.com/");
        roundtrip("https://example.com:80", "https://example.com:80/");
        roundtrip("http://example.com:/x", "http://example.com/x");
        roundtrip("http://example.com?q", "http://example.com/?q");
        roundtrip("http://example.com/?", "http://example.com/?");
        roundtrip("http://example.com/a/./b/../c", "http://example.com/a/c");
        roundtrip("http://example.com/../../x", "http://example.com/x");
        roundtrip("http://example.com/a//b", "http://example.com/a//b");
        roundtrip("http://example.com/%7e%2f", "http://example.com/%7E%2F");
        roundtrip(
            "http://example.com/é?q=é",
            "http://example.com/%C3%A9?q=%C3%A9",
        );
        roundtrip(
            "http://example.com/{x}|^",
            "http://example.com/%7Bx%7D%7C%5E",
        );
        roundtrip(
            "http://example.com/a?b=c/d?e:f@g",
            "http://example.com/a?b=c/d?e:f@g",
        );
        roundtrip("http://example.com/x#frag?no", "http://example.com/x");
        roundtrip("http://a@b@example.com/", "http://a%40b@example.com/");
        roundtrip("http://user:@example.com/", "http://user:@example.com/");
        roundtrip("http://:pw@example.com/", "http://:pw@example.com/");
        roundtrip("http://example.com./", "http://example.com./");
        roundtrip("http://my_host-1.local/", "http://my_host-1.local/");
        roundtrip("http://10.0.0.1:8080/", "http://10.0.0.1:8080/");
    }

    #[test]
    fn ipv6_literals() {
        let u = p("http://[::1]:8080/x");
        assert_eq!(u.host(), "::1");
        assert!(u.is_ipv6());
        assert_eq!(u.port(), 8080);
        assert_eq!(u.authority(), "[::1]:8080");
        assert_eq!(u.to_string(), "http://[::1]:8080/x");
        let u = p("https://[2001:DB8:0:0:0:0:0:1]/");
        assert_eq!(u.host(), "2001:db8::1");
        assert_eq!(u.authority(), "[2001:db8::1]");
        assert_eq!(u.host_port(), "[2001:db8::1]:443");
    }

    #[test]
    fn rejects_invalid() {
        let cases = [
            "",
            "example.com",
            "/path",
            "ftp://example.com/",
            "mailto:a@b.c",
            "http:example.com",
            "http:/example.com",
            "http://",
            "http:///path",
            "http://:80/",
            "http://user@/",
            "http://exa mple.com/",
            "http://example.com/a b",
            "http://example.com/\r\nX: y",
            "http://example.com/\0",
            "http://example.com/\t",
            " http://example.com/",
            "http://example.com:0/",
            "http://example.com:65536/",
            "http://example.com:99999999/",
            "http://example.com:-1/",
            "http://example.com:8o/",
            "http://example.com:80:80/",
            "http://[::1/",
            "http://[::1]x/",
            "http://[zz::1]/",
            "http://[1.2.3.4]/",
            "http://::1/",
            "http://exam%70le.com/",
            "http://ex~ample.com/",
            "http://exämple.com/",
            "http://a..b/",
            "http://.a/",
            "http://1.2.3.256/",
            "http://1.2.3/",
            "http://999/",
            "http://example.com/%",
            "http://example.com/%4",
            "http://example.com/%zz",
            "http://example.com/?q=%g0",
            "http://us%er@example.com/",
            "1http://example.com/",
        ];
        for c in cases {
            assert!(Url::parse(c).is_err(), "accepted {c:?}");
        }
        assert!(Url::parse(&format!("http://{}.com/", "a".repeat(64))).is_err());
        assert!(Url::parse(&format!("http://{}/", ["abc"; 70].join("."))).is_err());
        assert!(Url::parse(&format!("http://x/{}", "a".repeat(MAX_URL_LEN))).is_err());
    }

    #[test]
    fn error_reason() {
        match Url::parse("ftp://x/") {
            Err(Error::InvalidUrl { reason }) => assert_eq!(reason, "unsupported scheme"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn debug_hides_password() {
        let u = p("http://user:secret@example.com/");
        let d = format!("{u:?}");
        assert!(!d.contains("secret"), "{d}");
        assert!(d.contains("user:***@"), "{d}");
        assert!(u.to_string().contains("secret"));
    }

    #[test]
    fn rfc3986_reference_resolution() {
        // RFC 3986 section 5.4 (fragments are dropped by this crate).
        let base = p("http://a/b/c/d;p?q");
        let cases = [
            ("g", "http://a/b/c/g"),
            ("./g", "http://a/b/c/g"),
            ("g/", "http://a/b/c/g/"),
            ("/g", "http://a/g"),
            ("//g", "http://g/"),
            ("?y", "http://a/b/c/d;p?y"),
            ("g?y", "http://a/b/c/g?y"),
            ("#s", "http://a/b/c/d;p?q"),
            ("g#s", "http://a/b/c/g"),
            ("g?y#s", "http://a/b/c/g?y"),
            (";x", "http://a/b/c/;x"),
            ("g;x", "http://a/b/c/g;x"),
            ("g;x?y#s", "http://a/b/c/g;x?y"),
            ("", "http://a/b/c/d;p?q"),
            (".", "http://a/b/c/"),
            ("./", "http://a/b/c/"),
            ("..", "http://a/b/"),
            ("../", "http://a/b/"),
            ("../g", "http://a/b/g"),
            ("../..", "http://a/"),
            ("../../", "http://a/"),
            ("../../g", "http://a/g"),
            ("../../../g", "http://a/g"),
            ("../../../../g", "http://a/g"),
            ("/./g", "http://a/g"),
            ("/../g", "http://a/g"),
            ("g.", "http://a/b/c/g."),
            (".g", "http://a/b/c/.g"),
            ("g..", "http://a/b/c/g.."),
            ("..g", "http://a/b/c/..g"),
            ("./../g", "http://a/b/g"),
            ("./g/.", "http://a/b/c/g/"),
            ("g/./h", "http://a/b/c/g/h"),
            ("g/../h", "http://a/b/c/h"),
            ("g;x=1/./y", "http://a/b/c/g;x=1/y"),
            ("g;x=1/../y", "http://a/b/c/y"),
            ("g?y/./x", "http://a/b/c/g?y/./x"),
            ("g?y/../x", "http://a/b/c/g?y/../x"),
            ("g#s/./x", "http://a/b/c/g"),
            ("g#s/../x", "http://a/b/c/g"),
            ("https://other:8443/x", "https://other:8443/x"),
            ("HTTP://A/x", "http://a/x"),
        ];
        for (r, expected) in cases {
            match base.join(r) {
                Ok(u) => assert_eq!(u.to_string(), expected, "reference {r:?}"),
                Err(e) => panic!("reference {r:?}: {e}"),
            }
        }
        assert!(base.join("http:g").is_err());
        assert!(base.join("ftp://x/").is_err());
        assert!(base.join("a b").is_err());
        assert!(base.join("%zz").is_err());
        // Scheme-relative references keep the base scheme.
        assert_eq!(
            p("https://a/x").join("//b:8443/y").unwrap().to_string(),
            "https://b:8443/y"
        );
        // Userinfo is kept for same-host references.
        assert_eq!(
            p("http://u:p@a/x/y").join("z").unwrap().to_string(),
            "http://u:p@a/x/z"
        );
    }

    #[test]
    fn encoding_helpers() {
        assert_eq!(encode_query_component(""), "");
        assert_eq!(encode_query_component("AZaz09-._~"), "AZaz09-._~");
        assert_eq!(
            encode_query_component(" +&=?/#%"),
            "%20%2B%26%3D%3F%2F%23%25"
        );
        assert_eq!(encode_query_component("Việt"), "Vi%E1%BB%87t");
        assert_eq!(encode_path_segment("a:b@c d/e?"), "a:b@c%20d%2Fe%3F");
        assert_eq!(percent_decode("a%2Fb%2fc+d").unwrap(), b"a/b/c+d");
        assert_eq!(percent_decode("%E1%BB%87").unwrap(), "ệ".as_bytes());
        assert!(percent_decode("%").is_err());
        assert!(percent_decode("%1").is_err());
        assert!(percent_decode("%1g").is_err());
        let s = "k=v&x y/é";
        let enc = encode_query_component(s);
        assert_eq!(percent_decode(&enc).unwrap(), s.as_bytes());
        let u = p(&format!("http://h/?q={enc}"));
        assert_eq!(u.query(), Some(&*format!("q={enc}")));
    }

    #[test]
    fn remove_dots() {
        assert_eq!(remove_dot_segments("/"), "/");
        assert_eq!(remove_dot_segments("/a/b/c/./../../g"), "/a/g");
        assert_eq!(remove_dot_segments("/mid/content=5/../6"), "/mid/6");
        assert_eq!(remove_dot_segments("/.."), "/");
        assert_eq!(remove_dot_segments("/."), "/");
        assert_eq!(remove_dot_segments("/a/.."), "/");
    }
}
