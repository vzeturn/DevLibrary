//! Secret scrubbing for log lines: connection-string passwords, bearer tokens, URL
//! credentials, JWTs and key-like fields are replaced by `[redacted]` before a line is
//! written.

use std::borrow::Cow;

use crate::{Format, Record};

/// What replaces a secret.
pub const REDACTED: &str = "[redacted]";

/// Field names whose value is a secret wherever they appear (`name=value`, `name: value`,
/// `"name":"value"`); matched without regard to case, as the end of a longer name too
/// (`sql_password`, `x-api-key`, `tenant_key`, `DL_AGENT_ARRIVAL_TENANT_KEY`).
const SECRET_NAMES: [&str; 7] = [
    "password",
    "pwd",
    "secret",
    "token",
    "key",
    "authorization",
    "_sealed",
];

/// Names ending in `key` that hold no secret (an R2 object key, a public key); matched as the
/// end of the name. `key_id` and `kid` do not end in `key` and are never scrubbed.
const HARMLESS_NAMES: [&str; 7] = [
    "r2_key",
    "object_key",
    "public_key",
    "publickey",
    "pubkey",
    "sort_key",
    "cache_key",
];

/// Words that make a field name secret anywhere inside it (`password_hash`,
/// `client_secret_value`, `PasswordEnc`).
const SECRET_WORDS: [&str; 3] = ["password", "passwd", "secret"];

/// Words that make a field name secret as one of its `_` / `-` separated parts
/// (`token_value`, `refresh-token-enc`); `tokens` or `tokenizer` stay plain.
const SECRET_PARTS: [&str; 3] = ["token", "pwd", "apikey"];

/// Connection-string keywords whose unquoted value runs to the next `;` (ADO.NET rules).
const CONNECTION_PASSWORDS: [&str; 2] = ["password", "pwd"];

/// Whether `name` (lower case) names a secret.
fn is_secret_name(name: &str) -> bool {
    let secret = SECRET_NAMES.iter().any(|s| name.ends_with(s))
        || SECRET_WORDS.iter().any(|w| name.contains(w))
        || name
            .split(['_', '-'])
            .any(|part| SECRET_PARTS.contains(&part));
    secret && !HARMLESS_NAMES.iter().any(|h| name.ends_with(h))
}

fn is_b64url(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// Spans of JWT-looking strings: `eyJ…` header, `.`, payload, `.`, signature (base64url),
/// not inside a longer word.
fn jwts(b: &[u8], lower: &str, cuts: &mut Vec<(usize, usize)>) {
    let mut from = 0;
    while let Some(i) = lower[from..].find("eyj").map(|i| i + from) {
        from = i + 3;
        if b[i..].get(..3) != Some(b"eyJ".as_slice()) || (i > 0 && is_b64url(b[i - 1])) {
            continue;
        }
        let run = |at: usize| at + b[at..].iter().take_while(|c| is_b64url(**c)).count();
        let header = run(i);
        if header - i < 10 || b.get(header) != Some(&b'.') {
            continue;
        }
        let payload = run(header + 1);
        if payload - header < 3 || b.get(payload) != Some(&b'.') {
            continue;
        }
        let end = run(payload + 1);
        cuts.push((i, end));
        from = end;
    }
}

/// Spans of URL credentials: the password of `scheme://user:password@host`, or the whole
/// user part when it has no `:` (a token used as the user name).
fn url_userinfo(b: &[u8], text: &str, cuts: &mut Vec<(usize, usize)>) {
    let mut from = 0;
    while let Some(i) = text[from..].find("://").map(|i| i + from) {
        from = i + 3;
        let scheme = b[..i]
            .iter()
            .rev()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'.' | b'-'))
            .count();
        if scheme == 0 {
            continue;
        }
        let start = i + 3;
        let authority = b[start..]
            .iter()
            .take_while(|c| !c.is_ascii_whitespace() && !b"/?#\"'<>".contains(c))
            .count();
        let Some(at) = b[start..start + authority].iter().rposition(|c| *c == b'@') else {
            continue;
        };
        let user = &b[start..start + at];
        let s = user
            .iter()
            .position(|c| *c == b':')
            .map_or(start, |c| start + c + 1);
        if start + at > s {
            cuts.push((s, start + at));
        }
        from = start + at;
    }
}

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
/// name and `=` / `:` (quoted, or up to `;`, `,`, `&`, whitespace or a closing bracket), the
/// token after `Bearer` / `Basic`, URL credentials (`scheme://user:[redacted]@host`) and
/// JWT-looking strings (`eyJ….….…`). A connection-string `Password=` / `Pwd=` follows the
/// ADO.NET rules: `"…"` or `'…'` with the quote doubled inside, or unquoted up to the next
/// `;` (spaces and commas included). Borrowed when nothing was found.
pub fn scrub(text: &str) -> Cow<'_, str> {
    let lower = text.to_ascii_lowercase();
    let b = text.as_bytes();
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    jwts(b, &lower, &mut cuts);
    url_userinfo(b, text, &mut cuts);
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
        let conn = CONNECTION_PASSWORDS.iter().any(|p| name.ends_with(p));
        if is_secret_name(name)
            && let Some(v) = value_after(b, end, conn)
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
/// `=` or `:`, spaces and an opening quote. `conn` (a connection-string password after `=`)
/// takes an unquoted value up to `;`, a line end or an unescaped `"` (the end of a quoted
/// log field), trailing spaces left out. `None` when no separator follows or the value is
/// empty.
fn value_after(b: &[u8], mut at: usize, conn: bool) -> Option<(usize, usize)> {
    if matches!(b.get(at), Some(b'"' | b'\'')) {
        at += 1;
    }
    while b.get(at) == Some(&b' ') {
        at += 1;
    }
    let sep = *b.get(at)?;
    if !matches!(sep, b'=' | b':') {
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
            while e < b.len() {
                if b[e] == b'\\' {
                    e += 2;
                } else if b[e] == q && b.get(e + 1) == Some(&q) {
                    // ADO.NET doubles a quote inside a quoted value.
                    e += 2;
                } else if b[e] == q {
                    break;
                } else {
                    e += 1;
                }
            }
            (s, e.min(b.len()))
        }
        _ if conn && sep == b'=' => {
            let mut e = at;
            while e < b.len() && !matches!(b[e], b';' | b'\n' | b'\r') {
                if b[e] == b'"' && (e == at || b[e - 1] != b'\\') {
                    break;
                }
                e += 1;
            }
            while e > at && b[e - 1] == b' ' {
                e -= 1;
            }
            (at, e)
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
    fn ado_net_quoting() {
        assert_eq!(
            scrub(r#"Server=h;Password="a""b";Database=x"#),
            r#"Server=h;Password="[redacted]";Database=x"#
        );
        assert_eq!(
            scrub("Server=h;Pwd='it''s;x';Database=x"),
            "Server=h;Pwd='[redacted]';Database=x"
        );
        assert_eq!(
            scrub("Server=h;Pwd=a b,c;Database=x"),
            "Server=h;Pwd=[redacted];Database=x"
        );
        assert_eq!(scrub("Pwd=a b,c"), "Pwd=[redacted]");
        assert_eq!(scrub("Password=a b \nnext"), "Password=[redacted] \nnext");
        // Inside a quoted log field the field's closing quote ends the value.
        assert_eq!(
            scrub(r#"conn="Server=h;Password=a b,c" db=x"#),
            r#"conn="Server=h;Password=[redacted]" db=x"#
        );
        assert_eq!(
            scrub(r#"{"conn":"Server=h;Password=a b","name":"x"}"#),
            r#"{"conn":"Server=h;Password=[redacted]","name":"x"}"#
        );
        assert_eq!(
            scrub(r#"Password="""a";x=1"#),
            r#"Password="[redacted]";x=1"#
        );
        // An empty quoted value is no secret; the next field stays.
        assert_eq!(
            scrub(r#"{"password":"","name":"x"}"#),
            r#"{"password":"","name":"x"}"#
        );
    }

    #[test]
    fn names_ending_in_key_except_harmless_ones() {
        assert_eq!(
            scrub(r#"{"tenant_key":"tk-1","hmac_key":"h","key":"k","r2_key":"diag/a.zip"}"#),
            r#"{"tenant_key":"[redacted]","hmac_key":"[redacted]","key":"[redacted]","r2_key":"diag/a.zip"}"#
        );
        assert_eq!(
            scrub("DL_AGENT_ARRIVAL_TENANT_KEY=abc key=xyz key_id=7 kid=k1"),
            "DL_AGENT_ARRIVAL_TENANT_KEY=[redacted] key=[redacted] key_id=7 kid=k1"
        );
        assert_eq!(
            scrub(r#"{"public_key":"MCow","object_key":"o","private_key":"p"}"#),
            r#"{"public_key":"MCow","object_key":"o","private_key":"[redacted]"}"#
        );
    }

    /// Every secret shape the diagnostics bundle must never carry: `(input, expected)`.
    #[test]
    fn secret_shapes_table() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJhZ3QifQ.c2lnbmF0dXJl";
        let cases: Vec<(String, String)> = vec![
            // Connection-string passwords, any case and quoting.
            ("PASSWORD=Hunter2;".into(), "PASSWORD=[redacted];".into()),
            ("pAsSwOrD = Hunter2 ;x=1".into(), "pAsSwOrD = [redacted] ;x=1".into()),
            ("PWD='Hun''ter2';".into(), "PWD='[redacted]';".into()),
            (r#"pwd="Hun;ter2";"#.into(), r#"pwd="[redacted]";"#.into()),
            ("Data Source=x;Pwd=Hunter2".into(), "Data Source=x;Pwd=[redacted]".into()),
            // URL credentials.
            (
                "https://sa:Hunter2@db.example:1433/x".into(),
                "https://sa:[redacted]@db.example:1433/x".into(),
            ),
            (
                "proxy http://u:p%40ss@proxy:3128 up".into(),
                "proxy http://u:[redacted]@proxy:3128 up".into(),
            ),
            (
                "git https://ghp_tok@github.com/x".into(),
                "git https://[redacted]@github.com/x".into(),
            ),
            ("https://agent.pms.id.vn/v1".into(), "https://agent.pms.id.vn/v1".into()),
            // JWT-looking strings, bare or quoted.
            (format!("got {jwt} back"), "got [redacted] back".into()),
            (format!(r#"["{jwt}"]"#), r#"["[redacted]"]"#.into()),
            // Field names holding a secret word.
            (
                r#"{"client_secret_value":"s1","password_hash":"h","refresh_token_enc":"t"}"#.into(),
                r#"{"client_secret_value":"[redacted]","password_hash":"[redacted]","refresh_token_enc":"[redacted]"}"#.into(),
            ),
            (
                "SecretKey=a PasswordEnc: b sql-pwd-old=c".into(),
                "SecretKey=[redacted] PasswordEnc: [redacted] sql-pwd-old=[redacted]".into(),
            ),
            ("x-api-key: k1 apikey_v2=k2".into(), "x-api-key: [redacted] apikey_v2=[redacted]".into()),
        ];
        for (input, want) in cases {
            assert_eq!(scrub(&input), want, "{input}");
        }
    }

    #[test]
    fn plain_text_is_borrowed() {
        for t in [
            "heartbeat ok in 120 ms",
            "password reset requested",
            "tokens: 3",
            "key_id=7 kid=k1 r2_key=diag/x.zip",
            "tokenizer=ws tokens=3",
            "see https://agent.pms.id.vn/install.ps1 and user@example.com",
            "eyJ is not a token",
            "",
        ] {
            assert!(matches!(scrub(t), Cow::Borrowed(_)), "{t}");
        }
    }
}
