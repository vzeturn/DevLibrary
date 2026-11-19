//! The part of `Microsoft.Extensions.Configuration` the agent uses: JSON files (comments
//! and trailing commas allowed, case-insensitive duplicate keys refused), environment
//! variables (`__` as the separator, optional prefix), command-line arguments
//! (`--Key=value`, `--Key value`, `/Key value`, `Key=value`) and in-memory pairs, layered
//! so a later provider wins for a key, with case-insensitive `:`-separated keys.

use super::stj_read::eq_ci;
use std::cmp::Ordering;
use std::fmt;
use std::path::{Path, PathBuf};

/// A configuration source failure (the host would not start).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// `InvalidDataException: Failed to load configuration from file '<path>'.` with the
    /// inner `FormatException` message.
    File {
        /// The file.
        path: PathBuf,
        /// Why it could not be loaded.
        reason: String,
    },
    /// `FormatException` from the command-line provider.
    CommandLine(String),
    /// `InvalidOperationException: Failed to convert configuration value ...`.
    Convert {
        /// The value as configured.
        value: String,
        /// The configuration path, e.g. `Agent:HeartbeatSeconds`.
        path: String,
        /// The .NET type name, e.g. `System.Int32`.
        type_name: &'static str,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::File { path, reason } => write!(
                f,
                "Failed to load configuration from file '{}'. {reason}",
                path.display()
            ),
            ConfigError::CommandLine(m) => f.write_str(m),
            ConfigError::Convert {
                value,
                path,
                type_name,
            } => write!(
                f,
                "Failed to convert configuration value '{value}' at '{path}' to type '{type_name}'."
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

/// One provider's key/value pairs (keys unique ignoring case; a value may be null).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Provider {
    /// Where the data came from (for diagnostics).
    pub name: String,
    data: Vec<(String, Option<String>)>,
}

impl Provider {
    /// An empty provider.
    pub fn new(name: impl Into<String>) -> Provider {
        Provider {
            name: name.into(),
            data: Vec::new(),
        }
    }

    /// Sets `key` (replacing a key equal ignoring case, keeping its first spelling).
    pub fn set(&mut self, key: &str, value: Option<String>) {
        if let Some(slot) = self.data.iter_mut().find(|(k, _)| eq_ci(k, key)) {
            slot.1 = value;
        } else {
            self.data.push((key.to_owned(), value));
        }
    }

    fn contains(&self, key: &str) -> bool {
        self.data.iter().any(|(k, _)| eq_ci(k, key))
    }

    /// `TryGet`: `Some(value)` when the key is present (the value may be null).
    pub fn get(&self, key: &str) -> Option<Option<&str>> {
        self.data
            .iter()
            .find(|(k, _)| eq_ci(k, key))
            .map(|(_, v)| v.as_deref())
    }

    /// The pairs in insertion order.
    pub fn pairs(&self) -> impl Iterator<Item = (&str, Option<&str>)> {
        self.data.iter().map(|(k, v)| (k.as_str(), v.as_deref()))
    }

    /// In-memory pairs (`AddInMemoryCollection`).
    pub fn memory<'a>(
        name: &str,
        pairs: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    ) -> Provider {
        let mut p = Provider::new(name);
        for (k, v) in pairs {
            p.set(k, v.map(str::to_owned));
        }
        p
    }

    /// Environment variables (`AddEnvironmentVariables(prefix)`): `__` becomes `:`, then
    /// only names starting with `prefix` (ignoring case) are kept, without it.
    pub fn environment_from<I, K, V>(prefix: &str, vars: I) -> Provider
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut p = Provider::new(if prefix.is_empty() {
            "EnvironmentVariables".to_owned()
        } else {
            format!("EnvironmentVariables Prefix: '{prefix}'")
        });
        for (k, v) in vars {
            let key = k.as_ref().replace("__", ":");
            if key.len() >= prefix.len()
                && key.is_char_boundary(prefix.len())
                && eq_ci(&key[..prefix.len()], prefix)
            {
                p.set(&key[prefix.len()..], Some(v.as_ref().to_owned()));
            }
        }
        p
    }

    /// This process's environment variables with `prefix`.
    pub fn environment(prefix: &str) -> Provider {
        Provider::environment_from(
            prefix,
            std::env::vars_os().map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            }),
        )
    }

    /// Command-line arguments (`AddCommandLine(args)` without switch mappings).
    pub fn command_line<S: AsRef<str>>(args: &[S]) -> Result<Provider, ConfigError> {
        let mut p = Provider::new("CommandLine");
        let mut it = args.iter().map(AsRef::as_ref);
        while let Some(raw) = it.next() {
            let (arg, start) = if let Some(rest) = raw.strip_prefix("--") {
                (format!("--{rest}"), 2)
            } else if raw.starts_with('-') {
                (raw.to_owned(), 1)
            } else if let Some(rest) = raw.strip_prefix('/') {
                (format!("--{rest}"), 2)
            } else {
                (raw.to_owned(), 0)
            };
            match arg.find('=') {
                None => {
                    if start == 0 || start == 1 {
                        // Not a key (or a short switch without a mapping): ignored.
                        continue;
                    }
                    let key = arg[start..].to_owned();
                    let Some(value) = it.next() else { continue };
                    p.set(&key, Some(value.to_owned()));
                }
                Some(eq) => {
                    if start == 1 {
                        return Err(ConfigError::CommandLine(format!(
                            "The short switch '{arg}' is not defined in the switch mappings."
                        )));
                    }
                    p.set(&arg[start..eq], Some(arg[eq + 1..].to_owned()));
                }
            }
        }
        Ok(p)
    }

    /// A JSON file (`AddJsonFile(path, optional)`). `Ok(None)` for a missing optional file.
    pub fn json_file(path: &Path, optional: bool) -> Result<Option<Provider>, ConfigError> {
        let fail = |reason: String| ConfigError::File {
            path: path.to_path_buf(),
            reason,
        };
        if !path.is_file() {
            if optional {
                return Ok(None);
            }
            return Err(fail(format!(
                "The configuration file '{}' was not found and is not optional.",
                path.display()
            )));
        }
        let bytes = std::fs::read(path).map_err(|e| fail(e.to_string()))?;
        let mut p = Provider::new(format!("Json '{}'", path.display()));
        parse_json_into(&bytes, &mut p).map_err(fail)?;
        Ok(Some(p))
    }

    /// JSON text as `JsonConfigurationFileParser` flattens it.
    pub fn json_text(name: &str, text: &str) -> Result<Provider, String> {
        let mut p = Provider::new(name);
        parse_json_into(text.as_bytes(), &mut p)?;
        Ok(p)
    }
}

/// `ConfigurationKeyComparer` for one key segment: integers first in numeric order, then
/// text ignoring case.
fn compare_segment(a: &str, b: &str) -> Ordering {
    let int = |s: &str| s.trim().parse::<i64>().ok();
    match (int(a), int(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => {
            let fold = |s: &str| s.to_uppercase();
            fold(a).cmp(&fold(b))
        }
    }
}

/// Layered providers (`IConfigurationRoot`): the last provider holding a key wins, even
/// with a null value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Configuration {
    providers: Vec<Provider>,
}

impl Configuration {
    /// No providers.
    pub fn new() -> Configuration {
        Configuration::default()
    }

    /// Adds a provider on top.
    pub fn add(&mut self, p: Provider) -> &mut Self {
        self.providers.push(p);
        self
    }

    /// The providers, lowest priority first.
    pub fn providers(&self) -> &[Provider] {
        &self.providers
    }

    /// `Some(value)` when some provider holds `key`; the value may be null.
    pub fn lookup(&self, key: &str) -> Option<Option<&str>> {
        self.providers.iter().rev().find_map(|p| p.get(key))
    }

    /// `configuration[key]`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.lookup(key).flatten()
    }

    /// The immediate child segment names of `path` (`GetChildren`), sorted like .NET.
    pub fn children(&self, path: &str) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for p in &self.providers {
            for (k, _) in p.pairs() {
                let rest = if path.is_empty() {
                    Some(k)
                } else if k.len() > path.len()
                    && k.is_char_boundary(path.len())
                    && eq_ci(&k[..path.len()], path)
                    && k[path.len()..].starts_with(':')
                {
                    Some(&k[path.len() + 1..])
                } else {
                    None
                };
                if let Some(rest) = rest {
                    let seg = rest.split(':').next().unwrap_or("");
                    if !names.iter().any(|n| eq_ci(n, seg)) {
                        names.push(seg.to_owned());
                    }
                }
            }
        }
        names.sort_by(|a, b| compare_segment(a, b));
        names
    }

    /// Whether `path` has any child key.
    pub fn has_children(&self, path: &str) -> bool {
        !self.children(path).is_empty()
    }

    fn holds(&self, key: &str) -> bool {
        self.providers.iter().any(|p| p.contains(key))
    }

    /// Whether the section `path` has a value or children (`Exists()`), or is present with
    /// a null value.
    pub fn is_present(&self, path: &str) -> bool {
        self.holds(path) || self.has_children(path)
    }
}

// ---------------------------------------------------------------------------------------
// JSON configuration file parser.

struct JsonCursor<'a> {
    b: &'a [u8],
    i: usize,
    line: usize,
    line_start: usize,
}

impl JsonCursor<'_> {
    fn err(&self, what: &str) -> String {
        format!(
            "Could not parse the JSON file. {what} LineNumber: {} | BytePositionInLine: {}.",
            self.line,
            self.i - self.line_start
        )
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn bump(&mut self) {
        if self.b.get(self.i) == Some(&b'\n') {
            self.line += 1;
            self.line_start = self.i + 1;
        }
        self.i += 1;
    }

    /// Skips whitespace and (when `comments`) comments.
    fn skip(&mut self, comments: bool) -> Result<(), String> {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\n' | b'\r') => self.bump(),
                Some(b'/') if comments => {
                    match self.b.get(self.i + 1) {
                        Some(b'/') => {
                            while let Some(c) = self.peek() {
                                if c == b'\n' {
                                    break;
                                }
                                self.bump();
                            }
                        }
                        Some(b'*') => {
                            self.bump();
                            self.bump();
                            loop {
                                match self.peek() {
                                    None => return Err(self.err(
                                        "Expected end of comment, but instead reached end of data.",
                                    )),
                                    Some(b'*') if self.b.get(self.i + 1) == Some(&b'/') => {
                                        self.bump();
                                        self.bump();
                                        break;
                                    }
                                    Some(_) => self.bump(),
                                }
                            }
                        }
                        _ => return Err(self.err("'/' is an invalid start of a value.")),
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        // At the opening quote.
        self.bump();
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(self.err("Expected end of string, but instead reached end of data."));
            };
            match c {
                b'"' => {
                    self.bump();
                    return Ok(String::from_utf8_lossy(&out).into_owned());
                }
                b'\\' => {
                    self.bump();
                    let Some(e) = self.peek() else {
                        return Err(
                            self.err("Expected end of string, but instead reached end of data.")
                        );
                    };
                    self.bump();
                    match e {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let mut unit = self.hex4()?;
                            let ch = if (0xD800..0xDC00).contains(&unit)
                                && self.b.get(self.i) == Some(&b'\\')
                                && self.b.get(self.i + 1) == Some(&b'u')
                            {
                                self.bump();
                                self.bump();
                                let low = self.hex4()?;
                                if (0xDC00..0xE000).contains(&low) {
                                    let cp = 0x10000
                                        + ((u32::from(unit) - 0xD800) << 10)
                                        + (u32::from(low) - 0xDC00);
                                    char::from_u32(cp).unwrap_or('\u{fffd}')
                                } else {
                                    unit = low;
                                    let mut tmp = [0u8; 4];
                                    out.extend_from_slice(
                                        '\u{fffd}'.encode_utf8(&mut tmp).as_bytes(),
                                    );
                                    char::from_u32(u32::from(unit)).unwrap_or('\u{fffd}')
                                }
                            } else {
                                char::from_u32(u32::from(unit)).unwrap_or('\u{fffd}')
                            };
                            let mut tmp = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
                        }
                        _ => return Err(self.err("Invalid escape sequence in a JSON string.")),
                    }
                }
                0..=0x1F => {
                    return Err(self.err("Control characters are not allowed in a JSON string."));
                }
                _ => {
                    out.push(c);
                    self.bump();
                }
            }
        }
    }

    fn hex4(&mut self) -> Result<u16, String> {
        let digits = self
            .b
            .get(self.i..self.i + 4)
            .and_then(|d| std::str::from_utf8(d).ok())
            .and_then(|d| {
                u16::from_str_radix(d, 16)
                    .ok()
                    .filter(|_| d.bytes().all(|b| b.is_ascii_hexdigit()))
            });
        match digits {
            Some(v) => {
                for _ in 0..4 {
                    self.bump();
                }
                Ok(v)
            }
            None => Err(self.err("Invalid \\u escape in a JSON string.")),
        }
    }

    fn number(&mut self) -> Result<String, String> {
        let start = self.i;
        let bad = |c: &Self| c.err("Invalid JSON number.");
        if self.peek() == Some(b'-') {
            self.bump();
        }
        match self.peek() {
            Some(b'0') => self.bump(),
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.bump();
                }
            }
            _ => return Err(bad(self)),
        }
        if self.peek() == Some(b'.') {
            self.bump();
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(bad(self));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.bump();
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.bump();
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.bump();
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(bad(self));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.bump();
            }
        }
        match self.peek() {
            None | Some(b' ' | b'\t' | b'\n' | b'\r' | b',' | b'}' | b']' | b'/') => {}
            _ => return Err(bad(self)),
        }
        Ok(String::from_utf8_lossy(&self.b[start..self.i]).into_owned())
    }

    fn literal(&mut self, word: &[u8]) -> Result<(), String> {
        if self.b.get(self.i..self.i + word.len()) == Some(word) {
            for _ in 0..word.len() {
                self.bump();
            }
            Ok(())
        } else {
            Err(self.err("Invalid JSON literal."))
        }
    }
}

struct Flattener<'p> {
    out: &'p mut Provider,
    paths: Vec<String>,
}

impl Flattener<'_> {
    fn key(&self) -> String {
        self.paths.last().cloned().unwrap_or_default()
    }

    fn enter(&mut self, name: &str) {
        let next = match self.paths.last() {
            Some(p) => format!("{p}:{name}"),
            None => name.to_owned(),
        };
        self.paths.push(next);
    }

    fn leaf(&mut self, value: Option<String>) -> Result<(), String> {
        let key = self.key();
        if self.out.contains(&key) {
            return Err(format!("A duplicate key '{key}' was found."));
        }
        self.out.set(&key, value);
        Ok(())
    }

    fn empty(&mut self, value: Option<String>) {
        if !self.paths.is_empty() {
            let key = self.key();
            self.out.set(&key, value);
        }
    }

    fn value(&mut self, c: &mut JsonCursor<'_>, depth: usize) -> Result<(), String> {
        if depth > 64 {
            return Err(c.err("The maximum configured depth of 64 has been exceeded."));
        }
        match c.peek() {
            Some(b'{') => {
                c.bump();
                let mut any = false;
                loop {
                    c.skip(true)?;
                    match c.peek() {
                        Some(b'}') => {
                            c.bump();
                            break;
                        }
                        Some(b'"') => {}
                        Some(b',') if any => {
                            return Err(c.err("',' is an invalid start of a property name. Expected a '\"'."));
                        }
                        Some(ch) => {
                            return Err(c.err(&format!(
                                "'{}' is an invalid start of a property name. Expected a '\"'.",
                                ch as char
                            )));
                        }
                        None => return Err(c.err("Expected depth to be zero at the end of the JSON payload. There is an open JSON object or array that should be closed.")),
                    }
                    let name = c.string()?;
                    c.skip(false)?;
                    if c.peek() != Some(b':') {
                        let found = c.peek().map(|b| b as char).unwrap_or(' ');
                        return Err(c.err(&format!(
                            "'{found}' is invalid after a property name. Expected a ':'."
                        )));
                    }
                    c.bump();
                    c.skip(true)?;
                    any = true;
                    self.enter(&name);
                    self.value(c, depth + 1)?;
                    self.paths.pop();
                    c.skip(true)?;
                    match c.peek() {
                        Some(b',') => c.bump(),
                        Some(b'}') => {
                            c.bump();
                            break;
                        }
                        None => return Err(c.err("Expected depth to be zero at the end of the JSON payload. There is an open JSON object or array that should be closed.")),
                        Some(ch) => {
                            return Err(c.err(&format!(
                                "'{}' is invalid after a value. Expected either ',', '}}', or ']'.",
                                ch as char
                            )));
                        }
                    }
                }
                if !any {
                    self.empty(None);
                }
                Ok(())
            }
            Some(b'[') => {
                c.bump();
                let mut index = 0usize;
                loop {
                    c.skip(true)?;
                    match c.peek() {
                        Some(b']') => {
                            c.bump();
                            break;
                        }
                        None => return Err(c.err("Expected depth to be zero at the end of the JSON payload. There is an open JSON object or array that should be closed.")),
                        Some(b',') => return Err(c.err("',' is an invalid start of a value.")),
                        _ => {}
                    }
                    self.enter(&index.to_string());
                    self.value(c, depth + 1)?;
                    self.paths.pop();
                    index += 1;
                    c.skip(true)?;
                    match c.peek() {
                        Some(b',') => c.bump(),
                        Some(b']') => {
                            c.bump();
                            break;
                        }
                        None => return Err(c.err("Expected depth to be zero at the end of the JSON payload. There is an open JSON object or array that should be closed.")),
                        Some(ch) => {
                            return Err(c.err(&format!(
                                "'{}' is invalid after a value. Expected either ',', '}}', or ']'.",
                                ch as char
                            )));
                        }
                    }
                }
                if index == 0 {
                    // .NET 10 stores an empty array as an empty string (an empty object as null).
                    self.empty(Some(String::new()));
                }
                Ok(())
            }
            Some(b'"') => {
                let s = c.string()?;
                self.leaf(Some(s))
            }
            Some(b't') => {
                c.literal(b"true")?;
                self.leaf(Some("True".into()))
            }
            Some(b'f') => {
                c.literal(b"false")?;
                self.leaf(Some("False".into()))
            }
            Some(b'n') => {
                c.literal(b"null")?;
                self.leaf(None)
            }
            Some(b'-' | b'0'..=b'9') => {
                let n = c.number()?;
                self.leaf(Some(n))
            }
            Some(ch) => Err(c.err(&format!("'{}' is an invalid start of a value.", ch as char))),
            None => Err(c.err("Expected a value, but instead reached end of data.")),
        }
    }
}

fn parse_json_into(bytes: &[u8], out: &mut Provider) -> Result<(), String> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let mut c = JsonCursor {
        b: bytes,
        i: 0,
        line: 0,
        line_start: 0,
    };
    c.skip(true)?;
    let kind = match c.peek() {
        None => {
            return Err(c.err("The input does not contain any JSON tokens. Expected the input to start with a valid JSON token, when isFinalBlock is true."));
        }
        Some(b'{') => None,
        Some(b'[') => Some("Array"),
        Some(b'"') => Some("String"),
        Some(b't') => Some("True"),
        Some(b'f') => Some("False"),
        Some(b'n') => Some("Null"),
        Some(_) => Some("Number"),
    };
    let mut f = Flattener {
        out,
        paths: Vec::new(),
    };
    if let Some(kind) = kind {
        // Parse to report syntax errors first, like JsonDocument.Parse.
        let mut scratch = Provider::new("");
        let mut g = Flattener {
            out: &mut scratch,
            paths: vec![String::new()],
        };
        g.value(&mut c, 1)?;
        c.skip(true)?;
        if c.peek().is_some() {
            return Err(c.err("Expected end of data."));
        }
        return Err(format!(
            "Top-level JSON element must be an object. Instead, '{kind}' was found."
        ));
    }
    f.value(&mut c, 1)?;
    c.skip(true)?;
    if let Some(ch) = c.peek() {
        return Err(c.err(&format!(
            "'{}' is invalid after a single JSON value. Expected end of data.",
            ch as char
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(text: &str) -> Result<Provider, String> {
        Provider::json_text("t", text)
    }

    #[test]
    fn json_flattening_matches_dotnet() {
        let p = json(
            "{ // c\n \"Agent\": { \"HeartbeatSeconds\": 20, /* x */ \"AutoUpdate\": \"False\",\n \"PrintRelay\": { \"Ports\": [9101, \"x\", 9102], \"Enabled\": false, },\n \"CloudUrl\": null, \"N\": 1.50, \"E\": {}, \"A\": [] }, }",
        )
        .unwrap();
        assert_eq!(p.get("agent:heartbeatseconds"), Some(Some("20")));
        assert_eq!(p.get("Agent:PrintRelay:Ports:1"), Some(Some("x")));
        assert_eq!(p.get("Agent:PrintRelay:Enabled"), Some(Some("False")));
        assert_eq!(p.get("Agent:CloudUrl"), Some(None));
        assert_eq!(p.get("Agent:N"), Some(Some("1.50")));
        assert_eq!(p.get("Agent:E"), Some(None));
        assert_eq!(p.get("Agent:A"), Some(Some("")));
        assert_eq!(p.get("Agent:Missing"), None);
    }

    #[test]
    fn json_errors_match_dotnet() {
        assert_eq!(
            json("{\"Agent\":{\"HeartbeatSeconds\":1,\"heartbeatSeconds\":2}}").unwrap_err(),
            "A duplicate key 'Agent:heartbeatSeconds' was found."
        );
        assert!(json("{\"Agent\":{\"A\":{\"B\":1}},\"agent\":{\"a\":{\"C\":2}}}").is_ok());
        assert_eq!(
            json("[1]").unwrap_err(),
            "Top-level JSON element must be an object. Instead, 'Array' was found."
        );
        for bad in [
            "",
            "  \r\n ",
            "{\"Agent\": {",
            "{'Agent': 1}",
            "{\"Agent\" /* k */ : 1}",
            "{\"A\":3,,}",
            "{\"Agent\":{}} x",
        ] {
            assert!(json(bad).is_err(), "{bad:?}");
        }
        assert!(json("\u{feff}{\"Agent\":{\"HeartbeatSeconds\":3}}").is_ok());
        assert!(json("/* lead */ {\"A\": /* v */ 1 /* w */, // x\n \"B\":[1,2,]} // end").is_ok());
    }

    #[test]
    fn command_line_matches_dotnet() {
        let p = Provider::command_line(&[
            "enroll",
            "--Agent:DataDir",
            "D:\\x",
            "/Agent:WebSocketFailuresBeforePoll=7",
            "--agent:HEARTBEATSECONDS=30",
            "--Agent:CloudUrl==a=b",
            "Agent:SiteTimeZone=Asia/Ho_Chi_Minh",
            "-x",
            "1",
            "--Agent:AutoUpdateCheckMinutes",
        ])
        .unwrap();
        assert_eq!(p.get("Agent:DataDir"), Some(Some("D:\\x")));
        assert_eq!(p.get("Agent:WebSocketFailuresBeforePoll"), Some(Some("7")));
        assert_eq!(p.get("Agent:HeartbeatSeconds"), Some(Some("30")));
        assert_eq!(p.get("Agent:CloudUrl"), Some(Some("=a=b")));
        assert_eq!(p.get("Agent:SiteTimeZone"), Some(Some("Asia/Ho_Chi_Minh")));
        assert_eq!(p.get("Agent:AutoUpdateCheckMinutes"), None);
        let p = Provider::command_line(&["--a", "--b", "c"]).unwrap();
        assert_eq!(p.get("a"), Some(Some("--b")));
        assert_eq!(
            Provider::command_line(&["-x=1"]).unwrap_err(),
            ConfigError::CommandLine(
                "The short switch '-x=1' is not defined in the switch mappings.".into()
            )
        );
    }

    #[test]
    fn environment_and_layering() {
        let env = Provider::environment_from(
            "",
            [
                ("Agent__UploadBatchesPerCycle", "16"),
                ("agent:HeartbeatSeconds", "44"),
                ("DOTNET_Agent__X", "1"),
            ],
        );
        assert_eq!(env.get("Agent:UploadBatchesPerCycle"), Some(Some("16")));
        assert_eq!(env.get("Agent:HeartbeatSeconds"), Some(Some("44")));
        let dotnet =
            Provider::environment_from("DOTNET_", [("DOTNET_Agent__X", "1"), ("Agent__Y", "2")]);
        assert_eq!(dotnet.get("Agent:X"), Some(Some("1")));
        assert_eq!(dotnet.get("Agent:Y"), None);

        let mut c = Configuration::new();
        c.add(Provider::memory(
            "a",
            [
                ("Agent:HeartbeatSeconds", Some("7")),
                ("Agent:P:Ports:10", Some("10")),
                ("Agent:P:Ports:2", Some("2")),
            ],
        ));
        c.add(Provider::memory(
            "b",
            [
                ("Agent:HeartbeatSeconds", None),
                ("Agent:P:Ports:x", Some("5")),
                ("Agent:P:Ports:01", Some("1")),
            ],
        ));
        assert_eq!(c.lookup("agent:heartbeatseconds"), Some(None));
        assert_eq!(c.children("Agent:P:Ports"), ["01", "2", "10", "x"]);
        assert_eq!(c.children("Agent"), ["HeartbeatSeconds", "P"]);
    }
}
