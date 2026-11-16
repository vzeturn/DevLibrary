//! Bounds-checked little-endian reader over an untrusted byte slice, plus UTF-16 helpers.

use crate::Error;

/// Reads primitive TDS values from a byte slice. Every read checks the remaining length
/// and fails with [`Error::Malformed`] carrying the current token type and offset.
pub(crate) struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
    /// Offset of `buf[0]` within the whole message, for error reporting.
    base: usize,
    /// Token type currently being parsed, for error reporting.
    pub(crate) token: u8,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Cursor<'a> {
        Cursor {
            buf,
            pos: 0,
            base: 0,
            token: 0,
        }
    }

    /// Position within the message (for errors).
    pub(crate) fn offset(&self) -> usize {
        self.base + self.pos
    }

    pub(crate) fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    pub(crate) fn err(&self, what: &'static str) -> Error {
        Error::Malformed {
            token: self.token,
            offset: self.offset(),
            what,
        }
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if n > self.remaining() {
            return Err(self.err("unexpected end of data"));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    /// A sub-cursor over the next `n` bytes; the parent skips them.
    pub(crate) fn sub(&mut self, n: usize) -> Result<Cursor<'a>, Error> {
        let base = self.offset();
        let token = self.token;
        let buf = self.take(n)?;
        Ok(Cursor {
            buf,
            pos: 0,
            base,
            token,
        })
    }

    pub(crate) fn skip(&mut self, n: usize) -> Result<(), Error> {
        self.take(n).map(|_| ())
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let s = self.take(N)?;
        let mut a = [0u8; N];
        a.copy_from_slice(s);
        Ok(a)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.array::<1>()?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16, Error> {
        self.array().map(u16::from_le_bytes)
    }

    pub(crate) fn u32(&mut self) -> Result<u32, Error> {
        self.array().map(u32::from_le_bytes)
    }

    pub(crate) fn i32(&mut self) -> Result<i32, Error> {
        self.array().map(i32::from_le_bytes)
    }

    pub(crate) fn u64(&mut self) -> Result<u64, Error> {
        self.array().map(u64::from_le_bytes)
    }

    /// B_VARCHAR: a byte character count followed by UTF-16LE.
    pub(crate) fn b_varchar(&mut self) -> Result<String, Error> {
        let n = self.u8()? as usize;
        let at = self.offset();
        let bytes = self.take(n * 2)?;
        utf16le(bytes).map_err(|_| self.err_at(at, "invalid UTF-16 length"))
    }

    /// US_VARCHAR: a u16 character count followed by UTF-16LE.
    pub(crate) fn us_varchar(&mut self) -> Result<String, Error> {
        let n = self.u16()? as usize;
        let at = self.offset();
        let bytes = self.take(n * 2)?;
        utf16le(bytes).map_err(|_| self.err_at(at, "invalid UTF-16 length"))
    }

    /// B_VARBYTE: a byte length followed by bytes.
    pub(crate) fn b_varbyte(&mut self) -> Result<&'a [u8], Error> {
        let n = self.u8()? as usize;
        self.take(n)
    }

    pub(crate) fn err_at(&self, offset: usize, what: &'static str) -> Error {
        Error::Malformed {
            token: self.token,
            offset,
            what,
        }
    }
}

/// Decodes UTF-16LE. Unpaired surrogates become U+FFFD (as Go's `utf16.Decode`). An odd
/// byte count is an error (go-mssqldb rejects it too).
pub(crate) fn utf16le(bytes: &[u8]) -> Result<String, ()> {
    if !bytes.len().is_multiple_of(2) {
        return Err(());
    }
    let (pairs, _) = bytes.as_chunks::<2>();
    // Fast path for ASCII, the common case for names and most data.
    if pairs.iter().all(|c| c[1] == 0 && c[0] < 0x80) {
        return Ok(pairs.iter().map(|c| c[0] as char).collect());
    }
    let units = pairs.iter().map(|&c| u16::from_le_bytes(c));
    Ok(char::decode_utf16(units)
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect())
}

/// Encodes a string as UTF-16LE, appending to `out`.
pub(crate) fn put_utf16(out: &mut Vec<u8>, s: &str) {
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_le_bytes());
    }
}

/// UTF-16LE bytes of `s`.
pub(crate) fn to_utf16(s: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(s.len() * 2);
    put_utf16(&mut v, s);
    v
}

/// Number of UTF-16 code units in `s`.
pub(crate) fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_bounds() {
        let mut c = Cursor::new(&[1, 2, 3]);
        c.token = 0xAA;
        assert_eq!(c.u16().unwrap(), 0x0201);
        match c.u16() {
            Err(Error::Malformed { token, offset, .. }) => {
                assert_eq!(token, 0xAA);
                assert_eq!(offset, 2);
            }
            other => panic!("{other:?}"),
        }
        // A failed read does not consume.
        assert_eq!(c.u8().unwrap(), 3);
        assert!(c.is_empty());
    }

    #[test]
    fn sub_cursor_offsets() {
        let data = [9, 9, 2, 0, 0x41, 0x00, 0x42, 0x00];
        let mut c = Cursor::new(&data);
        c.skip(2).unwrap();
        let mut s = c.sub(6).unwrap();
        assert_eq!(s.offset(), 2);
        assert_eq!(s.us_varchar().unwrap(), "AB");
        assert!(s.u8().is_err());
        assert!(c.is_empty());
    }

    #[test]
    fn utf16_decoding() {
        assert_eq!(
            utf16le(&to_utf16("Xin chào ✓ 😀")).unwrap(),
            "Xin chào ✓ 😀"
        );
        // Lone high surrogate, lone low surrogate.
        assert_eq!(utf16le(&[0x00, 0xD8, 0x41, 0x00]).unwrap(), "\u{FFFD}A");
        assert_eq!(utf16le(&[0x00, 0xDC]).unwrap(), "\u{FFFD}");
        assert!(utf16le(&[0x41]).is_err());
        assert_eq!(utf16_len("a😀"), 3);
    }
}
