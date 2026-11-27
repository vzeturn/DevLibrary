//! ALPN wire encoding (RFC 7301).

use std::io;

/// The `ProtocolNameList` body: each name prefixed by its length byte.
pub(crate) fn encode_wire(protocols: &[Vec<u8>]) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    for p in protocols {
        let len = u8::try_from(p.len())
            .ok()
            .filter(|&l| l > 0)
            .ok_or_else(|| invalid("ALPN protocol names must be 1 to 255 bytes"))?;
        out.push(len);
        out.extend_from_slice(p);
    }
    if out.len() > 65_533 {
        return Err(invalid("ALPN protocol list is too long"));
    }
    Ok(out)
}

/// The `SEC_APPLICATION_PROTOCOLS` buffer SChannel expects: u32 total length of what
/// follows, u32 negotiation extension (2 = ALPN), u16 list length, then the list.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn schannel_buffer(protocols: &[Vec<u8>]) -> io::Result<Vec<u8>> {
    let list = encode_wire(protocols)?;
    let mut out = Vec::with_capacity(list.len() + 10);
    out.extend_from_slice(&((list.len() + 6) as u32).to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(list.len() as u16).to_le_bytes());
    out.extend_from_slice(&list);
    Ok(out)
}

/// The server's choice (RFC 7301 §3.2): its own most preferred protocol that the client's
/// `ProtocolNameList` body `client` also names. A malformed list matches nothing past the
/// point where it breaks.
pub(crate) fn select<'a>(server: &[Vec<u8>], client: &'a [u8]) -> Option<&'a [u8]> {
    server.iter().find_map(|wanted| {
        let mut rest = client;
        while let Some((&len, tail)) = rest.split_first() {
            let (name, after) = tail.split_at_checked(usize::from(len))?;
            if name.is_empty() {
                return None;
            }
            if name == wanted.as_slice() {
                return Some(name);
            }
            rest = after;
        }
        None
    })
}

pub(crate) fn invalid(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes() {
        let p = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        assert_eq!(encode_wire(&p).unwrap(), b"\x02h2\x08http/1.1");
        let b = schannel_buffer(&p).unwrap();
        assert_eq!(&b[..4], &(12u32 + 6).to_le_bytes());
        assert_eq!(&b[4..8], &2u32.to_le_bytes());
        assert_eq!(&b[8..10], &12u16.to_le_bytes());
        assert_eq!(&b[10..], b"\x02h2\x08http/1.1");
        assert!(encode_wire(&[Vec::new()]).is_err());
        assert!(encode_wire(&[vec![b'a'; 256]]).is_err());
        assert!(encode_wire(&[]).unwrap().is_empty());
    }

    #[test]
    fn server_preference_wins() {
        let client = b"\x02h2\x08http/1.1";
        let only_http = [b"http/1.1".to_vec()];
        assert_eq!(select(&only_http, client), Some(&b"http/1.1"[..]));
        let both = [b"http/1.1".to_vec(), b"h2".to_vec()];
        assert_eq!(select(&both, client), Some(&b"http/1.1"[..]));
        let h2_first = [b"h2".to_vec(), b"http/1.1".to_vec()];
        assert_eq!(select(&h2_first, client), Some(&b"h2"[..]));
        // The match is a slice of the client's list.
        let got = select(&h2_first, client).unwrap();
        assert_eq!(got.as_ptr(), client[1..].as_ptr());
    }

    #[test]
    fn no_overlap_or_malformed_lists_select_nothing() {
        let server = [b"http/1.1".to_vec()];
        assert_eq!(select(&server, b"\x02h2"), None);
        assert_eq!(select(&server, b""), None);
        assert_eq!(select(&[], b"\x08http/1.1"), None);
        // Prefix "http/1." is not "http/1.1"; lengths that run past the end match nothing.
        assert_eq!(select(&server, b"\x07http/1."), None);
        assert_eq!(select(&server, b"\x09http/1.1"), None);
        assert_eq!(select(&server, b"\x02h2\xffhttp/1.1"), None);
        // A zero-length entry is malformed (RFC 7301 §3.1); nothing after it counts.
        assert_eq!(select(&server, b"\x00\x08http/1.1"), None);
    }
}
