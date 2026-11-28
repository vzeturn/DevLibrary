//! AES-256-GCM (FIPS 197, NIST SP 800-38D) with a 16-byte tag appended to the ciphertext,
//! ported from DevLibrary's `dl-agent-backup` crypto (the workspace has no AES crate). The
//! agent only opens what the cloud sealed (`CashierPinSeal`, ADR 0150); [`seal`] exists for
//! the test vectors and fixtures.
//!
//! The S-box is a table, so lookups are not constant-time. The agent only opens values the
//! cloud sent it, on its own machine; nothing here is an oracle for a remote attacker.

/// The tag length in bytes.
pub const TAG_LEN: usize = 16;

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
struct Aes256 {
    round_keys: [[u8; 16]; 15],
}

impl Aes256 {
    /// Expands a 32-byte key (FIPS 197 §5.2).
    fn new(key: &[u8; 32]) -> Aes256 {
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
    fn encrypt_block(&self, block: &mut [u8; 16]) {
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

/// Multiplication in GF(2^128) with GCM's bit order (SP 800-38D §6.3, Algorithm 1).
fn gf_mul(x: u128, y: u128) -> u128 {
    const R: u128 = 0xe1 << 120;
    let mut z = 0u128;
    let mut v = y;
    for i in 0..128 {
        if (x >> (127 - i)) & 1 == 1 {
            z ^= v;
        }
        v = if v & 1 == 1 { (v >> 1) ^ R } else { v >> 1 };
    }
    z
}

fn block_u128(chunk: &[u8]) -> u128 {
    let mut b = [0u8; 16];
    b[..chunk.len()].copy_from_slice(chunk);
    u128::from_be_bytes(b)
}

/// GHASH over `aad` and `data`, each zero-padded, then their bit lengths.
fn ghash(h: u128, aad: &[u8], data: &[u8]) -> u128 {
    let mut y = 0u128;
    for chunk in aad.chunks(16).chain(data.chunks(16)) {
        y = gf_mul(y ^ block_u128(chunk), h);
    }
    let lens = ((aad.len() as u128 * 8) << 64) | (data.len() as u128 * 8);
    gf_mul(y ^ lens, h)
}

struct Gcm {
    aes: Aes256,
    h: u128,
    j0: u128,
}

impl Gcm {
    fn new(key: &[u8; 32], iv: &[u8]) -> Result<Gcm, String> {
        if iv.is_empty() {
            return Err("gcm: empty iv".into());
        }
        let aes = Aes256::new(key);
        let mut zero = [0u8; 16];
        aes.encrypt_block(&mut zero);
        let h = u128::from_be_bytes(zero);
        let j0 = if iv.len() == 12 {
            (block_u128(iv) & !0xffff_ffffu128) | 1
        } else {
            ghash(h, &[], iv)
        };
        Ok(Gcm { aes, h, j0 })
    }

    fn encrypt_counter(&self, counter: u128) -> [u8; 16] {
        let mut b = counter.to_be_bytes();
        self.aes.encrypt_block(&mut b);
        b
    }

    /// GCTR from inc32(J0): XORs the key stream into `data`.
    fn ctr(&self, data: &mut [u8]) {
        let high = self.j0 & !0xffff_ffffu128;
        let mut low = self.j0 as u32;
        for chunk in data.chunks_mut(16) {
            low = low.wrapping_add(1);
            let ks = self.encrypt_counter(high | low as u128);
            for (b, k) in chunk.iter_mut().zip(ks) {
                *b ^= k;
            }
        }
    }

    fn tag(&self, aad: &[u8], ciphertext: &[u8]) -> [u8; 16] {
        let s = ghash(self.h, aad, ciphertext);
        let mask = u128::from_be_bytes(self.encrypt_counter(self.j0));
        (s ^ mask).to_be_bytes()
    }
}

/// Encrypts `plain` and returns ciphertext ‖ tag.
pub fn seal(key: &[u8; 32], iv: &[u8], aad: &[u8], plain: &[u8]) -> Result<Vec<u8>, String> {
    let gcm = Gcm::new(key, iv)?;
    let mut out = plain.to_vec();
    gcm.ctr(&mut out);
    let tag = gcm.tag(aad, &out);
    out.extend_from_slice(&tag);
    Ok(out)
}

/// Checks the tag of ciphertext ‖ tag and returns the plaintext.
pub fn open(key: &[u8; 32], iv: &[u8], aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, String> {
    let gcm = Gcm::new(key, iv)?;
    let split = sealed
        .len()
        .checked_sub(TAG_LEN)
        .ok_or("gcm: ciphertext shorter than the tag")?;
    let (ct, tag) = sealed.split_at(split);
    let want = gcm.tag(aad, ct);
    // Constant-time compare: fold every byte difference before deciding.
    let diff = want.iter().zip(tag).fold(0u8, |acc, (a, b)| acc | (a ^ b));
    if diff != 0 {
        return Err("gcm: message authentication failed".into());
    }
    let mut out = ct.to_vec();
    gcm.ctr(&mut out);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        dl_encoding::hex::decode(s).unwrap()
    }

    fn key(s: &str) -> [u8; 32] {
        hex(s).try_into().unwrap()
    }

    #[test]
    fn fips197_appendix_c3_aes256() {
        let key = key("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f");
        let mut block: [u8; 16] = hex("00112233445566778899aabbccddeeff").try_into().unwrap();
        Aes256::new(&key).encrypt_block(&mut block);
        assert_eq!(block.to_vec(), hex("8ea2b7ca516745bfeafc49904b496089"));
    }

    const K15: &str = "feffe9928665731c6d6a8f9467308308feffe9928665731c6d6a8f9467308308";
    const P15: &str = "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a72\
                       1c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b391aafd255";

    // McGrew & Viega, "The Galois/Counter Mode of Operation", test cases 13–16 (AES-256),
    // also in NIST's GCM validation suite.
    #[test]
    fn nist_gcm_test_case_13_empty() {
        assert_eq!(
            seal(&[0; 32], &[0; 12], &[], &[]).unwrap(),
            hex("530f8afbc74536b9a963b4f1c4cb738b")
        );
        assert_eq!(
            open(
                &[0; 32],
                &[0; 12],
                &[],
                &hex("530f8afbc74536b9a963b4f1c4cb738b")
            )
            .unwrap(),
            Vec::<u8>::new()
        );
    }

    #[test]
    fn nist_gcm_test_case_15_opens() {
        let sealed = hex(
            "522dc1f099567d07f47f37a32a84427d643a8cdcbfe5c0c97598a2bd2555d1aa\
             8cb08e48590dbb3da7b08b1056828838c5f61e6393ba7a0abcc9f662898015ad\
             b094dac5d93471bdec1a502270e3cc6c",
        );
        let iv = hex("cafebabefacedbaddecaf888");
        assert_eq!(open(&key(K15), &iv, &[], &sealed).unwrap(), hex(P15));
        assert_eq!(seal(&key(K15), &iv, &[], &hex(P15)).unwrap(), sealed);
    }

    #[test]
    fn nist_gcm_test_case_16_aad_and_partial_block_opens() {
        let aad = hex("feedfacedeadbeeffeedfacedeadbeefabaddad2");
        let sealed = hex(
            "522dc1f099567d07f47f37a32a84427d643a8cdcbfe5c0c97598a2bd2555d1aa\
             8cb08e48590dbb3da7b08b1056828838c5f61e6393ba7a0abcc9f662\
             76fc6ece0f4e1768cddf8853bb2d551b",
        );
        let iv = hex("cafebabefacedbaddecaf888");
        assert_eq!(
            open(&key(K15), &iv, &aad, &sealed).unwrap(),
            hex(P15)[..60].to_vec()
        );
        assert!(open(&key(K15), &iv, b"other", &sealed).is_err());
    }

    #[test]
    fn tampering_fails_authentication() {
        let mut out = seal(&[7; 32], &[1; 12], b"a", b"hello").unwrap();
        out[0] ^= 1;
        assert!(open(&[7; 32], &[1; 12], b"a", &out).is_err());
        assert!(open(&[7; 32], &[1; 12], b"a", &out[..10]).is_err());
    }
}
