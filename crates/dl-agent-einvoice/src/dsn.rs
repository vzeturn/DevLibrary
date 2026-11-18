//! One SQL Server connection's fields and their go-mssqldb URL: a port of `DSN` and
//! `DSN.String` in `enroll.go`. The text is what gets sealed in `config.enc`, so it must
//! be byte-identical to Go's `net/url` output (checked against vectors exported from Go in
//! `tests/legacy_golden.rs`): a machine switches between the Go and Rust agents without
//! re-enrolling.
//!
//! ```text
//! sqlserver://user:pass@host[:port][/instance]?database=DB[&encrypt=disable][&tlsmin=1.0]
//! ```

use std::fmt;

/// A SQL Server connection. Never sent to the platform, put in a URL or logged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dsn {
    /// Server host name or IP address.
    pub host: String,
    /// TCP port; empty = driver default.
    pub port: String,
    /// Named instance; empty = default instance.
    pub instance: String,
    /// Database name.
    pub database: String,
    /// SQL login.
    pub user: String,
    /// SQL password.
    pub password: String,
    /// go-mssqldb `tlsmin` (e.g. `1.0`); empty = driver default (TLS 1.2).
    pub tls_min: String,
    /// go-mssqldb `encrypt` (e.g. `disable`); empty = driver default.
    pub encrypt: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Host,
    Path,
    UserPassword,
    QueryComponent,
}

/// `net/url.shouldEscape`.
fn should_escape(c: u8, mode: Mode) -> bool {
    if c.is_ascii_alphanumeric() {
        return false;
    }
    if mode == Mode::Host
        && matches!(
            c,
            b'!' | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b':'
                | b'['
                | b']'
                | b'<'
                | b'>'
                | b'"'
        )
    {
        return false;
    }
    match c {
        b'-' | b'_' | b'.' | b'~' => false,
        b'$' | b'&' | b'+' | b',' | b'/' | b':' | b';' | b'=' | b'?' | b'@' => match mode {
            Mode::Path => c == b'?',
            Mode::UserPassword => matches!(c, b'@' | b'/' | b'?' | b':'),
            Mode::QueryComponent | Mode::Host => true,
        },
        _ => true,
    }
}

/// `net/url.escape`.
fn escape(s: &str, mode: Mode) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for &c in s.as_bytes() {
        if c == b' ' && mode == Mode::QueryComponent {
            out.push('+');
        } else if should_escape(c, mode) {
            out.push('%');
            out.push(HEX[usize::from(c >> 4)] as char);
            out.push(HEX[usize::from(c & 15)] as char);
        } else {
            out.push(c as char);
        }
    }
    out
}

impl Dsn {
    /// The transport part (`tlsmin`, `encrypt`).
    pub fn transport(&self) -> Transport {
        Transport {
            tls_min: self.tls_min.clone(),
            encrypt: self.encrypt.clone(),
        }
    }

    /// The same connection with another transport.
    pub fn with_transport(mut self, t: &Transport) -> Dsn {
        self.tls_min = t.tls_min.clone();
        self.encrypt = t.encrypt.clone();
        self
    }
}

/// `DSN.String`: every component URL-escaped (`net/url` rules), query keys sorted.
impl fmt::Display for Dsn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // net.JoinHostPort brackets a host that contains a colon (IPv6).
        let host = if self.port.is_empty() {
            self.host.clone()
        } else if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        };
        write!(
            f,
            "sqlserver://{}:{}@{}",
            escape(&self.user, Mode::UserPassword),
            escape(&self.password, Mode::UserPassword),
            escape(&host, Mode::Host)
        )?;
        if !self.instance.is_empty() {
            write!(f, "{}", escape(&format!("/{}", self.instance), Mode::Path))?;
        }
        let mut q = vec![("database", self.database.as_str())];
        if !self.tls_min.is_empty() {
            q.push(("tlsmin", &self.tls_min));
        }
        if !self.encrypt.is_empty() {
            q.push(("encrypt", &self.encrypt));
        }
        q.sort_by_key(|(k, _)| *k);
        let q: Vec<String> = q
            .iter()
            .map(|(k, v)| format!("{k}={}", escape(v, Mode::QueryComponent)))
            .collect();
        write!(f, "?{}", q.join("&"))
    }
}

/// The go-mssqldb `encrypt` value that skips TLS: the last resort for a SQL Server whose
/// TLS stack shares no protocol or cipher with this agent.
pub const ENCRYPT_DISABLE: &str = "disable";
/// The TLS floor for a SQL Server too old for TLS 1.2 (2008, 2008 R2, 2012 without the
/// TLS 1.2 update).
pub const LEGACY_TLS_MIN: &str = "1.0";

/// How the agent secures the connection to SQL Server (`sqlTransport`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Transport {
    /// `tlsmin`; empty = default.
    pub tls_min: String,
    /// `encrypt`; empty = default.
    pub encrypt: String,
}

impl Transport {
    /// The driver default (TLS 1.2+ login handshake).
    pub fn is_default(&self) -> bool {
        self.tls_min.is_empty() && self.encrypt.is_empty()
    }

    /// The operator-facing note for a non-default transport (ASCII: it is printed to a
    /// legacy Windows console).
    pub fn describe(&self) -> String {
        if self.encrypt == ENCRYPT_DISABLE {
            "SQL Server cu: ket noi KHONG ma hoa, chi dung trong mang noi bo".to_owned()
        } else if !self.tls_min.is_empty() {
            format!("SQL Server cu: dung TLS {}", self.tls_min)
        } else {
            String::new()
        }
    }

    /// The fallback ladder, strongest first.
    pub fn legacy_ladder() -> [Transport; 2] {
        [
            Transport {
                tls_min: LEGACY_TLS_MIN.to_owned(),
                encrypt: String::new(),
            },
            Transport {
                tls_min: String::new(),
                encrypt: ENCRYPT_DISABLE.to_owned(),
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(host: &str, port: &str, instance: &str) -> Dsn {
        Dsn {
            host: host.into(),
            port: port.into(),
            instance: instance.into(),
            database: "SMILE_FO".into(),
            user: "u".into(),
            password: "p".into(),
            ..Dsn::default()
        }
    }

    #[test]
    fn forms() {
        assert_eq!(
            d("10.0.0.5", "1433", "").to_string(),
            "sqlserver://u:p@10.0.0.5:1433?database=SMILE_FO"
        );
        assert_eq!(
            d("srv", "", "SQLEXPRESS").to_string(),
            "sqlserver://u:p@srv/SQLEXPRESS?database=SMILE_FO"
        );
        assert_eq!(
            d("::1", "1433", "").to_string(),
            "sqlserver://u:p@[::1]:1433?database=SMILE_FO"
        );
        let mut x = d(".", "", "").with_transport(&Transport::legacy_ladder()[0]);
        assert_eq!(
            x.to_string(),
            "sqlserver://u:p@.?database=SMILE_FO&tlsmin=1.0"
        );
        x.encrypt = "disable".into();
        assert_eq!(
            x.to_string(),
            "sqlserver://u:p@.?database=SMILE_FO&encrypt=disable&tlsmin=1.0"
        );
        x.password = "p@ss word$&+,;=:/?#%".into();
        x.database = "SMILE POS".into();
        assert_eq!(
            x.to_string(),
            "sqlserver://u:p%40ss%20word$&+,;=%3A%2F%3F%23%25@.?database=SMILE+POS&encrypt=disable&tlsmin=1.0"
        );
    }

    #[test]
    fn dl_tds_reads_every_part() {
        let mut x = d("10.0.0.5", "14330", "");
        x.password = "p@ss word$&+,;=:/?#%".into();
        x.tls_min = "1.0".into();
        x.encrypt = "disable".into();
        let c: dl_tds::Config = x.to_string().parse().unwrap();
        assert_eq!(c.host(), "10.0.0.5");
        assert_eq!(c.port(), Some(14330));
        assert_eq!(c.password(), x.password);
        assert_eq!(c.tls_min(), dl_tds::TlsVersion::Tls10);
        assert_eq!(c.encrypt(), dl_tds::Encrypt::Disabled);
        let c: dl_tds::Config = d("srv", "", "SQL2008").to_string().parse().unwrap();
        assert_eq!(c.instance(), Some("SQL2008"));
        // A transport the driver cannot honour is an error at start, never ignored.
        let mut bad = d("srv", "", "");
        bad.tls_min = "0.9".into();
        assert!(bad.to_string().parse::<dl_tds::Config>().is_err());
    }
}
