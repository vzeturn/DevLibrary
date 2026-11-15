//! `cargo bench -p dl-jwt`
use dl_bench::Bench;
use dl_jwt::{Algorithm, Claims, Header, Key, Validation, decode, encode};

const NOW: u64 = 1_700_000_000;

/// A typical access token: registered claims plus a name, roles and a tenant.
fn claims() -> Claims {
    Claims::new()
        .issuer("https://auth.example.com")
        .subject("user-42")
        .audience("orders-api")
        .issued_at(NOW)
        .not_before(NOW)
        .expires_at(NOW + 900)
        .jwt_id("0f8fad5b-d9cb-469f-a165-70867728950e")
        .claim("name", "Nguyễn Văn An")
        .claim("roles", ["admin", "support"])
        .claim("tenant", "acme")
}

fn validation(alg: Algorithm) -> Validation {
    Validation {
        issuer: Some(vec!["https://auth.example.com".into()]),
        audience: Some(vec!["orders-api".into()]),
        ..Validation::new(alg)
    }
}

fn main() {
    let mut b = Bench::from_args();
    let claims = claims();
    let key256 = Key::from_secret([0x5a; 32]).unwrap();
    let key512 = Key::from_secret([0x5a; 64]).unwrap();

    let header = Header::new(Algorithm::HS256);
    let token256 = encode(&header, &claims, &key256).unwrap();
    println!("HS256 token: {} bytes", token256.len());
    b.run("encode HS256", || {
        encode(&header, &claims, &key256).unwrap()
    });
    let v = validation(Algorithm::HS256);
    b.run("decode+validate HS256", || {
        decode::<Claims>(&token256, &key256, &v, NOW).unwrap()
    });

    let header = Header::new(Algorithm::HS512);
    let token512 = encode(&header, &claims, &key512).unwrap();
    b.run("encode HS512", || {
        encode(&header, &claims, &key512).unwrap()
    });
    let v = validation(Algorithm::HS512);
    b.run("decode+validate HS512", || {
        decode::<Claims>(&token512, &key512, &v, NOW).unwrap()
    });

    // Rejections should be cheap: a forged signature never reaches the payload parser.
    let v = validation(Algorithm::HS256);
    let mut forged = token256.clone();
    forged.pop();
    forged.push(if token256.ends_with('A') { 'Q' } else { 'A' });
    b.run("reject forged signature HS256", || {
        decode::<Claims>(&forged, &key256, &v, NOW).unwrap_err()
    });
    b.run("decode_unverified", || {
        dl_jwt::decode_unverified(&token256).unwrap()
    });

    // Baselines: the two building blocks that dominate the cost.
    let signing_input = &token256[..token256.rfind('.').unwrap()];
    b.run("baseline: HMAC-SHA256 of signing input", || {
        dl_hash::Hmac::<dl_hash::Sha256>::mac(&[0x5a; 32], signing_input)
    });
    let payload = dl_json::to_string_of(&claims);
    println!(
        "signing input: {} bytes, payload JSON: {} bytes",
        signing_input.len(),
        payload.len()
    );
    b.run("baseline: dl_json::parse of payload", || {
        dl_json::parse(&payload).unwrap()
    });
}
