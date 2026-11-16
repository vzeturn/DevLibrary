//! TLS record framing, used to hand the OS stack exactly one record at a time.

/// What the start of a buffer holds.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Frame {
    /// Not enough bytes for a whole record yet.
    Incomplete,
    /// A whole record of this many bytes (header included).
    Complete(usize),
    /// Not a TLS record header.
    Malformed,
}

/// Largest record body: 2^14 plus the expansion RFC 8446 / RFC 5246 allow.
const MAX_BODY: usize = (1 << 14) + 2048;

pub(crate) fn frame(buf: &[u8]) -> Frame {
    if buf.len() < 5 {
        return Frame::Incomplete;
    }
    if !(20..=24).contains(&buf[0]) || buf[1] != 3 {
        return Frame::Malformed;
    }
    let len = usize::from(u16::from_be_bytes([buf[3], buf[4]]));
    if len > MAX_BODY {
        return Frame::Malformed;
    }
    if buf.len() < 5 + len {
        Frame::Incomplete
    } else {
        Frame::Complete(5 + len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames() {
        assert_eq!(frame(b"\x16\x03\x01"), Frame::Incomplete);
        assert_eq!(frame(b"\x16\x03\x01\x00\x02\xaa"), Frame::Incomplete);
        assert_eq!(
            frame(b"\x16\x03\x01\x00\x02\xaa\xbb\xcc"),
            Frame::Complete(7)
        );
        assert_eq!(frame(b"\x17\x03\x03\x00\x00"), Frame::Complete(5));
        assert_eq!(frame(b"GET / HTTP/1.1"), Frame::Malformed);
        assert_eq!(frame(b"\x17\x03\x03\xff\xff"), Frame::Malformed);
    }
}
