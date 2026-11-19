use crate::Digest;
use crate::block::{Buffer, LengthOrder};

fn compress(state: &mut [u32; 4], block: &[u8; 64]) {
    let mut x = [0u32; 16];
    for (xi, c) in x.iter_mut().zip(block.as_chunks::<4>().0) {
        *xi = u32::from_le_bytes(*c);
    }
    let [mut a, mut b, mut c, mut d] = *state;
    // Round 1: F(x, y, z) = (x & y) | (!x & z).
    for &i in &[0usize, 4, 8, 12] {
        a = (a.wrapping_add((b & c) | (!b & d)).wrapping_add(x[i])).rotate_left(3);
        d = (d.wrapping_add((a & b) | (!a & c)).wrapping_add(x[i + 1])).rotate_left(7);
        c = (c.wrapping_add((d & a) | (!d & b)).wrapping_add(x[i + 2])).rotate_left(11);
        b = (b.wrapping_add((c & d) | (!c & a)).wrapping_add(x[i + 3])).rotate_left(19);
    }
    // Round 2: G(x, y, z) = majority, plus 0x5A827999.
    const K2: u32 = 0x5A82_7999;
    let g = |x: u32, y: u32, z: u32| (x & y) | (x & z) | (y & z);
    for i in 0..4 {
        a = (a
            .wrapping_add(g(b, c, d))
            .wrapping_add(x[i])
            .wrapping_add(K2))
        .rotate_left(3);
        d = (d
            .wrapping_add(g(a, b, c))
            .wrapping_add(x[i + 4])
            .wrapping_add(K2))
        .rotate_left(5);
        c = (c
            .wrapping_add(g(d, a, b))
            .wrapping_add(x[i + 8])
            .wrapping_add(K2))
        .rotate_left(9);
        b = (b
            .wrapping_add(g(c, d, a))
            .wrapping_add(x[i + 12])
            .wrapping_add(K2))
        .rotate_left(13);
    }
    // Round 3: H(x, y, z) = x ^ y ^ z, plus 0x6ED9EBA1.
    const K3: u32 = 0x6ED9_EBA1;
    for &i in &[0usize, 2, 1, 3] {
        a = (a
            .wrapping_add(b ^ c ^ d)
            .wrapping_add(x[i])
            .wrapping_add(K3))
        .rotate_left(3);
        d = (d
            .wrapping_add(a ^ b ^ c)
            .wrapping_add(x[i + 8])
            .wrapping_add(K3))
        .rotate_left(9);
        c = (c
            .wrapping_add(d ^ a ^ b)
            .wrapping_add(x[i + 4])
            .wrapping_add(K3))
        .rotate_left(11);
        b = (b
            .wrapping_add(c ^ d ^ a)
            .wrapping_add(x[i + 12])
            .wrapping_add(K3))
        .rotate_left(15);
    }
    for (s, v) in state.iter_mut().zip([a, b, c, d]) {
        *s = s.wrapping_add(v);
    }
}

/// MD4 (RFC 1320).
///
/// MD4 is broken. It exists only because NTLM (MS-NLMP's `NTOWFv1`/`NTOWFv2`) is built on it;
/// never use it for anything new.
#[derive(Clone)]
pub struct Md4 {
    state: [u32; 4],
    buf: Buffer<64>,
}

impl Default for Md4 {
    fn default() -> Self {
        Self {
            state: [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476],
            buf: Buffer::default(),
        }
    }
}

impl Digest for Md4 {
    const OUTPUT_LEN: usize = 16;
    const BLOCK_LEN: usize = 64;
    type Output = [u8; 16];

    fn update(&mut self, data: &[u8]) {
        let state = &mut self.state;
        self.buf.update(data, |b| compress(state, b));
    }

    fn finalize(mut self) -> [u8; 16] {
        let state = &mut self.state;
        self.buf
            .finish(8, LengthOrder::Little, |b| compress(state, b));
        let mut out = [0u8; 16];
        for (o, s) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.state) {
            *o = s.to_le_bytes();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex;

    #[test]
    fn rfc1320_vectors() {
        let cases = [
            ("", "31d6cfe0d16ae931b73c59d7e0c089c0"),
            ("a", "bde52cb31de33e46245e05fbdbd6fb24"),
            ("abc", "a448017aaf21d8525fc10ae87aa6729d"),
            ("message digest", "d9130a8164549fe818874806e1c7014b"),
            (
                "abcdefghijklmnopqrstuvwxyz",
                "d79e1c308aa5bbcdeea8ed63df412da9",
            ),
            (
                "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                "043f8582f241db351ce627e153e7f0e4",
            ),
            (
                "12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "e33b4ddc9c38f2199c3e7b164fcc0536",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(hex(&Md4::digest(input)), want, "{input}");
        }
    }

    #[test]
    fn streaming_matches_one_shot() {
        let data: std::vec::Vec<u8> = (0..300u32).map(|i| i as u8).collect();
        for split in [0, 1, 55, 56, 63, 64, 65, 200, 300] {
            let h = Md4::default()
                .chain(&data[..split])
                .chain(&data[split..])
                .finalize();
            assert_eq!(h, Md4::digest(&data), "{split}");
        }
    }

    #[test]
    fn nt_hash_of_password() {
        // NTOWFv1("Password") from MS-NLMP 4.2.2.1.2: MD4 of the UTF-16LE password.
        let pw: std::vec::Vec<u8> = "Password"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(hex(&Md4::digest(pw)), "a4f49c406510bdcab6824ee7c30fd852");
    }
}
