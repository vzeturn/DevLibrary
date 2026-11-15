//! Small helpers for writing numbers without going through `core::fmt` machinery.

use core::fmt::{self, Write};

/// Fixed-capacity stack buffer for short ASCII renderings (dates, timestamps, offsets).
pub(crate) struct Buf {
    bytes: [u8; 64],
    len: usize,
}

impl Buf {
    pub(crate) const fn new() -> Self {
        Buf {
            bytes: [0; 64],
            len: 0,
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        // Only whole `&str`s and `char`s are ever appended, so the bytes are valid UTF-8;
        // the fallback is unreachable.
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}

impl Write for Buf {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len + s.len();
        if end > self.bytes.len() {
            return Err(fmt::Error);
        }
        self.bytes[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }

    fn write_char(&mut self, c: char) -> fmt::Result {
        if c.is_ascii() && self.len < self.bytes.len() {
            self.bytes[self.len] = c as u8;
            self.len += 1;
            Ok(())
        } else {
            self.write_str(c.encode_utf8(&mut [0; 4]))
        }
    }
}

/// Writes `n` in decimal, left-padded with zeros to at least `width` digits.
pub(crate) fn write_padded<W: Write>(w: &mut W, n: u64, width: usize) -> fmt::Result {
    let mut digits = [0u8; 20];
    let mut i = digits.len();
    let mut n = n;
    loop {
        i -= 1;
        digits[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    for _ in (digits.len() - i)..width {
        w.write_char('0')?;
    }
    for &d in &digits[i..] {
        w.write_char(d as char)?;
    }
    Ok(())
}

/// Writes a two-digit number (0..=99).
pub(crate) fn write2<W: Write>(w: &mut W, n: u8) -> fmt::Result {
    w.write_char((b'0' + n / 10) as char)?;
    w.write_char((b'0' + n % 10) as char)
}

/// Writes an ISO year: four digits at least, with a leading `-` for negative years.
pub(crate) fn write_year<W: Write>(w: &mut W, year: i64) -> fmt::Result {
    if year < 0 {
        w.write_char('-')?;
    }
    write_padded(w, year.unsigned_abs(), 4)
}

/// Writes `.` and the minimal 3, 6 or 9 digit fraction, or nothing for zero.
pub(crate) fn write_fraction<W: Write>(w: &mut W, nanos: u32) -> fmt::Result {
    if nanos == 0 {
        Ok(())
    } else if nanos.is_multiple_of(1_000_000) {
        w.write_char('.')?;
        write_padded(w, (nanos / 1_000_000) as u64, 3)
    } else if nanos.is_multiple_of(1_000) {
        w.write_char('.')?;
        write_padded(w, (nanos / 1_000) as u64, 6)
    } else {
        w.write_char('.')?;
        write_padded(w, nanos as u64, 9)
    }
}

/// Writes `.` and the fraction with trailing zeros removed, or nothing for zero.
pub(crate) fn write_fraction_trimmed<W: Write>(w: &mut W, nanos: u32) -> fmt::Result {
    if nanos == 0 {
        return Ok(());
    }
    let mut n = nanos;
    let mut width = 9;
    while n.is_multiple_of(10) {
        n /= 10;
        width -= 1;
    }
    w.write_char('.')?;
    write_padded(w, n as u64, width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    #[test]
    fn helpers() {
        let mut s = String::new();
        write_padded(&mut s, 7, 3).unwrap();
        write_padded(&mut s, 12345, 2).unwrap();
        write_year(&mut s, -44).unwrap();
        write_fraction(&mut s, 500_000_000).unwrap();
        write_fraction(&mut s, 1_000).unwrap();
        write_fraction(&mut s, 1).unwrap();
        write_fraction_trimmed(&mut s, 450_000_000).unwrap();
        assert_eq!(s, "00712345-0044.500.000001.000000001.45");
        let mut b = Buf::new();
        for _ in 0..64 {
            b.write_char('x').unwrap();
        }
        assert!(b.write_char('x').is_err());
        assert_eq!(b.as_str().len(), 64);
    }
}
