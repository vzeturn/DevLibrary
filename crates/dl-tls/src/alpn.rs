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
}
