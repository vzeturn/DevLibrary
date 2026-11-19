//! How the PMS connection string signs in to SQL Server: Windows (`Integrated Security`, the
//! service's own account) or a SQL login (`User Id` / `Password`). A fresh enrol preselects the
//! mode of the .NET agent's saved string and lets the operator choose the other one.
//!
//! Only ADO strings (`key=value;...`, the .NET agent's format) are handled. Values are kept as
//! written; a value holding `;`, a quote or outer spaces is written back quoted, as
//! `SqlConnectionStringBuilder` does. Nothing here logs or prints a value.

use std::fmt;

/// The two sign-in modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlAuth {
    /// `Integrated Security=true`: the service account (LocalSystem) signs in.
    Windows,
    /// `User Id` + `Password`.
    Sql,
}

impl SqlAuth {
    /// `windows` or `sql` (the installer's `-SqlAuth` values).
    pub fn as_str(self) -> &'static str {
        match self {
            SqlAuth::Windows => "windows",
            SqlAuth::Sql => "sql",
        }
    }

    /// Parses `windows` / `sql` (case-insensitive; `integrated` and `login` too).
    pub fn parse(s: &str) -> Option<SqlAuth> {
        match s.trim().to_ascii_lowercase().as_str() {
            "windows" | "integrated" | "sspi" => Some(SqlAuth::Windows),
            "sql" | "login" | "sqllogin" => Some(SqlAuth::Sql),
            _ => None,
        }
    }
}

impl fmt::Display for SqlAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

const INTEGRATED: &[&str] = &["integrated security", "trusted_connection"];
const USER: &[&str] = &["user id", "uid", "user", "username"];
const PASSWORD: &[&str] = &["password", "pwd"];
const DATABASE: &[&str] = &["database", "initial catalog"];

/// An ADO connection string as ordered `(key, value)` pairs.
#[derive(Clone, PartialEq, Eq)]
pub struct ConnStr {
    parts: Vec<(String, String)>,
}

impl fmt::Debug for ConnStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Keys only: a value may be a password.
        f.debug_list()
            .entries(self.parts.iter().map(|(k, _)| k))
            .finish()
    }
}

impl ConnStr {
    /// Splits `text` on `;`, honouring `'...'` and `"..."` values (a doubled quote inside
    /// is one quote). Parts without `=` are dropped.
    pub fn parse(text: &str) -> ConnStr {
        let mut parts = Vec::new();
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let mut key = String::new();
            while i < chars.len() && chars[i] != '=' && chars[i] != ';' {
                key.push(chars[i]);
                i += 1;
            }
            if i >= chars.len() || chars[i] == ';' {
                i += 1;
                continue;
            }
            i += 1; // '='
            while i < chars.len() && chars[i] == ' ' {
                i += 1;
            }
            let mut value = String::new();
            if i < chars.len() && (chars[i] == '\'' || chars[i] == '"') {
                let q = chars[i];
                i += 1;
                while i < chars.len() {
                    if chars[i] == q {
                        if chars.get(i + 1) == Some(&q) {
                            value.push(q);
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    value.push(chars[i]);
                    i += 1;
                }
                while i < chars.len() && chars[i] != ';' {
                    i += 1;
                }
            } else {
                while i < chars.len() && chars[i] != ';' {
                    value.push(chars[i]);
                    i += 1;
                }
                value = value.trim_end().to_string();
            }
            i += 1; // ';'
            let key = key.trim().to_string();
            if !key.is_empty() {
                parts.push((key, value));
            }
        }
        ConnStr { parts }
    }

    /// The last value of any of `keys` (case-insensitive).
    pub fn get(&self, keys: &[&str]) -> Option<&str> {
        self.parts
            .iter()
            .rev()
            .find(|(k, _)| keys.iter().any(|w| k.eq_ignore_ascii_case(w)))
            .map(|(_, v)| v.as_str())
    }

    /// Drops every part named by `keys`.
    pub fn remove(&mut self, keys: &[&str]) {
        self.parts
            .retain(|(k, _)| !keys.iter().any(|w| k.eq_ignore_ascii_case(w)));
    }

    /// Replaces the parts named by `keys` with one `key=value` at the end.
    pub fn set(&mut self, keys: &[&str], key: &str, value: &str) {
        self.remove(keys);
        self.parts.push((key.to_string(), value.to_string()));
    }

    /// The string again (`key=value;` each, quoted where needed).
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (k, v) in &self.parts {
            out.push_str(k);
            out.push('=');
            out.push_str(&quote(v));
            out.push(';');
        }
        out
    }
}

fn quote(v: &str) -> String {
    let needs =
        v.contains(';') || v.starts_with(['\'', '"']) || v.starts_with(' ') || v.ends_with(' ');
    if !needs {
        return v.to_string();
    }
    if v.contains('"') {
        format!("'{}'", v.replace('\'', "''"))
    } else {
        format!("\"{v}\"")
    }
}

fn truthy(v: &str) -> bool {
    ["true", "yes", "sspi"]
        .iter()
        .any(|t| v.trim().eq_ignore_ascii_case(t))
}

/// The mode `connection` uses: Windows when `Integrated Security` / `Trusted_Connection` is
/// on, a SQL login when it names a user, Windows otherwise (the .NET installer's default
/// `Integrated Security=true`).
pub fn detect(connection: &str) -> SqlAuth {
    let c = ConnStr::parse(connection);
    if c.get(INTEGRATED).is_some_and(truthy) {
        return SqlAuth::Windows;
    }
    if c.get(USER).is_some_and(|u| !u.trim().is_empty()) {
        return SqlAuth::Sql;
    }
    SqlAuth::Windows
}

/// The SQL login named by `connection` (none for Windows sign-in).
pub fn user(connection: &str) -> Option<String> {
    let c = ConnStr::parse(connection);
    (detect(connection) == SqlAuth::Sql)
        .then(|| c.get(USER).map(|u| u.trim().to_string()))
        .flatten()
}

/// The database `connection` opens.
pub fn database(connection: &str) -> Option<String> {
    ConnStr::parse(connection)
        .get(DATABASE)
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
}

/// `connection` signing in with `auth`. For [`SqlAuth::Sql`], `user` / `password` replace the
/// saved ones; a missing one keeps the saved value (the password only while the user stays
/// the same).
///
/// # Errors
/// A message (without any value) when a SQL login lacks a user or a password.
pub fn with_auth(
    connection: &str,
    auth: SqlAuth,
    user: Option<&str>,
    password: Option<&str>,
) -> Result<String, String> {
    let mut c = ConnStr::parse(connection);
    match auth {
        SqlAuth::Windows => {
            c.remove(USER);
            c.remove(PASSWORD);
            c.set(INTEGRATED, "Integrated Security", "true");
        }
        SqlAuth::Sql => {
            let saved_user = c.get(USER).map(str::to_string).unwrap_or_default();
            let saved_password = c.get(PASSWORD).map(str::to_string).unwrap_or_default();
            let user = user
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .map_or(saved_user.clone(), str::to_string);
            if user.is_empty() {
                return Err("a SQL login needs a user name (-SqlUser)".into());
            }
            let password = match password.filter(|p| !p.is_empty()) {
                Some(p) => p.to_string(),
                None if user.eq_ignore_ascii_case(&saved_user) => saved_password,
                None => String::new(),
            };
            if password.is_empty() {
                return Err(format!(
                    "the SQL login {user} needs a password (DL_AGENT_SQL_PASSWORD)"
                ));
            }
            c.remove(INTEGRATED);
            c.set(USER, "User Id", &user);
            c.set(PASSWORD, "Password", &password);
        }
    }
    Ok(c.render())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_the_mode_like_install_ps1() {
        for s in [
            "Server=.;Database=SMILE_FO;Integrated Security=true;TrustServerCertificate=true",
            "Server=.;Database=SMILE_FO;Integrated Security=SSPI",
            "Server=.;Database=SMILE_FO;Trusted_Connection=yes",
            "Server=.;Database=SMILE_FO",
        ] {
            assert_eq!(detect(s), SqlAuth::Windows, "{s}");
        }
        let sql = "Server=.\\SQL2008;Database=SMILE_FO;User Id=sa;Password='p;w''d'";
        assert_eq!(detect(sql), SqlAuth::Sql);
        assert_eq!(user(sql).as_deref(), Some("sa"));
        assert_eq!(database(sql).as_deref(), Some("SMILE_FO"));
        assert_eq!(
            ConnStr::parse(sql).get(PASSWORD),
            Some("p;w'd"),
            "quoted values keep their ; and doubled quotes"
        );
    }

    #[test]
    fn switches_between_modes_without_losing_other_keys() {
        let win = "Server=.;Database=SMILE_FO;Integrated Security=true;Encrypt=False";
        let sql = with_auth(win, SqlAuth::Sql, Some("checkin"), Some("s3;cret")).unwrap();
        assert_eq!(
            sql,
            "Server=.;Database=SMILE_FO;Encrypt=False;User Id=checkin;Password=\"s3;cret\";"
        );
        assert_eq!(detect(&sql), SqlAuth::Sql);
        assert_eq!(
            ConnStr::parse(&sql).get(PASSWORD),
            Some("s3;cret"),
            "the rendered string parses back"
        );
        let back = with_auth(&sql, SqlAuth::Windows, None, None).unwrap();
        assert_eq!(
            back,
            "Server=.;Database=SMILE_FO;Encrypt=False;Integrated Security=true;"
        );
        // Same user: the saved password stays; another user needs one.
        let kept = with_auth(&sql, SqlAuth::Sql, Some("CHECKIN"), None).unwrap();
        assert!(kept.contains("Password=\"s3;cret\""));
        let e = with_auth(&sql, SqlAuth::Sql, Some("other"), None).unwrap_err();
        assert!(!e.contains("s3;cret") && e.contains("password"));
        assert!(with_auth(win, SqlAuth::Sql, None, Some("x")).is_err());
    }

    #[test]
    fn debug_never_shows_values() {
        let c = ConnStr::parse("Server=.;Password=hunter2");
        assert!(!format!("{c:?}").contains("hunter2"));
        assert_eq!(SqlAuth::parse(" SQL "), Some(SqlAuth::Sql));
        assert_eq!(SqlAuth::parse("Windows"), Some(SqlAuth::Windows));
        assert_eq!(SqlAuth::parse("other"), None);
    }
}
