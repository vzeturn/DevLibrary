//! AES-256 block encryption (FIPS 197), forward direction only: GCM never decrypts a block.
//!
//! The S-box is computed at compile time (inverse in GF(2^8), then the affine map), so no
//! table is typed by hand. Lookups are table-indexed and therefore not constant-time; the
//! keys it protects never leave the hotel server, and the cloud side is .NET's AES-NI.

const fn gmul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0u8;
    while b != 0 {
        if b & 1 != 0 {
            p ^= a;
        }
        let hi = a & 0x80;
        a <<= 1;
        if hi != 0 {
            a ^= 0x1b;
        }
        b >>= 1;
    }
    p
}

const fn sbox_entry(x: u8) -> u8 {
    // x^254 is the multiplicative inverse (0 stays 0).
    let mut inv = 1u8;
    let mut base = x;
    let mut e = 254u32;
    while e > 0 {
        if e & 1 == 1 {
            inv = gmul(inv, base);
        }
        base = gmul(base, base);
        e >>= 1;
    }
    inv ^ inv.rotate_left(1) ^ inv.rotate_left(2) ^ inv.rotate_left(3) ^ inv.rotate_left(4) ^ 0x63
}

const SBOX: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = sbox_entry(i as u8);
        i += 1;
    }
    t
};

const RCON: [u8; 7] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40];
const ROUNDS: usize = 14;

/// An expanded AES-256 key.
#[derive(Clone)]
pub struct Aes256 {
    round_keys: [[u8; 16]; ROUNDS + 1],
}

fn sub_word(w: [u8; 4]) -> [u8; 4] {
    [
        SBOX[usize::from(w[0])],
        SBOX[usize::from(w[1])],
        SBOX[usize::from(w[2])],
        SBOX[usize::from(w[3])],
    ]
}

fn xtime(a: u8) -> u8 {
    (a << 1) ^ if a & 0x80 != 0 { 0x1b } else { 0 }
}

impl Aes256 {
    /// Expands `key`.
    pub fn new(key: &[u8; 32]) -> Aes256 {
        let mut w = [[0u8; 4]; 4 * (ROUNDS + 1)];
        for (i, word) in w.iter_mut().take(8).enumerate() {
            word.copy_from_slice(&key[4 * i..4 * i + 4]);
        }
        for i in 8..w.len() {
            let mut temp = w[i - 1];
            if i % 8 == 0 {
                temp.rotate_left(1);
                temp = sub_word(temp);
                temp[0] ^= RCON[i / 8 - 1];
            } else if i % 8 == 4 {
                temp = sub_word(temp);
            }
            for b in 0..4 {
                w[i][b] = w[i - 8][b] ^ temp[b];
            }
        }
        let mut round_keys = [[0u8; 16]; ROUNDS + 1];
        for (r, rk) in round_keys.iter_mut().enumerate() {
            for c in 0..4 {
                rk[4 * c..4 * c + 4].copy_from_slice(&w[4 * r + c]);
            }
        }
        Aes256 { round_keys }
    }

    /// Encrypts one block in place.
    pub fn encrypt_block(&self, block: &mut [u8; 16]) {
        add_round_key(block, &self.round_keys[0]);
        for key in &self.round_keys[1..ROUNDS] {
            sub_bytes(block);
            shift_rows(block);
            mix_columns(block);
            add_round_key(block, key);
        }
        sub_bytes(block);
        shift_rows(block);
        add_round_key(block, &self.round_keys[ROUNDS]);
    }
}

fn add_round_key(s: &mut [u8; 16], k: &[u8; 16]) {
    for (b, k) in s.iter_mut().zip(k) {
        *b ^= k;
    }
}

fn sub_bytes(s: &mut [u8; 16]) {
    for b in s.iter_mut() {
        *b = SBOX[usize::from(*b)];
    }
}

/// Row `r` (bytes `r`, `r+4`, `r+8`, `r+12`) rotates left by `r` columns.
fn shift_rows(s: &mut [u8; 16]) {
    let old = *s;
    for r in 1..4 {
        for c in 0..4 {
            s[r + 4 * c] = old[r + 4 * ((c + r) % 4)];
        }
    }
}

fn mix_columns(s: &mut [u8; 16]) {
    for c in 0..4 {
        let a = [s[4 * c], s[4 * c + 1], s[4 * c + 2], s[4 * c + 3]];
        let all = a[0] ^ a[1] ^ a[2] ^ a[3];
        for r in 0..4 {
            // 2a_r ^ 3a_{r+1} ^ a_{r+2} ^ a_{r+3} = a_r ^ all ^ 2(a_r ^ a_{r+1})
            s[4 * c + r] = a[r] ^ all ^ xtime(a[r] ^ a[(r + 1) % 4]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sbox_matches_fips_197() {
        assert_eq!(SBOX[0x00], 0x63);
        assert_eq!(SBOX[0x53], 0xed);
        assert_eq!(SBOX[0xff], 0x16);
    }

    #[test]
    fn fips_197_appendix_c3_vector() {
        let key: [u8; 32] = core::array::from_fn(|i| i as u8);
        let mut block: [u8; 16] = core::array::from_fn(|i| (i as u8) * 0x11);
        Aes256::new(&key).encrypt_block(&mut block);
        let hex: String = block.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "8ea2b7ca516745bfeafc49904b496089");
    }
}
