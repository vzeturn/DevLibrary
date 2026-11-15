//! TOTP enrolment and login, end to end, with the QR code rendered by `dl-qr`.
//!
//! `cargo run -p dl-otp --example enrol`
//!
//! Prints a terminal QR code for a fresh random secret (scan it with an authenticator app
//! to try it), then simulates the app to confirm enrolment, a login, a replay and a
//! recovery-code login.

use dl_otp::{
    Algorithm, OtpAuthUri, Secret, Totp, hash_recovery_code, recovery_codes, verify_recovery_code,
};
use dl_qr::{Ecc, QrCode, SvgOptions};
use std::time::{SystemTime, UNIX_EPOCH};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. New secret and enrolment URI.
    let totp = Totp::new(Secret::generate(Algorithm::Sha1)?);
    let uri = totp.to_uri("ACME Co", "user@example.com")?;

    // 2. QR code: SVG for a web page, half blocks for this terminal.
    let qr = QrCode::encode_text(&uri, Ecc::Medium)?;
    let svg = qr.to_svg(&SvgOptions::default());
    println!("QR version {} ({} bytes of SVG)", qr.version(), svg.len());
    print!("{}", qr.to_text(2));

    // 3. The app scans the code; here we parse the URI the way an app would.
    let app = OtpAuthUri::parse(&uri)?.to_totp()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();

    // 4. Confirm enrolment with the first code, keep the matched step.
    let first = app.generate_at(now);
    let mut last_step = totp
        .verify_at_with_last(first.as_str(), now, None)
        .ok_or("enrolment code rejected")?;
    println!("enrolled, step {last_step}");

    // 5. The same code again is a replay and is refused.
    assert_eq!(
        totp.verify_at_with_last(first.as_str(), now + 5, Some(last_step)),
        None
    );
    println!("replay refused");

    // 6. A later login with the next code succeeds and advances the stored step.
    let later = now + 30;
    let code = app.generate_at(later);
    last_step = totp
        .verify_at_with_last(code.as_str(), later, Some(last_step))
        .ok_or("login code rejected")?;
    println!("login ok, step {last_step}");

    // 7. Recovery codes: show once, store hashes, delete a hash when used.
    let codes = recovery_codes(10, 4, 4)?;
    let mut stored: Vec<[u8; 32]> = codes.iter().map(|c| hash_recovery_code(c)).collect();
    let typed = codes[3].to_uppercase().replace('-', " ");
    let used = stored
        .iter()
        .position(|h| verify_recovery_code(&typed, h))
        .ok_or("recovery code rejected")?;
    stored.remove(used);
    assert!(!stored.iter().any(|h| verify_recovery_code(&typed, h)));
    println!("recovery code accepted once, {} left", stored.len());
    Ok(())
}
