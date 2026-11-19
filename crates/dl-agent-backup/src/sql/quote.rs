//! The Go agent's quoting and path checks, kept for the statement text shown in job steps
//! and for anything that cannot be a parameter. The statements themselves pass the database
//! name and file paths as parameters (`BACKUP DATABASE @db TO DISK = @path`).

use crate::Error;

/// Longest SQL Server identifier (`sysname`).
pub const MAX_IDENTIFIER: usize = 128;

/// `QuoteName`: `[name]` with `]` doubled.
pub fn quote_name(name: &str) -> String {
    format!("[{}]", name.replace(']', "]]"))
}

/// A path as an N-quoted literal with `'` doubled.
pub fn quote_path(path: &str) -> String {
    format!("N'{}'", path.replace('\'', "''"))
}

/// A database name the agent will put in a statement: 1–128 characters, no control
/// characters (NUL would cut the name in some drivers).
pub fn check_identifier(name: &str) -> Result<&str, Error> {
    if name.is_empty() {
        return Err(Error::Sql("databaseName is required".into()));
    }
    if name.chars().count() > MAX_IDENTIFIER {
        return Err(Error::Sql(format!(
            "database name is longer than {MAX_IDENTIFIER} characters"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(Error::Sql(
            "database name contains a control character".into(),
        ));
    }
    Ok(name)
}

/// A server-side file path: not empty, no control characters.
pub fn check_path(path: &str) -> Result<&str, Error> {
    if path.trim().is_empty() {
        return Err(Error::Sql("backup file path is required".into()));
    }
    if path.chars().any(char::is_control) {
        return Err(Error::Sql(
            "backup file path contains a control character".into(),
        ));
    }
    Ok(path)
}

/// Go's restore checks: no UNC path (`\\server\share`, `//server/share`), and a `.bak` or
/// `.trn` extension (any case).
pub fn check_restore_path(path: &str) -> Result<&str, Error> {
    if path.is_empty() {
        return Err(Error::Sql("backupFilePath is required".into()));
    }
    check_path(path)?;
    let lead: String = path.chars().take(2).collect();
    if matches!(lead.as_str(), "\\\\" | "//" | "\\/" | "/\\") {
        return Err(Error::Sql(format!(
            "UNC paths are not allowed for backup file: {path}"
        )));
    }
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    let ext = name
        .rfind('.')
        .map(|i| name[i..].to_ascii_lowercase())
        .unwrap_or_default();
    if ext != ".bak" && ext != ".trn" {
        return Err(Error::Sql(format!(
            "backup file must have .bak or .trn extension, got: {ext}"
        )));
    }
    Ok(path)
}

/// The separator a server-side directory uses: `/` for a Linux SQL Server, else `\`.
pub fn separator(dir: &str) -> char {
    if dir.contains('/') && !dir.contains('\\') {
        '/'
    } else {
        '\\'
    }
}

/// Joins a server-side directory and a file name with the directory's own separator.
pub fn join(dir: &str, file: &str) -> String {
    let sep = separator(dir);
    let dir = dir.trim_end_matches(['\\', '/']);
    if dir.is_empty() {
        return format!("{sep}{file}");
    }
    format!("{dir}{sep}{file}")
}

/// The directory part of a server-side path (everything before the last separator).
pub fn parent(path: &str) -> &str {
    match path.rfind(['\\', '/']) {
        Some(i) => &path[..i],
        None => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_name_doubles_brackets() {
        // Go TestSQLQuoteName.
        assert_eq!(quote_name("MyDB"), "[MyDB]");
        assert_eq!(quote_name("My]DB"), "[My]]DB]");
        assert_eq!(quote_name("a]]b"), "[a]]]]b]");
        assert_eq!(quote_name(""), "[]");
    }

    #[test]
    fn quote_path_doubles_quotes() {
        assert_eq!(quote_path(r"C:\B\it's.bak"), r"N'C:\B\it''s.bak'");
    }

    #[test]
    fn identifiers_are_checked() {
        assert!(check_identifier("Sales DB").is_ok());
        assert!(check_identifier("").is_err());
        assert!(check_identifier("a\0b").is_err());
        assert!(check_identifier(&"x".repeat(129)).is_err());
        assert!(check_identifier(&"đ".repeat(128)).is_ok());
    }

    #[test]
    fn restore_path_rules_match_go() {
        for ok in [
            r"C:\b\x.bak",
            r"C:\b\x.TRN",
            "/var/opt/mssql/x.bak",
            "x.Bak",
        ] {
            assert!(check_restore_path(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            r"\\server\share\x.bak",
            "//server/share/x.bak",
            r"C:\b\x.zip",
            r"C:\b\x",
            r"C:\b.bak\x",
            "C:\\b\\x\n.bak",
        ] {
            assert!(check_restore_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn server_paths_keep_their_separator() {
        assert_eq!(join(r"D:\Backup\", "a.bak"), r"D:\Backup\a.bak");
        assert_eq!(
            join("/var/opt/mssql/data", "a.bak"),
            "/var/opt/mssql/data/a.bak"
        );
        assert_eq!(parent(r"D:\Data\master.mdf"), r"D:\Data");
        assert_eq!(
            parent("/var/opt/mssql/data/master.mdf"),
            "/var/opt/mssql/data"
        );
        assert_eq!(parent("x.mdf"), "");
    }
}
