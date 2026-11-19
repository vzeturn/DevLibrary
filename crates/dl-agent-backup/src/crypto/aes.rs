//! AES-256 block encryption (FIPS 197), encrypt direction only: GCM runs the cipher in
//! counter mode, so it never needs the inverse cipher.
//!
//! The S-box is a table, so lookups are not constant-time. The agent only opens
//! credentials the hub sent it, on its own machine; nothing here is an oracle for a remote
//! attacker.

const SBOX: [u8; 256] = build_sbox();

/// The S-box from the multiplicative inverse in GF(2^8) and the affine map, walking the
/// field with generator 3 (FIPS 197 §5.1.1).
const fn build_sbox() -> [u8; 256] {
    let mut sbox = [0u8; 256];
    let mut p: u8 = 1;
    let mut q: u8 = 1;
    loop {
        // p ← p · 3
        p = p ^ (p << 1) ^ if p & 0x80 != 0 { 0x1b } else { 0 };
        // q ← q / 3
        q ^= q << 1;
        q ^= q << 2;
        q ^= q << 4;
        if q & 0x80 != 0 {
            q ^= 0x09;
        }
        let x = q ^ q.rotate_left(1) ^ q.rotate_left(2) ^ q.rotate_left(3) ^ q.rotate_left(4);
        sbox[p as usize] = x ^ 0x63;
        if p == 1 {
            break;
        }
    }
    sbox[0] = 0x63;
    sbox
}

const fn xtime(b: u8) -> u8 {
    (b << 1) ^ if b & 0x80 != 0 { 0x1b } else { 0 }
}

/// An expanded AES-256 key.
#[derive(Clone)]
pub struct Aes256 {
    round_keys: [[u8; 16]; 15],
}

impl Aes256 {
    /// Expands a 32-byte key (FIPS 197 §5.2).
    pub fn new(key: &[u8; 32]) -> Aes256 {
        let mut w = [[0u8; 4]; 60];
        for (i, word) in w.iter_mut().take(8).enumerate() {
            word.copy_from_slice(&key[4 * i..4 * i + 4]);
        }
        let mut rcon: u8 = 1;
        for i in 8..60 {
            let mut t = w[i - 1];
            if i % 8 == 0 {
                t = [
                    SBOX[t[1] as usize] ^ rcon,
                    SBOX[t[2] as usize],
                    SBOX[t[3] as usize],
                    SBOX[t[0] as usize],
                ];
                rcon = xtime(rcon);
            } else if i % 8 == 4 {
                t = t.map(|b| SBOX[b as usize]);
            }
            for j in 0..4 {
                w[i][j] = w[i - 8][j] ^ t[j];
            }
        }
        let mut round_keys = [[0u8; 16]; 15];
        for (r, rk) in round_keys.iter_mut().enumerate() {
            for c in 0..4 {
                rk[4 * c..4 * c + 4].copy_from_slice(&w[4 * r + c]);
            }
        }
        Aes256 { round_keys }
    }

    /// Encrypts one 16-byte block in place.
    pub fn encrypt_block(&self, block: &mut [u8; 16]) {
        add_round_key(block, &self.round_keys[0]);
        for rk in &self.round_keys[1..14] {
            sub_shift(block);
            mix_columns(block);
            add_round_key(block, rk);
        }
        sub_shift(block);
        add_round_key(block, &self.round_keys[14]);
    }
}

fn add_round_key(s: &mut [u8; 16], rk: &[u8; 16]) {
    for (b, k) in s.iter_mut().zip(rk) {
        *b ^= k;
    }
}

/// SubBytes then ShiftRows; the state is column-major (`s[4c + r]`).
fn sub_shift(s: &mut [u8; 16]) {
    let t = *s;
    for c in 0..4 {
        for r in 0..4 {
            s[4 * c + r] = SBOX[t[4 * ((c + r) % 4) + r] as usize];
        }
    }
}

fn mix_columns(s: &mut [u8; 16]) {
    for c in 0..4 {
        let col = [s[4 * c], s[4 * c + 1], s[4 * c + 2], s[4 * c + 3]];
        let all = col[0] ^ col[1] ^ col[2] ^ col[3];
        for r in 0..4 {
            s[4 * c + r] = col[r] ^ all ^ xtime(col[r] ^ col[(r + 1) % 4]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        dl_encoding::hex::decode(s).unwrap()
    }

    #[test]
    fn sbox_matches_fips197_figure7_corners() {
        assert_eq!(SBOX[0x00], 0x63);
        assert_eq!(SBOX[0x01], 0x7c);
        assert_eq!(SBOX[0x53], 0xed);
        assert_eq!(SBOX[0xff], 0x16);
    }

    #[test]
    fn fips197_appendix_c3_aes256() {
        let key: [u8; 32] = hex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f")
            .try_into()
            .unwrap();
        let mut block: [u8; 16] = hex("00112233445566778899aabbccddeeff").try_into().unwrap();
        Aes256::new(&key).encrypt_block(&mut block);
        assert_eq!(block.to_vec(), hex("8ea2b7ca516745bfeafc49904b496089"));
    }
}
