//! Connection configuration and connection-string parsing (go-mssqldb compatible).

use std::collections::HashMap;
use std::str::FromStr;
use std::time::Duration;

use crate::Error;

/// Encryption mode, the `encrypt` connection-string key.
///
/// | Mode | `encrypt=` | What is encrypted | TDS |
/// |---|---|---|---|
/// | [`Disabled`](Encrypt::Disabled) | `disable` | nothing (unless the server insists) | 7.4 |
/// | [`Off`](Encrypt::Off) (default) | `false`, `no`, `0`, `optional` | the LOGIN7 packet only (credentials), if the server supports TLS; the session if the server requires it | 7.4 |
/// | [`Required`](Encrypt::Required) | `true`, `yes`, `1`, `mandatory` | the whole session; fails if the server cannot encrypt | 7.4 |
/// | [`Strict`](Encrypt::Strict) | `strict` | the whole session, TLS before any TDS byte, certificate always verified | 8.0 |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encrypt {
    /// Do not offer encryption.
    Disabled,
    /// Encrypt the login only (go-mssqldb's default).
    #[default]
    Off,
    /// Encrypt the whole session.
    Required,
    /// TDS 8.0: TLS first (ALPN `tds/8.0`), then TDS inside it.
    Strict,
}

/// Connection settings. Build one by parsing a connection string, or with
/// [`Config::new`] and the setters.
///
/// Two formats are accepted, with go-mssqldb's semantics:
///
/// * URL: `sqlserver://user:password@host:port/instance?database=db&encrypt=true&app+name=x`
///   (user, password, path and query are percent-decoded; `+` is a space in the query).
/// * ADO: `Server=host\instance,port;Database=db;User Id=user;Password=pw;Encrypt=false`
///   (keys are case-insensitive; `Data Source`, `Address`, `Addr`, `Network Address`,
///   `UID`, `User`, `PWD`, `Initial Catalog`, `Application Name` are synonyms; values may
///   be double-quoted with `""` as an escaped quote; a `tcp:` prefix on the server is
///   ignored).
///
/// Recognised keys: `server`, `port`, `user id`, `password`, `database`, `encrypt`,
/// `trustservercertificate`, `hostnameincertificate`, `packet size`, `connection timeout`
/// (seconds, per read/write like go-mssqldb; 0 = none), `dial timeout` (seconds,
/// default 15), `app name`, `workstation id`, `applicationintent`. Other keys go-mssqldb
/// knows (`log`, `keepalive`, `protocol`, ...) are accepted and ignored.
///
/// When `encrypt` is absent, `TrustServerCertificate` defaults to `true` (go-mssqldb does
/// the same), so an on-premises server with a self-signed certificate works out of the box.
///
/// ```
/// let c: dl_tds::Config = "sqlserver://smile:p%40ss@localhost:1433?database=SMILE_FO".parse()?;
/// assert_eq!(c.database(), "SMILE_FO");
/// assert_eq!(c.password(), "p@ss");
/// let c: dl_tds::Config = "Server=.\\SQLEXPRESS;Database=SMILE_POS;User Id=sa;Password=x".parse()?;
/// assert_eq!(c.instance(), Some("SQLEXPRESS"));
/// # Ok::<(), dl_tds::Error>(())
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    pub(crate) host: String,
    pub(crate) instance: Option<String>,
    pub(crate) port: Option<u16>,
    pub(crate) user: String,
    pub(crate) password: String,
    pub(crate) database: String,
    pub(crate) app_name: String,
    pub(crate) workstation: String,
    pub(crate) language: String,
    pub(crate) encrypt: Encrypt,
    pub(crate) trust_server_certificate: bool,
    pub(crate) host_name_in_certificate: Option<String>,
    pub(crate) packet_size: u16,
    pub(crate) connect_timeout: Duration,
    pub(crate) timeout: Option<Duration>,
    pub(crate) read_only_intent: bool,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("host", &self.host)
            .field("instance", &self.instance)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("password", &"<redacted>")
            .field("database", &self.database)
            .field("app_name", &self.app_name)
            .field("workstation", &self.workstation)
            .field("encrypt", &self.encrypt)
            .field("trust_server_certificate", &self.trust_server_certificate)
            .field("packet_size", &self.packet_size)
            .field("connect_timeout", &self.connect_timeout)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl Default for Config {
    fn default() -> Config {
        Config::new("localhost")
    }
}

/// The local computer name, sent as the LOGIN7 host name (go-mssqldb uses `os.Hostname`).
fn local_hostname() -> String {
    for var in ["COMPUTERNAME", "HOSTNAME"] {
        if let Ok(h) = std::env::var(var)
            && !h.is_empty()
        {
            return h;
        }
    }
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

impl Config {
    /// Defaults for `host`: port 1433, SQL authentication with empty credentials,
    /// [`Encrypt::Off`] with `TrustServerCertificate=true`, packet size 4096, 15 s
    /// connect timeout, no read/write timeout, app name `dl-tds`.
    pub fn new(host: &str) -> Config {
        Config {
            host: host.to_string(),
            instance: None,
            port: None,
            user: String::new(),
            password: String::new(),
            database: String::new(),
            app_name: "dl-tds".into(),
            workstation: local_hostname(),
            language: String::new(),
            encrypt: Encrypt::Off,
            trust_server_certificate: true,
            host_name_in_certificate: None,
            packet_size: 4096,
            connect_timeout: Duration::from_secs(15),
            timeout: None,
            read_only_intent: false,
        }
    }

    /// Sets SQL Server authentication credentials.
    pub fn credentials(mut self, user: &str, password: &str) -> Config {
        self.user = user.to_string();
        self.password = password.to_string();
        self
    }

    /// Sets the initial database.
    pub fn with_database(mut self, database: &str) -> Config {
        self.database = database.to_string();
        self
    }

    /// Sets the TCP port (skips the SQL Server Browser lookup for named instances).
    pub fn with_port(mut self, port: u16) -> Config {
        self.port = Some(port);
        self
    }

    /// Sets the named instance (`host\instance`), resolved through SQL Server Browser
    /// unless a port is set.
    pub fn with_instance(mut self, instance: &str) -> Config {
        self.instance = (!instance.is_empty()).then(|| instance.to_string());
        self
    }

    /// Sets the encryption mode. [`Encrypt::Strict`] also forces certificate validation.
    pub fn with_encrypt(mut self, encrypt: Encrypt) -> Config {
        self.encrypt = encrypt;
        if encrypt == Encrypt::Strict {
            self.trust_server_certificate = false;
        }
        self
    }

    /// Accept any server certificate (`TrustServerCertificate`).
    pub fn with_trust_server_certificate(mut self, trust: bool) -> Config {
        self.trust_server_certificate = trust;
        self
    }

    /// Sets the requested packet size (clamped to 512–32767; the server may lower it).
    pub fn with_packet_size(mut self, size: u16) -> Config {
        self.packet_size = size.clamp(512, 32767);
        self
    }

    /// Sets the application name reported to the server (`APP_NAME()`).
    pub fn with_app_name(mut self, name: &str) -> Config {
        self.app_name = name.to_string();
        self
    }

    /// Sets the TCP connect timeout (per address tried).
    pub fn with_connect_timeout(mut self, t: Duration) -> Config {
        self.connect_timeout = t;
        self
    }

    /// Sets the read/write timeout for every socket operation, including waiting for a
    /// query's results. On expiry the request is cancelled and [`Error::Timeout`] returned.
    pub fn with_timeout(mut self, t: Option<Duration>) -> Config {
        self.timeout = t.filter(|d| !d.is_zero());
        self
    }

    /// Server host name or address.
    pub fn host(&self) -> &str {
        &self.host
    }
    /// Named instance, if any.
    pub fn instance(&self) -> Option<&str> {
        self.instance.as_deref()
    }
    /// Explicit port, if any.
    pub fn port(&self) -> Option<u16> {
        self.port
    }
    /// SQL login name.
    pub fn user(&self) -> &str {
        &self.user
    }
    /// SQL login password.
    pub fn password(&self) -> &str {
        &self.password
    }
    /// Initial database (empty = the login's default).
    pub fn database(&self) -> &str {
        &self.database
    }
    /// Application name.
    pub fn app_name(&self) -> &str {
        &self.app_name
    }
    /// Workstation (client host) name.
    pub fn workstation(&self) -> &str {
        &self.workstation
    }
    /// Encryption mode.
    pub fn encrypt(&self) -> Encrypt {
        self.encrypt
    }
    /// Whether any server certificate is accepted.
    pub fn trust_server_certificate(&self) -> bool {
        self.trust_server_certificate
    }
    /// Name the certificate must match (defaults to the host).
    pub fn host_name_in_certificate(&self) -> &str {
        self.host_name_in_certificate
            .as_deref()
            .unwrap_or(&self.host)
    }
    /// Requested packet size.
    pub fn packet_size(&self) -> u16 {
        self.packet_size
    }
    /// TCP connect timeout.
    pub fn connect_timeout(&self) -> Duration {
        self.connect_timeout
    }
    /// Read/write timeout.
    pub fn timeout(&self) -> Option<Duration> {
        self.timeout
    }
    /// `ApplicationIntent=ReadOnly`.
    pub fn read_only_intent(&self) -> bool {
        self.read_only_intent
    }

    /// Builds a config from go-mssqldb's key/value parameter map (lower-case keys).
    fn from_params(params: &HashMap<String, String>) -> Result<Config, Error> {
        let get = |k: &str| params.get(k).map(String::as_str);
        let server = get("server").unwrap_or("");
        let (host, instance) = match server.split_once('\\') {
            Some((h, i)) => (h, Some(i)),
            None => (server, None),
        };
        let host = if host.is_empty() || host == "." || host.eq_ignore_ascii_case("(local)") {
            "localhost"
        } else {
            host
        };
        let mut c = Config::new(host).with_instance(instance.unwrap_or(""));
        c.user = get("user id").unwrap_or("").to_string();
        c.password = get("password").unwrap_or("").to_string();
        c.database = get("database").unwrap_or("").to_string();
        if let Some(p) = get("port") {
            c.port = Some(
                p.parse::<u16>()
                    .map_err(|e| Error::config("port", format!("invalid tcp port '{p}': {e}")))?,
            );
        }
        if let Some(p) = get("packet size") {
            let n = parse_uint_base0(p).ok_or_else(|| {
                Error::config("packet size", format!("invalid packet size '{p}'"))
            })?;
            c.packet_size = u16::try_from(n)
                .map_err(|_| Error::config("packet size", format!("invalid packet size '{p}'")))?
                .clamp(512, 32767);
        }
        if let Some(t) = get("connection timeout") {
            let s: u64 = t.parse().map_err(|e| {
                Error::config(
                    "connection timeout",
                    format!("invalid connection timeout '{t}': {e}"),
                )
            })?;
            c.timeout = (s > 0).then(|| Duration::from_secs(s));
        }
        if let Some(t) = get("dial timeout") {
            let s: u64 = t.parse().map_err(|e| {
                Error::config("dial timeout", format!("invalid dial timeout '{t}': {e}"))
            })?;
            c.connect_timeout = Duration::from_secs(s);
        }
        if let Some(a) = get("app name") {
            c.app_name = a.to_string();
        }
        if let Some(w) = get("workstation id") {
            c.workstation = w.to_string();
        }
        if get("applicationintent") == Some("ReadOnly") {
            if c.database.is_empty() {
                return Err(Error::config(
                    "applicationintent",
                    "database must be specified when ApplicationIntent is ReadOnly",
                ));
            }
            c.read_only_intent = true;
        }
        if let Some(h) = get("hostnameincertificate") {
            c.host_name_in_certificate = Some(h.to_string());
        }

        let mut trust = false;
        let encrypt_raw = get("encrypt").map(str::to_ascii_lowercase);
        c.encrypt = match encrypt_raw.as_deref() {
            None => {
                trust = true;
                Encrypt::Off
            }
            Some("mandatory" | "yes" | "1" | "t" | "true") => Encrypt::Required,
            Some("disable") => Encrypt::Disabled,
            Some("strict") => Encrypt::Strict,
            Some("optional" | "no" | "0" | "f" | "false") => Encrypt::Off,
            Some(other) => {
                return Err(Error::config(
                    "encrypt",
                    format!("invalid encrypt '{other}'"),
                ));
            }
        };
        if let Some(t) = get("trustservercertificate") {
            trust = go_parse_bool(t).ok_or_else(|| {
                Error::config(
                    "trustservercertificate",
                    format!("invalid trust server certificate '{t}'"),
                )
            })?;
        }
        if c.encrypt == Encrypt::Strict {
            trust = false;
        }
        c.trust_server_certificate = trust;
        Ok(c)
    }
}

fn go_parse_bool(s: &str) -> Option<bool> {
    match s {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
        _ => None,
    }
}

/// Go's `strconv.ParseUint(s, 0, ...)`: decimal, `0x` hex, `0o`/`0` octal, `0b` binary.
fn parse_uint_base0(s: &str) -> Option<u64> {
    let s = s.replace('_', "");
    let lower = s.to_ascii_lowercase();
    if let Some(h) = lower.strip_prefix("0x") {
        u64::from_str_radix(h, 16).ok()
    } else if let Some(b) = lower.strip_prefix("0b") {
        u64::from_str_radix(b, 2).ok()
    } else if let Some(o) = lower.strip_prefix("0o") {
        u64::from_str_radix(o, 8).ok()
    } else if lower.len() > 1 && lower.starts_with('0') {
        u64::from_str_radix(&lower[1..], 8).ok()
    } else {
        lower.parse().ok()
    }
}

impl FromStr for Config {
    type Err = Error;

    fn from_str(s: &str) -> Result<Config, Error> {
        let params = if s.starts_with("sqlserver://") {
            split_url(s)?
        } else if s.starts_with("odbc:") {
            return Err(Error::config(
                "",
                "the odbc: connection string format is not supported",
            ));
        } else {
            split_ado(s)
        };
        Config::from_params(&params)
    }
}

/// go-mssqldb's `splitConnectionString` (ADO format).
fn split_ado(dsn: &str) -> HashMap<String, String> {
    let mut res = HashMap::new();
    for part in split_ado_parts(dsn) {
        if part.is_empty() {
            continue;
        }
        let (k, v) = match part.split_once('=') {
            Some((k, v)) => (k, Some(v)),
            None => (part.as_str(), None),
        };
        let mut name = k.trim().to_lowercase();
        if name.is_empty() {
            continue;
        }
        let mut value = String::new();
        if let Some(v) = v {
            value = v.trim().to_string();
            if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
                value = value[1..value.len() - 1].replace("\"\"", "\"");
            }
        }
        name = match name.as_str() {
            "application name" => "app name",
            "data source" | "address" | "network address" | "addr" => "server",
            "user" | "uid" => "user id",
            "pwd" => "password",
            "initial catalog" => "database",
            "column encryption setting" => "columnencryption",
            other => other,
        }
        .to_string();
        if name == "server" {
            for prefix in ["tcp:", "admin:"] {
                if let Some(rest) = value.strip_prefix(prefix) {
                    res.insert(
                        "protocol".to_string(),
                        prefix.trim_end_matches(':').to_string(),
                    );
                    value = rest.to_string();
                }
            }
            let parts: Vec<&str> = value.split(',').collect();
            if parts.len() == 2 && !parts[1].is_empty() {
                res.insert("port".to_string(), parts[1].to_string());
                value = parts[0].to_string();
            }
        }
        res.insert(name, value);
    }
    res
}

/// Splits on `;` outside double quotes, keeping the quotes (go-mssqldb's
/// `splitAdoConnectionStringParts`).
fn split_ado_parts(dsn: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = dsn.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            if in_quotes && chars.peek() == Some(&'"') {
                cur.push('"');
                cur.push('"');
                chars.next();
            } else {
                in_quotes = !in_quotes;
                cur.push(c);
            }
        } else if c == ';' && !in_quotes {
            parts.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    parts
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Percent-decoding as Go's `url.unescape`; `plus_is_space` for query components.
fn unescape(s: &str, plus_is_space: bool) -> Result<String, Error> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let hi = b.get(i + 1).copied().and_then(hex_val);
                let lo = b.get(i + 2).copied().and_then(hex_val);
                match (hi, lo) {
                    (Some(h), Some(l)) => out.push(h << 4 | l),
                    _ => {
                        return Err(Error::config(
                            "",
                            "unable to parse connection string: invalid URL format",
                        ));
                    }
                }
                i += 3;
            }
            b'+' if plus_is_space => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out)
        .map_err(|_| Error::config("", "connection string is not valid UTF-8 after decoding"))
}

/// go-mssqldb's `splitConnectionStringURL`.
fn split_url(dsn: &str) -> Result<HashMap<String, String>, Error> {
    let bad = || Error::config("", "unable to parse connection string: invalid URL format");
    let mut res = HashMap::new();
    let rest = &dsn["sqlserver://".len()..];
    // Strip the fragment, then split off the query.
    let rest = rest.split_once('#').map_or(rest, |(a, _)| a);
    let (rest, query) = match rest.split_once('?') {
        Some((a, q)) => (a, Some(q)),
        None => (rest, None),
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    // Userinfo ends at the last '@' of the authority (Go: strings.LastIndex).
    let (userinfo, hostport) = match authority.rfind('@') {
        Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
        None => (None, authority),
    };
    if let Some(ui) = userinfo {
        match ui.split_once(':') {
            Some((u, p)) => {
                res.insert("user id".to_string(), unescape(u, false)?);
                res.insert("password".to_string(), unescape(p, false)?);
            }
            None => {
                res.insert("user id".to_string(), unescape(ui, false)?);
            }
        }
    }
    let hostport = unescape(hostport, false)?;
    let (host, port) = split_host_port(&hostport).unwrap_or((hostport.clone(), String::new()));
    if host.contains(['@', '/', '?']) {
        return Err(bad());
    }
    let path = unescape(path, false)?;
    let server = if path.len() > 1 {
        format!("{host}\\{}", &path[1..])
    } else {
        host
    };
    res.insert("server".to_string(), server);
    if !port.is_empty() {
        res.insert("port".to_string(), port);
    }
    if let Some(q) = query {
        let mut seen: HashMap<String, String> = HashMap::new();
        for pair in q.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let k = unescape(k, true)?;
            let v = unescape(v, true)?;
            if seen.contains_key(&k) {
                return Err(Error::config(
                    &k.to_lowercase(),
                    format!("key {k} provided more than once"),
                ));
            }
            seen.insert(k, v);
        }
        for (k, v) in seen {
            let lk = k.to_lowercase();
            if res.contains_key(&lk) {
                return Err(Error::config(
                    &lk,
                    format!(
                        "key {k:?} provided more than once (connection string keys are case-insensitive; remove the duplicate)"
                    ),
                ));
            }
            res.insert(lk, v);
        }
    }
    Ok(res)
}

/// Go's `net.SplitHostPort` (enough for `host:port` and `[v6]:port`).
fn split_host_port(s: &str) -> Option<(String, String)> {
    if let Some(rest) = s.strip_prefix('[') {
        let (h, after) = rest.split_once(']')?;
        let port = after.strip_prefix(':')?;
        return Some((h.to_string(), port.to_string()));
    }
    let (h, p) = s.rsplit_once(':')?;
    if h.contains(':') {
        return None; // unbracketed IPv6
    }
    Some((h.to_string(), p.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Config {
        s.parse().unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    #[test]
    fn url_basic() {
        let c = p("sqlserver://smile:pw@localhost:1433?database=SMILE_FO");
        assert_eq!(c.host(), "localhost");
        assert_eq!(c.port(), Some(1433));
        assert_eq!(c.user(), "smile");
        assert_eq!(c.password(), "pw");
        assert_eq!(c.database(), "SMILE_FO");
        assert_eq!(c.encrypt(), Encrypt::Off);
        assert!(c.trust_server_certificate());
        assert_eq!(c.instance(), None);
        assert_eq!(c.app_name(), "dl-tds");
    }

    #[test]
    fn url_instance_and_escapes() {
        let c = p(
            "sqlserver://sa:p%40ss%3Aw+rd@srv01/SQLEXPRESS?database=SMILE%20POS&app+name=Hotel+Agent&connection+timeout=30&dial+timeout=5",
        );
        assert_eq!(c.host(), "srv01");
        assert_eq!(c.instance(), Some("SQLEXPRESS"));
        assert_eq!(c.port(), None);
        assert_eq!(c.password(), "p@ss:w+rd"); // '+' is literal in userinfo
        assert_eq!(c.database(), "SMILE POS");
        assert_eq!(c.app_name(), "Hotel Agent");
        assert_eq!(c.timeout(), Some(Duration::from_secs(30)));
        assert_eq!(c.connect_timeout(), Duration::from_secs(5));
        // '@' inside the password: the last '@' separates the host.
        let c = p("sqlserver://u:a@b@host?database=d");
        assert_eq!(c.password(), "a@b");
        assert_eq!(c.host(), "host");
        let c = p("sqlserver://[::1]:14333?encrypt=disable");
        assert_eq!(c.host(), "::1");
        assert_eq!(c.port(), Some(14333));
        let c = p("sqlserver://user@host");
        assert_eq!((c.user(), c.password()), ("user", ""));
        let c = p("sqlserver://./SQL2019");
        assert_eq!((c.host(), c.instance()), ("localhost", Some("SQL2019")));
    }

    #[test]
    fn url_encrypt_and_trust() {
        let c = p("sqlserver://h?encrypt=true");
        assert_eq!(c.encrypt(), Encrypt::Required);
        assert!(!c.trust_server_certificate());
        let c = p("sqlserver://h?encrypt=true&TrustServerCertificate=true");
        assert!(c.trust_server_certificate());
        let c = p("sqlserver://h?encrypt=DISABLE");
        assert_eq!(c.encrypt(), Encrypt::Disabled);
        assert!(!c.trust_server_certificate());
        let c = p("sqlserver://h?encrypt=strict&trustservercertificate=true");
        assert_eq!(c.encrypt(), Encrypt::Strict);
        assert!(!c.trust_server_certificate());
        for v in ["false", "no", "0", "f", "optional", "False"] {
            assert_eq!(
                p(&format!("sqlserver://h?encrypt={v}")).encrypt(),
                Encrypt::Off
            );
        }
        for v in ["mandatory", "yes", "1", "t", "TRUE"] {
            assert_eq!(
                p(&format!("sqlserver://h?encrypt={v}")).encrypt(),
                Encrypt::Required
            );
        }
        let c = p("sqlserver://h?trustservercertificate=false");
        assert!(!c.trust_server_certificate());
        assert_eq!(c.encrypt(), Encrypt::Off);
        let c = p("sqlserver://h?encrypt=true&hostnameincertificate=sql.example.com");
        assert_eq!(c.host_name_in_certificate(), "sql.example.com");
    }

    #[test]
    fn url_errors() {
        for bad in [
            "sqlserver://h?encrypt=maybe",
            "sqlserver://h?trustservercertificate=yes",
            "sqlserver://h:70000",
            "sqlserver://h:abc",
            "sqlserver://h?database=a&database=b",
            "sqlserver://h?Database=a&database=b",
            "sqlserver://h?packet+size=x",
            "sqlserver://h?connection+timeout=-1",
            "sqlserver://u:%zz@h",
            "sqlserver://h?applicationintent=ReadOnly",
            "odbc:server=h",
        ] {
            assert!(bad.parse::<Config>().is_err(), "{bad}");
        }
        let e = "sqlserver://h?encrypt=maybe".parse::<Config>().unwrap_err();
        assert!(matches!(e, Error::Config { ref key, .. } if key == "encrypt"));
    }

    #[test]
    fn packet_size_and_intent() {
        assert_eq!(p("sqlserver://h?packet+size=32767").packet_size(), 32767);
        assert_eq!(p("sqlserver://h?packet+size=100").packet_size(), 512);
        assert_eq!(p("sqlserver://h?packet+size=0x1000").packet_size(), 4096);
        assert_eq!(p("Server=h;Packet Size=65535").packet_size(), 32767);
        assert!("Server=h;Packet Size=65536".parse::<Config>().is_err());
        let c = p("Server=h;Database=d;ApplicationIntent=ReadOnly");
        assert!(c.read_only_intent());
        assert!(!p("Server=h;ApplicationIntent=readonly").read_only_intent());
    }

    #[test]
    fn ado_basic() {
        let c = p(
            "Server=.;Database=SMILE_FO;User Id=smile;Password=pw;Encrypt=false;TrustServerCertificate=true",
        );
        assert_eq!(c.host(), "localhost");
        assert_eq!(c.database(), "SMILE_FO");
        assert_eq!(c.user(), "smile");
        assert_eq!(c.password(), "pw");
        assert_eq!(c.encrypt(), Encrypt::Off);
        assert!(c.trust_server_certificate());
        let c = p(
            "Data Source=tcp:10.0.0.5,1500;Initial Catalog=SMILE_POS;UID=sa;PWD=x;Application Name=agent;Workstation ID=PC1",
        );
        assert_eq!(c.host(), "10.0.0.5");
        assert_eq!(c.port(), Some(1500));
        assert_eq!(c.database(), "SMILE_POS");
        assert_eq!(c.user(), "sa");
        assert_eq!(c.app_name(), "agent");
        assert_eq!(c.workstation(), "PC1");
        let c = p("server=(LOCAL)\\SQLEXPRESS;user=u;password=p");
        assert_eq!((c.host(), c.instance()), ("localhost", Some("SQLEXPRESS")));
        let c = p("Address=srv;Port=1444;encrypt=true");
        assert_eq!(c.port(), Some(1444));
        assert_eq!(c.encrypt(), Encrypt::Required);
        assert!(!c.trust_server_certificate());
    }

    #[test]
    fn ado_quoting_and_edge_cases() {
        let c = p("Server=h;Password=\"a;b\"\"c\";User Id = u ;");
        assert_eq!(c.password(), "a;b\"c");
        assert_eq!(c.user(), "u");
        let c = p("Server=h;Password=x=y;;=ignored;Database");
        assert_eq!(c.password(), "x=y");
        assert_eq!(c.database(), "");
        // Server with three comma parts keeps the whole value.
        let c = p("Server=a,b,c");
        assert_eq!(c.host(), "a,b,c");
        assert_eq!(c.port(), None);
        // Trailing comma: no port.
        let c = p("Server=h,");
        assert_eq!(c.host(), "h,");
        // Empty string: localhost defaults.
        let c = p("");
        assert_eq!(c.host(), "localhost");
        assert_eq!(c.packet_size(), 4096);
        assert_eq!(c.connect_timeout(), Duration::from_secs(15));
        assert_eq!(c.timeout(), None);
        let c = p("Server=h;connection timeout=0;dial timeout=2;log=63;keepalive=30");
        assert_eq!(c.timeout(), None);
        assert_eq!(c.connect_timeout(), Duration::from_secs(2));
    }

    #[test]
    fn builder_and_debug_redacts_password() {
        let c = Config::new("db")
            .credentials("u", "secret")
            .with_database("d")
            .with_port(14333)
            .with_encrypt(Encrypt::Strict)
            .with_packet_size(100)
            .with_timeout(Some(Duration::ZERO));
        assert_eq!(c.packet_size(), 512);
        assert!(!c.trust_server_certificate());
        assert_eq!(c.timeout(), None);
        assert!(!format!("{c:?}").contains("secret"));
    }
}
