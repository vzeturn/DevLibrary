use crate::{Digest, ct_eq};

/// Largest block size among supported digests (SHA-384/512).
const MAX_BLOCK: usize = 128;

/// HMAC (RFC 2104) over any [`Digest`].
#[derive(Clone)]
pub struct Hmac<D: Digest> {
    inner: D,
    outer: D,
}

impl<D: Digest> Hmac<D> {
    /// Creates a MAC keyed with `key`. Keys longer than the block size are hashed first.
    pub fn new(key: &[u8]) -> Self {
        let mut k = [0u8; MAX_BLOCK];
        if key.len() > D::BLOCK_LEN {
            let h = D::digest(key);
            k[..D::OUTPUT_LEN].copy_from_slice(h.as_ref());
        } else {
            k[..key.len()].copy_from_slice(key);
        }
        let mut ipad = [0u8; MAX_BLOCK];
        let mut opad = [0u8; MAX_BLOCK];
        for ((i, o), k) in ipad
            .iter_mut()
            .zip(opad.iter_mut())
            .zip(k.iter())
            .take(D::BLOCK_LEN)
        {
            *i = k ^ 0x36;
            *o = k ^ 0x5c;
        }
        let mut inner = D::default();
        inner.update(&ipad[..D::BLOCK_LEN]);
        let mut outer = D::default();
        outer.update(&opad[..D::BLOCK_LEN]);
        Self { inner, outer }
    }

    /// Feeds message data.
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// Feeds message data and returns `self`, for chaining.
    pub fn chain(mut self, data: impl AsRef<[u8]>) -> Self {
        self.update(data.as_ref());
        self
    }

    /// Returns the authentication tag.
    pub fn finalize(self) -> D::Output {
        let inner = self.inner.finalize();
        let mut outer = self.outer;
        outer.update(inner.as_ref());
        outer.finalize()
    }

    /// Checks `tag` against the computed tag in constant time.
    pub fn verify(self, tag: &[u8]) -> bool {
        ct_eq(self.finalize().as_ref(), tag)
    }

    /// Computes `HMAC(key, data)` in one call.
    pub fn mac(key: &[u8], data: impl AsRef<[u8]>) -> D::Output {
        Self::new(key).chain(data).finalize()
    }
}

/// PBKDF2 (RFC 8018) with HMAC-`D`. Fills `out` with derived key material.
///
/// Matches .NET `Rfc2898DeriveBytes.Pbkdf2` and Go `pbkdf2.Key`. OWASP (2023) recommends
/// at least 600 000 iterations for HMAC-SHA256 when hashing passwords.
///
/// # Panics
/// Panics if `iterations` is 0.
pub fn pbkdf2<D: Digest>(password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8]) {
    assert!(iterations > 0, "pbkdf2 needs at least one iteration");
    let prf = Hmac::<D>::new(password);
    for (i, chunk) in out.chunks_mut(D::OUTPUT_LEN).enumerate() {
        let block_index = (i as u32).wrapping_add(1);
        let mut u = prf
            .clone()
            .chain(salt)
            .chain(block_index.to_be_bytes())
            .finalize();
        let mut t = u;
        for _ in 1..iterations {
            u = prf.clone().chain(u).finalize();
            for (tb, ub) in t.as_mut().iter_mut().zip(u.as_ref()) {
                *tb ^= ub;
            }
        }
        chunk.copy_from_slice(&t.as_ref()[..chunk.len()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Sha1, Sha256, Sha384, Sha512, hex};

    #[test]
    fn rfc4231_sha2() {
        // Test case 1
        let key = [0x0b; 20];
        assert_eq!(
            hex(&Hmac::<Sha256>::mac(&key, b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hex(&Hmac::<Sha384>::mac(&key, b"Hi There")),
            "afd03944d84895626b0825f4ab46907f15f9dadbe4101ec682aa034c7cebc59c\
             faea9ea9076ede7f4af152e8b2fa9cb6"
        );
        assert_eq!(
            hex(&Hmac::<Sha512>::mac(&key, b"Hi There")),
            "87aa7cdea5ef619d4ff0b4241a1d6cb02379f4e2ce4ec2787ad0b30545e17cde\
             daa833b7d6b8a702038b274eaea3f4e4be9d914eeb61f1702e696c203a126854"
        );
        // Test case 2
        assert_eq!(
            hex(&Hmac::<Sha256>::mac(
                b"Jefe",
                b"what do ya want for nothing?"
            )),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // Test case 6: key longer than the block size.
        let key = [0xaa; 131];
        assert_eq!(
            hex(&Hmac::<Sha256>::mac(
                &key,
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
        assert_eq!(
            hex(&Hmac::<Sha512>::mac(
                &key,
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "80b24263c7c1a3ebb71493c1dd7be8b49b46d1f41b4aeec1121b013783f8f352\
             6b56d037e05f2598bd0fd2215d6a1e5295e64f73f63f0aec8b915a985d786598"
        );
    }

    #[test]
    fn rfc2202_sha1() {
        assert_eq!(
            hex(&Hmac::<Sha1>::mac(&[0x0b; 20], b"Hi There")),
            "b617318655057264e28bc0b6fb378c8ef146be00"
        );
        assert_eq!(
            hex(&Hmac::<Sha1>::mac(b"Jefe", b"what do ya want for nothing?")),
            "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79"
        );
    }

    #[test]
    fn verify() {
        let tag = Hmac::<Sha256>::mac(b"k", b"m");
        assert!(Hmac::<Sha256>::new(b"k").chain(b"m").verify(&tag));
        assert!(!Hmac::<Sha256>::new(b"k").chain(b"n").verify(&tag));
        assert!(!Hmac::<Sha256>::new(b"k").chain(b"m").verify(&tag[..31]));
    }

    #[test]
    fn rfc6070_pbkdf2_sha1() {
        let mut out = [0u8; 20];
        pbkdf2::<Sha1>(b"password", b"salt", 1, &mut out);
        assert_eq!(hex(&out), "0c60c80f961f0e71f3a9b524af6012062fe037a6");
        pbkdf2::<Sha1>(b"password", b"salt", 2, &mut out);
        assert_eq!(hex(&out), "ea6c014dc72d6f8ccd1ed92ace1d41f0d8de8957");
        pbkdf2::<Sha1>(b"password", b"salt", 4096, &mut out);
        assert_eq!(hex(&out), "4b007901b765489abead49d926f721d065a429c1");
        let mut out = [0u8; 25];
        pbkdf2::<Sha1>(
            b"passwordPASSWORDpassword",
            b"saltSALTsaltSALTsaltSALTsaltSALTsalt",
            4096,
            &mut out,
        );
        assert_eq!(
            hex(&out),
            "3d2eec4fe41c849b80c8d83662c0e44a8b291a964cf2f07038"
        );
    }

    #[test]
    fn pbkdf2_sha256() {
        let mut out = [0u8; 32];
        pbkdf2::<Sha256>(b"password", b"salt", 1, &mut out);
        assert_eq!(
            hex(&out),
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
        );
        pbkdf2::<Sha256>(b"password", b"salt", 4096, &mut out);
        assert_eq!(
            hex(&out),
            "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a"
        );
    }
}
