# dl-otp

HOTP (RFC 4226) and TOTP (RFC 6238) one-time passwords for multi-factor authentication,
with `otpauth://` enrolment URIs, constant-time verification, replay protection and
single-use recovery codes. HMAC-SHA-1, SHA-256 and SHA-512, 6 to 10 digits. Core
generation and verification are `no_std` + `alloc`; no third-party dependencies, no
`unsafe`, builds for `wasm32-unknown-unknown`.

Part of [DevLibrary](https://github.com/vzeturn/DevLibrary).

## Replaces

| Ecosystem | Package | Notes |
|-----------|---------|-------|
| C# / .NET | [Otp.NET](https://github.com/kspearrin/Otp.NET) | `Totp`, `Hotp`, key generation, Base32 |
| TypeScript / JS | [`otplib`](https://www.npmjs.com/package/otplib), [`speakeasy`](https://www.npmjs.com/package/speakeasy) | Via WASM, or on the server side of a JS front end |
| Python | [`pyotp`](https://pypi.org/project/pyotp/) | `TOTP`, `HOTP`, `random_base32`, provisioning URIs |
| Go | [`pquerna/otp`](https://github.com/pquerna/otp) | `totp` / `hotp` generate and validate, key URLs |

## Features

- **HOTP** with a caller-held counter and a bounded look-ahead window; `verify` returns
  the matched counter.
- **TOTP** with configurable period (default 30 s), `T0` and clock skew (default ±1
  step). `verify_at` returns the matched time step; `verify_at_with_last` rejects any
  step at or before the last one used, so a code works once (RFC 6238 section 5.2).
- Every function takes the Unix time explicitly; with `std`, `generate_now` /
  `verify_now` read the system clock.
- **Constant-time comparison** of codes (`dl_hash::ct_eq`). TOTP verification computes
  every step of the window, so its timing does not depend on where or whether the code
  matched.
- **Strict code input**: ASCII whitespace is ignored (`"123 456"`), anything else that
  is not exactly the configured number of ASCII digits is rejected before any HMAC runs.
- **`Secret`**: random generation from the OS CSPRNG (20 / 32 / 64 bytes for SHA-1 /
  SHA-256 / SHA-512), lenient Base32 input (case, spaces, dashes, padding), redacted
  `Debug`, constant-time `==`, best-effort zeroing on drop.
- **`otpauth://` URIs** (Google Key Uri Format): build with RFC 3986 percent-encoding,
  parse with typed errors that carry byte offsets; `hotp` URIs must have a `counter`.
- **Recovery codes** from a 32-character alphabet without `0 o 1 l`, normalised hashing
  and constant-time verification.
- Tested against every value in RFC 4226 Appendix D and RFC 6238 Appendix B, plus
  vectors from an independent Python implementation (`testdata/cross_check.py`).

## Installation

```toml
[dependencies]
dl-otp = { git = "https://github.com/vzeturn/DevLibrary" }
```

From a local checkout of the repository:

```toml
[dependencies]
dl-otp = { path = "../DevLibrary/crates/dl-otp" }
```

Inside the DevLibrary workspace, use `dl-otp.workspace = true`.

Without the standard library (embedded, or a `no_std` WASM module):

```toml
[dependencies]
dl-otp = { git = "https://github.com/vzeturn/DevLibrary", default-features = false }
```

`no_std` keeps HOTP, TOTP with explicit timestamps, `Secret::from_bytes` /
`from_base32`, URIs and recovery-code hashing. It drops `Secret::generate`,
`recovery_codes` and the `*_now` helpers, which need OS entropy or a clock. Minimum Rust
version: 1.88 (edition 2024).

## Quick start

```rust
use dl_otp::{Algorithm, Hotp, Secret, Totp};

fn main() -> Result<(), dl_otp::Error> {
    // TOTP with the defaults every app supports: SHA-1, 6 digits, 30 s.
    let totp = Totp::new(Secret::from_base32("JBSWY3DPEHPK3PXP")?);
    let now = 1_700_000_000; // Unix seconds
    assert_eq!(totp.generate_at(now).as_str(), "324550");
    assert_eq!(totp.verify_at("324 550", now), Some(56_666_666)); // matched time step
    assert_eq!(totp.verify_at("324550", now + 90), None); // 3 steps later: outside skew

    // A fresh secret and its enrolment URI (render it as a QR code).
    let totp = Totp::new(Secret::generate(Algorithm::Sha1)?);
    let uri = totp.to_uri("ACME Co", "user@example.com")?;
    assert!(uri.starts_with("otpauth://totp/ACME%20Co:user%40example.com?secret="));

    // HOTP, RFC 4226 Appendix D.
    let hotp = Hotp::new(Secret::from_bytes(b"12345678901234567890")?);
    assert_eq!(hotp.generate(0).as_str(), "755224");
    assert_eq!(hotp.verify("359152", 0, 5), Some(2)); // store 3 as the next counter
    Ok(())
}
```

## Guide: MFA enrolment and login

The flow below is TOTP with an authenticator app. `examples/enrol.rs` runs all of it,
including the QR code: `cargo run -p dl-otp --example enrol`.

### 1. Generate a secret and build the URI

Create the secret on the server when the user starts enrolment, and keep it as
*pending* until the user proves the app has it (step 3).

```rust
use dl_otp::{Algorithm, Secret, Totp};

fn main() -> Result<(), dl_otp::Error> {
    let totp = Totp::new(Secret::generate(Algorithm::Sha1)?); // 160 bits from the OS
    let uri = totp.to_uri("ACME Co", "user@example.com")?;
    assert!(uri.ends_with("&issuer=ACME%20Co&algorithm=SHA1&digits=6&period=30"));

    // For users who cannot scan: the same key for manual entry. Showing it in groups of
    // four ("ABCD EFGH ...") is fine; `Secret::from_base32` ignores the spaces.
    let key = totp.secret().to_base32();
    assert_eq!(key.len(), 32); // 20 bytes, unpadded Base32
    Ok(())
}
```

`to_uri` writes the label as `issuer:account` and repeats the issuer as a parameter, as
the Key Uri Format recommends. Issuer and account must not contain `:`, the account must
not be empty or start with a space; both are percent-encoded (`ACME%20Co`,
`user%40example.com`).

### 2. Show it as a QR code

Use the sibling crate [dl-qr](../dl-qr) (add `dl-qr = { git =
"https://github.com/vzeturn/DevLibrary" }`; dl-otp does not depend on it):

```text
use dl_qr::{Ecc, QrCode, SvgOptions};

let qr = QrCode::encode_text(&uri, Ecc::Medium)?;
let svg = qr.to_svg(&SvgOptions::default()); // put into the enrolment page
print!("{}", qr.to_text(2));                 // or print to a terminal
```

The URI contains the secret. Serve the page over HTTPS to the signed-in user only, with
`Cache-Control: no-store`, do not log it, and do not offer to show it again later
(start a new enrolment instead).

### 3. Confirm with the first code

Ask the user for the code the app now shows. Only when it verifies, mark MFA as
enabled and save the secret together with the matched step.

### 4. Store the secret encrypted

`secret.as_bytes()` is the HMAC key. Encrypt it before writing it to the database (an
AEAD such as AES-GCM, with the key in a KMS or vault, not next to the data), and load it
back with `Secret::from_bytes`. A database dump alone then does not let an attacker
generate codes. dl-otp does not include encryption.

### 5. Verify logins with replay protection

Store the last accepted step per user and pass it to `verify_at_with_last`. A code is
then accepted once, even inside its validity window.

```rust
use dl_otp::{Secret, Totp};

/// What the server keeps per user. `secret` is shown decrypted for brevity.
struct MfaRecord {
    secret: Vec<u8>,
    last_step: Option<u64>,
}

/// Checks a code typed by the user and records the step it matched.
fn check_code(record: &mut MfaRecord, code: &str, now: u64) -> Result<bool, dl_otp::Error> {
    let totp = Totp::new(Secret::from_bytes(&record.secret)?);
    match totp.verify_at_with_last(code, now, record.last_step) {
        Some(step) => {
            // In a database, make this a conditional update
            // (`... WHERE last_step IS NULL OR last_step < step`) so that two concurrent
            // requests with the same code cannot both succeed.
            record.last_step = Some(step);
            Ok(true)
        }
        None => Ok(false),
    }
}

fn main() -> Result<(), dl_otp::Error> {
    let secret = Secret::from_base32("JBSWY3DPEHPK3PXP")?;
    let mut record = MfaRecord { secret: secret.as_bytes().to_vec(), last_step: None };
    let now = 1_700_000_000;

    // Enrolment confirmation (step 3) is the first successful check.
    assert!(check_code(&mut record, "324550", now)?);
    // The same code again, 10 seconds later: replay, rejected.
    assert!(!check_code(&mut record, "324550", now + 10)?);
    // The next code is accepted.
    let next = Totp::new(secret).generate_at(now + 30);
    assert!(check_code(&mut record, next.as_str(), now + 30)?);
    assert_eq!(record.last_step, Some(56_666_667));
    Ok(())
}
```

With `std` on a target that has a clock, `verify_now_with_last(code, last)` reads the
time for you. Count failures and lock or slow down the account (see
[Security notes](#security-notes)).

### 6. Recovery codes

Give the user single-use codes for when the phone is lost. Show them once, store only
hashes, and delete a hash when its code is used.

```rust
use dl_otp::{hash_recovery_code, recovery_codes, verify_recovery_code};

fn main() -> Result<(), dl_otp::Error> {
    // 10 codes of 4 groups of 4 characters (80 bits each), like "k7m2-9xq4-ab3d-wz8p".
    let codes = recovery_codes(10, 4, 4)?;
    let mut stored: Vec<[u8; 32]> = codes.iter().map(|c| hash_recovery_code(c)).collect();

    // Case, spaces and dashes do not matter when the user types one in.
    let typed = codes[0].to_uppercase().replace('-', " ");
    let used = stored.iter().position(|h| verify_recovery_code(&typed, h));
    assert_eq!(used, Some(0));
    stored.swap_remove(0); // single use
    assert!(!stored.iter().any(|h| verify_recovery_code(&typed, h)));
    Ok(())
}
```

`hash_recovery_code` is unsalted SHA-256, which is fine for 80-bit codes but not for
short ones: if the hashes leak, a GPU tries billions of SHA-256 per second, and a 40-bit
code (`recovery_codes(10, 2, 4)`) falls in minutes. For codes shorter than 16 characters,
hash with salted PBKDF2 instead (needs `dl-hash` and `dl-rand` in your `Cargo.toml`).
One random salt per user lets you hash the typed code once and compare it with all of the
user's stored hashes:

```rust,no_run
use dl_hash::{Sha256, ct_eq, pbkdf2};
use dl_otp::normalize_recovery_code;

/// PBKDF2-HMAC-SHA256 work factor recommended by the OWASP Password Storage Cheat Sheet.
const ITERATIONS: u32 = 600_000;

/// Create once per user when the codes are issued, store next to the hashes.
fn new_salt() -> Result<[u8; 16], dl_rand::Error> {
    dl_rand::secure_bytes()
}

fn hash_code(code: &str, salt: &[u8; 16]) -> [u8; 32] {
    let mut out = [0u8; 32];
    pbkdf2::<Sha256>(normalize_recovery_code(code).as_bytes(), salt, ITERATIONS, &mut out);
    out
}

/// Index of the matching stored hash; delete it afterwards.
fn find_code(typed: &str, salt: &[u8; 16], stored: &[[u8; 32]]) -> Option<usize> {
    let h = hash_code(typed, salt);
    stored.iter().position(|s| ct_eq(&h, s))
}
```

### HOTP

HOTP works the same way with a counter instead of the clock. Store the counter per user,
call `verify(code, counter, look_ahead)`, and on `Some(matched)` store `matched + 1`.
Codes at or below the stored counter are never accepted. Keep `look_ahead` small (for
example 5 to 10): each extra counter costs one HMAC and gives an attacker one more chance
per guess. `Hotp::to_uri(issuer, account, counter)` builds the `otpauth://hotp/...` URI.

## Authenticator app compatibility

Google's [Key Uri Format](https://github.com/google/google-authenticator/wiki/Key-Uri-Format)
page, the de facto specification of `otpauth://`, states:

- `algorithm` may be `SHA1` (default), `SHA256` or `SHA512`, but the Google Authenticator
  implementations ignore it.
- `digits` may be 6 (default) or 8, and Google Authenticator on Android and BlackBerry
  ignores it.
- `period` defaults to 30, and the Google Authenticator implementations ignore it.
- Older Google Authenticator versions ignore the `issuer` parameter and use the label
  prefix, so both should be set (`to_uri` does).

For other apps (Microsoft Authenticator, Authy, 1Password, Bitwarden, ...) this crate
makes no claims. For consumer MFA, **use the defaults: SHA-1, 6 digits, 30 s**
(`Totp::new`, `Secret::generate(Algorithm::Sha1)`). Use other parameters only with
clients you control, such as your own app or hardware tokens with a known
configuration. dl-otp accepts `digits` 6 to 10 in URIs, which is more than the format
documents.

HMAC-SHA-1 is not a weakness here: HOTP relies on HMAC as a keyed function, not on
collision resistance. RFC 4226 Appendix B ("SHA-1 Attacks") covers the status of SHA-1,
HMAC-SHA-1 and HOTP in detail.

## Security notes

- **Constant time.** Codes are compared with `dl_hash::ct_eq`. `Totp::verify_at*`
  computes and compares every step in the window before returning. `Hotp::verify` stops
  at the first match, which reveals how far ahead the matching counter was, but nothing
  about the code. Malformed input is rejected before any HMAC, so its timing only reveals
  that the input was malformed.
- **Rate limiting is your job.** A 6-digit code has 10^6 values; with skew 1 three codes
  are valid at once, so each guess succeeds with probability 3 × 10^-6. Limit failed
  attempts per account, not per IP. NIST SP 800-63B
  ([section 5.2.2](https://pages.nist.gov/800-63-3/sp800-63b.html#throttle)) caps
  consecutive failures at 100; after 100 guesses an attacker's odds are about 0.03 %.
  Lower limits or growing delays are better.
- **Replay.** Use `verify_at_with_last` (TOTP) or advance the counter (HOTP) and store the
  result atomically. Without it, a phished or shoulder-surfed code works again for up to
  90 s with the default skew.
- **Clock skew.** The default skew of 1 accepts the previous, current and next 30-second
  step, which RFC 6238 section 5.2 recommends as the maximum. Keep the server on NTP
  rather than widening the window; every extra step adds two valid codes. A client that
  is ahead can produce a code for a future step, after which `verify_at_with_last`
  rejects codes from earlier steps until the clock catches up; this is the price of
  replay protection.
- **Secret storage.** Encrypt secrets at rest with a key kept outside the database.
  Never log a `Secret`, its Base32 form or an `otpauth://` URI (`Debug` on `Secret` and
  `OtpAuthUri` prints `Secret(<redacted>)`). Generated secrets are at least 128 bits from
  the OS CSPRNG (RFC 4226 R6); `from_bytes` / `from_base32` accept any non-empty key so
  that existing shorter secrets keep working.
- **Zeroing.** `Secret` overwrites its buffer on drop without `unsafe` (best effort;
  copies such as Base32 strings, URIs and the HMAC state are not covered). See the
  `Secret` docs.
- **Recovery codes** are as strong as their length and storage: see
  [step 6](#6-recovery-codes). Rate-limit them like codes.

## API overview

| Item | Purpose |
|------|---------|
| `Secret::from_bytes(&[u8])`, `Secret::from_base32(&str)` | Existing key; lenient Base32 (case, spaces, `-`, padding) |
| `Secret::generate(Algorithm)`, `Secret::generate_len(n)` | Random key, 20/32/64 bytes or `n >= 16` (`std`) |
| `Secret::as_bytes`, `Secret::to_base32` | Raw key, unpadded upper-case Base32 |
| `Algorithm` | `Sha1` (default), `Sha256`, `Sha512`; `as_str`, `FromStr`, `secret_len` |
| `Hotp::new(secret)`, `Hotp::with_params(secret, digits, algorithm)` | 6 digits SHA-1, or explicit (`digits` 6..=10) |
| `Hotp::generate(counter)`, `Hotp::verify(code, counter, look_ahead)` | Code; matched counter |
| `Totp::new(secret)`, `Totp::with_params(secret, digits, algorithm, period)` | 6 digits, SHA-1, 30 s, or explicit (`period >= 1`) |
| `Totp::with_t0(t0)`, `Totp::with_skew(steps)` | Step 0 start (default 0), accepted steps each side (default 1) |
| `Totp::generate_at(t)`, `Totp::verify_at(code, t)` | Code; matched time step |
| `Totp::verify_at_with_last(code, t, last_step)` | Matched step after `last_step` only (replay protection) |
| `Totp::time_step(t)`, `Totp::seconds_remaining(t)` | Step number, seconds until the next step |
| `Totp::generate_now`, `verify_now`, `verify_now_with_last`, `seconds_remaining_now` | System clock (`std`, not on `wasm32-unknown-unknown`) |
| `Totp::to_uri(issuer, account)`, `Hotp::to_uri(issuer, account, counter)` | `otpauth://` enrolment URI |
| `OtpAuthUri::parse(&str)` / `FromStr` | Parsed URI: `kind`, `label_issuer`, `account`, `issuer`, `secret`, `algorithm`, `digits`, `period`, `counter`; `issuer()`, `to_totp()`, `to_hotp()` |
| `Code` | Generated code: `as_str`, `as_bytes`, `digits`, `value`, `Display`, constant-time `==` (also with `&str`) |
| `recovery_codes(count, groups, group_len)` | Distinct random codes like `k7m2-9xq4` (`std`) |
| `normalize_recovery_code`, `hash_recovery_code`, `verify_recovery_code` | Canonical form, SHA-256, constant-time check |
| `RECOVERY_ALPHABET` | `23456789abcdefghijkmnpqrstuvwxyz` |
| `Error` | One enum for everything; URI errors carry byte offsets; `Display` + `core::error::Error` |

## Platform support

| Target | Status |
|--------|--------|
| Linux, macOS, Windows | Everything, with the default `std` feature |
| `wasm32-unknown-unknown` | Builds with and without `std`. The `*_now` helpers are compiled out (`SystemTime::now` panics there); pass `Math.floor(Date.now() / 1000)` to `generate_at` / `verify_at`. `Secret::generate` and `recovery_codes` need the `dl_rand.fill` host import described in `dl-rand` |
| `no_std` + `alloc` | `default-features = false`: everything except secret and recovery-code generation and the clock helpers |

## Performance

`cargo bench -p dl-otp`: median time per operation, Intel Core i7-9850H (2.6 GHz),
Windows, rustc 1.100.0. Best median of three runs on a shared machine; individual runs
varied by up to 40 %.

| Case | Time |
|------|-----:|
| HOTP generate, SHA-1 | 0.90 µs |
| HOTP verify, look-ahead 10, no match (11 HMACs) | 5.6 µs |
| TOTP generate, SHA-1 / SHA-256 / SHA-512 | 0.89 / 1.15 / 1.58 µs |
| TOTP verify, skew 1 (3 HMACs), SHA-1 / SHA-256 / SHA-512 | 1.84 / 2.48 / 3.71 µs |
| `Totp::to_uri` | 0.30 µs |
| `OtpAuthUri::parse` | 0.83 µs |
| `recovery_codes(10, 2, 4)` (OS entropy included) | 2.7 µs |

A verification costs microseconds, so the HMAC is never the bottleneck of a login; the
limiting factor should be your rate limiter.

## Standards

- [RFC 4226](https://www.rfc-editor.org/rfc/rfc4226): HOTP, an HMAC-based one-time
  password algorithm (Appendix D test values).
- [RFC 6238](https://www.rfc-editor.org/rfc/rfc6238): TOTP, the time-based variant
  (Appendix B test values).
- [RFC 2104](https://www.rfc-editor.org/rfc/rfc2104): HMAC.
- [RFC 4648](https://www.rfc-editor.org/rfc/rfc4648): Base32.
- [RFC 3986](https://www.rfc-editor.org/rfc/rfc3986): URI syntax and percent-encoding.
- [Key Uri Format](https://github.com/google/google-authenticator/wiki/Key-Uri-Format):
  the `otpauth://` scheme.
- [NIST SP 800-63B](https://pages.nist.gov/800-63-3/sp800-63b.html): authenticator and
  rate-limiting requirements.

## Limitations

- No automatic clock-drift tracking (RFC 6238 section 6 resynchronisation). Use
  `with_skew` for known drift.
- No encryption of stored secrets; use your platform's KMS or an AEAD library.
- No QR rendering in this crate; use [dl-qr](../dl-qr).
- No OCRA (RFC 6287), Steam Guard codes or other non-numeric variants.
- `otpauth://` parsing ignores unknown parameters (`image`, `lock`, ...) and does not
  require the `issuer` parameter to match the label prefix.
- Zeroing of secrets is best effort (see [Security notes](#security-notes)).

## License

Licensed under either of [MIT](https://opensource.org/licenses/MIT) or
[Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0) at your option.
