//! `Display`, `Debug` and grouped formatting.

use crate::{Decimal, RoundingMode};
use alloc::string::String;
use core::fmt;

/// Fixed-capacity ASCII buffer implementing `fmt::Write`; writes past the end fail.
pub(crate) struct StackBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> StackBuf<N> {
    pub(crate) const fn new() -> Self {
        StackBuf {
            buf: [0; N],
            len: 0,
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        // Only `write_str` fills the buffer and it copies whole `&str`s, so the content
        // is valid UTF-8; the fallback is unreachable.
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl<const N: usize> fmt::Write for StackBuf<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self
            .len
            .checked_add(s.len())
            .filter(|&e| e <= N)
            .ok_or(fmt::Error)?;
        self.buf[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// Writes the decimal digits of `v` right-aligned into `out` and returns the index of
/// the first digit. Zero produces `"0"`.
fn write_u128(v: u128, out: &mut [u8; 40]) -> usize {
    const P19: u128 = 10_000_000_000_000_000_000;
    // v < 10^38 = (10^19)^2: split once into two u64 halves of 19 digits so the digit
    // loop runs on fast 64-bit arithmetic; values below 10^19 skip the u128 division.
    let (hi, lo) = if v < P19 {
        (0, v as u64)
    } else {
        let q = v / P19;
        (q as u64, (v - q * P19) as u64)
    };
    let mut pos = out.len();
    let mut put = |mut c: u64, min_digits: usize| {
        let mut written = 0;
        while c != 0 || written < min_digits {
            pos -= 1;
            out[pos] = b'0' + (c % 10) as u8;
            c /= 10;
            written += 1;
        }
    };
    if hi == 0 {
        put(lo, 1);
    } else {
        put(lo, 19);
        put(hi, 1);
    }
    pos
}

/// Formats the magnitude of `d` as `digits[.digits]` into `buf`; returns the length.
/// At most 40 bytes: 38 digits + point + leading zero.
fn write_abs(d: Decimal, buf: &mut [u8; 48]) -> usize {
    let mut digits = [0u8; 40];
    let start = write_u128(d.mantissa().unsigned_abs(), &mut digits);
    let ds = &digits[start..];
    let scale = d.scale() as usize;
    let mut len = 0;
    let mut push = |b: u8| {
        buf[len] = b;
        len += 1;
    };
    if scale == 0 {
        ds.iter().for_each(|&b| push(b));
    } else if ds.len() > scale {
        let (int, frac) = ds.split_at(ds.len() - scale);
        int.iter().for_each(|&b| push(b));
        push(b'.');
        frac.iter().for_each(|&b| push(b));
    } else {
        push(b'0');
        push(b'.');
        (0..scale - ds.len()).for_each(|_| push(b'0'));
        ds.iter().for_each(|&b| push(b));
    }
    len
}

impl fmt::Display for Decimal {
    /// Prints the exact stored value, trailing zeros included (`1.50`), never in
    /// scientific notation.
    ///
    /// - `{:.N}` rounds to `N` decimal places **half-up** (away from zero), the usual
    ///   expectation for money, or pads with zeros. A value that rounds to zero prints
    ///   without a sign (`-0.004` with `{:.2}` is `0.00`).
    /// - `{:+}`, width, fill, alignment and `{:08}` work as for integers.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (d, pad) = match f.precision() {
            Some(p) if p < self.scale() as usize => {
                (self.round_dp(p as u32, RoundingMode::HalfUp), 0)
            }
            Some(p) => (*self, p - self.scale() as usize),
            None => (*self, 0),
        };
        let mut buf = [0u8; 48];
        let len = write_abs(d, &mut buf);
        let digits = core::str::from_utf8(&buf[..len]).map_err(|_| fmt::Error)?;
        if pad == 0 {
            return f.pad_integral(!d.is_negative(), "", digits);
        }
        let mut s = String::with_capacity(len + pad + 1);
        s.push_str(digits);
        if d.scale() == 0 {
            s.push('.');
        }
        s.extend(core::iter::repeat_n('0', pad));
        f.pad_integral(!d.is_negative(), "", &s)
    }
}

impl fmt::Debug for Decimal {
    /// Same as `Display`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl Decimal {
    /// Formats with `dp` decimal places (rounded with `mode`, or zero-padded), grouping
    /// the integer part in threes. A negative sign goes in front.
    ///
    /// ```
    /// use dl_decimal::{Decimal, RoundingMode::HalfUp};
    /// let x: Decimal = "-1234567.505".parse().unwrap();
    /// assert_eq!(x.to_string_grouped(2, '.', ',', HalfUp), "-1.234.567,51"); // Vietnam
    /// assert_eq!(x.to_string_grouped(2, ',', '.', HalfUp), "-1,234,567.51"); // US
    /// assert_eq!(x.to_string_grouped(0, '.', ',', HalfUp), "-1.234.568");
    /// ```
    pub fn to_string_grouped(
        &self,
        dp: u32,
        group_sep: char,
        decimal_sep: char,
        mode: RoundingMode,
    ) -> String {
        let d = self.round_dp(dp, mode);
        let mut buf = [0u8; 48];
        let len = write_abs(d, &mut buf);
        let text = &buf[..len];
        let int_len = text.iter().position(|&b| b == b'.').unwrap_or(len);
        let (int, frac) = text.split_at(int_len);
        let frac = frac.get(1..).unwrap_or(&[]);
        let mut s = String::with_capacity(len + len / 3 + dp as usize + 8);
        if d.is_negative() {
            s.push('-');
        }
        for (i, &b) in int.iter().enumerate() {
            if i > 0 && (int.len() - i) % 3 == 0 {
                s.push(group_sep);
            }
            s.push(char::from(b));
        }
        if dp > 0 {
            s.push(decimal_sep);
            frac.iter().for_each(|&b| s.push(char::from(b)));
            s.extend(core::iter::repeat_n('0', dp as usize - frac.len()));
        }
        s
    }
}
