//! DEFLATE decompression (RFC 1951).

use alloc::vec;
use alloc::vec::Vec;

use crate::{Error, ErrorKind};

/// Base lengths and extra bits of length symbols 257..=285.
pub(crate) const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
pub(crate) const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
/// Base distances and extra bits of distance symbols 0..=29.
pub(crate) const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
pub(crate) const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
/// Order of the code length code lengths in a dynamic block header.
pub(crate) const CL_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Bits looked up at once; longer codes take the bit-by-bit path.
const FAST_BITS: u32 = 9;

/// LSB-first bit reader over a byte slice.
pub(crate) struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    cnt: u32,
}

impl<'a> Bits<'a> {
    pub(crate) fn new(data: &'a [u8], pos: usize) -> Bits<'a> {
        Bits {
            data,
            pos,
            buf: 0,
            cnt: 0,
        }
    }

    fn refill(&mut self) {
        while self.cnt <= 56 && self.pos < self.data.len() {
            self.buf |= u64::from(self.data[self.pos]) << self.cnt;
            self.pos += 1;
            self.cnt += 8;
        }
    }

    /// Offset of the next unread byte (after discarding partial-byte bits).
    pub(crate) fn byte_offset(&self) -> usize {
        self.pos - (self.cnt / 8) as usize
    }

    fn eof(&self) -> Error {
        Error::new(ErrorKind::UnexpectedEof, self.data.len())
    }

    fn bits(&mut self, n: u32) -> Result<u32, Error> {
        if n == 0 {
            return Ok(0);
        }
        if self.cnt < n {
            self.refill();
            if self.cnt < n {
                return Err(self.eof());
            }
        }
        let v = (self.buf & ((1u64 << n) - 1)) as u32;
        self.buf >>= n;
        self.cnt -= n;
        Ok(v)
    }

    /// Drops the bits left in the current byte (before a stored block).
    fn align(&mut self) {
        let drop = self.cnt % 8;
        self.buf >>= drop;
        self.cnt -= drop;
    }
}

/// A canonical Huffman code: counts per length and symbols in code order, plus a lookup
/// table for codes up to `FAST_BITS` bits.
pub(crate) struct Huffman {
    count: [u16; 16],
    symbol: Vec<u16>,
    /// `(symbol << 4) | length`, 0 = not in the table.
    fast: Vec<u16>,
}

/// What a set of code lengths may be.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Need {
    /// Complete, or a single code of length 1 (literal/length and distance codes, as zlib).
    Lenient,
    /// Complete (the code length code).
    Complete,
}

impl Huffman {
    fn new(lengths: &[u8], need: Need, at: usize) -> Result<Huffman, Error> {
        let mut count = [0u16; 16];
        for &l in lengths {
            count[usize::from(l)] += 1;
        }
        let mut left: i32 = 1;
        let mut max = 0;
        for (len, &c) in count.iter().enumerate().skip(1) {
            left <<= 1;
            left -= i32::from(c);
            if left < 0 {
                return Err(Error::new(ErrorKind::InvalidCode, at));
            }
            if c > 0 {
                max = len;
            }
        }
        if left > 0 && !(need == Need::Lenient && max == 1) && max != 0 {
            return Err(Error::new(ErrorKind::InvalidCode, at));
        }
        if max == 0 && need == Need::Complete {
            return Err(Error::new(ErrorKind::InvalidCode, at));
        }
        let mut offs = [0u16; 16];
        for len in 1..15 {
            offs[len + 1] = offs[len] + count[len];
        }
        let mut symbol = vec![0u16; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbol[usize::from(offs[usize::from(l)])] = sym as u16;
                offs[usize::from(l)] += 1;
            }
        }
        // Lookup table: canonical codes, bit-reversed for the LSB-first reader.
        let mut fast = vec![0u16; 1 << FAST_BITS];
        let mut code: u32 = 0;
        let mut index = 0usize;
        for len in 1..=15u32 {
            for _ in 0..count[len as usize] {
                if len <= FAST_BITS {
                    let rev = code.reverse_bits() >> (32 - len);
                    let entry = (symbol[index] << 4) | len as u16;
                    let mut k = rev;
                    while k < (1 << FAST_BITS) {
                        fast[k as usize] = entry;
                        k += 1 << len;
                    }
                }
                code += 1;
                index += 1;
            }
            code <<= 1;
        }
        Ok(Huffman {
            count,
            symbol,
            fast,
        })
    }

    fn decode(&self, b: &mut Bits<'_>) -> Result<u16, Error> {
        if b.cnt < FAST_BITS {
            b.refill();
        }
        let e = self.fast[(b.buf & ((1 << FAST_BITS) - 1)) as usize];
        if e != 0 {
            let len = u32::from(e & 15);
            if len <= b.cnt {
                b.buf >>= len;
                b.cnt -= len;
                return Ok(e >> 4);
            }
            return Err(b.eof());
        }
        // Bit by bit (codes longer than FAST_BITS).
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= b.bits(1)? as i32;
            let c = i32::from(self.count[len]);
            if code - c < first {
                return Ok(self.symbol[(index + (code - first)) as usize]);
            }
            index += c;
            first += c;
            first <<= 1;
            code <<= 1;
        }
        Err(Error::new(ErrorKind::InvalidCode, b.byte_offset()))
    }
}

pub(crate) fn fixed_lengths() -> ([u8; 288], [u8; 32]) {
    let mut lit = [0u8; 288];
    for (i, l) in lit.iter_mut().enumerate() {
        *l = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    (lit, [5u8; 32])
}

/// Inflates the raw DEFLATE stream starting at `data[start..]` into `out`, refusing to
/// produce more than `limit` bytes in total. Returns the offset just after the stream.
pub(crate) fn inflate_into(
    data: &[u8],
    start: usize,
    out: &mut Vec<u8>,
    limit: usize,
) -> Result<usize, Error> {
    let mut b = Bits::new(data, start);
    let base = out.len();
    loop {
        let last = b.bits(1)? == 1;
        let kind = b.bits(2)?;
        match kind {
            0 => {
                b.align();
                let at = b.byte_offset();
                let len = b.bits(16)?;
                let nlen = b.bits(16)?;
                if len != !nlen & 0xFFFF {
                    return Err(Error::new(ErrorKind::StoredLength, at));
                }
                if out.len() - base + len as usize > limit {
                    return Err(Error::new(ErrorKind::TooLarge { limit }, at));
                }
                // Bytes still in the bit buffer first, then straight from the input.
                let mut n = len as usize;
                while n > 0 && b.cnt >= 8 {
                    out.push(b.bits(8)? as u8);
                    n -= 1;
                }
                if b.pos + n > data.len() {
                    return Err(b.eof());
                }
                out.extend_from_slice(&data[b.pos..b.pos + n]);
                b.pos += n;
            }
            1 => {
                let (lit, dist) = fixed_lengths();
                let at = b.byte_offset();
                let lit = Huffman::new(&lit, Need::Lenient, at)?;
                let dist = Huffman::new(&dist, Need::Lenient, at)?;
                codes(&mut b, out, base, limit, &lit, &dist)?;
            }
            2 => {
                let (lit, dist) = dynamic_tables(&mut b)?;
                codes(&mut b, out, base, limit, &lit, &dist)?;
            }
            _ => return Err(Error::new(ErrorKind::InvalidBlockType, b.byte_offset())),
        }
        if last {
            return Ok(b.byte_offset());
        }
    }
}

fn dynamic_tables(b: &mut Bits<'_>) -> Result<(Huffman, Huffman), Error> {
    let at = b.byte_offset();
    let nlen = b.bits(5)? as usize + 257;
    let ndist = b.bits(5)? as usize + 1;
    let ncode = b.bits(4)? as usize + 4;
    if nlen > 286 || ndist > 30 {
        return Err(Error::new(ErrorKind::InvalidCode, at));
    }
    let mut cl = [0u8; 19];
    for &i in CL_ORDER.iter().take(ncode) {
        cl[i] = b.bits(3)? as u8;
    }
    let clcode = Huffman::new(&cl, Need::Complete, at)?;
    let mut lengths = [0u8; 316];
    let mut i = 0;
    while i < nlen + ndist {
        let sym = clcode.decode(b)?;
        if sym < 16 {
            lengths[i] = sym as u8;
            i += 1;
            continue;
        }
        let (value, repeat) = match sym {
            16 => {
                if i == 0 {
                    return Err(Error::new(ErrorKind::InvalidCode, b.byte_offset()));
                }
                (lengths[i - 1], 3 + b.bits(2)? as usize)
            }
            17 => (0, 3 + b.bits(3)? as usize),
            _ => (0, 11 + b.bits(7)? as usize),
        };
        if i + repeat > nlen + ndist {
            return Err(Error::new(ErrorKind::InvalidCode, b.byte_offset()));
        }
        lengths[i..i + repeat].fill(value);
        i += repeat;
    }
    if lengths[256] == 0 {
        // No end-of-block code.
        return Err(Error::new(ErrorKind::InvalidCode, at));
    }
    let lit = Huffman::new(&lengths[..nlen], Need::Lenient, at)?;
    let dist = Huffman::new(&lengths[nlen..nlen + ndist], Need::Lenient, at)?;
    Ok((lit, dist))
}

fn codes(
    b: &mut Bits<'_>,
    out: &mut Vec<u8>,
    base: usize,
    limit: usize,
    lit: &Huffman,
    dist: &Huffman,
) -> Result<(), Error> {
    loop {
        let sym = lit.decode(b)?;
        if sym < 256 {
            if out.len() - base >= limit {
                return Err(Error::new(ErrorKind::TooLarge { limit }, b.byte_offset()));
            }
            out.push(sym as u8);
            continue;
        }
        if sym == 256 {
            return Ok(());
        }
        let s = usize::from(sym - 257);
        if s >= 29 {
            return Err(Error::new(ErrorKind::InvalidSymbol, b.byte_offset()));
        }
        let len = usize::from(LEN_BASE[s]) + b.bits(u32::from(LEN_EXTRA[s]))? as usize;
        let d = usize::from(dist.decode(b)?);
        if d >= 30 {
            return Err(Error::new(ErrorKind::InvalidSymbol, b.byte_offset()));
        }
        let distance = usize::from(DIST_BASE[d]) + b.bits(u32::from(DIST_EXTRA[d]))? as usize;
        if distance > out.len() - base {
            return Err(Error::new(ErrorKind::DistanceTooFar, b.byte_offset()));
        }
        if out.len() - base + len > limit {
            return Err(Error::new(ErrorKind::TooLarge { limit }, b.byte_offset()));
        }
        let from = out.len() - distance;
        if distance >= len {
            out.extend_from_within(from..from + len);
        } else {
            for k in 0..len {
                let c = out[from + k];
                out.push(c);
            }
        }
    }
}
