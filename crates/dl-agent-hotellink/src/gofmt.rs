//! The bits of Go's `fmt` the ported messages rely on.

/// `fmt.Sprintf("%q", s)`: a double-quoted Go string literal.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{7}' => out.push_str("\\a"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\u{b}' => out.push_str("\\v"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `fmt.Sscanf(s, "%d", &n)`: leading spaces, an optional sign, then digits; anything
/// after the digits is ignored. `None` when there are no digits.
pub fn scan_int(s: &str) -> Option<i64> {
    let t = s.trim_start();
    let (neg, rest) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let digits: &str = &rest[..rest.bytes().take_while(u8::is_ascii_digit).count()];
    if digits.is_empty() {
        return None;
    }
    let n: i64 = digits.parse().ok()?;
    Some(if neg { -n } else { n })
}

/// `fmt.Sprintf("%.0f", v)`.
pub fn f0(v: f64) -> String {
    format!("{v:.0}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_matches_go() {
        assert_eq!(quote("ab"), "\"ab\"");
        assert_eq!(quote("a\"b\\"), "\"a\\\"b\\\\\"");
        assert_eq!(quote("x\ny\u{1}"), "\"x\\ny\\x01\"");
        assert_eq!(quote("Nguyễn"), "\"Nguyễn\"");
    }

    #[test]
    fn scan_int_matches_sscanf() {
        assert_eq!(scan_int("12"), Some(12));
        assert_eq!(scan_int(" 3abc"), Some(3));
        assert_eq!(scan_int("-4"), Some(-4));
        assert_eq!(scan_int(""), None);
        assert_eq!(scan_int("x1"), None);
    }

    #[test]
    fn f0_rounds() {
        assert_eq!(f0(100001.0), "100001");
        assert_eq!(f0(2.4), "2");
    }
}
