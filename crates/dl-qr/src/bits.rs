//! Append-only bit buffer, most significant bit first.

use alloc::vec::Vec;

/// A growable sequence of bits packed MSB-first into bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub(crate) struct BitBuf {
    bytes: Vec<u8>,
    len: usize,
}

impl BitBuf {
    pub(crate) fn with_capacity(bits: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(bits.div_ceil(8)),
            len: 0,
        }
    }

    /// Number of bits written so far.
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Appends one bit.
    pub(crate) fn push_bit(&mut self, bit: bool) {
        if self.len.is_multiple_of(8) {
            self.bytes.push(0);
        }
        if bit {
            // The push above guarantees that byte `len / 8` exists.
            self.bytes[self.len / 8] |= 0x80 >> (self.len % 8);
        }
        self.len += 1;
    }

    /// Appends the low `n` bits of `value`, most significant first. `n` must be at most 32.
    pub(crate) fn push(&mut self, value: u32, n: usize) {
        debug_assert!(n <= 32 && (n == 32 || value >> n == 0));
        for i in (0..n).rev() {
            self.push_bit((value >> i) & 1 != 0);
        }
    }

    /// Returns the packed bytes. A partial last byte is padded with zero bits.
    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_msb_first() {
        let mut a = BitBuf::default();
        a.push(0b0010, 4);
        a.push(0b000001011, 9);
        assert_eq!(a.len(), 13);
        assert_eq!(a.clone().into_bytes(), [0x20, 0x58]);

        let mut c = BitBuf::with_capacity(8);
        c.push(1, 1);
        c.push(0xAB, 8);
        c.push(0, 0);
        c.push(0xFFFF_FFFF, 32);
        assert_eq!(c.len(), 41);
        assert_eq!(c.into_bytes(), [0xD5, 0xFF, 0xFF, 0xFF, 0xFF, 0x80]);
    }
}
