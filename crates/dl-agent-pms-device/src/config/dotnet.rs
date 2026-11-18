//! .NET exception text and string rules shared by the configuration reader and binder.

use std::fmt;

/// A .NET exception as the host prints it (type name, message, inner exceptions), without
/// stack traces. Startup failures are reported with the same text as the .NET agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DotnetError {
    /// Full .NET type name, for example `System.InvalidOperationException`.
    pub type_name: String,
    /// `Exception.Message`.
    pub message: String,
    /// `Exception.InnerException`.
    pub inner: Option<Box<DotnetError>>,
}

impl DotnetError {
    /// An exception without inner exception.
    pub fn new(type_name: &str, message: impl Into<String>) -> DotnetError {
        DotnetError {
            type_name: type_name.to_owned(),
            message: message.into(),
            inner: None,
        }
    }

    /// This exception with `inner` as its inner exception.
    pub fn with_inner(mut self, inner: DotnetError) -> DotnetError {
        self.inner = Some(Box::new(inner));
        self
    }

    /// Number of inner exceptions below this one.
    fn depth(&self) -> usize {
        self.inner.as_ref().map_or(0, |i| 1 + i.depth())
    }
}

/// `Exception.ToString()` without the `at ...` stack lines: inner exceptions follow with
/// ` ---> ` and each closes with `--- End of inner exception stack trace ---`.
impl fmt::Display for DotnetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.type_name, self.message)?;
        let mut inner = self.inner.as_deref();
        while let Some(e) = inner {
            write!(f, "\n ---> {}: {}", e.type_name, e.message)?;
            inner = e.inner.as_deref();
        }
        for _ in 0..self.depth() {
            f.write_str("\n   --- End of inner exception stack trace ---")?;
        }
        Ok(())
    }
}

impl std::error::Error for DotnetError {}

/// `char.ToUpperInvariant` (simple, one-to-one case mapping).
pub(crate) fn upper_char(c: char) -> char {
    let mut up = c.to_uppercase();
    match (up.next(), up.next()) {
        (Some(u), None) => u,
        _ => c,
    }
}

/// `StringComparer.OrdinalIgnoreCase` equality.
pub(crate) fn eq_ignore_case(a: &str, b: &str) -> bool {
    a.chars().map(upper_char).eq(b.chars().map(upper_char))
}

/// The key under which `OrdinalIgnoreCase` dictionaries file `key`.
pub(crate) fn fold_key(key: &str) -> String {
    key.chars().map(upper_char).collect()
}

/// `string.IsNullOrWhiteSpace` for a present string (`char.IsWhiteSpace` is the Unicode
/// White_Space property, the same set as Rust's `char::is_whitespace`).
pub(crate) fn is_blank(s: &str) -> bool {
    s.chars().all(char::is_whitespace)
}

/// `string.Trim()`: removes leading and trailing white space.
pub(crate) fn trim(s: &str) -> &str {
    s.trim_matches(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_matches_the_dotnet_layout_without_stack_lines() {
        let e = DotnetError::new("System.InvalidOperationException", "outer").with_inner(
            DotnetError::new("System.FormatException", "middle")
                .with_inner(DotnetError::new("System.FormatException", "inner")),
        );
        assert_eq!(
            e.to_string(),
            "System.InvalidOperationException: outer\n ---> System.FormatException: middle\n ---> System.FormatException: inner\n   --- End of inner exception stack trace ---\n   --- End of inner exception stack trace ---"
        );
        assert_eq!(DotnetError::new("A", "m").to_string(), "A: m");
    }

    #[test]
    fn ordinal_ignore_case_uses_simple_upper_case() {
        assert!(eq_ignore_case("agent:agentid", "Agent:AgentId"));
        assert!(eq_ignore_case("folıonum", "FOLIONUM")); // dotless i upper-cases to I
        assert!(!eq_ignore_case("straße", "STRASSE")); // no full case folding
    }
}
