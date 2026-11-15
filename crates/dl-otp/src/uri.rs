//! Key Uri Format (`otpauth://`) building and parsing.

use crate::hotp::{MAX_DIGITS, MIN_DIGITS};
use crate::secret::wipe;
use crate::{Algorithm, Error, Hotp, Secret, Totp};
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::{self, Write};
use core::str::FromStr;

/// OTP type in an otpauth URI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OtpKind {
    /// Counter-based (`otpauth://hotp/...`, RFC 4226).
    Hotp,
    /// Time-based (`otpauth://totp/...`, RFC 6238).
    Totp,
}

impl OtpKind {
    /// `"hotp"` or `"totp"`.
    pub const fn as_str(self) -> &'static str {
        match self {
            OtpKind::Hotp => "hotp",
            OtpKind::Totp => "totp",
        }
    }
}

impl fmt::Display for OtpKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Everything needed to write a URI.
pub(crate) struct Params<'a> {
    pub kind: OtpKind,
    pub issuer: &'a str,
    pub account: &'a str,
    pub secret: &'a Secret,
    pub algorithm: Algorithm,
    pub digits: u8,
    /// TOTP only.
    pub period: u64,
    /// HOTP only.
    pub counter: u64,
}

pub(crate) fn build(p: Params<'_>) -> Result<String, Error> {
    // Parsers drop spaces before the account name (Key Uri Format label grammar), so an
    // account that starts with one would not round-trip.
    if p.account.is_empty()
        || p.account.starts_with(' ')
        || p.account.contains(':')
        || p.issuer.contains(':')
    {
        return Err(Error::InvalidLabel);
    }
    let mut out = String::with_capacity(
        64 + 6 * (p.issuer.len() + p.account.len()) + 2 * p.secret.as_bytes().len(),
    );
    out.push_str("otpauth://");
    out.push_str(p.kind.as_str());
    out.push('/');
    if !p.issuer.is_empty() {
        percent_encode(p.issuer, &mut out);
        out.push(':');
    }
    percent_encode(p.account, &mut out);
    out.push_str("?secret=");
    let mut b32 = p.secret.to_base32().into_bytes();
    // Base32 output is ASCII.
    out.push_str(core::str::from_utf8(&b32).unwrap_or(""));
    wipe(&mut b32);
    if !p.issuer.is_empty() {
        out.push_str("&issuer=");
        percent_encode(p.issuer, &mut out);
    }
    // Writing to a `String` cannot fail.
    let _ = write!(
        out,
        "&algorithm={}&digits={}",
        p.algorithm.as_str(),
        p.digits
    );
    let _ = match p.kind {
        OtpKind::Totp => write!(out, "&period={}", p.period),
        OtpKind::Hotp => write!(out, "&counter={}", p.counter),
    };
    Ok(out)
}

/// RFC 3986 percent-encoding: everything except unreserved characters
/// (`A-Z a-z 0-9 - . _ ~`) becomes `%XX` with upper-case hex, byte by byte (UTF-8).
fn percent_encode(s: &str, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 15)]));
        }
    }
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Decodes `%XX` escapes (and `+` as space if `plus_as_space`, for query values written
/// by form encoders such as Go's `url.Values`). `offset` is the position of `s` in the URI.
fn percent_decode(s: &str, offset: usize, plus_as_space: bool) -> Result<Vec<u8>, Error> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let hi = b.get(i + 1).copied().and_then(hex_val);
                let lo = b.get(i + 2).copied().and_then(hex_val);
                match (hi, lo) {
                    (Some(h), Some(l)) => out.push(h << 4 | l),
                    _ => {
                        wipe(&mut out);
                        return Err(Error::InvalidPercentEncoding { index: offset + i });
                    }
                }
                i += 3;
            }
            b'+' if plus_as_space => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Ok(out)
}

fn decode_text(s: &str, offset: usize, plus_as_space: bool) -> Result<String, Error> {
    String::from_utf8(percent_decode(s, offset, plus_as_space)?)
        .map_err(|_| Error::InvalidUtf8 { index: offset })
}

/// Strict unsigned decimal: ASCII digits only (no sign, no spaces), no overflow.
fn parse_u64(s: &str) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    s.bytes().try_fold(0u64, |acc, b| {
        if b.is_ascii_digit() {
            acc.checked_mul(10)?.checked_add(u64::from(b - b'0'))
        } else {
            None
        }
    })
}

/// A parsed `otpauth://` Key URI, as produced by authenticator enrolment QR codes.
///
/// Fields are plain data; [`to_totp`](Self::to_totp) and [`to_hotp`](Self::to_hotp)
/// validate them again. Absent optional parameters take the Key Uri Format defaults
/// (SHA1, 6 digits, 30 s). Unknown parameters (`image`, `color`, ...) are ignored.
///
/// ```
/// use dl_otp::{Algorithm, OtpAuthUri, OtpKind};
///
/// let uri = OtpAuthUri::parse(
///     "otpauth://totp/ACME%20Co:user%40example.com?secret=JBSWY3DPEHPK3PXP&issuer=ACME%20Co",
/// )?;
/// assert_eq!(uri.kind, OtpKind::Totp);
/// assert_eq!(uri.issuer(), Some("ACME Co"));
/// assert_eq!(uri.account, "user@example.com");
/// assert_eq!((uri.algorithm, uri.digits, uri.period), (Algorithm::Sha1, 6, 30));
/// let totp = uri.to_totp()?;
/// assert_eq!(totp.generate_at(1_700_000_000).as_str(), "324550");
/// # Ok::<(), dl_otp::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtpAuthUri {
    /// `hotp` or `totp`.
    pub kind: OtpKind,
    /// Issuer prefix of the label (`Issuer:account`), percent-decoded. `None` if the
    /// label has no `:` or the prefix is empty.
    pub label_issuer: Option<String>,
    /// Account name from the label, percent-decoded, leading spaces removed.
    pub account: String,
    /// The `issuer` parameter, percent-decoded. `None` if absent or empty.
    pub issuer: Option<String>,
    /// The `secret` parameter, Base32-decoded.
    pub secret: Secret,
    /// The `algorithm` parameter; [`Algorithm::Sha1`] if absent.
    pub algorithm: Algorithm,
    /// The `digits` parameter, `6..=10`; 6 if absent.
    pub digits: u8,
    /// The `period` parameter in seconds, at least 1; 30 if absent.
    pub period: u64,
    /// The `counter` parameter. Always `Some` for `hotp` URIs.
    pub counter: Option<u64>,
}

impl OtpAuthUri {
    /// Parses a Key Uri Format string.
    ///
    /// Accepted leniently: any ASCII case in the scheme, the type, parameter names and the
    /// algorithm; `%3A` as the label separator and spaces after it; `+` as a space in
    /// parameter values; a trailing `#fragment`; the lenient Base32 accepted by
    /// [`Secret::from_base32`].
    ///
    /// Rejected with a specific [`Error`]: another scheme or type, an empty account name,
    /// a missing or invalid secret, `hotp` without `counter`, malformed `%` escapes or
    /// non-UTF-8 text, a repeated known parameter (different parsers could pick different
    /// values), and invalid `algorithm` / `digits` / `period` / `counter` values.
    ///
    /// When both are present, the `issuer` parameter is not required to match the label
    /// prefix; [`issuer`](Self::issuer) prefers the parameter, as Google Authenticator
    /// does. Compare them yourself if a mismatch should be refused.
    pub fn parse(input: &str) -> Result<Self, Error> {
        const SCHEME: &str = "otpauth://";
        let has_scheme = input
            .get(..SCHEME.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(SCHEME));
        if !has_scheme {
            return Err(Error::InvalidScheme);
        }
        let rest = &input[SCHEME.len()..];
        let rest = rest.split_once('#').map_or(rest, |(r, _)| r);

        let type_end = rest.find(['/', '?']).unwrap_or(rest.len());
        let kind = match &rest[..type_end] {
            t if t.eq_ignore_ascii_case("totp") => OtpKind::Totp,
            t if t.eq_ignore_ascii_case("hotp") => OtpKind::Hotp,
            _ => return Err(Error::InvalidType),
        };
        let after_type = &rest[type_end..];
        let (label, label_offset, query, query_offset) = {
            let base = SCHEME.len() + type_end;
            let (path, query) = match after_type.split_once('?') {
                Some((p, q)) => (p, Some(q)),
                None => (after_type, None),
            };
            let label = path.strip_prefix('/').unwrap_or(path);
            let label_offset = base + (path.len() - label.len());
            (label, label_offset, query, base + path.len() + 1)
        };

        let label = decode_text(label, label_offset, false)?;
        let (label_issuer, account) = match label.split_once(':') {
            Some((issuer, account)) => {
                ((!issuer.is_empty()).then(|| String::from(issuer)), account)
            }
            None => (None, label.as_str()),
        };
        let account = String::from(account.trim_start_matches(' '));
        if account.is_empty() {
            return Err(Error::MissingAccount);
        }

        let mut secret = None;
        let mut issuer: Option<Option<String>> = None;
        let mut algorithm = None;
        let mut digits = None;
        let mut period = None;
        let mut counter = None;

        let mut pos = query_offset;
        for segment in query.unwrap_or("").split('&') {
            let seg_start = pos;
            pos += segment.len() + 1;
            if segment.is_empty() {
                continue;
            }
            let (key, value, value_offset) = match segment.split_once('=') {
                Some((k, v)) => (k, v, seg_start + k.len() + 1),
                None => (segment, "", seg_start + segment.len()),
            };
            let name = match key {
                k if k.eq_ignore_ascii_case("secret") => "secret",
                k if k.eq_ignore_ascii_case("issuer") => "issuer",
                k if k.eq_ignore_ascii_case("algorithm") => "algorithm",
                k if k.eq_ignore_ascii_case("digits") => "digits",
                k if k.eq_ignore_ascii_case("period") => "period",
                k if k.eq_ignore_ascii_case("counter") => "counter",
                _ => continue,
            };
            let duplicate = match name {
                "secret" => secret.is_some(),
                "issuer" => issuer.is_some(),
                "algorithm" => algorithm.is_some(),
                "digits" => digits.is_some(),
                "period" => period.is_some(),
                _ => counter.is_some(),
            };
            if duplicate {
                return Err(Error::DuplicateParameter {
                    name,
                    index: seg_start,
                });
            }
            let invalid = Error::InvalidParameter {
                name,
                index: value_offset,
            };
            match name {
                "secret" => {
                    let mut raw = percent_decode(value, value_offset, true)?;
                    let parsed = match core::str::from_utf8(&raw) {
                        Ok(text) => Secret::from_base32(text),
                        Err(_) => Err(Error::InvalidUtf8 {
                            index: value_offset,
                        }),
                    };
                    wipe(&mut raw);
                    secret = Some(parsed?);
                }
                "issuer" => {
                    let text = decode_text(value, value_offset, true)?;
                    issuer = Some((!text.is_empty()).then_some(text));
                }
                "algorithm" => {
                    let text = decode_text(value, value_offset, true)?;
                    algorithm = Some(Algorithm::from_str(&text).map_err(|_| invalid)?);
                }
                "digits" => {
                    let d = parse_u64(value)
                        .and_then(|d| u8::try_from(d).ok())
                        .filter(|d| (MIN_DIGITS..=MAX_DIGITS).contains(d))
                        .ok_or(invalid)?;
                    digits = Some(d);
                }
                "period" => {
                    period = Some(parse_u64(value).filter(|&p| p > 0).ok_or(invalid)?);
                }
                _ => counter = Some(parse_u64(value).ok_or(invalid)?),
            }
        }

        let secret = secret.ok_or(Error::MissingSecret)?;
        if kind == OtpKind::Hotp && counter.is_none() {
            return Err(Error::MissingCounter);
        }
        Ok(Self {
            kind,
            label_issuer,
            account,
            issuer: issuer.flatten(),
            secret,
            algorithm: algorithm.unwrap_or_default(),
            digits: digits.unwrap_or(6),
            period: period.unwrap_or(30),
            counter,
        })
    }

    /// The issuer to display: the `issuer` parameter if present, else the label prefix.
    pub fn issuer(&self) -> Option<&str> {
        self.issuer.as_deref().or(self.label_issuer.as_deref())
    }

    /// A [`Totp`] with this URI's secret, digits, algorithm and period (skew 1, `t0 = 0`).
    /// Fails with [`Error::WrongKind`] for an `hotp` URI.
    pub fn to_totp(&self) -> Result<Totp, Error> {
        if self.kind != OtpKind::Totp {
            return Err(Error::WrongKind);
        }
        Totp::with_params(
            self.secret.clone(),
            self.digits,
            self.algorithm,
            self.period,
        )
    }

    /// An [`Hotp`] with this URI's secret, digits and algorithm, and the initial counter.
    /// Fails with [`Error::WrongKind`] for a `totp` URI.
    pub fn to_hotp(&self) -> Result<(Hotp, u64), Error> {
        if self.kind != OtpKind::Hotp {
            return Err(Error::WrongKind);
        }
        let counter = self.counter.ok_or(Error::MissingCounter)?;
        Ok((
            Hotp::with_params(self.secret.clone(), self.digits, self.algorithm)?,
            counter,
        ))
    }
}

impl FromStr for OtpAuthUri {
    type Err = Error;

    /// Same as [`OtpAuthUri::parse`].
    fn from_str(s: &str) -> Result<Self, Error> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::format;
    use std::string::ToString;

    fn secret() -> Secret {
        Secret::from_base32("JBSWY3DPEHPK3PXP").unwrap()
    }

    #[test]
    fn build_totp_uri() {
        let t = Totp::new(secret());
        assert_eq!(
            t.to_uri("ACME Co", "user@example.com").unwrap(),
            "otpauth://totp/ACME%20Co:user%40example.com?secret=JBSWY3DPEHPK3PXP\
             &issuer=ACME%20Co&algorithm=SHA1&digits=6&period=30"
        );
        let t = Totp::with_params(secret(), 8, Algorithm::Sha512, 60).unwrap();
        assert_eq!(
            t.to_uri("", "alice").unwrap(),
            "otpauth://totp/alice?secret=JBSWY3DPEHPK3PXP&algorithm=SHA512&digits=8&period=60"
        );
    }

    #[test]
    fn build_hotp_uri() {
        let h = Hotp::new(secret());
        assert_eq!(
            h.to_uri("Example", "alice@google.com", 7).unwrap(),
            "otpauth://hotp/Example:alice%40google.com?secret=JBSWY3DPEHPK3PXP\
             &issuer=Example&algorithm=SHA1&digits=6&counter=7"
        );
    }

    #[test]
    fn percent_encoding() {
        let mut s = String::new();
        percent_encode("ACME Co", &mut s);
        assert_eq!(s, "ACME%20Co");
        s.clear();
        percent_encode("user@example.com", &mut s);
        assert_eq!(s, "user%40example.com");
        s.clear();
        percent_encode("a-b.c_d~e/f?g&h=i+j#k%l", &mut s);
        assert_eq!(s, "a-b.c_d~e%2Ff%3Fg%26h%3Di%2Bj%23k%25l");
        s.clear();
        percent_encode("Công ty Việt", &mut s);
        assert_eq!(s, "C%C3%B4ng%20ty%20Vi%E1%BB%87t");
    }

    #[test]
    fn label_validation() {
        let t = Totp::new(secret());
        assert_eq!(t.to_uri("ACME", ""), Err(Error::InvalidLabel));
        assert_eq!(t.to_uri("AC:ME", "a"), Err(Error::InvalidLabel));
        assert_eq!(t.to_uri("ACME", "a:b"), Err(Error::InvalidLabel));
        assert_eq!(t.to_uri("ACME", " bob"), Err(Error::InvalidLabel));
        assert_eq!(t.to_uri("", " bob"), Err(Error::InvalidLabel));
        // Inner and trailing spaces are fine.
        assert!(t.to_uri("ACME", "bob smith ").is_ok());
        assert_eq!(
            Hotp::new(secret()).to_uri("", "", 0),
            Err(Error::InvalidLabel)
        );
    }

    #[test]
    fn roundtrip() {
        let cases = [
            ("ACME Co", "user@example.com", 6, Algorithm::Sha1, 30),
            ("", "bob", 8, Algorithm::Sha256, 60),
            (
                "Công ty Việt",
                "người dùng+tag@ví.dụ",
                10,
                Algorithm::Sha512,
                15,
            ),
            ("a&b=c?d#e%f", "x/y z", 7, Algorithm::Sha1, 1),
        ];
        for (issuer, account, digits, alg, period) in cases {
            let t = Totp::with_params(fixed_secret(), digits, alg, period).unwrap();
            let uri = t.to_uri(issuer, account).unwrap();
            assert!(uri.is_ascii());
            let p = OtpAuthUri::parse(&uri).unwrap();
            assert_eq!(p.kind, OtpKind::Totp);
            assert_eq!(p.account, account);
            let want_issuer = (!issuer.is_empty()).then_some(issuer);
            assert_eq!(p.issuer(), want_issuer);
            assert_eq!(p.label_issuer.as_deref(), want_issuer);
            assert_eq!(p.issuer.as_deref(), want_issuer);
            assert_eq!(&p.secret, t.secret());
            assert_eq!(
                (p.algorithm, p.digits, p.period, p.counter),
                (alg, digits, period, None)
            );
            let back = p.to_totp().unwrap();
            assert_eq!(
                back.generate_at(1_700_000_000),
                t.generate_at(1_700_000_000)
            );
            assert_eq!(p.to_hotp().unwrap_err(), Error::WrongKind);
        }
        let h = Hotp::with_params(fixed_secret(), 8, Algorithm::Sha256).unwrap();
        let uri = h.to_uri("Bank", "acct 42", u64::MAX).unwrap();
        let p: OtpAuthUri = uri.parse().unwrap();
        assert_eq!((p.kind, p.counter), (OtpKind::Hotp, Some(u64::MAX)));
        let (h2, counter) = p.to_hotp().unwrap();
        assert_eq!(counter, u64::MAX);
        assert_eq!(h2.generate(5), h.generate(5));
        assert_eq!(p.to_totp().unwrap_err(), Error::WrongKind);
    }

    /// Deterministic 20-byte secret, so these tests also run without `std`.
    fn fixed_secret() -> Secret {
        Secret::from_bytes(&core::array::from_fn::<u8, 20, _>(|i| (i * 37 + 11) as u8)).unwrap()
    }

    #[test]
    fn parse_google_examples() {
        // From the Key Uri Format documentation.
        let p = OtpAuthUri::parse(
            "otpauth://totp/Example:alice@google.com?secret=JBSWY3DPEHPK3PXP&issuer=Example",
        )
        .unwrap();
        assert_eq!(p.issuer(), Some("Example"));
        assert_eq!(p.account, "alice@google.com");
        assert_eq!(p.secret, secret());
        let p = OtpAuthUri::parse(
            "otpauth://totp/ACME%20Co:john.doe@email.com?secret=HXDMVJECJJWSRB3HWIZR4IFUGFTMXBOZ\
             &issuer=ACME%20Co&algorithm=SHA1&digits=6&period=30",
        )
        .unwrap();
        assert_eq!(p.issuer(), Some("ACME Co"));
        assert_eq!(p.account, "john.doe@email.com");
        assert_eq!(p.secret.as_bytes().len(), 20);
    }

    #[test]
    fn parse_lenient_forms() {
        // %3A separator, spaces after it, mixed case, '+' in a value, padding, fragment,
        // unknown parameters, empty segments.
        let p = OtpAuthUri::parse(
            "OTPAUTH://TOTP/My%20Bank%3A%20%20jane?SECRET=jbsw+y3dp-ehpk3pxp%3D%3D%3D&&\
             Issuer=My+Bank&Algorithm=sha256&image=https%3A%2F%2Fx&Digits=8#frag",
        )
        .unwrap();
        assert_eq!(p.label_issuer.as_deref(), Some("My Bank"));
        assert_eq!(p.issuer.as_deref(), Some("My Bank"));
        assert_eq!(p.account, "jane");
        assert_eq!(p.secret, secret());
        assert_eq!(
            (p.algorithm, p.digits, p.period),
            (Algorithm::Sha256, 8, 30)
        );
        // '+' stays literal in the label.
        let p = OtpAuthUri::parse("otpauth://totp/a+b?secret=JBSWY3DPEHPK3PXP").unwrap();
        assert_eq!((p.account.as_str(), p.issuer()), ("a+b", None));
        // Parameter issuer wins over the label prefix.
        let p =
            OtpAuthUri::parse("otpauth://totp/Old:me?secret=JBSWY3DPEHPK3PXP&issuer=New").unwrap();
        assert_eq!(
            (p.label_issuer.as_deref(), p.issuer()),
            (Some("Old"), Some("New"))
        );
        // Empty issuer prefix and empty issuer parameter mean "no issuer".
        let p = OtpAuthUri::parse("otpauth://totp/:me?secret=JBSWY3DPEHPK3PXP&issuer=").unwrap();
        assert_eq!((p.account.as_str(), p.issuer()), ("me", None));
        // Leading spaces are dropped with or without an issuer prefix.
        let p = OtpAuthUri::parse("otpauth://totp/%20%20me%20?secret=JBSWY3DPEHPK3PXP").unwrap();
        assert_eq!(p.account, "me ");
        assert_eq!(
            OtpAuthUri::parse("otpauth://totp/%20?secret=JBSWY3DPEHPK3PXP"),
            Err(Error::MissingAccount)
        );
        // Parameter without '=' is treated as empty (here: unknown, ignored).
        assert!(OtpAuthUri::parse("otpauth://totp/me?lock&secret=JBSWY3DPEHPK3PXP").is_ok());
    }

    #[test]
    fn parse_errors() {
        use Error::*;
        let cases: &[(&str, Error)] = &[
            ("", InvalidScheme),
            ("otpauth:/totp/a?secret=JBSWY3DP", InvalidScheme),
            ("https://totp/a?secret=JBSWY3DP", InvalidScheme),
            ("otpauth://motp/a?secret=JBSWY3DP", InvalidType),
            ("otpauth://", InvalidType),
            ("otpauth://totp", MissingAccount),
            ("otpauth://totp?secret=JBSWY3DP", MissingAccount),
            ("otpauth://totp/?secret=JBSWY3DP", MissingAccount),
            ("otpauth://totp/Issuer:?secret=JBSWY3DP", MissingAccount),
            ("otpauth://totp/Issuer:%20?secret=JBSWY3DP", MissingAccount),
            ("otpauth://totp/a", MissingSecret),
            ("otpauth://totp/a?issuer=x", MissingSecret),
            (
                "otpauth://totp/a?secret=",
                SecretTooShort { len: 0, min: 1 },
            ),
            ("otpauth://hotp/a?secret=JBSWY3DP", MissingCounter),
            (
                "otpauth://totp/a%2?secret=JBSWY3DP",
                InvalidPercentEncoding { index: 16 },
            ),
            (
                "otpauth://totp/a%zz?secret=JBSWY3DP",
                InvalidPercentEncoding { index: 16 },
            ),
            (
                "otpauth://totp/a?secret=JBSWY3DP%",
                InvalidPercentEncoding { index: 32 },
            ),
            (
                "otpauth://totp/a%FF?secret=JBSWY3DP",
                InvalidUtf8 { index: 15 },
            ),
            (
                "otpauth://totp/a?secret=JBSWY3DP&issuer=%C3",
                InvalidUtf8 { index: 40 },
            ),
            ("otpauth://totp/a?secret=%FF", InvalidUtf8 { index: 24 }),
            (
                "otpauth://totp/a?secret=JBSWY3DP&secret=AAAA",
                DuplicateParameter {
                    name: "secret",
                    index: 33,
                },
            ),
            (
                "otpauth://totp/a?digits=6&secret=JBSWY3DP&DIGITS=6",
                DuplicateParameter {
                    name: "digits",
                    index: 42,
                },
            ),
            (
                "otpauth://totp/a?secret=JBSWY3DP&algorithm=MD5",
                InvalidParameter {
                    name: "algorithm",
                    index: 43,
                },
            ),
            (
                "otpauth://totp/a?secret=JBSWY3DP&digits=5",
                InvalidParameter {
                    name: "digits",
                    index: 40,
                },
            ),
            (
                "otpauth://totp/a?secret=JBSWY3DP&digits=11",
                InvalidParameter {
                    name: "digits",
                    index: 40,
                },
            ),
            (
                "otpauth://totp/a?secret=JBSWY3DP&digits=262",
                InvalidParameter {
                    name: "digits",
                    index: 40,
                },
            ),
            (
                "otpauth://totp/a?secret=JBSWY3DP&digits=+6",
                InvalidParameter {
                    name: "digits",
                    index: 40,
                },
            ),
            (
                "otpauth://totp/a?secret=JBSWY3DP&digits",
                InvalidParameter {
                    name: "digits",
                    index: 39,
                },
            ),
            (
                "otpauth://totp/a?secret=JBSWY3DP&period=0",
                InvalidParameter {
                    name: "period",
                    index: 40,
                },
            ),
            (
                "otpauth://totp/a?secret=JBSWY3DP&period=-30",
                InvalidParameter {
                    name: "period",
                    index: 40,
                },
            ),
            (
                "otpauth://hotp/a?secret=JBSWY3DP&counter=18446744073709551616",
                InvalidParameter {
                    name: "counter",
                    index: 41,
                },
            ),
            (
                "otpauth://hotp/a?secret=JBSWY3DP&counter=1.5",
                InvalidParameter {
                    name: "counter",
                    index: 41,
                },
            ),
        ];
        for (uri, want) in cases {
            assert_eq!(OtpAuthUri::parse(uri).unwrap_err(), *want, "{uri}");
            assert!(!want.to_string().is_empty());
        }
        assert!(matches!(
            OtpAuthUri::parse("otpauth://totp/a?secret=JBSW1"),
            Err(InvalidSecret(_))
        ));
    }

    #[test]
    fn parse_never_panics_on_mangled_input() {
        let good = "otpauth://hotp/C%C3%B4ng:me%40x?secret=JBSWY3DPEHPK3PXP&issuer=C%C3%B4ng\
                    &algorithm=SHA256&digits=8&period=60&counter=5";
        assert!(OtpAuthUri::parse(good).is_ok());
        // Every prefix, at every char boundary.
        for (i, _) in good.char_indices() {
            let _ = OtpAuthUri::parse(&good[..i]);
        }
        // Every single-byte substitution with characters that matter to the parser.
        let bytes = good.as_bytes();
        for i in 0..bytes.len() {
            for &c in b"%:/?&=#+ \x00\xffA0" {
                let mut m = bytes.to_vec();
                m[i] = c;
                let s = std::string::String::from_utf8_lossy(&m);
                let _ = OtpAuthUri::parse(&s);
            }
        }
        let long = format!(
            "otpauth://totp/{}?secret=JBSWY3DPEHPK3PXP",
            "%41".repeat(100_000)
        );
        assert_eq!(OtpAuthUri::parse(&long).unwrap().account.len(), 100_000);
    }

    #[test]
    fn debug_does_not_leak_secret() {
        let p = OtpAuthUri::parse("otpauth://totp/a?secret=JBSWY3DPEHPK3PXP").unwrap();
        let dbg = format!("{p:?}");
        assert!(dbg.contains("Secret(<redacted>)"));
        assert!(!dbg.contains("JBSWY3DPEHPK3PXP"));
    }

    #[test]
    fn kind_names() {
        assert_eq!(OtpKind::Totp.to_string(), "totp");
        assert_eq!(OtpKind::Hotp.as_str(), "hotp");
    }
}
