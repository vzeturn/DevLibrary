//! AES-256-GCM with a 12-byte nonce and a 16-byte tag (NIST SP 800-38D), the AEAD .NET's
//! `AesGcm` provides. Output is `ciphertext ‖ tag`, as the envelope and the key vault store it.

use super::aes::Aes256;

/// Tag length.
pub const TAG_LEN: usize = 16;
/// Nonce length.
pub const NONCE_LEN: usize = 12;

/// An AES-256-GCM key.
#[derive(Clone)]
pub struct AesGcm {
    cipher: Aes256,
    h: u128,
}

const R: u128 = 0xE1 << 120;

fn gf_mul(x: u128, y: u128) -> u128 {
    let mut z = 0u128;
    let mut v = y;
    for i in 0..128 {
        // Branch-free select keeps the timing independent of the bits.
        let bit = (x >> (127 - i)) & 1;
        z ^= v & 0u128.wrapping_sub(bit);
        let lsb = v & 1;
        v = (v >> 1) ^ (R & 0u128.wrapping_sub(lsb));
    }
    z
}

fn block_u128(chunk: &[u8]) -> u128 {
    let mut b = [0u8; 16];
    b[..chunk.len()].copy_from_slice(chunk);
    u128::from_be_bytes(b)
}

impl AesGcm {
    /// A GCM context for `key`.
    pub fn new(key: &[u8; 32]) -> AesGcm {
        let cipher = Aes256::new(key);
        let mut h = [0u8; 16];
        cipher.encrypt_block(&mut h);
        AesGcm {
            cipher,
            h: u128::from_be_bytes(h),
        }
    }

    fn ghash(&self, aad: &[u8], ct: &[u8]) -> u128 {
        let mut x = 0u128;
        for chunk in aad.chunks(16) {
            x = gf_mul(x ^ block_u128(chunk), self.h);
        }
        for chunk in ct.chunks(16) {
            x = gf_mul(x ^ block_u128(chunk), self.h);
        }
        let lens = (u128::from(aad.len() as u64 * 8) << 64) | u128::from(ct.len() as u64 * 8);
        gf_mul(x ^ lens, self.h)
    }

    fn ctr(&self, nonce: &[u8; NONCE_LEN], data: &mut [u8]) {
        let mut counter = 2u32;
        for chunk in data.chunks_mut(16) {
            let mut ks = [0u8; 16];
            ks[..12].copy_from_slice(nonce);
            ks[12..].copy_from_slice(&counter.to_be_bytes());
            self.cipher.encrypt_block(&mut ks);
            for (b, k) in chunk.iter_mut().zip(ks) {
                *b ^= k;
            }
            counter = counter.wrapping_add(1);
        }
    }

    fn tag(&self, nonce: &[u8; NONCE_LEN], aad: &[u8], ct: &[u8]) -> [u8; TAG_LEN] {
        let mut j0 = [0u8; 16];
        j0[..12].copy_from_slice(nonce);
        j0[15] = 1;
        self.cipher.encrypt_block(&mut j0);
        (u128::from_be_bytes(j0) ^ self.ghash(aad, ct)).to_be_bytes()
    }

    /// Encrypts `plain`; returns `ciphertext ‖ tag`.
    pub fn seal(&self, nonce: &[u8; NONCE_LEN], plain: &[u8], aad: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(plain.len() + TAG_LEN);
        out.extend_from_slice(plain);
        self.ctr(nonce, &mut out);
        let tag = self.tag(nonce, aad, &out);
        out.extend_from_slice(&tag);
        out
    }

    /// Decrypts `ciphertext ‖ tag`; `None` when the tag does not verify (or `data` is
    /// shorter than a tag).
    pub fn open(&self, nonce: &[u8; NONCE_LEN], data: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
        let split = data.len().checked_sub(TAG_LEN)?;
        let (ct, tag) = data.split_at(split);
        if !dl_hash::ct_eq(&self.tag(nonce, aad, ct), tag) {
            return None;
        }
        let mut plain = ct.to_vec();
        self.ctr(nonce, &mut plain);
        Some(plain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_what_it_seals_and_rejects_tampering() {
        let g = AesGcm::new(&[7u8; 32]);
        let nonce = [1u8; 12];
        let sealed = g.seal(&nonce, b"hello world, longer than a block", b"aad");
        assert_eq!(
            g.open(&nonce, &sealed, b"aad").as_deref(),
            Some(&b"hello world, longer than a block"[..])
        );
        assert!(g.open(&nonce, &sealed, b"aaD").is_none());
        let mut bad = sealed.clone();
        bad[0] ^= 1;
        assert!(g.open(&nonce, &bad, b"aad").is_none());
        assert!(g.open(&nonce, &sealed[..10], b"aad").is_none());
    }
}
