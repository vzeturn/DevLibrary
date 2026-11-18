//! `AgentOptionsValidator`: refuses to start on unsafe or inconsistent settings, with the
//! same messages (joined by `"; "` in the `OptionsValidationException`).

use super::dotnet::{DotnetError, is_blank};
use super::{AgentOptions, TimeSpan};

/// `AgentOptionsValidator.MaxCommandTimeout`: the command timeout must end well before the
/// server-side claim lease.
pub const MAX_COMMAND_TIMEOUT: TimeSpan = TimeSpan::from_secs(45);

/// The validation failures, in the C# order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationErrors(pub Vec<String>);

impl ValidationErrors {
    /// The `OptionsValidationException` the .NET host throws at start.
    pub fn to_dotnet(&self) -> DotnetError {
        DotnetError::new(
            "Microsoft.Extensions.Options.OptionsValidationException",
            self.0.join("; "),
        )
    }
}

impl std::fmt::Display for ValidationErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.join("; "))
    }
}

impl std::error::Error for ValidationErrors {}

/// `AgentOptionsValidator.Validate`.
pub fn validate(o: &AgentOptions) -> Result<(), ValidationErrors> {
    let mut errors = Vec::new();

    match api_url(&o.api_base_url) {
        None => errors.push("Agent:ApiBaseUrl must be an absolute URL".to_owned()),
        Some((scheme, loopback)) => {
            if !(scheme == "https" || scheme == "http" && loopback) {
                errors.push(
                    "Agent:ApiBaseUrl must use https (plain http is allowed only for loopback)"
                        .to_owned(),
                );
            }
        }
    }

    if !agent_id_matches(&o.agent_id) {
        errors
            .push("Agent:AgentId must be 1-64 characters of A-Z, a-z, 0-9, '_' or '-'".to_owned());
    }

    if is_blank(&o.token_file) {
        errors.push("Agent:TokenFile is required".to_owned());
    }

    if is_blank(&o.data_directory) || !is_path_fully_qualified(&o.data_directory) {
        errors.push("Agent:DataDirectory must be an absolute path".to_owned());
    }

    if o.command_timeout <= TimeSpan::ZERO || o.command_timeout > MAX_COMMAND_TIMEOUT {
        errors.push(format!(
            "Agent:CommandTimeout must be between 0 and {} s",
            MAX_COMMAND_TIMEOUT.total_seconds()
        ));
    }

    if o.poll_interval < TimeSpan::from_millis(250) || o.poll_interval > TimeSpan::from_secs(60) {
        errors.push("Agent:PollInterval must be between 250 ms and 60 s".to_owned());
    }

    if !crate::adapters::NAMES.contains(&o.adapter.as_str()) {
        errors.push(format!(
            "Agent:Adapter must be one of {}",
            crate::adapters::NAMES.join(", ")
        ));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors(errors))
    }
}

/// `^[A-Za-z0-9_-]{1,64}$` with .NET semantics: `$` also matches before one final `\n`, so
/// `"FD01\n"` passes in C# too (kept).
fn agent_id_matches(id: &str) -> bool {
    let id = id.strip_suffix('\n').unwrap_or(id);
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `Path.IsPathFullyQualified` on Windows: a drive with a separator (`C:\`, `C:/`) or a
/// path starting with two separators or `\?` (UNC and device paths).
#[cfg(windows)]
fn is_path_fully_qualified(p: &str) -> bool {
    let b = p.as_bytes();
    let sep = |c: u8| c == b'\\' || c == b'/';
    if b.len() < 2 {
        return false;
    }
    if sep(b[0]) {
        return b[1] == b'?' || sep(b[1]);
    }
    b.len() >= 3 && b[1] == b':' && sep(b[2]) && b[0].is_ascii_alphabetic()
}

/// `Path.IsPathFullyQualified` on Unix: the path starts with `/`.
#[cfg(not(windows))]
fn is_path_fully_qualified(p: &str) -> bool {
    p.starts_with('/')
}

/// `Uri.TryCreate(url, UriKind.Absolute)`, reduced to what the validator reads: the
/// lower-case scheme and `Uri.IsLoopback`. `None` when .NET would not create an absolute URI.
fn api_url(url: &str) -> Option<(String, bool)> {
    let url = super::dotnet::trim(url);
    if is_local_file_path(url) {
        return Some(("file".to_owned(), true));
    }
    let colon = url.find(':')?;
    let scheme = &url[..colon];
    let mut chars = scheme.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        || !chars.all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    {
        return None;
    }
    let scheme = scheme.to_ascii_lowercase();
    let rest = &url[colon + 1..];
    if scheme != "http" && scheme != "https" {
        // Other schemes only need a non-empty remainder here; the validator refuses them anyway.
        return (!rest.is_empty()).then_some((scheme, false));
    }
    let rest = rest.strip_prefix("//")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let (host, port) = split_host_port(host_port)?;
    if let Some(port) = port
        && !port.is_empty()
        && (port.len() > 5
            || !port.bytes().all(|b| b.is_ascii_digit())
            || port.parse::<u32>().ok()? > 65535)
    {
        return None;
    }
    let loopback = if let Some(v6) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        let ip: std::net::Ipv6Addr = v6.parse().ok()?;
        ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
    } else {
        if !valid_reg_name(host) {
            return None;
        }
        let lower = host.to_ascii_lowercase();
        lower == "localhost"
            || lower == "loopback"
            || lenient_ipv4(host).is_some_and(|ip| ip >> 24 == 127)
    };
    Some((scheme, loopback))
}

/// A Windows path `.NET` turns into a `file:` URI: `C:\...`, `C:/...` or `\\server\...`.
#[cfg(windows)]
fn is_local_file_path(s: &str) -> bool {
    let b = s.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
        || s.starts_with(r"\\")
}

/// A Unix absolute path, which .NET on Unix turns into a `file:` URI.
#[cfg(not(windows))]
fn is_local_file_path(s: &str) -> bool {
    s.starts_with('/')
}

/// Splits `host[:port]`, keeping the brackets of an IPv6 literal. `None` for an empty host.
fn split_host_port(s: &str) -> Option<(&str, Option<&str>)> {
    let (host, port) = if s.starts_with('[') {
        let close = s.find(']')?;
        let (h, after) = s.split_at(close + 1);
        match after.strip_prefix(':') {
            Some(p) => (h, Some(p)),
            None if after.is_empty() => (h, None),
            None => return None,
        }
    } else {
        match s.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (s, None),
        }
    };
    (!host.is_empty()).then_some((host, port))
}

/// A host name .NET accepts: dot-separated non-empty labels (one trailing dot allowed), no
/// percent-encoding, white space or delimiter characters.
fn valid_reg_name(host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host);
    !host.is_empty()
        && host.split('.').all(|label| !label.is_empty())
        && !host
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "%<>\"{}|\\^`[]@:/?#".contains(c))
}

/// The lenient IPv4 forms `System.Uri` accepts (`127.1`, `0x7f.0.0.1`, `2130706433`): one to
/// four parts in decimal, octal (leading 0) or hex (0x), the last part filling the rest.
fn lenient_ipv4(host: &str) -> Option<u32> {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut values = Vec::with_capacity(parts.len());
    for p in &parts {
        let (digits, radix) = if let Some(h) = p.strip_prefix("0x").or_else(|| p.strip_prefix("0X"))
        {
            (h, 16)
        } else if p.len() > 1 && p.starts_with('0') {
            (&p[1..], 8)
        } else {
            (*p, 10)
        };
        if digits.is_empty() && radix != 8 {
            return None;
        }
        let v = if digits.is_empty() {
            0
        } else {
            u64::from_str_radix(digits, radix).ok()?
        };
        values.push(v);
    }
    let (last, head) = values.split_last()?;
    let mut ip: u64 = 0;
    for (i, v) in head.iter().enumerate() {
        if *v > 255 {
            return None;
        }
        ip |= v << (24 - 8 * i);
    }
    let room = 32 - 8 * head.len() as u32;
    if *last >= 1u64 << room {
        return None;
    }
    u32::try_from(ip | last).ok()
}

#[cfg(test)]
mod tests {
    //! Port of AgentOptionsTests.cs (the validator cases) plus the System.Uri behaviour
    //! measured with the .NET 10 runtime.
    use super::*;

    fn valid() -> AgentOptions {
        AgentOptions {
            api_base_url: "https://pms.example".into(),
            agent_id: "FD01".into(),
            token_file: "/tmp/agent.token".into(),
            ..AgentOptions::default()
        }
    }

    fn is_valid(o: &AgentOptions) -> bool {
        validate(o).is_ok()
    }

    #[test]
    fn i0_o1_http_is_refused_unless_loopback() {
        let cases = [
            ("http://10.0.0.5:3001", false),
            ("http://pms.example", false),
            ("http://localhost:3001", true),
            ("http://127.0.0.1:3001", true),
            ("https://pms.example", true),
            ("ftp://pms.example", false),
            ("not a url", false),
        ];
        for (url, ok) in cases {
            let o = AgentOptions {
                api_base_url: url.into(),
                ..valid()
            };
            assert_eq!(is_valid(&o), ok, "{url}");
        }
    }

    #[test]
    fn i0_o2_agent_id_must_match_the_safe_pattern() {
        for (id, ok) in [
            ("FD01", true),
            ("front-desk_2", true),
            ("", false),
            ("FD 01", false),
            ("FD01/../x", false),
        ] {
            let o = AgentOptions {
                agent_id: id.into(),
                ..valid()
            };
            assert_eq!(is_valid(&o), ok, "{id}");
        }
    }

    #[test]
    fn i0_o2b_agent_id_longer_than_64_is_refused() {
        assert!(!is_valid(&AgentOptions {
            agent_id: "A".repeat(65),
            ..valid()
        }));
        assert!(is_valid(&AgentOptions {
            agent_id: "A".repeat(64),
            ..valid()
        }));
    }

    #[test]
    fn agent_id_keeps_the_dotnet_dollar_quirk() {
        assert!(agent_id_matches("FD01\n"));
        assert!(!agent_id_matches("FD01\n\n"));
    }

    #[test]
    fn i0_o3_command_timeout_must_stay_under_the_60_s_lease() {
        for (seconds, ok) in [(45, true), (46, false), (0, false)] {
            let o = AgentOptions {
                command_timeout: TimeSpan::from_secs(seconds),
                ..valid()
            };
            assert_eq!(is_valid(&o), ok, "{seconds}");
        }
    }

    #[test]
    fn i0_o4_poll_interval_between_250_ms_and_60_s() {
        for (ms, ok) in [(250, true), (60_000, true), (249, false), (60_001, false)] {
            let o = AgentOptions {
                poll_interval: TimeSpan::from_millis(ms),
                ..valid()
            };
            assert_eq!(is_valid(&o), ok, "{ms}");
        }
    }

    #[test]
    fn i0_o5_token_file_is_required() {
        assert!(!is_valid(&AgentOptions {
            token_file: " ".into(),
            ..valid()
        }));
    }

    /// The C# case `"/var/lib/smile-agent" -> valid` holds only on Unix: on Windows
    /// `Path.IsPathFullyQualified` refuses it (measured), so the C# test fails there too.
    #[test]
    fn i0_o6_data_directory_for_the_outbox_must_be_absolute() {
        let unix_ok = cfg!(not(windows));
        for (dir, ok) in [
            ("data", false),
            ("", false),
            ("/var/lib/smile-agent", unix_ok),
        ] {
            let o = AgentOptions {
                data_directory: dir.into(),
                ..valid()
            };
            assert_eq!(is_valid(&o), ok, "{dir}");
        }
        assert!(is_valid(&valid()), "the default data directory is valid");
    }

    #[cfg(windows)]
    #[test]
    fn windows_fully_qualified_paths_match_dotnet() {
        let cases = [
            ("/var/lib/smile-agent", false),
            (r"C:\x", true),
            ("C:x", false),
            (r"\x", false),
            (r"\\srv\share", true),
            ("C:/x", true),
            ("//srv/share", true),
            (r"\\?\C:\x", true),
        ];
        for (p, ok) in cases {
            assert_eq!(is_path_fully_qualified(p), ok, "{p}");
        }
    }

    #[test]
    fn i1_o2_only_known_adapters_start() {
        for (adapter, ok) in [
            ("none", true),
            ("simulated-lock", true),
            ("smile-lock", true),
            ("simulated", false),
            ("", false),
            ("None", false),
        ] {
            let o = AgentOptions {
                adapter: adapter.into(),
                ..valid()
            };
            assert_eq!(is_valid(&o), ok, "{adapter}");
        }
    }

    #[test]
    fn messages_and_order_match_the_csharp_validator() {
        let o = AgentOptions {
            api_base_url: "x".into(),
            agent_id: String::new(),
            token_file: String::new(),
            data_directory: "d".into(),
            command_timeout: TimeSpan::ZERO,
            poll_interval: TimeSpan::ZERO,
            adapter: "q".into(),
            ..AgentOptions::default()
        };
        let err = validate(&o).unwrap_err();
        assert_eq!(
            err.to_dotnet().to_string(),
            "Microsoft.Extensions.Options.OptionsValidationException: Agent:ApiBaseUrl must be an absolute URL; \
             Agent:AgentId must be 1-64 characters of A-Z, a-z, 0-9, '_' or '-'; Agent:TokenFile is required; \
             Agent:DataDirectory must be an absolute path; Agent:CommandTimeout must be between 0 and 45 s; \
             Agent:PollInterval must be between 250 ms and 60 s; Agent:Adapter must be one of none, simulated-lock, smile-lock"
        );
        let ftp = AgentOptions {
            api_base_url: "ftp://pms.example".into(),
            ..valid()
        };
        assert_eq!(
            validate(&ftp).unwrap_err().0,
            ["Agent:ApiBaseUrl must use https (plain http is allowed only for loopback)"]
        );
    }

    /// `Uri.TryCreate(..., UriKind.Absolute)` and `Uri.IsLoopback` measured on .NET 10.
    #[test]
    fn uri_rules_match_dotnet() {
        let cases: [(&str, Option<(&str, bool)>); 31] = [
            ("http://loopback:3001", Some(("http", true))),
            ("http://127.1", Some(("http", true))),
            ("http://LOCALHOST", Some(("http", true))),
            ("http://[::1]:3001", Some(("http", true))),
            ("HTTPS://x", Some(("https", false))),
            ("https://", None),
            ("http:foo", None),
            ("https:/x", None),
            ("http://localhost:99999", None),
            ("https://pms example", None),
            (" https://pms.example ", Some(("https", false))),
            ("http://127.0.0.1.", Some(("http", false))),
            ("http://localhost.", Some(("http", false))),
            ("https://user@pms", Some(("https", false))),
            ("http://0x7f.0.0.1", Some(("http", true))),
            ("http://2130706433", Some(("http", true))),
            ("file:///x", Some(("file", false))),
            ("mailto:a@b", Some(("mailto", false))),
            ("http://127.255.0.1", Some(("http", true))),
            ("http://[0:0:0:0:0:0:0:1]", Some(("http", true))),
            ("http://localhost:3001/path?q#f", Some(("http", true))),
            ("https://pms.example:443", Some(("https", false))),
            ("http://localhost:", Some(("http", true))),
            ("http://:80", None),
            ("https://pms_x.example", Some(("https", false))),
            ("https://a..b", None),
            ("x:", None),
            ("1a:b", None),
            ("https://ex%41mple", None),
            ("http://[::ffff:127.0.0.1]", Some(("http", true))),
            ("http://localhost\\x", None),
        ];
        for (url, expected) in cases {
            let got = api_url(url);
            match expected {
                None => assert_eq!(got, None, "{url}"),
                Some((scheme, loopback)) => {
                    let (s, l) = got.unwrap_or_else(|| panic!("{url} should be absolute"));
                    assert_eq!(s, scheme, "{url}");
                    // .NET reports file: URIs as loopback; only http's flag matters here.
                    if scheme != "file" {
                        assert_eq!(l, loopback, "{url}");
                    }
                }
            }
        }
        assert!(api_url("https://ví-dụ.vn").is_some());
        assert!(api_url("https://pms.example/ a").is_some());
    }
}
