//! `Convert.FromBase64String`: standard alphabet, padding required, ASCII whitespace
//! (space, tab, CR, LF) ignored anywhere and unused trailing bits ignored (`"QR=="` is
//! `[0x41]`), as .NET does. dl-encoding refuses trailing bits, so this small decoder keeps
//! the .NET acceptance exactly.

/// The decoded bytes, or `None` where .NET throws `FormatException`.
pub(super) fn decode_dotnet(input: &str) -> Option<Vec<u8>> {
    let chars: Vec<u8> = input
        .bytes()
        .filter(|b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
        .collect();
    if !chars.len().is_multiple_of(4) {
        return None;
    }
    let (quads, _) = chars.as_chunks::<4>();
    let mut out = Vec::with_capacity(quads.len() * 3);
    for (i, q) in quads.iter().enumerate() {
        let last = i + 1 == quads.len();
        let a = sextet(q[0])?;
        let b = sextet(q[1])?;
        match (q[2], q[3]) {
            (b'=', b'=') if last => out.push((a << 2) | (b >> 4)),
            (c, b'=') if last => {
                let c = sextet(c)?;
                out.push((a << 2) | (b >> 4));
                out.push((b << 4) | (c >> 2));
            }
            (c, d) => {
                let (c, d) = (sextet(c)?, sextet(d)?);
                out.push((a << 2) | (b >> 4));
                out.push((b << 4) | (c >> 2));
                out.push((c << 6) | d);
            }
        }
    }
    Some(out)
}

fn sextet(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cases checked against .NET 10 `Convert.FromBase64String`.
    #[test]
    fn matches_dotnet() {
        let ok: [(&str, &[u8]); 9] = [
            ("QR==", &[0x41]),
            ("QQ==", &[0x41]),
            ("Q Q = =", &[0x41]),
            ("QUJD", b"ABC"),
            ("QUJ=", b"AB"),
            ("QUI=", b"AB"),
            (" QUJD\r\n", b"ABC"),
            ("", &[]),
            ("QU\tJD", b"ABC"),
        ];
        for (s, bytes) in ok {
            assert_eq!(decode_dotnet(s).as_deref(), Some(bytes), "{s:?}");
        }
        for s in [
            "QQ=",
            "QQ",
            "Q===",
            "QQ==QQ==",
            "=",
            "QUJD====",
            "not base64!",
            "QQ-=",
        ] {
            assert_eq!(decode_dotnet(s), None, "{s:?}");
        }
        assert_eq!(decode_dotnet("G0BIZWxsbwo=").unwrap(), b"\x1b@Hello\n");
    }
}
