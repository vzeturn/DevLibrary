//! The fixed patterns the Go agent checks with `regexp`, written out by hand. Each
//! function names its Go expression; like Go's `^...$` they match the whole text (no
//! trailing newline allowed).

fn all(s: &str, f: impl Fn(u8) -> bool) -> bool {
    s.bytes().all(f)
}

fn digits(s: &str, lo: usize, hi: usize) -> bool {
    (lo..=hi).contains(&s.len()) && all(s, |b| b.is_ascii_digit())
}

fn lower_hex(b: u8) -> bool {
    b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
}

/// `^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$`
pub fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, &c)| match i {
            8 | 13 | 18 | 23 => c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// `^[0-9a-f]{64}$`
pub fn is_hex64(s: &str) -> bool {
    s.len() == 64 && all(s, lower_hex)
}

/// `^[A-Za-z0-9+/]{86}==$` (a base64 Ed25519 signature).
pub fn is_sig_b64(s: &str) -> bool {
    s.len() == 88
        && s.ends_with("==")
        && all(&s[..86], |b| {
            b.is_ascii_alphanumeric() || b == b'+' || b == b'/'
        })
}

/// `^pms-[0-9]{1,9}-[0-9]{1,9}-[0-9a-f]{16}$`
pub fn is_bundle_id(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("pms-") else {
        return false;
    };
    let mut parts = rest.splitn(3, '-');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(a), Some(b), Some(h)) => {
            digits(a, 1, 9) && digits(b, 1, 9) && h.len() == 16 && all(h, lower_hex)
        }
        _ => false,
    }
}

/// `^([0-9]{3,6})_[A-Za-z0-9][A-Za-z0-9_-]*\.sql$`; returns the leading number's text.
pub fn migration_file(s: &str) -> Option<&str> {
    let stem = s.strip_suffix(".sql")?;
    let us = stem.find('_')?;
    let (num, rest) = (&stem[..us], &stem[us + 1..]);
    if !digits(num, 3, 6) {
        return None;
    }
    let mut it = rest.bytes();
    let first = it.next()?;
    if !first.is_ascii_alphanumeric() {
        return None;
    }
    it.all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        .then_some(num)
}

/// `^[A-Z][A-Z0-9_]{1,63}$`
pub fn is_migrate_code(s: &str) -> bool {
    let b = s.as_bytes();
    (2..=64).contains(&b.len())
        && b[0].is_ascii_uppercase()
        && b[1..]
            .iter()
            .all(|&c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
}

/// `^[a-z0-9][a-z0-9._-]{0,63}$` (a bundle signing key id).
pub fn is_key_id(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=64).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b[1..].iter().all(|&c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'.' || c == b'_' || c == b'-'
        })
}

/// `^\d{1,4}\.\d{1,4}\.\d{1,4}$`
pub fn is_short_semver(s: &str) -> bool {
    let p: Vec<&str> = s.split('.').collect();
    p.len() == 3 && p.iter().all(|x| digits(x, 1, 4))
}

/// `^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$`
pub fn is_utc_second(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 20
        && b.iter().enumerate().all(|(i, &c)| match i {
            4 | 7 => c == b'-',
            10 => c == b'T',
            13 | 16 => c == b':',
            19 => c == b'Z',
            _ => c.is_ascii_digit(),
        })
}

/// `^[A-Za-z_][A-Za-z0-9_]*$`
pub fn is_bare_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b[1..]
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || c == b'_')
}

/// `^[a-z][a-zA-Z0-9]*(\.[a-z][a-zA-Z0-9]*){2,}$` (`<domain>.<area>.<verb>`).
pub fn is_op_name(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() >= 3
        && parts.iter().all(|p| {
            let b = p.as_bytes();
            !b.is_empty() && b[0].is_ascii_lowercase() && b.iter().all(u8::is_ascii_alphanumeric)
        })
}

/// `^(\d{1,6})\.(\d{1,6})\.(\d{1,6})$` with an optional `(?:[-+][0-9A-Za-z.-]{1,64})?`
/// suffix when `suffix` is true; returns the three numbers.
pub fn version3(s: &str, suffix: bool) -> Option<[u32; 3]> {
    let mut core = s;
    if suffix && let Some(i) = s.find(['-', '+']) {
        let tail = &s[i + 1..];
        if !(1..=64).contains(&tail.len())
            || !all(tail, |b| {
                b.is_ascii_alphanumeric() || b == b'.' || b == b'-'
            })
        {
            return None;
        }
        core = &s[..i];
    }
    let p: Vec<&str> = core.split('.').collect();
    if p.len() != 3 || !p.iter().all(|x| digits(x, 1, 6)) {
        return None;
    }
    Some([p[0].parse().ok()?, p[1].parse().ok()?, p[2].parse().ok()?])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        assert!(is_uuid("6f1c2b7e-8d4a-4c3b-9e2f-1a2b3c4d5e6F"));
        assert!(!is_uuid("6f1c2b7e-8d4a-4c3b-9e2f-1a2b3c4d5e6"));
        assert!(is_bundle_id("pms-0-5-0123456789abcdef"));
        assert!(!is_bundle_id("pms-0-5-0123456789abcdeF"));
        assert!(!is_bundle_id("pms--5-0123456789abcdef"));
        assert_eq!(migration_file("001_fixture_table.sql"), Some("001"));
        assert_eq!(migration_file("003_fixture-index_stats.sql"), Some("003"));
        assert_eq!(migration_file("01_x.sql"), None);
        assert_eq!(migration_file("001__x.sql"), None);
        assert_eq!(migration_file("001_x.sql\n"), None);
        assert_eq!(migration_file("001_x;drop.sql"), None);
        assert!(is_migrate_code("BUNDLE_HASH"));
        assert!(!is_migrate_code("B"));
        assert!(!is_migrate_code("bundle"));
        assert!(is_key_id("dev-2026.11"));
        assert!(!is_key_id("Dev"));
        assert!(is_short_semver("1.2.30"));
        assert!(!is_short_semver("1.2"));
        assert!(is_utc_second("2026-11-04T10:00:00Z"));
        assert!(is_bare_name("MM_Op_GetLog"));
        assert!(!is_bare_name("dbo.x"));
        assert!(is_op_name("fo.room.list"));
        assert!(!is_op_name("fo.room"));
        assert!(!is_op_name("Fo.room.list"));
        assert_eq!(version3("0.1.0-dev", true), Some([0, 1, 0]));
        assert_eq!(version3("0.1.0-dev", false), None);
        assert_eq!(version3("v1.0.0", true), None);
        assert!(is_sig_b64(&format!("{}==", "A".repeat(86))));
    }
}
