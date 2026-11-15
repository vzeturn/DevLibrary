//! Shared machinery for power-of-two radix encodings (Base32, Base64).

use crate::Error;
use alloc::vec::Vec;

/// Marker for bytes outside the alphabet in a decode table.
pub(crate) const INVALID: u8 = 0xff;

/// Builds a 256-entry reverse lookup table for an alphabet at compile time.
pub(crate) const fn decode_table(alphabet: &[u8]) -> [u8; 256] {
    let mut table = [INVALID; 256];
    let mut i = 0;
    while i < alphabet.len() {
        table[alphabet[i] as usize] = i as u8;
        i += 1;
    }
    table
}

/// Decodes one group of at most 8 symbols, each carrying `bits` bits, and appends the
/// whole bytes it contains to `out`. `base` is the input offset of `group[0]`.
#[inline]
pub(crate) fn decode_group(
    table: &[u8; 256],
    bits: u32,
    group: &[u8],
    base: usize,
    out: &mut Vec<u8>,
) -> Result<(), Error> {
    let mut acc: u64 = 0;
    for (j, &byte) in group.iter().enumerate() {
        let v = table[byte as usize];
        if v == INVALID {
            return Err(Error::InvalidByte {
                index: base + j,
                byte,
            });
        }
        acc = (acc << bits) | u64::from(v);
    }
    let total = group.len() as u32 * bits;
    let extra = total % 8;
    if acc & ((1u64 << extra) - 1) != 0 {
        return Err(Error::TrailingBits {
            index: base + group.len() - 1,
        });
    }
    acc >>= extra;
    let n = total / 8;
    for k in (0..n).rev() {
        out.push((acc >> (8 * k)) as u8);
    }
    Ok(())
}
