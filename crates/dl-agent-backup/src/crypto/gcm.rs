//! AES-256-GCM (NIST SP 800-38D) with any IV length and a 16-byte tag appended to the
//! ciphertext, the layout WebCrypto and Go's `cipher.AEAD` both use.

use super::aes::Aes256;

/// The tag length in bytes.
pub const TAG_LEN: usize = 16;

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

    const K15: &str = "feffe9928665731c6d6a8f9467308308feffe9928665731c6d6a8f9467308308";
    const P15: &str = "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a72\
                       1c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b391aafd255";

    // Vectors: McGrew & Viega, "The Galois/Counter Mode of Operation", test cases 13–18
    // (AES-256), also published in NIST's GCM validation suite.
    #[test]
    fn test_case_13_empty() {
        let out = seal(&[0; 32], &[0; 12], &[], &[]).unwrap();
        assert_eq!(out, hex("530f8afbc74536b9a963b4f1c4cb738b"));
    }

    #[test]
    fn test_case_14_one_zero_block() {
        let out = seal(&[0; 32], &[0; 12], &[], &[0; 16]).unwrap();
        assert_eq!(
            out,
            hex("cea7403d4d606b6e074ec5d3baf39d18d0d1c8a799996bf0265b98b5d48ab919")
        );
    }

    #[test]
    fn test_case_15_four_blocks() {
        let out = seal(&key(K15), &hex("cafebabefacedbaddecaf888"), &[], &hex(P15)).unwrap();
        let want = "522dc1f099567d07f47f37a32a84427d643a8cdcbfe5c0c97598a2bd2555d1aa\
                    8cb08e48590dbb3da7b08b1056828838c5f61e6393ba7a0abcc9f662898015ad\
                    b094dac5d93471bdec1a502270e3cc6c";
        assert_eq!(out, hex(want));
        assert_eq!(
            open(&key(K15), &hex("cafebabefacedbaddecaf888"), &[], &out).unwrap(),
            hex(P15)
        );
    }

    #[test]
    fn test_case_16_aad_and_partial_block() {
        let plain = &hex(P15)[..60];
        let aad = hex("feedfacedeadbeeffeedfacedeadbeefabaddad2");
        let out = seal(&key(K15), &hex("cafebabefacedbaddecaf888"), &aad, plain).unwrap();
        let want = "522dc1f099567d07f47f37a32a84427d643a8cdcbfe5c0c97598a2bd2555d1aa\
                    8cb08e48590dbb3da7b08b1056828838c5f61e6393ba7a0abcc9f662\
                    76fc6ece0f4e1768cddf8853bb2d551b";
        assert_eq!(out, hex(want));
    }

    #[test]
    fn test_case_17_short_iv() {
        let plain = &hex(P15)[..60];
        let aad = hex("feedfacedeadbeeffeedfacedeadbeefabaddad2");
        let out = seal(&key(K15), &hex("cafebabefacedbad"), &aad, plain).unwrap();
        let want = "c3762df1ca787d32ae47c13bf19844cbaf1ae14d0b976afac52ff7d79bba9de0\
                    feb582d33934a4f0954cc2363bc73f7862ac430e64abe499f47c9b1f\
                    3a337dbf46a792c45e454913fe2ea8f2";
        assert_eq!(out, hex(want));
    }

    #[test]
    fn test_case_18_long_iv() {
        let plain = &hex(P15)[..60];
        let aad = hex("feedfacedeadbeeffeedfacedeadbeefabaddad2");
        let iv = "9313225df88406e555909c5aff5269aa6a7a9538534f7da1e4c303d2a318a728\
                  c3c0c95156809539fcf0e2429a6b525416aedbf5a0de6a57a637b39b";
        let out = seal(&key(K15), &hex(iv), &aad, plain).unwrap();
        let want = "5a8def2f0c9e53f1f75d7853659e2a20eeb2b22aafde6419a058ab4f6f746bf4\
                    0fc0c3b780f244452da3ebf1c5d82cdea2418997200ef82e44ae7e3f\
                    a44a8266ee1c8eb0c8b5d4cf5ae9f19a";
        assert_eq!(out, hex(want));
    }

    #[test]
    fn tampering_fails_authentication() {
        let mut out = seal(&[7; 32], &[1; 12], b"a", b"hello").unwrap();
        out[0] ^= 1;
        assert!(open(&[7; 32], &[1; 12], b"a", &out).is_err());
        assert!(open(&[7; 32], &[1; 12], b"a", &out[..10]).is_err());
    }
}
