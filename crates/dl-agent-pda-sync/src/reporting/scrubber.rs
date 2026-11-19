//! The one place secrets are removed from text before it is written to a log or queued as
//! a site event (port of `Reporting/SecretScrubber.cs`): connection string passwords,
//! bearer tokens, token fields of JSON bodies, `--code` arguments, and every secret value
//! the process has registered. The C# regular expressions are matched by hand here with
//! the same leftmost, non-overlapping replacement semantics.

use std::sync::{Mutex, PoisonError};

/// What a secret is replaced with.
pub const MASK: &str = "***";

const MIN_LITERAL_LENGTH: usize = 4;

static LITERALS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Masks `secret` wherever it appears from now on in this process (values shorter than
/// four UTF-16 units, or only white space, are ignored).
pub fn register(secret: Option<&str>) {
    let Some(s) = secret else { return };
    if s.encode_utf16().count() < MIN_LITERAL_LENGTH || s.trim().is_empty() {
        return;
    }
    let mut l = LITERALS.lock().unwrap_or_else(PoisonError::into_inner);
    if !l.iter().any(|x| x == s) {
        l.push(s.to_owned());
    }
}

/// `\w` of .NET regular expressions (letters, digits, marks, connector punctuation).
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || ('\u{300}'..='\u{36f}').contains(&c)
}

/// Case-insensitive ASCII prefix match; returns the byte length matched.
fn starts_ci(s: &str, word: &str) -> bool {
    s.len() >= word.len() && s.as_bytes()[..word.len()].eq_ignore_ascii_case(word.as_bytes())
}

fn skip_ws(s: &str, mut i: usize) -> usize {
    while let Some(c) = s[i..].chars().next() {
        if !c.is_whitespace() {
            break;
        }
        i += c.len_utf8();
    }
    i
}

fn prev_is_word(s: &str, i: usize) -> bool {
    s[..i].chars().next_back().is_some_and(is_word)
}

/// Runs a matcher at every char boundary, left to right, replacing non-overlapping
/// matches. `try_at(s, i)` returns `(end, replacement)` for a match starting at `i`.
fn replace_all(s: &str, try_at: impl Fn(&str, usize) -> Option<(usize, String)>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut copied = 0;
    while i < s.len() {
        if let Some((end, rep)) = try_at(s, i) {
            out.push_str(&s[copied..i]);
            out.push_str(&rep);
            copied = end;
            i = if end > i { end } else { i + 1 };
            continue;
        }
        i += s[i..].chars().next().map_or(1, char::len_utf8);
    }
    out.push_str(&s[copied..]);
    out
}

/// `(?<key>\b(?:password|pwd))\s*=\s*(?:"[^"]*"|'[^']*'|[^;'"\s,]*)`.
fn connection_password(s: &str, i: usize) -> Option<(usize, String)> {
    if prev_is_word(s, i) {
        return None;
    }
    let rest = &s[i..];
    let key_len = if starts_ci(rest, "password") {
        8
    } else if starts_ci(rest, "pwd") {
        3
    } else {
        return None;
    };
    let key = &s[i..i + key_len];
    let mut j = skip_ws(s, i + key_len);
    if !s[j..].starts_with('=') {
        return None;
    }
    j = skip_ws(s, j + 1);
    let tail = &s[j..];
    let end = match tail.chars().next() {
        Some(q @ ('"' | '\'')) => match tail[1..].find(q) {
            Some(k) => j + 1 + k + 1,
            // The quoted alternatives fail; the bare one matches nothing.
            None => j,
        },
        _ => {
            let len: usize = tail
                .chars()
                .take_while(|c| !matches!(c, ';' | '\'' | '"' | ',') && !c.is_whitespace())
                .map(char::len_utf8)
                .sum();
            j + len
        }
    };
    Some((end, format!("{key}={MASK}")))
}

/// `(?<scheme>\bbearer)\s+[A-Za-z0-9\-._~+/]+=*`.
fn bearer(s: &str, i: usize) -> Option<(usize, String)> {
    if prev_is_word(s, i) || !starts_ci(&s[i..], "bearer") {
        return None;
    }
    let scheme = &s[i..i + 6];
    let j = skip_ws(s, i + 6);
    if j == i + 6 {
        return None;
    }
    let token = s[j..]
        .bytes()
        .take_while(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'+' | b'/')
        })
        .count();
    if token == 0 {
        return None;
    }
    let mut end = j + token;
    end += s[end..].bytes().take_while(|b| *b == b'=').count();
    Some((end, format!("{scheme} {MASK}")))
}

const JSON_SECRET_KEYS: [&str; 9] = [
    "agentToken",
    "agent_token",
    "accessToken",
    "access_token",
    "refreshToken",
    "refresh_token",
    "enrollmentCode",
    "enrollment_code",
    "password",
];

/// `(?<key>"(?:agentToken|…|password)"\s*:\s*)"[^"]*"`.
fn json_secret(s: &str, i: usize) -> Option<(usize, String)> {
    let rest = &s[i..];
    if !rest.starts_with('"') {
        return None;
    }
    for name in JSON_SECRET_KEYS {
        let after = &rest[1..];
        if !starts_ci(after, name) || !after[name.len()..].starts_with('"') {
            continue;
        }
        let mut j = skip_ws(s, i + 1 + name.len() + 1);
        if !s[j..].starts_with(':') {
            continue;
        }
        j = skip_ws(s, j + 1);
        if !s[j..].starts_with('"') {
            continue;
        }
        let Some(close) = s[j + 1..].find('"') else {
            continue;
        };
        let key = &s[i..j];
        return Some((j + 1 + close + 1, format!("{key}\"{MASK}\"")));
    }
    None
}

/// `(?<flag>--code)\s+(?!--)\S+`.
fn code_argument(s: &str, i: usize) -> Option<(usize, String)> {
    if !starts_ci(&s[i..], "--code") {
        return None;
    }
    let flag = &s[i..i + 6];
    let j = skip_ws(s, i + 6);
    if j == i + 6 || s[j..].starts_with("--") {
        return None;
    }
    let len: usize = s[j..]
        .chars()
        .take_while(|c| !c.is_whitespace())
        .map(char::len_utf8)
        .sum();
    if len == 0 {
        return None;
    }
    Some((j + len, format!("{flag} {MASK}")))
}

/// Removes every secret from `text` (empty for empty text).
pub fn scrub(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut s = replace_all(text, connection_password);
    s = replace_all(&s, bearer);
    s = replace_all(&s, json_secret);
    s = replace_all(&s, code_argument);
    let literals = LITERALS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    for literal in literals {
        if s.contains(&literal) {
            s = s.replace(&literal, MASK);
        }
    }
    s
}

/// [`scrub`] of an optional text (`Scrub(null)` is empty).
pub fn scrub_opt(text: Option<&str>) -> String {
    text.map(scrub).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_string_passwords_are_masked() {
        for (text, secret) in [
            (
                "Server=.;Database=POS;User Id=pda_sync;Password=Pa55word!;Encrypt=false",
                "Pa55word!",
            ),
            ("server=.;pwd = \"semi;colon\";", "semi;colon"),
            ("PASSWORD='quoted value'", "quoted value"),
        ] {
            let s = scrub(text);
            assert!(!s.contains(secret), "{s}");
            assert!(s.contains(MASK), "{s}");
        }
    }

    #[test]
    fn bearer_tokens_json_token_fields_and_code_arguments_are_masked() {
        let text = "Authorization: Bearer eyJhbGciOi.payload.sig== body {\"agentToken\":\"tok-123\",\"accessToken\": \"jwt-9\",\"code\":\"keep\"} args: enroll --code ABCD-EFGH --url https://x";
        let s = scrub(text);
        for secret in ["eyJhbGciOi", "tok-123", "jwt-9", "ABCD-EFGH"] {
            assert!(!s.contains(secret), "{s}");
        }
        assert!(
            s.contains("Bearer ***") && s.contains("--code ***") && s.contains("--url https://x"),
            "{s}"
        );
        assert!(s.contains("\"code\":\"keep\""), "{s}");
    }

    #[test]
    fn registered_values_are_masked_everywhere_and_short_values_are_ignored() {
        let secret = format!("reg-{}", dl_uuid::Uuid::new_v4().simple());
        register(Some(&secret));
        register(Some("abc"));
        register(None);
        assert_eq!(
            scrub(&format!("login failed for {secret} at abc")),
            "login failed for *** at abc"
        );
    }

    #[test]
    fn empty_text_stays_empty() {
        assert_eq!(scrub_opt(None), "");
        assert_eq!(scrub(""), "");
    }

    /// .NET 10 output (`sa-core` check, "SCRUB").
    #[test]
    fn edge_cases_match_dotnet() {
        assert_eq!(
            scrub(
                "Pwd = \"a;b\" password=x,y PASSWORD='q' bearer abc.def== \"refresh_token\" : \"r\" --code --url --CODE x"
            ),
            "Pwd=*** password=***,y PASSWORD=*** bearer *** \"refresh_token\" : \"***\" --code --url --CODE ***"
        );
        assert_eq!(
            scrub("mypassword=x pwd=\"open"),
            "mypassword=x pwd=***\"open"
        );
        assert_eq!(scrub("Bearerx y --codex z"), "Bearerx y --codex z");
    }
}
