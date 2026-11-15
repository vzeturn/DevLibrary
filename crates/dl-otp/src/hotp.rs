use crate::{Error, Secret, uri};
use alloc::string::String;
use core::fmt;
use core::str::FromStr;
use dl_hash::{Hmac, Sha1, Sha256, Sha512};

/// Smallest and largest supported number of digits.
pub(crate) const MIN_DIGITS: u8 = 6;
pub(crate) const MAX_DIGITS: u8 = 10;

/// HMAC hash function used to compute codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Algorithm {
    /// HMAC-SHA-1, the RFC 4226 default and the only one every authenticator app supports.
    #[default]
    Sha1,
    /// HMAC-SHA-256 (RFC 6238).
    Sha256,
    /// HMAC-SHA-512 (RFC 6238).
    Sha512,
}

impl Algorithm {
    /// Name used in otpauth URIs: `"SHA1"`, `"SHA256"` or `"SHA512"`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Algorithm::Sha1 => "SHA1",
            Algorithm::Sha256 => "SHA256",
            Algorithm::Sha512 => "SHA512",
        }
    }

    /// Recommended secret length in bytes: the hash output size (20, 32 or 64), as used
    /// for the RFC 6238 test seeds.
    pub const fn secret_len(self) -> usize {
        match self {
            Algorithm::Sha1 => 20,
            Algorithm::Sha256 => 32,
            Algorithm::Sha512 => 64,
        }
    }
}

impl fmt::Display for Algorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Algorithm {
    type Err = Error;

    /// Parses `SHA1`, `SHA256` or `SHA512`, ignoring ASCII case.
    fn from_str(s: &str) -> Result<Self, Error> {
        [Algorithm::Sha1, Algorithm::Sha256, Algorithm::Sha512]
            .into_iter()
            .find(|a| a.as_str().eq_ignore_ascii_case(s))
            .ok_or(Error::UnknownAlgorithm)
    }
}

/// An HMAC keyed once, cloned for each counter value.
#[derive(Clone)]
pub(crate) enum Keyed {
    Sha1(Hmac<Sha1>),
    Sha256(Hmac<Sha256>),
    Sha512(Hmac<Sha512>),
}

impl Keyed {
    pub(crate) fn new(algorithm: Algorithm, key: &[u8]) -> Self {
        match algorithm {
            Algorithm::Sha1 => Keyed::Sha1(Hmac::new(key)),
            Algorithm::Sha256 => Keyed::Sha256(Hmac::new(key)),
            Algorithm::Sha512 => Keyed::Sha512(Hmac::new(key)),
        }
    }

    /// RFC 4226 section 5.3: HMAC of the 8-byte big-endian counter, then dynamic
    /// truncation to a 31-bit integer.
    pub(crate) fn truncate(&self, counter: u64) -> u32 {
        let msg = counter.to_be_bytes();
        match self {
            Keyed::Sha1(h) => dynamic_truncation(h.clone().chain(msg).finalize().as_ref()),
            Keyed::Sha256(h) => dynamic_truncation(h.clone().chain(msg).finalize().as_ref()),
            Keyed::Sha512(h) => dynamic_truncation(h.clone().chain(msg).finalize().as_ref()),
        }
    }

    pub(crate) fn code(&self, counter: u64, digits: u8) -> Code {
        Code::new(self.truncate(counter), digits)
    }
}

fn dynamic_truncation(mac: &[u8]) -> u32 {
    // Every supported MAC is at least 20 bytes and the offset is at most 15, so
    // `offset + 4 <= 19 < mac.len()`; the fallbacks are unreachable.
    let offset = usize::from(mac.last().copied().unwrap_or(0) & 0x0f);
    let mut word = [0u8; 4];
    if let Some(bytes) = mac.get(offset..offset + 4) {
        word.copy_from_slice(bytes);
    }
    u32::from_be_bytes(word) & 0x7fff_ffff
}

/// A generated one-time code: `digits` ASCII digits, zero-padded on the left.
///
/// `==` compares in constant time. Use [`as_str`](Self::as_str) or `Display` to show it.
#[derive(Clone, Copy)]
pub struct Code {
    buf: [u8; MAX_DIGITS as usize],
    len: u8,
}

impl Code {
    /// `value mod 10^digits`, zero-padded. `digits` is in `MIN_DIGITS..=MAX_DIGITS`.
    fn new(value: u32, digits: u8) -> Self {
        let mut buf = [b'0'; MAX_DIGITS as usize];
        let mut v = value;
        for slot in buf[..usize::from(digits)].iter_mut().rev() {
            *slot = b'0' + (v % 10) as u8;
            v /= 10;
        }
        Self { buf, len: digits }
    }

    /// The code as a string of ASCII digits, for example `"012345"`.
    pub fn as_str(&self) -> &str {
        // The buffer only ever holds ASCII digits, so this cannot fail.
        core::str::from_utf8(self.as_bytes()).unwrap_or("")
    }

    /// The code as ASCII bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..usize::from(self.len)]
    }

    /// Number of digits (6..=10).
    pub fn digits(&self) -> u8 {
        self.len
    }

    /// The numeric value (leading zeros dropped).
    pub fn value(&self) -> u32 {
        self.as_bytes()
            .iter()
            .fold(0, |acc, &d| acc * 10 + u32::from(d - b'0'))
    }
}

impl PartialEq for Code {
    fn eq(&self, other: &Self) -> bool {
        dl_hash::ct_eq(self.as_bytes(), other.as_bytes())
    }
}

impl Eq for Code {}

impl PartialEq<str> for Code {
    fn eq(&self, other: &str) -> bool {
        dl_hash::ct_eq(self.as_bytes(), other.as_bytes())
    }
}

impl PartialEq<&str> for Code {
    fn eq(&self, other: &&str) -> bool {
        dl_hash::ct_eq(self.as_bytes(), other.as_bytes())
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

impl fmt::Debug for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Code").field(&self.as_str()).finish()
    }
}

/// Normalises user input: drops ASCII whitespace, then requires exactly `digits` ASCII
/// digits. Returns the digits, or `None` for anything else. Never panics, and stops
/// reading as soon as the input is known to be invalid.
pub(crate) fn normalize_code(input: &str, digits: u8) -> Option<Code> {
    let mut code = Code {
        buf: [0; MAX_DIGITS as usize],
        len: 0,
    };
    for b in input.bytes() {
        if b.is_ascii_whitespace() {
            continue;
        }
        if !b.is_ascii_digit() || code.len >= digits {
            return None;
        }
        code.buf[usize::from(code.len)] = b;
        code.len += 1;
    }
    (code.len == digits).then_some(code)
}

pub(crate) fn check_digits(digits: u8) -> Result<u8, Error> {
    if (MIN_DIGITS..=MAX_DIGITS).contains(&digits) {
        Ok(digits)
    } else {
        Err(Error::InvalidDigits(digits))
    }
}

/// HMAC-based one-time passwords (RFC 4226).
///
/// The counter is kept by the caller: after a successful [`verify`](Self::verify) store
/// `matched + 1` so that the same code cannot be used twice.
///
/// ```
/// use dl_otp::{Hotp, Secret};
///
/// let hotp = Hotp::new(Secret::from_bytes(b"12345678901234567890")?);
/// assert_eq!(hotp.generate(0).as_str(), "755224");
/// // The user is two presses ahead; a look-ahead of 5 finds counter 2.
/// assert_eq!(hotp.verify("359 152", 0, 5), Some(2));
/// # Ok::<(), dl_otp::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct Hotp {
    secret: Secret,
    digits: u8,
    algorithm: Algorithm,
}

impl Hotp {
    /// HOTP with the defaults every app supports: 6 digits, SHA-1.
    pub fn new(secret: Secret) -> Self {
        Self {
            secret,
            digits: 6,
            algorithm: Algorithm::Sha1,
        }
    }

    /// HOTP with explicit parameters. `digits` must be in `6..=10`.
    ///
    /// With 10 digits the code is the whole 31-bit truncated value (at most
    /// 2 147 483 647) zero-padded to 10 places, so the first digit is always 0, 1 or 2.
    pub fn with_params(secret: Secret, digits: u8, algorithm: Algorithm) -> Result<Self, Error> {
        Ok(Self {
            secret,
            digits: check_digits(digits)?,
            algorithm,
        })
    }

    /// The shared secret.
    pub fn secret(&self) -> &Secret {
        &self.secret
    }

    /// Number of digits per code.
    pub fn digits(&self) -> u8 {
        self.digits
    }

    /// HMAC algorithm.
    pub fn algorithm(&self) -> Algorithm {
        self.algorithm
    }

    pub(crate) fn keyed(&self) -> Keyed {
        Keyed::new(self.algorithm, self.secret.as_bytes())
    }

    /// The code for `counter`.
    pub fn generate(&self, counter: u64) -> Code {
        self.keyed().code(counter, self.digits)
    }

    /// Checks `code` against counters `counter ..= counter + look_ahead` and returns the
    /// first counter that matches. Store `matched + 1` as the next expected counter.
    ///
    /// `code` may contain ASCII whitespace (`"123 456"`); anything that is not exactly
    /// [`digits`](Self::digits) ASCII digits is rejected with `None` before any HMAC is
    /// computed. Each candidate is compared in constant time. The search stops at the
    /// first match, so the run time reveals how far ahead the matching counter was, but
    /// nothing about the code itself.
    ///
    /// The cost is one HMAC per candidate: keep `look_ahead` small (RFC 4226 section 7.4
    /// suggests a bounded resynchronisation window) and rate-limit attempts, because every
    /// extra candidate also gives an attacker one more chance to guess.
    pub fn verify(&self, code: &str, counter: u64, look_ahead: u32) -> Option<u64> {
        let input = normalize_code(code, self.digits)?;
        let keyed = self.keyed();
        let last = counter.saturating_add(u64::from(look_ahead));
        (counter..=last).find(|&c| keyed.code(c, self.digits) == input)
    }

    /// Builds an `otpauth://hotp/...` URI with `counter` as the initial counter. See
    /// [`Totp::to_uri`](crate::Totp::to_uri) for the label and encoding rules.
    pub fn to_uri(&self, issuer: &str, account: &str, counter: u64) -> Result<String, Error> {
        uri::build(uri::Params {
            kind: crate::OtpKind::Hotp,
            issuer,
            account,
            secret: &self.secret,
            algorithm: self.algorithm,
            digits: self.digits,
            period: 0,
            counter,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::ToString;
    use std::{format, vec::Vec};

    const RFC_SEED: &[u8] = b"12345678901234567890";

    fn hotp(digits: u8, alg: Algorithm, seed: &[u8]) -> Hotp {
        Hotp::with_params(Secret::from_bytes(seed).unwrap(), digits, alg).unwrap()
    }

    #[test]
    fn rfc4226_appendix_d() {
        // Intermediate truncated values from Appendix D as well as the 6-digit codes.
        let truncated = [
            1284755224u32,
            1094287082,
            137359152,
            1726969429,
            1640338314,
            868254676,
            1918287922,
            82162583,
            673399871,
            645520489,
        ];
        let codes = [
            "755224", "287082", "359152", "969429", "338314", "254676", "287922", "162583",
            "399871", "520489",
        ];
        let h = Hotp::new(Secret::from_bytes(RFC_SEED).unwrap());
        let keyed = h.keyed();
        for (c, (&t, &code)) in truncated.iter().zip(&codes).enumerate() {
            assert_eq!(keyed.truncate(c as u64), t, "counter {c}");
            assert_eq!(h.generate(c as u64).as_str(), code, "counter {c}");
            assert_eq!(h.generate(c as u64), code);
        }
    }

    #[test]
    fn digits_7_to_10_python_cross_check() {
        // testdata/cross_check.py (Python hmac/hashlib).
        let expected: [(u8, [&str; 10]); 4] = [
            (
                7,
                [
                    "4755224", "4287082", "7359152", "6969429", "0338314", "8254676", "8287922",
                    "2162583", "3399871", "5520489",
                ],
            ),
            (
                8,
                [
                    "84755224", "94287082", "37359152", "26969429", "40338314", "68254676",
                    "18287922", "82162583", "73399871", "45520489",
                ],
            ),
            (
                9,
                [
                    "284755224",
                    "094287082",
                    "137359152",
                    "726969429",
                    "640338314",
                    "868254676",
                    "918287922",
                    "082162583",
                    "673399871",
                    "645520489",
                ],
            ),
            (
                10,
                [
                    "1284755224",
                    "1094287082",
                    "0137359152",
                    "1726969429",
                    "1640338314",
                    "0868254676",
                    "1918287922",
                    "0082162583",
                    "0673399871",
                    "0645520489",
                ],
            ),
        ];
        for (digits, codes) in expected {
            let h = hotp(digits, Algorithm::Sha1, RFC_SEED);
            for (c, code) in codes.iter().enumerate() {
                let got = h.generate(c as u64);
                assert_eq!(got.as_str(), *code);
                assert_eq!(got.digits(), digits);
                assert_eq!(h.verify(code, c as u64, 0), Some(c as u64));
            }
        }
        // 10 digits = the full 31-bit value.
        assert_eq!(
            hotp(10, Algorithm::Sha1, RFC_SEED).generate(0).value(),
            1284755224
        );
    }

    #[test]
    fn sha256_sha512_python_cross_check() {
        let s256 = hotp(6, Algorithm::Sha256, b"12345678901234567890123456789012");
        let got: Vec<_> = (0..5).map(|c| s256.generate(c).to_string()).collect();
        assert_eq!(got, ["920136", "119246", "882438", "975832", "791148"]);
        let s512 = hotp(
            6,
            Algorithm::Sha512,
            b"1234567890123456789012345678901234567890123456789012345678901234",
        );
        let got: Vec<_> = (0..5).map(|c| s512.generate(c).to_string()).collect();
        assert_eq!(got, ["550594", "693936", "765371", "628588", "977270"]);
    }

    #[test]
    fn verify_look_ahead() {
        let h = Hotp::new(Secret::from_bytes(RFC_SEED).unwrap());
        assert_eq!(h.verify("755224", 0, 0), Some(0));
        assert_eq!(h.verify("969429", 0, 2), None); // counter 3, window 0..=2
        assert_eq!(h.verify("969429", 0, 3), Some(3));
        assert_eq!(h.verify("969429", 3, 0), Some(3));
        // Codes behind the stored counter are never accepted (replay).
        assert_eq!(h.verify("755224", 1, 10), None);
        // No overflow at the top of the counter range.
        assert_eq!(
            h.verify(h.generate(u64::MAX).as_str(), u64::MAX, u32::MAX),
            Some(u64::MAX)
        );
        assert_eq!(
            h.verify(h.generate(u64::MAX).as_str(), u64::MAX - 1, 5),
            Some(u64::MAX)
        );
    }

    #[test]
    fn code_normalisation() {
        let h = Hotp::new(Secret::from_bytes(RFC_SEED).unwrap());
        for ok in ["755224", "755 224", " 755224 ", "7 5 5 2 2 4", "755\t224\n"] {
            assert_eq!(h.verify(ok, 0, 0), Some(0), "{ok:?}");
        }
        for bad in [
            "",
            " ",
            "75522",
            "7552240",
            "755-224",
            "75522a",
            "+755224",
            "７５５２２４",
            "755224\0",
            "0755224",
            "-755224",
        ] {
            assert_eq!(h.verify(bad, 0, 0), None, "{bad:?}");
        }
        // Long garbage is rejected quickly and without panicking.
        let long = "1".repeat(1 << 20);
        assert_eq!(h.verify(&long, 0, 0), None);
        let spaces = format!("{}755224", " ".repeat(1 << 16));
        assert_eq!(h.verify(&spaces, 0, 0), Some(0));
    }

    #[test]
    fn digits_validation() {
        let s = || Secret::from_bytes(RFC_SEED).unwrap();
        for d in [0, 1, 5, 11, 255] {
            assert_eq!(
                Hotp::with_params(s(), d, Algorithm::Sha1).unwrap_err(),
                Error::InvalidDigits(d)
            );
        }
        for d in 6..=10 {
            let h = Hotp::with_params(s(), d, Algorithm::Sha256).unwrap();
            assert_eq!(h.digits(), d);
            assert_eq!(h.algorithm(), Algorithm::Sha256);
            assert_eq!(h.generate(1).as_str().len(), usize::from(d));
        }
    }

    #[test]
    fn code_type() {
        let c = Code::new(42, 6);
        assert_eq!(c.as_str(), "000042");
        assert_eq!(c.value(), 42);
        assert_eq!(format!("{c}"), "000042");
        assert_eq!(format!("{c:>8}"), "  000042");
        assert_eq!(format!("{c:?}"), "Code(\"000042\")");
        assert_eq!(c, "000042");
        assert_ne!(c, Code::new(43, 6));
        assert_ne!(c, Code::new(42, 7));
        let max = Code::new(0x7fff_ffff, 10);
        assert_eq!(max.as_str(), "2147483647");
        assert_eq!(max.value(), 2147483647);
    }

    #[test]
    fn algorithm_names() {
        for a in [Algorithm::Sha1, Algorithm::Sha256, Algorithm::Sha512] {
            assert_eq!(a.as_str().parse::<Algorithm>().unwrap(), a);
            assert_eq!(
                a.as_str()
                    .to_ascii_lowercase()
                    .parse::<Algorithm>()
                    .unwrap(),
                a
            );
            assert_eq!(a.to_string(), a.as_str());
        }
        assert_eq!(Algorithm::default(), Algorithm::Sha1);
        for bad in ["", "SHA-1", "MD5", "SHA384", "sha1 "] {
            assert_eq!(bad.parse::<Algorithm>(), Err(Error::UnknownAlgorithm));
        }
    }
}
