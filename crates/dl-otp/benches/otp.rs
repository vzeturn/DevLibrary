//! `cargo bench -p dl-otp`
use dl_bench::Bench;
use dl_otp::{Algorithm, Hotp, Secret, Totp};
use std::hint::black_box;

fn main() {
    let mut b = Bench::from_args();
    let now = 1_700_000_000u64;

    let sha1 = Secret::from_bytes(b"12345678901234567890").unwrap();
    let sha256 = Secret::from_bytes(b"12345678901234567890123456789012").unwrap();
    let sha512 =
        Secret::from_bytes(b"1234567890123456789012345678901234567890123456789012345678901234")
            .unwrap();

    let hotp = Hotp::new(sha1.clone());
    b.run("hotp generate sha1", || hotp.generate(black_box(42)));
    b.run("hotp verify sha1 look-ahead 10 (miss)", || {
        hotp.verify(black_box("000000"), black_box(0), 10)
    });

    for (name, secret, alg) in [
        ("sha1", sha1, Algorithm::Sha1),
        ("sha256", sha256, Algorithm::Sha256),
        ("sha512", sha512, Algorithm::Sha512),
    ] {
        let totp = Totp::with_params(secret, 6, alg, 30).unwrap();
        let code = totp.generate_at(now);
        b.run(&format!("totp generate {name}"), || {
            totp.generate_at(black_box(now))
        });
        b.run(&format!("totp verify {name} skew 1"), || {
            totp.verify_at(black_box(code.as_str()), black_box(now))
        });
    }

    let totp = Totp::new(Secret::from_base32("JBSWY3DPEHPK3PXP").unwrap());
    let uri = totp.to_uri("ACME Co", "user@example.com").unwrap();
    b.run("otpauth to_uri", || {
        totp.to_uri(black_box("ACME Co"), "user@example.com")
    });
    b.run("otpauth parse", || {
        dl_otp::OtpAuthUri::parse(black_box(&uri))
    });
    #[cfg(feature = "std")]
    b.run("recovery_codes 10 x 2x4", || {
        dl_otp::recovery_codes(10, 2, 4)
    });
}
