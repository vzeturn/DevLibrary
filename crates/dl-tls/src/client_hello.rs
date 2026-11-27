//! Reads the TLS ClientHello a server receives first (RFC 8446 §4.1.2; TLS 1.0-1.2 hellos
//! have the same layout): the server name (RFC 6066 §3) and the ALPN list (RFC 7301 §3.1).
//! The TLS stack parses the hello again and enforces the protocol; this module only looks.

/// What the client offered.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Offer {
    /// The `host_name` of the server_name extension, when it is printable ASCII.
    pub(crate) server_name: Option<String>,
    /// The ALPN `ProtocolNameList` (length-prefixed names), when present and well formed.
    pub(crate) alpn: Option<Vec<u8>>,
}

/// The state of the bytes received so far.
#[derive(Debug)]
pub(crate) enum Hello {
    /// More bytes are needed.
    Incomplete,
    /// The whole ClientHello was seen, or the bytes cannot start one (`None`).
    Done(Option<Offer>),
}

const HANDSHAKE: u8 = 22;
const CLIENT_HELLO: u8 = 1;
const SERVER_NAME: u16 = 0;
const HOST_NAME: u8 = 0;
const ALPN: u16 = 16;
/// Largest record body before encryption, 2^14 (RFC 8446 §5.1).
const MAX_FRAGMENT: usize = 1 << 14;
/// Largest ClientHello looked at; bigger ones are left to the TLS stack alone.
const MAX_HELLO: usize = 1 << 16;

/// Looks at the start of what a client sent: handshake records (RFC 8446 §5.1) whose
/// fragments join into the ClientHello message (§4).
pub(crate) fn parse(buf: &[u8]) -> Hello {
    let mut msg = Vec::new();
    let mut rest = buf;
    loop {
        if let [kind, a, b, c, ..] = msg[..] {
            let len = usize::from(a) << 16 | usize::from(b) << 8 | usize::from(c);
            if kind != CLIENT_HELLO || len > MAX_HELLO {
                return Hello::Done(None);
            }
            if let Some(body) = msg.get(4..4 + len) {
                return Hello::Done(offer(body));
            }
        }
        // Give up as soon as the bytes cannot be a handshake record.
        if rest.first().is_some_and(|&t| t != HANDSHAKE) || rest.get(1).is_some_and(|&v| v != 3) {
            return Hello::Done(None);
        }
        let Some(header) = rest.get(..5) else {
            return Hello::Incomplete;
        };
        let len = usize::from(u16::from_be_bytes([header[3], header[4]]));
        if len == 0 || len > MAX_FRAGMENT {
            return Hello::Done(None);
        }
        let Some(fragment) = rest.get(5..5 + len) else {
            return Hello::Incomplete;
        };
        msg.extend_from_slice(fragment);
        rest = &rest[5 + len..];
    }
}

/// A cursor over TLS presentation-language vectors (RFC 8446 §3).
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, tail) = self.0.split_at_checked(n)?;
        self.0 = tail;
        Some(head)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        let b = self.take(2)?;
        Some(u16::from_be_bytes([b[0], b[1]]))
    }

    fn vec8(&mut self) -> Option<&'a [u8]> {
        let n = self.u8()?;
        self.take(usize::from(n))
    }

    fn vec16(&mut self) -> Option<&'a [u8]> {
        let n = self.u16()?;
        self.take(usize::from(n))
    }
}

/// The ClientHello body: version, random, session id, cipher suites, compression methods,
/// then the extensions, which may be absent before TLS 1.3 (RFC 5246 §7.4.1.2).
fn offer(body: &[u8]) -> Option<Offer> {
    let mut r = Reader(body);
    r.take(2 + 32)?;
    r.vec8()?;
    r.vec16()?;
    r.vec8()?;
    let mut offer = Offer {
        server_name: None,
        alpn: None,
    };
    if r.0.is_empty() {
        return Some(offer);
    }
    let mut extensions = Reader(r.vec16()?);
    let (mut seen_name, mut seen_alpn) = (false, false);
    while !extensions.0.is_empty() {
        let kind = extensions.u16()?;
        let data = extensions.vec16()?;
        // Each extension appears at most once (RFC 8446 §4.2); later copies are ignored.
        if kind == SERVER_NAME && !seen_name {
            seen_name = true;
            offer.server_name = host_name(data);
        } else if kind == ALPN && !seen_alpn {
            seen_alpn = true;
            offer.alpn = protocol_names(data);
        }
    }
    Some(offer)
}

/// The first `host_name` of a `ServerNameList` (RFC 6066 §3), when it is printable ASCII.
fn host_name(data: &[u8]) -> Option<String> {
    let mut r = Reader(data);
    let mut list = Reader(r.vec16()?);
    while !list.0.is_empty() {
        let kind = list.u8()?;
        let name = list.vec16()?;
        if kind == HOST_NAME {
            if name.is_empty() || !name.iter().all(|b| (0x21..=0x7e).contains(b)) {
                return None;
            }
            return String::from_utf8(name.to_vec()).ok();
        }
    }
    None
}

/// The body of a well-formed, non-empty `ProtocolNameList` (RFC 7301 §3.1).
fn protocol_names(data: &[u8]) -> Option<Vec<u8>> {
    let mut r = Reader(data);
    let list = r.vec16()?;
    if !r.0.is_empty() || list.is_empty() {
        return None;
    }
    let mut names = Reader(list);
    while !names.0.is_empty() {
        if names.vec8()?.is_empty() {
            return None;
        }
    }
    Some(list.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The complete ClientHello record of RFC 8448 §3 ("Simple 1-RTT Handshake"), which
    /// names the server "server" and offers no ALPN.
    const RFC8448: [u8; 201] = [
        0x16, 0x03, 0x01, 0x00, 0xc4, 0x01, 0x00, 0x00, 0xc0, 0x03, 0x03, 0xcb, 0x34, 0xec, 0xb1,
        0xe7, 0x81, 0x63, 0xba, 0x1c, 0x38, 0xc6, 0xda, 0xcb, 0x19, 0x6a, 0x6d, 0xff, 0xa2, 0x1a,
        0x8d, 0x99, 0x12, 0xec, 0x18, 0xa2, 0xef, 0x62, 0x83, 0x02, 0x4d, 0xec, 0xe7, 0x00, 0x00,
        0x06, 0x13, 0x01, 0x13, 0x03, 0x13, 0x02, 0x01, 0x00, 0x00, 0x91, 0x00, 0x00, 0x00, 0x0b,
        0x00, 0x09, 0x00, 0x00, 0x06, 0x73, 0x65, 0x72, 0x76, 0x65, 0x72, 0xff, 0x01, 0x00, 0x01,
        0x00, 0x00, 0x0a, 0x00, 0x14, 0x00, 0x12, 0x00, 0x1d, 0x00, 0x17, 0x00, 0x18, 0x00, 0x19,
        0x01, 0x00, 0x01, 0x01, 0x01, 0x02, 0x01, 0x03, 0x01, 0x04, 0x00, 0x23, 0x00, 0x00, 0x00,
        0x33, 0x00, 0x26, 0x00, 0x24, 0x00, 0x1d, 0x00, 0x20, 0x99, 0x38, 0x1d, 0xe5, 0x60, 0xe4,
        0xbd, 0x43, 0xd2, 0x3d, 0x8e, 0x43, 0x5a, 0x7d, 0xba, 0xfe, 0xb3, 0xc0, 0x6e, 0x51, 0xc1,
        0x3c, 0xae, 0x4d, 0x54, 0x13, 0x69, 0x1e, 0x52, 0x9a, 0xaf, 0x2c, 0x00, 0x2b, 0x00, 0x03,
        0x02, 0x03, 0x04, 0x00, 0x0d, 0x00, 0x20, 0x00, 0x1e, 0x04, 0x03, 0x05, 0x03, 0x06, 0x03,
        0x02, 0x03, 0x08, 0x04, 0x08, 0x05, 0x08, 0x06, 0x04, 0x01, 0x05, 0x01, 0x06, 0x01, 0x02,
        0x01, 0x04, 0x02, 0x05, 0x02, 0x06, 0x02, 0x02, 0x02, 0x00, 0x2d, 0x00, 0x02, 0x01, 0x01,
        0x00, 0x1c, 0x00, 0x02, 0x40, 0x01,
    ];

    fn done(buf: &[u8]) -> Option<Offer> {
        match parse(buf) {
            Hello::Done(offer) => offer,
            Hello::Incomplete => panic!("incomplete"),
        }
    }

    /// Splits a record's handshake message into records of at most `max` bytes
    /// (RFC 8446 §5.1 lets a handshake message span records).
    fn fragment(record: &[u8], max: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in record[5..].chunks(max) {
            out.extend_from_slice(&[0x16, 0x03, 0x03]);
            out.extend_from_slice(&(chunk.len() as u16).to_be_bytes());
            out.extend_from_slice(chunk);
        }
        out
    }

    /// The RFC 8448 hello with `ext` (type, body) appended to its extensions, lengths fixed
    /// per RFC 8446 §4.1.2.
    fn with_extension(ext_type: u16, body: &[u8]) -> Vec<u8> {
        let mut out = RFC8448.to_vec();
        out.extend_from_slice(&ext_type.to_be_bytes());
        out.extend_from_slice(&(body.len() as u16).to_be_bytes());
        out.extend_from_slice(body);
        let add = 4 + body.len();
        let bump16 = |b: &mut [u8], at: usize| {
            let v = u16::from_be_bytes([b[at], b[at + 1]]) as usize + add;
            b[at..at + 2].copy_from_slice(&(v as u16).to_be_bytes());
        };
        bump16(&mut out, 3); // record length
        bump16(&mut out, 7); // low 16 bits of the 24-bit handshake length
        bump16(&mut out, 0x36); // extensions length (0x0091 in RFC 8448)
        out
    }

    #[test]
    fn reads_rfc8448_server_name() {
        let o = done(&RFC8448).expect("well formed");
        assert_eq!(o.server_name.as_deref(), Some("server"));
        assert_eq!(o.alpn, None);
        // Bytes after the hello (more records) do not matter.
        let mut more = RFC8448.to_vec();
        more.extend_from_slice(b"\x17\x03\x03\x00\x01x");
        assert_eq!(done(&more).unwrap().server_name.as_deref(), Some("server"));
    }

    #[test]
    fn waits_for_the_whole_hello() {
        for n in 0..RFC8448.len() {
            assert!(
                matches!(parse(&RFC8448[..n]), Hello::Incomplete),
                "prefix of {n} bytes"
            );
        }
    }

    #[test]
    fn joins_a_hello_split_over_records() {
        for max in [1, 3, 50, 195] {
            let split = fragment(&RFC8448, max);
            assert_eq!(
                done(&split).unwrap().server_name.as_deref(),
                Some("server"),
                "fragments of {max}"
            );
            assert!(matches!(
                parse(&split[..split.len() - 1]),
                Hello::Incomplete
            ));
        }
    }

    #[test]
    fn reads_alpn() {
        // ProtocolNameList (RFC 7301 §3.1): u16 length, then length-prefixed names.
        let hello = with_extension(16, b"\x00\x0c\x02h2\x08http/1.1");
        let o = done(&hello).unwrap();
        assert_eq!(o.server_name.as_deref(), Some("server"));
        assert_eq!(o.alpn.as_deref(), Some(&b"\x02h2\x08http/1.1"[..]));
        // A malformed list is ignored (the TLS stack rejects it).
        let bad = with_extension(16, b"\x00\x0c\x02h2\x09http/1.1");
        assert_eq!(done(&bad).unwrap().alpn, None);
        let empty_name = with_extension(16, b"\x00\x01\x00");
        assert_eq!(done(&empty_name).unwrap().alpn, None);
    }

    #[test]
    fn ignores_unusable_server_names() {
        // Rewrite "server" (offset 0x41) into bytes that are not a printable ASCII host name.
        for bad in [*b"serv\0r", *b"serv r", *b"serv\xc3\xa9"] {
            let mut h = RFC8448.to_vec();
            h[0x41..0x47].copy_from_slice(&bad);
            let o = done(&h).unwrap();
            assert_eq!(o.server_name, None, "{bad:?}");
        }
    }

    #[test]
    fn non_tls_input_is_done_at_once() {
        assert!(done(b"GET / HTTP/1.1\r\n").is_none());
        assert!(done(b"G").is_none());
        assert!(done(b"\x16\x02").is_none());
        // An application-data record first.
        assert!(done(b"\x17\x03\x03\x00\x01x").is_none());
        // A handshake message that is not a ClientHello (ServerHello = 2).
        let mut h = RFC8448;
        h[5] = 2;
        assert!(done(&h).is_none());
        // Zero-length and oversized records (RFC 8446 §5.1).
        assert!(done(b"\x16\x03\x01\x00\x00").is_none());
        assert!(done(b"\x16\x03\x01\x40\x01").is_none());
    }

    #[test]
    fn hello_without_extensions_has_no_offer_details() {
        // legacy_version, random, empty session id, one suite, null compression, nothing else
        // (allowed before TLS 1.3, RFC 5246 §7.4.1.2).
        let mut body = vec![0x03, 0x03];
        body.extend_from_slice(&[0x11; 32]);
        body.extend_from_slice(&[0x00, 0x00, 0x02, 0x00, 0x2f, 0x01, 0x00]);
        let mut rec = vec![
            0x16,
            0x03,
            0x01,
            0x00,
            (body.len() + 4) as u8,
            0x01,
            0x00,
            0x00,
        ];
        rec.push(body.len() as u8);
        rec.extend_from_slice(&body);
        let o = done(&rec).unwrap();
        assert_eq!(o.server_name, None);
        assert_eq!(o.alpn, None);
    }

    #[test]
    fn corrupted_hellos_never_panic() {
        for i in 0..RFC8448.len() {
            for flip in [0x01, 0x80, 0xff] {
                let mut h = RFC8448;
                h[i] ^= flip;
                let _ = parse(&h);
            }
        }
        // A handshake length far beyond the cap ends the scan.
        let mut h = RFC8448;
        h[6] = 0xff;
        assert!(done(&h).is_none());
    }
}
