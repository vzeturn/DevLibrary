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

/// `^[a-z][a-zA-Z0-9]*(\.[a-zA-Z0-9]+)+@[1-9]\d{0,3}$` (one `hello.ops` entry,
/// `name@contract_ver`).
pub fn is_op_cap(s: &str) -> bool {
    let Some((name, ver)) = s.rsplit_once('@') else {
        return false;
    };
    let parts: Vec<&str> = name.split('.').collect();
    let first = parts[0].as_bytes();
    parts.len() >= 2
        && !first.is_empty()
        && first[0].is_ascii_lowercase()
        && parts
            .iter()
            .all(|p| !p.is_empty() && all(p, |b| b.is_ascii_alphanumeric()))
        && digits(ver, 1, 4)
        && !ver.starts_with('0')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        assert!(is_uuid("6f1c2b7e-8d4a-4c3b-9e2f-1a2b3c4d5e6F"));
        assert!(!is_uuid("6f1c2b7e-8d4a-4c3b-9e2f-1a2b3c4d5e6"));
        assert!(is_bare_name("GetHotelDate1"));
        assert!(!is_bare_name("dbo.x"));
        assert!(is_op_name("fo.room.list"));
        assert!(!is_op_name("fo.room"));
        assert!(!is_op_name("Fo.room.list"));
        assert!(is_op_cap("fo.hotelDate.get@1"));
        assert!(is_op_cap("fo.x@9999"));
        assert!(!is_op_cap("fo@1"));
        assert!(!is_op_cap("fo.x@0"));
        assert!(!is_op_cap("fo.x@01"));
        assert!(!is_op_cap("fo.x@10000"));
        assert!(!is_op_cap("Fo.x@1"));
        assert!(!is_op_cap("fo..x@1"));
        assert!(!is_op_cap("DROP TABLE x"));
        assert!(is_sig_b64(&format!("{}==", "A".repeat(86))));
    }
}
