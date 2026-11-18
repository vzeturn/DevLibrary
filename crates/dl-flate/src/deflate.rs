//! DEFLATE compression (RFC 1951): LZ77 with hash chains and lazy matching, then per
//! block the smallest of a stored, a fixed-Huffman and a dynamic-Huffman encoding.

use alloc::vec;
use alloc::vec::Vec;

use crate::Level;
use crate::inflate::{CL_ORDER, DIST_BASE, DIST_EXTRA, LEN_BASE, LEN_EXTRA, fixed_lengths};

const WINDOW: usize = 1 << 15;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const HASH_BITS: u32 = 15;
/// Tokens per block before the block is written.
const BLOCK_TOKENS: usize = 1 << 14;

/// A literal (`dist == 0`) or a match.
#[derive(Clone, Copy)]
struct Token {
    len_or_lit: u16,
    dist: u16,
}

/// LSB-first bit writer.
struct BitWriter {
    out: Vec<u8>,
    buf: u64,
    cnt: u32,
}

impl BitWriter {
    fn new(out: Vec<u8>) -> BitWriter {
        BitWriter {
            out,
            buf: 0,
            cnt: 0,
        }
    }

    fn put(&mut self, value: u32, n: u32) {
        debug_assert!(n <= 32);
        self.buf |= u64::from(value) << self.cnt;
        self.cnt += n;
        while self.cnt >= 8 {
            self.out.push(self.buf as u8);
            self.buf >>= 8;
            self.cnt -= 8;
        }
    }

    fn align(&mut self) {
        if self.cnt > 0 {
            self.out.push(self.buf as u8);
            self.buf = 0;
            self.cnt = 0;
        }
    }

    fn finish(mut self) -> Vec<u8> {
        self.align();
        self.out
    }
}

struct Params {
    max_chain: usize,
    nice: usize,
    lazy: bool,
}

fn params(level: Level) -> Params {
    match level {
        Level::Store => Params {
            max_chain: 0,
            nice: 0,
            lazy: false,
        },
        Level::Fast => Params {
            max_chain: 8,
            nice: 32,
            lazy: false,
        },
        Level::Default => Params {
            max_chain: 128,
            nice: 128,
            lazy: true,
        },
        Level::Best => Params {
            max_chain: 4096,
            nice: MAX_MATCH,
            lazy: true,
        },
    }
}

fn hash(d: &[u8], i: usize) -> usize {
    let v = u32::from(d[i]) | (u32::from(d[i + 1]) << 8) | (u32::from(d[i + 2]) << 16);
    (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
}

struct Matcher {
    head: Vec<i32>,
    prev: Vec<i32>,
}

impl Matcher {
    fn insert(&mut self, d: &[u8], i: usize) {
        if i + MIN_MATCH <= d.len() {
            let h = hash(d, i);
            self.prev[i % WINDOW] = self.head[h];
            self.head[h] = i as i32;
        }
    }

    /// Longest match for position `i` among earlier positions in the window.
    fn find(&self, d: &[u8], i: usize, p: &Params) -> (usize, usize) {
        if i + MIN_MATCH > d.len() {
            return (0, 0);
        }
        let max = (d.len() - i).min(MAX_MATCH);
        let mut best = (0, 0);
        let mut cand = self.prev[i % WINDOW];
        let mut chain = p.max_chain;
        while cand >= 0 && chain > 0 {
            let c = cand as usize;
            if i - c > WINDOW - 1 || c >= i {
                break;
            }
            if d[c + best.0] == d[i + best.0] || best.0 == 0 {
                let mut n = 0;
                while n < max && d[c + n] == d[i + n] {
                    n += 1;
                }
                if n > best.0 {
                    best = (n, i - c);
                    if n >= p.nice || n == max {
                        break;
                    }
                }
            }
            cand = self.prev[c % WINDOW];
            chain -= 1;
        }
        if best.0 >= MIN_MATCH { best } else { (0, 0) }
    }
}

fn len_symbol(len: usize) -> (usize, u32, u32) {
    let mut s = 28;
    while usize::from(LEN_BASE[s]) > len {
        s -= 1;
    }
    (
        257 + s,
        u32::from(LEN_EXTRA[s]),
        (len - usize::from(LEN_BASE[s])) as u32,
    )
}

fn dist_symbol(dist: usize) -> (usize, u32, u32) {
    let mut s = 29;
    while usize::from(DIST_BASE[s]) > dist {
        s -= 1;
    }
    (
        s,
        u32::from(DIST_EXTRA[s]),
        (dist - usize::from(DIST_BASE[s])) as u32,
    )
}

/// Huffman code lengths for `freq`, at most `limit` bits, every used symbol coded. Two
/// symbols at least are always given a code, so the code is complete.
fn code_lengths(freq: &[u32], limit: usize) -> Vec<u8> {
    let n = freq.len();
    let mut lengths = vec![0u8; n];
    let mut syms: Vec<usize> = (0..n).filter(|&i| freq[i] > 0).collect();
    if syms.len() < 2 {
        // Add symbols until there are two, so the code is complete (1 bit each).
        while syms.len() < 2 {
            match (0..n).find(|i| !syms.contains(i)) {
                Some(extra) => syms.push(extra),
                None => break,
            }
        }
        syms.sort_unstable();
        for &s in &syms {
            lengths[s] = 1;
        }
        return lengths;
    }
    syms.sort_by_key(|&s| (freq[s], s));
    // Two-queue Huffman construction over the sorted leaves.
    let leaves = syms.len();
    let mut weight: Vec<u64> = syms.iter().map(|&s| u64::from(freq[s])).collect();
    let mut parent = vec![usize::MAX; 2 * leaves - 1];
    let (mut q1, mut q2) = (0usize, leaves);
    for node in leaves..2 * leaves - 1 {
        let mut pick = || {
            if q1 < leaves && (q2 >= node || weight[q1] <= weight[q2]) {
                q1 += 1;
                q1 - 1
            } else {
                q2 += 1;
                q2 - 1
            }
        };
        let (a, b) = (pick(), pick());
        weight.push(weight[a] + weight[b]);
        parent[a] = node;
        parent[b] = node;
    }
    // Depth of every node, from the root down (parents have larger indexes).
    let root = 2 * leaves - 2;
    let mut depth = vec![0usize; 2 * leaves - 1];
    for node in (0..root).rev() {
        depth[node] = depth[parent[node]] + 1;
    }
    let max_depth = depth[..leaves].iter().copied().max().unwrap_or(1);
    let top = max_depth.max(limit);
    let mut count = vec![0usize; top + 1];
    for &d in &depth[..leaves] {
        count[d] += 1;
    }
    if max_depth > limit {
        // Fold the long codes into `limit` and restore Kraft equality (miniz's method).
        for l in limit + 1..=top {
            count[limit] += count[l];
            count[l] = 0;
        }
        let mut total: u64 = 0;
        for (l, &c) in count.iter().enumerate().take(limit + 1).skip(1) {
            total += (c as u64) << (limit - l);
        }
        while total != 1u64 << limit {
            count[limit] -= 1;
            for l in (1..limit).rev() {
                if count[l] > 0 {
                    count[l] -= 1;
                    count[l + 1] += 2;
                    break;
                }
            }
            total -= 1;
        }
    }
    // Most frequent symbols get the shortest codes.
    let mut by_freq = syms.iter().rev();
    for (l, &c) in count.iter().enumerate().skip(1) {
        for _ in 0..c {
            if let Some(&s) = by_freq.next() {
                lengths[s] = l as u8;
            }
        }
    }
    lengths
}

/// Canonical codes for `lengths`, bit-reversed for the LSB-first writer.
fn codes_for(lengths: &[u8]) -> Vec<u16> {
    let mut count = [0u16; 16];
    for &l in lengths {
        count[usize::from(l)] += 1;
    }
    count[0] = 0;
    let mut next = [0u16; 16];
    let mut code = 0u16;
    for l in 1..16 {
        code = (code + count[l - 1]) << 1;
        next[l] = code;
    }
    lengths
        .iter()
        .map(|&l| {
            if l == 0 {
                return 0;
            }
            let c = next[usize::from(l)];
            next[usize::from(l)] += 1;
            c.reverse_bits() >> (16 - u32::from(l))
        })
        .collect()
}

/// Run-length encodes code lengths into code length symbols `(symbol, extra bits, extra)`.
fn rle(lengths: &[u8]) -> Vec<(u8, u32, u32)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lengths.len() {
        let v = lengths[i];
        let mut run = 1;
        while i + run < lengths.len() && lengths[i + run] == v {
            run += 1;
        }
        i += run;
        if v == 0 {
            while run >= 11 {
                let n = run.min(138);
                out.push((18, 7, (n - 11) as u32));
                run -= n;
            }
            if run >= 3 {
                out.push((17, 3, (run - 3) as u32));
                run = 0;
            }
            for _ in 0..run {
                out.push((0, 0, 0));
            }
        } else {
            out.push((v, 0, 0));
            run -= 1;
            while run >= 3 {
                let n = run.min(6);
                out.push((16, 2, (n - 3) as u32));
                run -= n;
            }
            for _ in 0..run {
                out.push((v, 0, 0));
            }
        }
    }
    out
}

struct Tables {
    lit_len: Vec<u8>,
    dist_len: Vec<u8>,
    lit_code: Vec<u16>,
    dist_code: Vec<u16>,
}

fn write_tokens(w: &mut BitWriter, tokens: &[Token], t: &Tables) {
    for tok in tokens {
        if tok.dist == 0 {
            let s = usize::from(tok.len_or_lit);
            w.put(u32::from(t.lit_code[s]), u32::from(t.lit_len[s]));
        } else {
            let (s, eb, ev) = len_symbol(usize::from(tok.len_or_lit));
            w.put(u32::from(t.lit_code[s]), u32::from(t.lit_len[s]));
            w.put(ev, eb);
            let (d, db, dv) = dist_symbol(usize::from(tok.dist));
            w.put(u32::from(t.dist_code[d]), u32::from(t.dist_len[d]));
            w.put(dv, db);
        }
    }
    w.put(u32::from(t.lit_code[256]), u32::from(t.lit_len[256]));
}

fn data_bits(tokens: &[Token], lit_len: &[u8], dist_len: &[u8]) -> u64 {
    let mut bits = u64::from(lit_len[256]);
    for tok in tokens {
        if tok.dist == 0 {
            bits += u64::from(lit_len[usize::from(tok.len_or_lit)]);
        } else {
            let (s, eb, _) = len_symbol(usize::from(tok.len_or_lit));
            let (d, db, _) = dist_symbol(usize::from(tok.dist));
            bits += u64::from(lit_len[s]) + u64::from(eb) + u64::from(dist_len[d]) + u64::from(db);
        }
    }
    bits
}

/// The dynamic block header (after BFINAL/BTYPE) as bits in a scratch writer.
fn dynamic_header(lit_len: &[u8], dist_len: &[u8]) -> BitWriter {
    let mut hlit = 286;
    while hlit > 257 && lit_len[hlit - 1] == 0 {
        hlit -= 1;
    }
    let mut hdist = 30;
    while hdist > 1 && dist_len[hdist - 1] == 0 {
        hdist -= 1;
    }
    let mut all = Vec::with_capacity(hlit + hdist);
    all.extend_from_slice(&lit_len[..hlit]);
    all.extend_from_slice(&dist_len[..hdist]);
    let symbols = rle(&all);
    let mut freq = [0u32; 19];
    for &(s, _, _) in &symbols {
        freq[usize::from(s)] += 1;
    }
    let cl_len = code_lengths(&freq, 7);
    let cl_code = codes_for(&cl_len);
    let mut hclen = 19;
    while hclen > 4 && cl_len[CL_ORDER[hclen - 1]] == 0 {
        hclen -= 1;
    }
    let mut w = BitWriter::new(Vec::new());
    w.put((hlit - 257) as u32, 5);
    w.put((hdist - 1) as u32, 5);
    w.put((hclen - 4) as u32, 4);
    for &i in CL_ORDER.iter().take(hclen) {
        w.put(u32::from(cl_len[i]), 3);
    }
    for &(s, eb, ev) in &symbols {
        w.put(
            u32::from(cl_code[usize::from(s)]),
            u32::from(cl_len[usize::from(s)]),
        );
        w.put(ev, eb);
    }
    w
}

/// Appends the bits of `src` (whole bytes plus a partial byte) to `w`.
fn append(w: &mut BitWriter, src: BitWriter) {
    for &b in &src.out {
        w.put(u32::from(b), 8);
    }
    w.put(src.buf as u32, src.cnt);
}

fn write_stored(w: &mut BitWriter, data: &[u8], last: bool) {
    let mut chunks = data.chunks(0xFFFF).peekable();
    if data.is_empty() {
        w.put(u32::from(last), 1);
        w.put(0, 2);
        w.align();
        w.put(0, 16);
        w.put(0xFFFF, 16);
        return;
    }
    while let Some(c) = chunks.next() {
        let final_chunk = last && chunks.peek().is_none();
        w.put(u32::from(final_chunk), 1);
        w.put(0, 2);
        w.align();
        w.put(c.len() as u32, 16);
        w.put(!(c.len() as u32) & 0xFFFF, 16);
        w.out.extend_from_slice(c);
    }
}

fn write_block(w: &mut BitWriter, tokens: &[Token], raw: &[u8], last: bool) {
    let mut lit_freq = [0u32; 286];
    let mut dist_freq = [0u32; 30];
    lit_freq[256] = 1;
    for tok in tokens {
        if tok.dist == 0 {
            lit_freq[usize::from(tok.len_or_lit)] += 1;
        } else {
            lit_freq[len_symbol(usize::from(tok.len_or_lit)).0] += 1;
            dist_freq[dist_symbol(usize::from(tok.dist)).0] += 1;
        }
    }
    let lit_len = code_lengths(&lit_freq, 15);
    let dist_len = code_lengths(&dist_freq, 15);
    let header = dynamic_header(&lit_len, &dist_len);
    let header_bits = header.out.len() as u64 * 8 + u64::from(header.cnt);
    let dynamic = 3 + header_bits + data_bits(tokens, &lit_len, &dist_len);
    let (fixed_lit, fixed_dist) = fixed_lengths();
    let fixed = 3 + data_bits(tokens, &fixed_lit, &fixed_dist);
    let stored = (raw.len() as u64 + 5 * (raw.len() as u64 / 0xFFFF + 1)) * 8 + 8;
    if stored <= fixed && stored <= dynamic {
        write_stored(w, raw, last);
    } else if fixed <= dynamic {
        w.put(u32::from(last), 1);
        w.put(1, 2);
        let t = Tables {
            lit_code: codes_for(&fixed_lit),
            dist_code: codes_for(&fixed_dist),
            lit_len: fixed_lit.to_vec(),
            dist_len: fixed_dist.to_vec(),
        };
        write_tokens(w, tokens, &t);
    } else {
        w.put(u32::from(last), 1);
        w.put(2, 2);
        append(w, header);
        let t = Tables {
            lit_code: codes_for(&lit_len),
            dist_code: codes_for(&dist_len),
            lit_len,
            dist_len,
        };
        write_tokens(w, tokens, &t);
    }
}

/// Compresses `data` as a raw DEFLATE stream appended to `out`.
pub(crate) fn deflate_into(data: &[u8], level: Level, out: Vec<u8>) -> Vec<u8> {
    let mut w = BitWriter::new(out);
    if level == Level::Store {
        write_stored(&mut w, data, true);
        return w.finish();
    }
    let p = params(level);
    let mut m = Matcher {
        head: vec![-1; 1 << HASH_BITS],
        prev: vec![-1; WINDOW],
    };
    let mut tokens: Vec<Token> = Vec::with_capacity(BLOCK_TOKENS + 2);
    let mut block_start = 0;
    let mut i = 0;
    while i < data.len() {
        m.insert(data, i);
        let (mut len, mut dist) = m.find(data, i, &p);
        if p.lazy && len >= MIN_MATCH && len < p.nice && i + 1 < data.len() {
            m.insert(data, i + 1);
            let (l2, d2) = m.find(data, i + 1, &p);
            if l2 > len {
                // A literal now and the longer match from the next position.
                tokens.push(Token {
                    len_or_lit: u16::from(data[i]),
                    dist: 0,
                });
                i += 1;
                len = l2;
                dist = d2;
                for k in 1..len {
                    m.insert(data, i + k);
                }
            } else {
                for k in 2..len {
                    m.insert(data, i + k);
                }
            }
        } else if len >= MIN_MATCH {
            for k in 1..len {
                m.insert(data, i + k);
            }
        }
        if len >= MIN_MATCH {
            tokens.push(Token {
                len_or_lit: len as u16,
                dist: dist as u16,
            });
            i += len;
        } else {
            tokens.push(Token {
                len_or_lit: u16::from(data[i]),
                dist: 0,
            });
            i += 1;
        }
        if tokens.len() >= BLOCK_TOKENS {
            write_block(&mut w, &tokens, &data[block_start..i], i == data.len());
            tokens.clear();
            block_start = i;
            if i == data.len() {
                return w.finish();
            }
        }
    }
    write_block(&mut w, &tokens, &data[block_start..], true);
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_are_limited_and_complete() {
        // Fibonacci frequencies give a degenerate tree deeper than 15.
        let mut freq = [0u32; 40];
        let (mut a, mut b) = (1u32, 1u32);
        for f in freq.iter_mut().take(30) {
            *f = a;
            let c = a.saturating_add(b);
            a = b;
            b = c;
        }
        let l = code_lengths(&freq, 15);
        assert!(l.iter().all(|&x| x <= 15));
        let kraft: f64 = l
            .iter()
            .filter(|&&x| x > 0)
            .map(|&x| 1.0 / f64::from(1u32 << x))
            .sum();
        assert!((kraft - 1.0).abs() < 1e-12, "{kraft}");
        assert_eq!(l.iter().filter(|&&x| x > 0).count(), 30);
        // A single symbol still gets a complete two-code tree.
        let one = code_lengths(&[0, 5, 0], 15);
        assert_eq!(one, [1, 1, 0]);
        assert_eq!(code_lengths(&[0, 0], 7), [1, 1]);
    }

    #[test]
    fn rle_round_trip() {
        let lengths = [
            0u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5, 5, 5, 5, 5, 5, 5, 5, 3, 0, 0,
        ];
        let mut back = Vec::new();
        for (s, _, v) in rle(&lengths) {
            match s {
                16 => {
                    let p = *back.last().unwrap();
                    back.extend(core::iter::repeat_n(p, 3 + v as usize));
                }
                17 => back.extend(core::iter::repeat_n(0, 3 + v as usize)),
                18 => back.extend(core::iter::repeat_n(0, 11 + v as usize)),
                s => back.push(s),
            }
        }
        assert_eq!(back, lengths);
    }
}
