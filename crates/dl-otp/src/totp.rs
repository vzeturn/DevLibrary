use crate::hotp::{check_digits, normalize_code};
use crate::{Algorithm, Code, Error, Hotp, OtpKind, Secret, uri};
use alloc::string::String;

/// Time-based one-time passwords (RFC 6238).
///
/// A code is the HOTP value of the time step `(unix_secs - t0) / period`. All methods
/// take the time explicitly as seconds since the Unix epoch, so the type works in
/// `no_std` and in the browser (pass `Math.floor(Date.now() / 1000)`). With the `std`
/// feature, on targets that have a clock, `generate_now`, `verify_now`,
/// `verify_now_with_last` and `seconds_remaining_now` read `SystemTime::now()`.
///
/// ```
/// use dl_otp::{Algorithm, Secret, Totp};
///
/// // RFC 6238 Appendix B, SHA-1 row for T = 59 with 8 digits.
/// let secret = Secret::from_bytes(b"12345678901234567890")?;
/// let totp = Totp::with_params(secret, 8, Algorithm::Sha1, 30)?;
/// assert_eq!(totp.generate_at(59).as_str(), "94287082");
/// assert_eq!(totp.verify_at("94287082", 59), Some(1)); // matched time step 1
/// # Ok::<(), dl_otp::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct Totp {
    hotp: Hotp,
    period: u64,
    t0: u64,
    skew: u8,
}

impl Totp {
    /// TOTP with the defaults every app supports: 6 digits, SHA-1, 30-second period,
    /// `t0 = 0`, and a skew of 1 step (codes from the previous and next 30 s are accepted).
    pub fn new(secret: Secret) -> Self {
        Self {
            hotp: Hotp::new(secret),
            period: 30,
            t0: 0,
            skew: 1,
        }
    }

    /// TOTP with explicit code parameters, `t0 = 0` and a skew of 1 step.
    /// `digits` must be in `6..=10` and `period` (seconds) at least 1.
    ///
    /// Many authenticator apps only honour 6 digits, SHA-1 and 30 s; see the README's
    /// compatibility table before changing them.
    pub fn with_params(
        secret: Secret,
        digits: u8,
        algorithm: Algorithm,
        period: u64,
    ) -> Result<Self, Error> {
        check_digits(digits)?;
        if period == 0 {
            return Err(Error::InvalidPeriod);
        }
        let hotp = Hotp::with_params(secret, digits, algorithm)?;
        Ok(Self {
            hotp,
            period,
            t0: 0,
            skew: 1,
        })
    }

    /// Sets the Unix time at which step 0 starts (RFC 6238 `T0`, default 0). Times before
    /// `t0` all map to step 0. otpauth URIs cannot carry `t0`, so
    /// [`to_uri`](Self::to_uri) fails if it is not 0.
    #[must_use]
    pub fn with_t0(mut self, t0: u64) -> Self {
        self.t0 = t0;
        self
    }

    /// Sets how many steps before and after the current one [`verify_at`](Self::verify_at)
    /// accepts (default 1). 0 accepts only the current step. Each extra step adds two
    /// HMACs per verification and doubles an attacker's odds per guess, so keep it at 1
    /// unless clients are known to drift.
    #[must_use]
    pub fn with_skew(mut self, skew: u8) -> Self {
        self.skew = skew;
        self
    }

    /// The shared secret.
    pub fn secret(&self) -> &Secret {
        self.hotp.secret()
    }

    /// Number of digits per code.
    pub fn digits(&self) -> u8 {
        self.hotp.digits()
    }

    /// HMAC algorithm.
    pub fn algorithm(&self) -> Algorithm {
        self.hotp.algorithm()
    }

    /// Step length in seconds.
    pub fn period(&self) -> u64 {
        self.period
    }

    /// Unix time of step 0.
    pub fn t0(&self) -> u64 {
        self.t0
    }

    /// Accepted steps on each side of the current one.
    pub fn skew(&self) -> u8 {
        self.skew
    }

    /// The time step for `unix_secs`: `(unix_secs - t0) / period`, or 0 before `t0`.
    pub fn time_step(&self, unix_secs: u64) -> u64 {
        unix_secs.saturating_sub(self.t0) / self.period
    }

    /// Seconds until the next step begins: `1..=period` from `t0` on. Before `t0` the time
    /// is in step 0, which ends at `t0 + period`, so the result can exceed `period`
    /// (saturating at `u64::MAX`).
    pub fn seconds_remaining(&self, unix_secs: u64) -> u64 {
        match unix_secs.checked_sub(self.t0) {
            Some(elapsed) => self.period - elapsed % self.period,
            None => (self.t0 - unix_secs).saturating_add(self.period),
        }
    }

    /// The code for the step containing `unix_secs`.
    pub fn generate_at(&self, unix_secs: u64) -> Code {
        self.hotp.generate(self.time_step(unix_secs))
    }

    /// Checks `code` against the steps `current - skew ..= current + skew` and returns the
    /// matching time step.
    ///
    /// To stop the same code being used twice (RFC 6238 section 5.2), store the returned
    /// step and use [`verify_at_with_last`](Self::verify_at_with_last) next time.
    ///
    /// `code` may contain ASCII whitespace; anything else that is not exactly
    /// [`digits`](Self::digits) ASCII digits returns `None`. Every step in the window is
    /// computed and compared in constant time, so the run time does not depend on whether
    /// or where the code matched. If (with probability about `10^-digits`) the code matches
    /// more than one step, the earliest is returned.
    pub fn verify_at(&self, code: &str, unix_secs: u64) -> Option<u64> {
        self.verify_window(code, unix_secs, None)
    }

    /// Like [`verify_at`](Self::verify_at), but only accepts steps after
    /// `last_used_step` (the step returned by the user's previous successful login, or
    /// `None` if there was none). This rejects replayed codes, including a code
    /// intercepted and reused within its validity window.
    ///
    /// ```
    /// use dl_otp::{Secret, Totp};
    ///
    /// let totp = Totp::new(Secret::from_base32("JBSWY3DPEHPK3PXP")?);
    /// let now = 1_700_000_000;
    /// let code = totp.generate_at(now);
    /// let step = totp.verify_at_with_last(code.as_str(), now, None).unwrap();
    /// // Saved `step` with the user. The same code a second later is a replay:
    /// assert_eq!(totp.verify_at_with_last(code.as_str(), now + 1, Some(step)), None);
    /// # Ok::<(), dl_otp::Error>(())
    /// ```
    pub fn verify_at_with_last(
        &self,
        code: &str,
        unix_secs: u64,
        last_used_step: Option<u64>,
    ) -> Option<u64> {
        self.verify_window(code, unix_secs, last_used_step)
    }

    fn verify_window(&self, code: &str, unix_secs: u64, last: Option<u64>) -> Option<u64> {
        let input = normalize_code(code, self.digits())?;
        let current = self.time_step(unix_secs);
        let skew = u64::from(self.skew);
        let mut first = current.saturating_sub(skew);
        if let Some(last) = last {
            first = first.max(last.checked_add(1)?);
        }
        let end = current.saturating_add(skew);
        let keyed = self.hotp.keyed();
        let mut found = None;
        for step in first..=end {
            let matches = keyed.code(step, self.digits()) == input;
            if matches && found.is_none() {
                found = Some(step);
            }
        }
        found
    }

    /// Builds a Key Uri Format string for enrolment, for example
    /// `otpauth://totp/ACME%20Co:user%40example.com?secret=...&issuer=ACME%20Co&algorithm=SHA1&digits=6&period=30`.
    ///
    /// The label is `issuer:account` (or just `account` when `issuer` is empty) and the
    /// `issuer` parameter repeats the issuer, as Google recommends. Both are
    /// percent-encoded per RFC 3986: everything except `A-Z a-z 0-9 - . _ ~` becomes
    /// `%XX` (UTF-8), so spaces are `%20`. `algorithm`, `digits` and `period` are always
    /// written, even with default values.
    ///
    /// Fails with [`Error::InvalidLabel`] if `account` is empty or starts with a space, or
    /// either string contains `:`, and with [`Error::NonZeroT0`] if `t0 != 0`. The skew is a server-side setting
    /// and is not part of the URI.
    ///
    /// The returned string contains the secret: do not log it, and serve the QR code
    /// only over HTTPS to the authenticated user.
    pub fn to_uri(&self, issuer: &str, account: &str) -> Result<String, Error> {
        if self.t0 != 0 {
            return Err(Error::NonZeroT0);
        }
        uri::build(uri::Params {
            kind: OtpKind::Totp,
            issuer,
            account,
            secret: self.secret(),
            algorithm: self.algorithm(),
            digits: self.digits(),
            period: self.period,
            counter: 0,
        })
    }
}

#[cfg(all(
    feature = "std",
    not(all(target_arch = "wasm32", target_os = "unknown"))
))]
impl Totp {
    /// The code for the current time. Requires `std`; not available on
    /// `wasm32-unknown-unknown`, where `SystemTime::now` panics (use
    /// [`generate_at`](Self::generate_at) with the time from JavaScript).
    pub fn generate_now(&self) -> Code {
        self.generate_at(unix_now())
    }

    /// [`verify_at`](Self::verify_at) at the current time. Requires `std`; not available
    /// on `wasm32-unknown-unknown`.
    pub fn verify_now(&self, code: &str) -> Option<u64> {
        self.verify_at(code, unix_now())
    }

    /// [`verify_at_with_last`](Self::verify_at_with_last) at the current time. Requires
    /// `std`; not available on `wasm32-unknown-unknown`.
    pub fn verify_now_with_last(&self, code: &str, last_used_step: Option<u64>) -> Option<u64> {
        self.verify_at_with_last(code, unix_now(), last_used_step)
    }

    /// [`seconds_remaining`](Self::seconds_remaining) at the current time. Requires
    /// `std`; not available on `wasm32-unknown-unknown`.
    pub fn seconds_remaining_now(&self) -> u64 {
        self.seconds_remaining(unix_now())
    }
}

/// Seconds since the Unix epoch, or 0 if the system clock is set before 1970 (codes
/// generated then will simply fail to verify).
#[cfg(all(
    feature = "std",
    not(all(target_arch = "wasm32", target_os = "unknown"))
))]
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED_SHA1: &[u8] = b"12345678901234567890";
    const SEED_SHA256: &[u8] = b"12345678901234567890123456789012";
    const SEED_SHA512: &[u8] = b"1234567890123456789012345678901234567890123456789012345678901234";

    fn totp(seed: &[u8], digits: u8, alg: Algorithm, period: u64) -> Totp {
        Totp::with_params(Secret::from_bytes(seed).unwrap(), digits, alg, period).unwrap()
    }

    #[test]
    fn rfc6238_appendix_b() {
        // (time, step in hex from the RFC table, SHA1, SHA256, SHA512)
        let rows: [(u64, u64, &str, &str, &str); 6] = [
            (59, 0x1, "94287082", "46119246", "90693936"),
            (1111111109, 0x23523EC, "07081804", "68084774", "25091201"),
            (1111111111, 0x23523ED, "14050471", "67062674", "99943326"),
            (1234567890, 0x273EF07, "89005924", "91819424", "93441116"),
            (2000000000, 0x3F940AA, "69279037", "90698825", "38618901"),
            (20000000000, 0x27BC86AA, "65353130", "77737706", "47863826"),
        ];
        let s1 = totp(SEED_SHA1, 8, Algorithm::Sha1, 30);
        let s256 = totp(SEED_SHA256, 8, Algorithm::Sha256, 30);
        let s512 = totp(SEED_SHA512, 8, Algorithm::Sha512, 30);
        for (t, step, c1, c256, c512) in rows {
            for (otp, code) in [(&s1, c1), (&s256, c256), (&s512, c512)] {
                assert_eq!(otp.time_step(t), step);
                assert_eq!(
                    otp.generate_at(t).as_str(),
                    code,
                    "{:?} T={t}",
                    otp.algorithm()
                );
                assert_eq!(otp.verify_at(code, t), Some(step));
            }
        }
    }

    #[test]
    fn skew_window_edges() {
        let t = totp(SEED_SHA1, 6, Algorithm::Sha1, 30);
        // Step 10 covers [300, 330).
        let code = t.generate_at(300);
        let c = code.as_str();
        assert_eq!(t.generate_at(329), code);
        assert_ne!(t.generate_at(330), code);
        // Skew 1: accepted while the current step is 9, 10 or 11, i.e. t in [270, 360).
        assert_eq!(t.verify_at(c, 269), None);
        assert_eq!(t.verify_at(c, 270), Some(10));
        assert_eq!(t.verify_at(c, 359), Some(10));
        assert_eq!(t.verify_at(c, 360), None);
        // Skew 0: only [300, 330).
        let strict = t.clone().with_skew(0);
        assert_eq!(strict.verify_at(c, 299), None);
        assert_eq!(strict.verify_at(c, 300), Some(10));
        assert_eq!(strict.verify_at(c, 329), Some(10));
        assert_eq!(strict.verify_at(c, 330), None);
        // Skew 2: [240, 390).
        let wide = t.clone().with_skew(2);
        assert_eq!(wide.verify_at(c, 239), None);
        assert_eq!(wide.verify_at(c, 240), Some(10));
        assert_eq!(wide.verify_at(c, 389), Some(10));
        assert_eq!(wide.verify_at(c, 390), None);
        // Window clamps at step 0 and at u64::MAX without overflow.
        let first = t.generate_at(0);
        assert_eq!(t.verify_at(first.as_str(), 0), Some(0));
        assert_eq!(t.verify_at(first.as_str(), 30), Some(0));
        assert_eq!(t.verify_at(first.as_str(), 60), None);
        let max_t = t.clone().with_skew(255);
        assert_eq!(
            max_t.verify_at(t.generate_at(u64::MAX).as_str(), u64::MAX),
            Some(u64::MAX / 30)
        );
    }

    #[test]
    fn replay_protection() {
        let t = totp(SEED_SHA1, 6, Algorithm::Sha1, 30);
        let now = 1_111_111_109;
        let step = t.time_step(now);
        let code = t.generate_at(now);
        let c = code.as_str();
        assert_eq!(t.verify_at_with_last(c, now, None), Some(step));
        // The same code again, in the same step or within the skew window: rejected.
        assert_eq!(t.verify_at_with_last(c, now, Some(step)), None);
        assert_eq!(t.verify_at_with_last(c, now + 30, Some(step)), None);
        // An older step was used last: still accepted.
        assert_eq!(t.verify_at_with_last(c, now, Some(step - 1)), Some(step));
        // A code from the previous step is rejected once a later step was used.
        let prev = t.generate_at(now - 30);
        assert_eq!(
            t.verify_at_with_last(prev.as_str(), now, None),
            Some(step - 1)
        );
        assert_eq!(t.verify_at_with_last(prev.as_str(), now, Some(step)), None);
        // The next code is accepted after the current one was used.
        let next = t.generate_at(now + 30);
        assert_eq!(
            t.verify_at_with_last(next.as_str(), now + 30, Some(step)),
            Some(step + 1)
        );
        // last = u64::MAX leaves nothing to accept, without overflow.
        assert_eq!(t.verify_at_with_last(c, now, Some(u64::MAX)), None);
    }

    #[test]
    fn period_60_python_cross_check() {
        let t = totp(SEED_SHA1, 6, Algorithm::Sha1, 60);
        for (time, code) in [
            (0, "755224"),
            (59, "755224"),
            (60, "287082"),
            (1111111109, "360094"),
            (1700000000, "895298"),
        ] {
            assert_eq!(t.generate_at(time).as_str(), code, "T={time}");
            assert_eq!(t.verify_at(code, time), Some(time / 60));
        }
        assert_eq!(t.seconds_remaining(0), 60);
        assert_eq!(t.seconds_remaining(59), 1);
        assert_eq!(t.seconds_remaining(60), 60);
    }

    #[test]
    fn t0_python_cross_check() {
        let t = Totp::new(Secret::from_bytes(SEED_SHA1).unwrap()).with_t0(1000);
        assert_eq!(t.t0(), 1000);
        for (time, code, step) in [
            (1000, "755224", 0),
            (1029, "755224", 0),
            (1030, "287082", 1),
            (1111111109, "277312", 37037003),
        ] {
            assert_eq!(t.time_step(time), step);
            assert_eq!(t.generate_at(time).as_str(), code, "T={time}");
        }
        // Before t0 everything maps to step 0, which ends at t0 + period.
        assert_eq!(t.time_step(0), 0);
        assert_eq!(t.generate_at(0).as_str(), "755224");
        assert_eq!(t.seconds_remaining(0), 1030);
        assert_eq!(t.seconds_remaining(999), 31);
        assert_eq!(t.seconds_remaining(1000), 30);
        assert_eq!(t.seconds_remaining(1029), 1);
        assert_eq!(t.seconds_remaining(1030), 30);
        let far = Totp::new(Secret::from_bytes(SEED_SHA1).unwrap()).with_t0(u64::MAX);
        assert_eq!(far.seconds_remaining(0), u64::MAX);
        assert_eq!(far.seconds_remaining(u64::MAX), 30);
        assert_eq!(t.to_uri("ACME", "a"), Err(Error::NonZeroT0));
    }

    #[test]
    fn base32_secret_python_cross_check() {
        let secret = || Secret::from_base32("JBSWY3DPEHPK3PXP").unwrap();
        let t = Totp::new(secret());
        for (time, code) in [
            (0, "282760"),
            (1700000000, "324550"),
            (1700000029, "367665"),
            (1700000030, "367665"),
        ] {
            assert_eq!(t.generate_at(time).as_str(), code, "T={time}");
        }
        let t = Totp::with_params(secret(), 8, Algorithm::Sha256, 60).unwrap();
        assert_eq!(t.generate_at(1700000000).as_str(), "71205722");
        let t = Totp::with_params(secret(), 10, Algorithm::Sha512, 30).unwrap();
        assert_eq!(t.generate_at(1700000000).as_str(), "0514045688");
        assert_eq!(
            t.verify_at("05140 45688", 1700000000),
            Some(1700000000 / 30)
        );
    }

    #[test]
    fn digits_6_7_8_10() {
        for d in [6u8, 7, 8, 10] {
            let t = totp(SEED_SHA1, d, Algorithm::Sha1, 30);
            let code = t.generate_at(59);
            assert_eq!(code.digits(), d);
            assert_eq!(t.verify_at(code.as_str(), 59), Some(1));
            // A code of another length never matches.
            let shorter = &code.as_str()[1..];
            assert_eq!(t.verify_at(shorter, 59), None);
        }
        // Lengths agree with HOTP counter 1 from RFC 4226 (step 1 at T = 59).
        assert_eq!(
            totp(SEED_SHA1, 7, Algorithm::Sha1, 30)
                .generate_at(59)
                .as_str(),
            "4287082"
        );
        assert_eq!(
            totp(SEED_SHA1, 10, Algorithm::Sha1, 30)
                .generate_at(59)
                .as_str(),
            "1094287082"
        );
    }

    #[test]
    fn verify_rejects_malformed_codes() {
        let t = Totp::new(Secret::from_bytes(SEED_SHA1).unwrap());
        let code = t.generate_at(59);
        let spaced = std::format!("{} {}", &code.as_str()[..3], &code.as_str()[3..]);
        assert_eq!(t.verify_at(&spaced, 59), Some(1));
        for bad in ["", "abcdef", "12345", "1234567", "12-3456", "١٢٣٤٥٦"] {
            assert_eq!(t.verify_at(bad, 59), None, "{bad:?}");
        }
    }

    #[test]
    fn parameter_validation() {
        let s = || Secret::from_bytes(SEED_SHA1).unwrap();
        assert_eq!(
            Totp::with_params(s(), 6, Algorithm::Sha1, 0).unwrap_err(),
            Error::InvalidPeriod
        );
        assert_eq!(
            Totp::with_params(s(), 5, Algorithm::Sha1, 30).unwrap_err(),
            Error::InvalidDigits(5)
        );
        assert_eq!(
            Totp::with_params(s(), 11, Algorithm::Sha1, 0).unwrap_err(),
            Error::InvalidDigits(11)
        );
        let t = Totp::new(s());
        assert_eq!(
            (t.digits(), t.algorithm(), t.period(), t.t0(), t.skew()),
            (6, Algorithm::Sha1, 30, 0, 1)
        );
        // Period 1 and huge periods work.
        let t = Totp::with_params(s(), 6, Algorithm::Sha1, 1).unwrap();
        assert_eq!(t.time_step(59), 59);
        assert_eq!(t.seconds_remaining(59), 1);
        let t = Totp::with_params(s(), 6, Algorithm::Sha1, u64::MAX).unwrap();
        assert_eq!(t.time_step(u64::MAX - 1), 0);
        assert_eq!(t.time_step(u64::MAX), 1);
    }

    #[cfg(all(
        feature = "std",
        not(all(target_arch = "wasm32", target_os = "unknown"))
    ))]
    #[test]
    fn now_helpers() {
        let t = Totp::new(Secret::from_bytes(SEED_SHA1).unwrap()).with_skew(1);
        let code = t.generate_now();
        // With skew 1 this holds even if the step changes between the two calls.
        let step = t.verify_now(code.as_str()).unwrap();
        assert_eq!(t.verify_now_with_last(code.as_str(), Some(step)), None);
        assert!((1..=30).contains(&t.seconds_remaining_now()));
        assert!(step >= t.time_step(1_700_000_000));
    }
}
