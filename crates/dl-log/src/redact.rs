//! Secret scrubbing for log lines: connection-string passwords, bearer tokens and key-like
//! fields are replaced by `[redacted]` before a line is written.

use std::borrow::Cow;

use crate::{Format, Record};

/// What replaces a secret.
pub const REDACTED: &str = "[redacted]";

/// Field names whose value is a secret wherever they appear (`name=value`, `name: value`,
/// `"name":"value"`); matched without regard to case, as the end of a longer name too
/// (`sql_password`, `x-api-key`).
const SECRET_NAMES: [&str; 11] = [
    "password",
    "pwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "api-key",
    "private_key",
    "client_secret",
    "authorization",
    "_sealed",
];

/// A [`Format`] that renders with `inner` and then scrubs the line with [`scrub`].
#[derive(Debug, Clone)]
pub struct Redacted<F>(pub F);

impl<F: Format> Format for Redacted<F> {
    fn format(&self, record: &Record, out: &mut String) {
        let start = out.len();
        self.0.format(record, out);
        if let Cow::Owned(clean) = scrub(&out[start..]) {
            out.truncate(start);
            out.push_str(&clean);
        }
    }
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// `text` with every secret value replaced by [`REDACTED`]: values after a secret field
/// name and `=` / `:` (quoted or up to `;`, `,`, `&`, whitespace or a closing bracket), and
/// the token after `Bearer` / `Basic`. Borrowed when nothing was found.
pub fn scrub(text: &str) -> Cow<'_, str> {
    let lower = text.to_ascii_lowercase();
    let b = text.as_bytes();
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    for scheme in ["bearer ", "basic "] {
        let mut from = 0;
        while let Some(i) = lower[from..].find(scheme).map(|i| i + from) {
            let s = i + scheme.len();
            let e = s + b[s..]
                .iter()
                .take_while(|c| !c.is_ascii_whitespace() && !b"\"',;".contains(c))
                .count();
            if e > s && (i == 0 || !is_name_byte(b[i - 1])) {
                cuts.push((s, e));
            }
            from = s;
        }
    }
    let mut i = 0;
    while i < b.len() {
        if !is_name_byte(b[i]) || (i > 0 && is_name_byte(b[i - 1])) {
            i += 1;
            continue;
        }
        let end = i + b[i..].iter().take_while(|c| is_name_byte(**c)).count();
        let name = &lower[i..end];
        if SECRET_NAMES.iter().any(|s| name.ends_with(s))
            && let Some(v) = value_after(b, end)
        {
            cuts.push(v);
        }
        i = end;
    }
    if cuts.is_empty() {
        return Cow::Borrowed(text);
    }
    cuts.sort_unstable();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    let mut cut_before = false;
    for (s, e) in cuts {
        if s < at {
            at = at.max(e);
            continue;
        }
        let gap = &text[at..s];
        // `Authorization: Bearer x` is one secret, not two.
        if cut_before && gap.bytes().all(|c| c == b' ') {
            at = e;
            continue;
        }
        out.push_str(gap);
        out.push_str(REDACTED);
        at = e;
        cut_before = true;
    }
    out.push_str(&text[at..]);
    Cow::Owned(out)
}

/// The value span after a field name ending at `at`: skips a closing quote, spaces, then
/// `=` or `:`, spaces and an opening quote. `None` when no separator follows or the value is
/// empty.
fn value_after(b: &[u8], mut at: usize) -> Option<(usize, usize)> {
    if matches!(b.get(at), Some(b'"' | b'\'')) {
        at += 1;
    }
    while b.get(at) == Some(&b' ') {
        at += 1;
    }
    if !matches!(b.get(at), Some(b'=' | b':')) {
        return None;
    }
    at += 1;
    while b.get(at) == Some(&b' ') {
        at += 1;
    }
    let (start, end) = match b.get(at) {
        Some(&q @ (b'"' | b'\'')) => {
            let s = at + 1;
            let mut e = s;
            while e < b.len() && b[e] != q {
                e += if b[e] == b'\\' { 2 } else { 1 };
            }
            (s, e.min(b.len()))
        }
        _ => {
            let e = at
                + b[at..]
                    .iter()
                    .take_while(|c| !c.is_ascii_whitespace() && !b";,&)}]".contains(c))
                    .count();
            (at, e)
        }
    };
    (end > start).then_some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_string_passwords() {
        assert_eq!(
            scrub("Server=.;User Id=sa;Password=Pa$$w0rd!;Database=SMILE_FO"),
            "Server=.;User Id=sa;Password=[redacted];Database=SMILE_FO"
        );
        assert_eq!(
            scrub("server=db;PWD = 'p;w';uid=smile"),
            "server=db;PWD = '[redacted]';uid=smile"
        );
        // An escaped quote is part of the value.
        assert_eq!(
            scrub(r#"conn="Server=x;Password=\"q\";""#),
            r#"conn="Server=x;Password=[redacted];""#
        );
    }

    #[test]
    fn bearer_tokens_and_key_fields() {
        assert_eq!(
            scrub("Authorization: Bearer eyJhbGciOi.abc.def"),
            "Authorization: [redacted]"
        );
        assert_eq!(
            scrub("calling with bearer abc123 now"),
            "calling with bearer [redacted] now"
        );
        assert_eq!(
            scrub(r#"{"api_key":"k-123","password_sealed":"AQAAANCM","name":"x"}"#),
            r#"{"api_key":"[redacted]","password_sealed":"[redacted]","name":"x"}"#
        );
        assert_eq!(
            scrub("token=abc&user=bob x-api-key: zzz"),
            "token=[redacted]&user=bob x-api-key: [redacted]"
        );
    }

    #[test]
    fn plain_text_is_borrowed() {
        for t in [
            "heartbeat ok in 120 ms",
            "password reset requested",
            "tokens: 3",
            "",
        ] {
            assert!(matches!(scrub(t), Cow::Borrowed(_)), "{t}");
        }
    }
}
