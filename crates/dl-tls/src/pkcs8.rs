//! The header of a PKCS#8 `PrivateKeyInfo` / `OneAsymmetricKey` (RFC 5208 §5,
//! RFC 5958 §2): just enough DER to tell which algorithm a key is for. The TLS stack of the OS
//! parses the key material itself.

use crate::KeyAlgorithm;

pub(crate) const MALFORMED: &str = "the private key is not unencrypted PKCS#8 DER";
pub(crate) const UNSUPPORTED_ALGORITHM: &str =
    "unsupported private key algorithm (ECDSA P-256 and RSA are supported)";
pub(crate) const UNSUPPORTED_CURVE: &str = "unsupported EC curve (only P-256 is supported)";

/// `rsaEncryption`, 1.2.840.113549.1.1.1 (RFC 8017 appendix C), as DER OID contents.
const RSA_ENCRYPTION: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
/// `id-ecPublicKey`, 1.2.840.10045.2.1 (RFC 5480 §2.1.1).
const EC_PUBLIC_KEY: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
/// `secp256r1` (P-256), 1.2.840.10045.3.1.7 (RFC 5480 §2.1.1.1).
const SECP256R1: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];

const INTEGER: u8 = 0x02;
const OCTET_STRING: u8 = 0x04;
const NULL: u8 = 0x05;
const OID: u8 = 0x06;
const SEQUENCE: u8 = 0x30;

/// Splits one DER element (X.690 §8.1) off `input`: its tag, contents and the bytes after
/// it. Definite lengths of up to four length bytes only; `None` when anything is short.
fn element(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let n = usize::from(first & 0x7f);
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        let len = rest[..n].iter().try_fold(0usize, |acc, &b| {
            acc.checked_mul(256)?.checked_add(usize::from(b))
        })?;
        (len, &rest[n..])
    };
    if rest.len() < len {
        return None;
    }
    Some((tag, &rest[..len], &rest[len..]))
}

/// The algorithm of the PKCS#8 key `der`, or why it cannot be used.
///
/// ```text
/// PrivateKeyInfo ::= SEQUENCE {
///   version             INTEGER,                -- 0 (RFC 5208) or 1 (RFC 5958)
///   privateKeyAlgorithm AlgorithmIdentifier,    -- SEQUENCE { OID, parameters }
///   privateKey          OCTET STRING,
///   ... }                                       -- optional attributes and public key
/// ```
pub(crate) fn algorithm(der: &[u8]) -> Result<KeyAlgorithm, &'static str> {
    let (tag, info, after) = element(der).ok_or(MALFORMED)?;
    if tag != SEQUENCE || !after.is_empty() {
        return Err(MALFORMED);
    }
    let (tag, version, rest) = element(info).ok_or(MALFORMED)?;
    if tag != INTEGER || (version != [0] && version != [1]) {
        return Err(MALFORMED);
    }
    let (tag, algorithm_id, rest) = element(rest).ok_or(MALFORMED)?;
    if tag != SEQUENCE {
        return Err(MALFORMED);
    }
    let (tag, key, _optional) = element(rest).ok_or(MALFORMED)?;
    if tag != OCTET_STRING || key.is_empty() {
        return Err(MALFORMED);
    }
    let (tag, oid, params) = element(algorithm_id).ok_or(MALFORMED)?;
    if tag != OID {
        return Err(MALFORMED);
    }
    if oid == RSA_ENCRYPTION {
        // The parameters are NULL (RFC 8017 appendix C); some encoders omit them.
        return if params.is_empty() || params == [NULL, 0] {
            Ok(KeyAlgorithm::Rsa)
        } else {
            Err(MALFORMED)
        };
    }
    if oid == EC_PUBLIC_KEY {
        // namedCurve OID (RFC 5480 §2.1.1); implicit and specified curves are not allowed.
        return match element(params) {
            Some((OID, curve, [])) if curve == SECP256R1 => Ok(KeyAlgorithm::EcdsaP256),
            _ => Err(UNSUPPORTED_CURVE),
        };
    }
    Err(UNSUPPORTED_ALGORITHM)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Keys made by OpenSSL (testdata/gen.sh), an implementation independent of this parser.
    const EC: &[u8] = include_bytes!("../testdata/ec-key.p8.der");
    const RSA: &[u8] = include_bytes!("../testdata/rsa-key.p8.der");
    const LEAF: &[u8] = include_bytes!("../testdata/leaf-key.p8.der");
    const P384: &[u8] = include_bytes!("../testdata/p384-key.p8.der");
    /// The Ed25519 example private key of RFC 8410 §10.3.
    const ED25519: &[u8] = &[
        0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04,
        0x20, 0xd4, 0xee, 0x72, 0xdb, 0xf9, 0x13, 0x58, 0x4a, 0xd5, 0xb6, 0xd8, 0xf1, 0xf7, 0x69,
        0xf8, 0xad, 0x3a, 0xfe, 0x7c, 0x28, 0xcb, 0xf1, 0xd4, 0xfb, 0xe0, 0x97, 0xa8, 0x8f, 0x44,
        0x75, 0x58, 0x42,
    ];

    #[test]
    fn recognises_supported_algorithms() {
        assert_eq!(algorithm(EC), Ok(KeyAlgorithm::EcdsaP256));
        assert_eq!(algorithm(LEAF), Ok(KeyAlgorithm::EcdsaP256));
        assert_eq!(algorithm(RSA), Ok(KeyAlgorithm::Rsa));
    }

    #[test]
    fn refuses_other_algorithms_and_curves() {
        assert_eq!(algorithm(P384), Err(UNSUPPORTED_CURVE));
        assert_eq!(algorithm(ED25519), Err(UNSUPPORTED_ALGORITHM));
    }

    #[test]
    fn refuses_malformed_input_without_panicking() {
        assert_eq!(algorithm(&[]), Err(MALFORMED));
        for n in 0..EC.len() {
            assert_eq!(algorithm(&EC[..n]), Err(MALFORMED), "prefix of {n} bytes");
        }
        let mut trailing = EC.to_vec();
        trailing.push(0);
        assert_eq!(algorithm(&trailing), Err(MALFORMED));
        // Version 2 does not exist (RFC 5958 defines v1 = 0 and v2 = 1).
        let mut v3 = EC.to_vec();
        let at = v3.windows(3).position(|w| w == [0x02, 0x01, 0x00]).unwrap();
        v3[at + 2] = 2;
        assert_eq!(algorithm(&v3), Err(MALFORMED));
        // Flipping any single byte never panics.
        for i in 0..RSA.len() {
            let mut k = RSA.to_vec();
            k[i] ^= 0xff;
            let _ = algorithm(&k);
        }
        // Indefinite and over-long lengths.
        assert_eq!(algorithm(&[0x30, 0x80, 0x00, 0x00]), Err(MALFORMED));
        assert_eq!(algorithm(&[0x30, 0x85, 1, 0, 0, 0, 0]), Err(MALFORMED));
    }
}
