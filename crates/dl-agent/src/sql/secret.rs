//! SQL login mode and the sealed password of a `sql.json` entry (PROTOCOL.md §9.8).
//!
//! An entry stores its connection string without the password; the password sits next
//! to it in `password_sealed` (base64 of a DPAPI machine-scope blob on Windows), and
//! `auth` says which login the site uses:
//!
//! ```json
//! {"connection": "Server=.;Database=SMILE_FO;User Id=smile", "auth": "sql",
//!  "password_sealed": "AQAAANCMnd8B…"}
//! {"connection": "Server=.;Database=SMILE_FO;Integrated Security=SSPI", "auth": "windows"}
//! ```

use dl_encoding::base64::STANDARD as B64;
use dl_json::Value;

use crate::Error;

/// DPAPI entropy: binds a sealed password to this use.
const ENTROPY: &[u8] = b"dl-agent sql password v1";

/// Entry key of the sealed password.
pub const PASSWORD_KEY: &str = "password_sealed";
/// Entry key of the login mode.
pub const AUTH_KEY: &str = "auth";

/// How the agent logs in to SQL Server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthMode {
    /// Windows (`Integrated Security=SSPI`): the service account, LocalSystem by default.
    Windows,
    /// A SQL Server login (`User Id` + password).
    Sql,
}

impl AuthMode {
    /// Wire and file name: `windows` or `sql`.
    pub fn as_str(self) -> &'static str {
        match self {
            AuthMode::Windows => "windows",
            AuthMode::Sql => "sql",
        }
    }

    /// Parses `windows` / `sql` (case-insensitive; `integrated` and `sspi` mean Windows).
    ///
    /// # Errors
    /// [`Error::Config`] for anything else.
    pub fn parse(s: &str) -> Result<AuthMode, Error> {
        match s.trim().to_ascii_lowercase().as_str() {
            "windows" | "integrated" | "sspi" => Ok(AuthMode::Windows),
            "sql" => Ok(AuthMode::Sql),
            other => Err(Error::Config(format!(
                "auth must be windows or sql, not {other:?}"
            ))),
        }
    }

    /// The mode a parsed connection string uses.
    pub fn of(config: &dl_tds::Config) -> AuthMode {
        if config.integrated_auth() {
            AuthMode::Windows
        } else {
            AuthMode::Sql
        }
    }
}

/// Splits an ADO connection string into the string without `Password`/`PWD` and the
/// password (as `dl_tds` reads it). URL-form strings are returned unchanged.
///
/// # Errors
/// [`Error::Config`] when the string does not parse.
pub fn split_password(connection: &str) -> Result<(String, Option<String>), Error> {
    let config = parse(connection)?;
    if connection
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("sqlserver://")
        || config.password().is_empty()
    {
        return Ok((connection.to_string(), None));
    }
    let kept: Vec<&str> = ado_pairs(connection)
        .into_iter()
        .filter(|p| {
            let key = p
                .split('=')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            key != "password" && key != "pwd"
        })
        .collect();
    let stripped = kept.join(";");
    // The split must not change anything but the password.
    let check = parse(&stripped)?;
    if !check.password().is_empty() || check.user() != config.user() {
        return Err(Error::Config(
            "connection string: cannot separate the password".into(),
        ));
    }
    Ok((stripped, Some(config.password().to_string())))
}

/// `connection` with `password` added back as a quoted ADO value.
pub fn with_password(connection: &str, password: &str) -> String {
    format!(
        "{};Password=\"{}\"",
        connection.trim_end_matches(';'),
        password.replace('"', "\"\"")
    )
}

/// Seals a password for `password_sealed`.
///
/// # Errors
/// DPAPI failures.
pub fn seal(password: &str) -> Result<String, Error> {
    Ok(B64.encode(crate::identity::seal(password.as_bytes(), ENTROPY)?))
}

/// Opens a `password_sealed` value.
///
/// # Errors
/// [`Error::Config`] for bad base64, a blob sealed on another machine, or non-UTF-8.
pub fn unseal(sealed: &str) -> Result<String, Error> {
    let blob = B64
        .decode(sealed.trim())
        .map_err(|_| Error::Config(format!("{PASSWORD_KEY} is not base64")))?;
    let plain = crate::identity::unseal(&blob, ENTROPY)?;
    String::from_utf8(plain).map_err(|_| Error::Config(format!("{PASSWORD_KEY} is not UTF-8")))
}

/// The full connection string of an entry (sealed password restored) and its login mode.
///
/// # Errors
/// [`Error::Config`] for a missing connection, a bad `auth` value, an `auth` that does
/// not match the string, or a password that cannot be unsealed.
pub fn resolve(entry: &Value) -> Result<(String, AuthMode), Error> {
    let connection = entry
        .get("connection")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Config("missing \"connection\"".into()))?;
    let full = match entry.get(PASSWORD_KEY).and_then(Value::as_str) {
        Some(sealed) => {
            if connection
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("sqlserver://")
            {
                return Err(Error::Config(format!(
                    "{PASSWORD_KEY} needs an ADO connection string"
                )));
            }
            with_password(connection, &unseal(sealed)?)
        }
        None => connection.to_string(),
    };
    let mode = AuthMode::of(&parse(&full)?);
    if let Some(declared) = entry.get(AUTH_KEY).and_then(Value::as_str)
        && AuthMode::parse(declared)? != mode
    {
        return Err(Error::Config(format!(
            "auth is {declared} but the connection string uses {} login",
            mode.as_str()
        )));
    }
    Ok((full, mode))
}

fn parse(connection: &str) -> Result<dl_tds::Config, Error> {
    connection
        .parse()
        .map_err(|e: dl_tds::Error| Error::Config(format!("connection string: {e}")))
}

/// `key=value` pairs of an ADO string, split on `;` outside a double-quoted value.
fn ado_pairs(s: &str) -> Vec<&str> {
    // The delimiters are ASCII, so byte offsets are always char boundaries.
    let b = s.as_bytes();
    let mut out = Vec::new();
    let (mut start, mut i) = (0, 0);
    while i < b.len() {
        match b[i] {
            b';' => {
                out.push(&s[start..i]);
                start = i + 1;
                i += 1;
            }
            b'=' => {
                i += 1;
                while b.get(i) == Some(&b' ') {
                    i += 1;
                }
                if b.get(i) == Some(&b'"') {
                    i += 1;
                    while i < b.len() {
                        if b[i] == b'"' {
                            if b.get(i + 1) == Some(&b'"') {
                                i += 2;
                                continue;
                            }
                            i += 1;
                            break;
                        }
                        i += 1;
                    }
                }
                while i < b.len() && b[i] != b';' {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    out.push(&s[start..]);
    out.into_iter().filter(|p| !p.trim().is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dl_json::json;

    #[test]
    fn password_is_split_out_and_restored() {
        let conn = "Server=.\\SQLEXPRESS;Database=SMILE_FO;User Id=smile;Password=\"p;a\"\"ss\";Encrypt=false";
        let (stripped, pw) = split_password(conn).unwrap();
        assert_eq!(pw.as_deref(), Some("p;a\"ss"));
        assert!(!stripped.contains("p;a"), "{stripped}");
        assert!(stripped.contains("Encrypt=false"));
        let back: dl_tds::Config = with_password(&stripped, "p;a\"ss").parse().unwrap();
        assert_eq!(back.password(), "p;a\"ss");
        assert_eq!(back.user(), "smile");
        assert_eq!(back.database(), "SMILE_FO");
    }

    #[test]
    fn pwd_synonym_and_windows_strings() {
        let (s, pw) = split_password("Server=.;UID=sa;PWD=x1;Database=SMILE_BO").unwrap();
        assert_eq!(pw.as_deref(), Some("x1"));
        assert_eq!(s, "Server=.;UID=sa;Database=SMILE_BO");
        let win = "Server=.;Database=SMILE_FO;Integrated Security=true;TrustServerCertificate=true";
        assert_eq!(split_password(win).unwrap(), (win.to_string(), None));
        let url = "sqlserver://sa:x@host?database=SMILE_FO";
        assert_eq!(split_password(url).unwrap(), (url.to_string(), None));
    }

    #[test]
    fn auth_modes_parse_and_follow_the_string() {
        assert_eq!(AuthMode::parse("Windows").unwrap(), AuthMode::Windows);
        assert_eq!(AuthMode::parse("SSPI").unwrap(), AuthMode::Windows);
        assert_eq!(AuthMode::parse("sql").unwrap(), AuthMode::Sql);
        assert!(AuthMode::parse("kerberos").is_err());
        for (conn, mode) in [
            ("Server=.;Integrated Security=SSPI", AuthMode::Windows),
            ("Server=.;Trusted_Connection=yes", AuthMode::Windows),
            ("Server=.;User Id=sa;Password=x", AuthMode::Sql),
        ] {
            let (_, m) = resolve(&json!({"connection": conn})).unwrap();
            assert_eq!(m, mode, "{conn}");
        }
    }

    #[test]
    fn sealed_entry_round_trips_and_auth_mismatch_is_refused() {
        let entry = json!({"connection": "Server=.;Database=SMILE_FO;User Id=smile",
                           "auth": "sql", "password_sealed": seal("s3cret;\"x").unwrap()});
        let (full, mode) = resolve(&entry).unwrap();
        assert_eq!(mode, AuthMode::Sql);
        let c: dl_tds::Config = full.parse().unwrap();
        assert_eq!(c.password(), "s3cret;\"x");
        let wrong = json!({"connection": "Server=.;Integrated Security=SSPI", "auth": "sql"});
        assert!(
            resolve(&wrong)
                .unwrap_err()
                .to_string()
                .contains("auth is sql")
        );
        let url = json!({"connection": "sqlserver://sa@h", "password_sealed": seal("x").unwrap()});
        assert!(resolve(&url).is_err());
        assert!(unseal("%%").is_err());
    }
}
