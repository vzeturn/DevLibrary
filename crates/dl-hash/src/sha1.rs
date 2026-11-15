use crate::Digest;
use crate::block::{Buffer, LengthOrder};

fn compress(state: &mut [u32; 5], block: &[u8; 64]) {
    let mut w = [0u32; 80];
    for (wi, c) in w.iter_mut().zip(block.as_chunks::<4>().0) {
        *wi = u32::from_be_bytes(*c);
    }
    for i in 16..80 {
        w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }
    let [mut a, mut b, mut c, mut d, mut e] = *state;
    for (i, &wi) in w.iter().enumerate() {
        let (f, k) = match i {
            0..=19 => ((b & c) | (!b & d), 0x5a827999),
            20..=39 => (b ^ c ^ d, 0x6ed9eba1),
            40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
            _ => (b ^ c ^ d, 0xca62c1d6),
        };
        let t = a
            .rotate_left(5)
            .wrapping_add(f)
            .wrapping_add(e)
            .wrapping_add(k)
            .wrapping_add(wi);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = t;
    }
    for (s, v) in state.iter_mut().zip([a, b, c, d, e]) {
        *s = s.wrapping_add(v);
    }
}

/// SHA-1 (FIPS 180-4).
///
/// SHA-1 is broken for collision resistance. Use it only where a protocol requires it,
/// such as HMAC-SHA1 in TOTP (RFC 6238) or legacy integrations.
#[derive(Clone)]
pub struct Sha1 {
    state: [u32; 5],
    buf: Buffer<64>,
}

impl Default for Sha1 {
    fn default() -> Self {
        Self {
            state: [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0],
            buf: Buffer::default(),
        }
    }
}

impl Digest for Sha1 {
    const OUTPUT_LEN: usize = 20;
    const BLOCK_LEN: usize = 64;
    type Output = [u8; 20];

    fn update(&mut self, data: &[u8]) {
        let state = &mut self.state;
        self.buf.update(data, |b| compress(state, b));
    }

    fn finalize(mut self) -> [u8; 20] {
        let state = &mut self.state;
        self.buf.finish(8, LengthOrder::Big, |b| compress(state, b));
        let mut out = [0u8; 20];
        for (o, s) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.state) {
            *o = s.to_be_bytes();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex;

    #[test]
    fn vectors() {
        assert_eq!(
            hex(&Sha1::digest(b"")),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
        assert_eq!(
            hex(&Sha1::digest(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hex(&Sha1::digest(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
        let mut h = Sha1::default();
        for _ in 0..1000 {
            h.update(&[b'a'; 1000]);
        }
        assert_eq!(
            hex(&h.finalize()),
            "34aa973cd4c4daa4f61eeb2bdbad27316534016f"
        );
    }
}
