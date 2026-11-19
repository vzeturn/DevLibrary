//! Masks secrets in check details: the self-test reports what it found, never a password,
//! token, connection-string credential or key.

/// Name endings that mark the value after `=` or `:` as secret (compared lowercased, with
/// `-`, `_`, `.` and spaces removed): `Password=`, `agent_token:`, `"apiKey":`, ...
const SECRET_NAMES: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "token",
    "tokens",
    "secret",
    "apikey",
    "accountkey",
    "privatekey",
    "tenantkey",
    "authorization",
    "cookie",
];

/// Shortest run of key-like characters masked as a blob.
const BLOB_MIN: usize = 32;

/// `text` with secrets masked as `***`, cut to `max` characters.
pub fn sanitize(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let masked = blobs(&userinfo(&bearer(&pairs(&chars))));
    clip(&masked, max)
}

fn clip(chars: &[char], max: usize) -> String {
    if chars.len() <= max {
        return chars.iter().collect();
    }
    let mut s: String = chars[..max.saturating_sub(1)].iter().collect();
    s.push('…');
    s
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.')
}

fn is_secret_name(word: &[char]) -> bool {
    let norm: String = word
        .iter()
        .filter(|c| !matches!(c, '_' | '-' | '.' | ' '))
        .flat_map(|c| c.to_lowercase())
        .collect();
    SECRET_NAMES.iter().any(|n| norm.ends_with(n))
}

fn ends_value(c: char) -> bool {
    c.is_whitespace() || matches!(c, ';' | '&' | ',' | '}' | ')' | ']' | '"' | '\'')
}

fn skip_spaces(c: &[char], mut i: usize) -> usize {
    while i < c.len() && c[i] == ' ' {
        i += 1;
    }
    i
}

/// Where a value that starts at `i` ends (at its closing quote when quoted).
fn value_end(c: &[char], mut i: usize, quote: Option<char>) -> usize {
    while i < c.len() && quote.map_or(!ends_value(c[i]), |q| c[i] != q) {
        i += 1;
    }
    i
}

/// `Authorization: Basic <credentials>`: the scheme word is followed by the secret.
fn is_auth_scheme(word: &[char]) -> bool {
    let w: String = word.iter().flat_map(|c| c.to_lowercase()).collect();
    matches!(
        w.as_str(),
        "bearer" | "basic" | "digest" | "negotiate" | "ntlm"
    )
}

/// `name=value`, `name: value` and `"name":"value"` with a secret name.
fn pairs(c: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(c.len());
    let mut i = 0;
    while i < c.len() {
        if !is_name_char(c[i]) {
            out.push(c[i]);
            i += 1;
            continue;
        }
        let start = i;
        while i < c.len() && is_name_char(c[i]) {
            i += 1;
        }
        out.extend_from_slice(&c[start..i]);
        if !is_secret_name(&c[start..i]) {
            continue;
        }
        let mut j = i;
        if j < c.len() && matches!(c[j], '"' | '\'') {
            j += 1;
        }
        j = skip_spaces(c, j);
        if j >= c.len() || !matches!(c[j], '=' | ':') {
            continue;
        }
        j = skip_spaces(c, j + 1);
        let quote = c.get(j).copied().filter(|q| matches!(q, '"' | '\''));
        let value_start = j + usize::from(quote.is_some());
        let mut k = value_end(c, value_start, quote);
        if quote.is_none() && is_auth_scheme(&c[value_start..k]) && c.get(k) == Some(&' ') {
            k = value_end(c, skip_spaces(c, k), None);
        }
        if k > value_start {
            out.extend_from_slice(&c[i..value_start]);
            out.extend("***".chars());
            i = k;
        }
    }
    out
}

/// `Bearer <token>` (any case).
fn bearer(c: &[char]) -> Vec<char> {
    const WORD: [char; 7] = ['b', 'e', 'a', 'r', 'e', 'r', ' '];
    let mut out = Vec::with_capacity(c.len());
    let mut i = 0;
    while i < c.len() {
        let hit = c.len() - i > WORD.len()
            && c[i..i + WORD.len()]
                .iter()
                .zip(WORD)
                .all(|(a, b)| a.eq_ignore_ascii_case(&b));
        if !hit {
            out.push(c[i]);
            i += 1;
            continue;
        }
        out.extend_from_slice(&c[i..i + WORD.len()]);
        let mut k = skip_spaces(c, i + WORD.len());
        let start = k;
        while k < c.len() && !c[k].is_whitespace() && !matches!(c[k], '"' | '\'' | ',') {
            k += 1;
        }
        if k > start {
            out.extend("***".chars());
        }
        i = k;
    }
    out
}

/// `scheme://user:pass@host` → `scheme://***@host`.
fn userinfo(c: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(c.len());
    let mut i = 0;
    while i < c.len() {
        if c[i..].starts_with(&[':', '/', '/']) {
            out.extend_from_slice(&c[i..i + 3]);
            let start = i + 3;
            let mut k = start;
            while k < c.len() && !matches!(c[k], '/' | '@' | '?' | '#') && !c[k].is_whitespace() {
                k += 1;
            }
            if k < c.len() && c[k] == '@' && k > start {
                out.extend("***".chars());
                i = k;
            } else {
                i = start;
            }
            continue;
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

fn is_blob_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '=' | '_' | '-')
}

/// Long runs of mixed-case letters and digits (keys, JWT parts, base64 secrets).
fn blobs(c: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(c.len());
    let mut i = 0;
    while i < c.len() {
        if !is_blob_char(c[i]) {
            out.push(c[i]);
            i += 1;
            continue;
        }
        let start = i;
        while i < c.len() && is_blob_char(c[i]) {
            i += 1;
        }
        let run = &c[start..i];
        let keylike = run.len() >= BLOB_MIN
            && run.iter().any(char::is_ascii_digit)
            && run.iter().any(char::is_ascii_uppercase)
            && run.iter().any(char::is_ascii_lowercase);
        if keylike {
            out.extend("***".chars());
        } else {
            out.extend_from_slice(run);
        }
    }
    out
}
