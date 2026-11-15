//! End-to-end tests: RFC vectors, interop tokens computed independently with Python
//! (`testdata/gen_vectors.py`), and every decoding error.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use dl_encoding::base64::URL_SAFE_NO_PAD;
use dl_json::{FromJson, ToJson, Value, json};

use crate::*;

// ---- Vectors ----

/// RFC 7515 Appendix A.1 (also RFC 7519 section 3.1).
const RFC7515_A1: &str = "eyJ0eXAiOiJKV1QiLA0KICJhbGciOiJIUzI1NiJ9\
    .eyJpc3MiOiJqb2UiLA0KICJleHAiOjEzMDA4MTkzODAsDQogImh0dHA6Ly9leGFtcGxlLmNvbS9pc19yb290Ijp0cnVlfQ\
    .dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const RFC7515_A1_KEY: &str =
    "AyM1SysPpbyDfgZld3umj1qzKObwVMkoqQ-EstJQLr_T-1qS0gZH75aKtMN3Yj0iPS4hcgUuTwjAzZr1Z9CAow";
/// RFC 7519 section 6.1: an unsecured JWT (`alg` = `none`).
const RFC7519_UNSECURED: &str = "eyJhbGciOiJub25lIn0\
    .eyJpc3MiOiJqb2UiLA0KICJleHAiOjEzMDA4MTkzODAsDQogImh0dHA6Ly9leGFtcGxlLmNvbS9pc19yb290Ijp0cnVlfQ.";

// Output of `python testdata/gen_vectors.py`.
const K256: &[u8] = b"dl-jwt/HS256/test-key/0123456789";
const K384: &[u8] = b"dl-jwt HS384 test key, forty-eight bytes long!!!";
const K512: &[u8] = b"dl-jwt HS512 test key; it is exactly sixty-four bytes long, see?";
const KDOTNET: &[u8] = b"ThisIsMyVerySecretKeyForJwtSigning!";
const HS256_FULL: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJodHRwczovL2F1dGguZXhhbXBsZS5jb20iLCJzdWIiOiJ1c2VyLTQyIiwiYXVkIjoib3JkZXJzLWFwaSIsImV4cCI6MTcwMDAwMDkwMCwibmJmIjoxNzAwMDAwMDAwLCJpYXQiOjE3MDAwMDAwMDAsImp0aSI6ImExYjJjMyIsIm5hbWUiOiJOZ3V54buFbiBWxINuIEFuIiwiYWRtaW4iOnRydWV9.Avqo-ZTpdzlxbyNiFa8lKQxI_ny0LSn9S5PWSbk_TOY";
const HS384_KID: &str = "eyJhbGciOiJIUzM4NCIsInR5cCI6IkpXVCIsImtpZCI6IjIwMjQtMDYifQ.eyJzdWIiOiJzdmMiLCJhdWQiOlsiYmlsbGluZyIsIm9yZGVycy1hcGkiXSwiZXhwIjoxNzAwMDAzNjAwLCJpYXQiOjE3MDAwMDAwMDB9.7tS7rjSm748ucEHNsp4Ro1nO8WqrchUQu6NQ7b8UnS5M6DWRSn3k12_Rw-TB1EAs";
const HS512: &str = "eyJhbGciOiJIUzUxMiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJ4IiwiZXhwIjoxNzAwMDAwMDYwLCJzY29wZSI6InJlYWQgd3JpdGUifQ.9mqPSPMNLLkU3li5CjHRtUEAZHxq8vuLZOln_fnSrecHPcc9Cx2XfSnj6ESTjgnUL5LIwQfjhU6F1gbE5gwfqQ";
const DOTNET: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJ1bmlxdWVfbmFtZSI6ImFsaWNlIiwibmFtZWlkIjoiNDIiLCJyb2xlIjpbIkFkbWluIiwiVXNlciJdLCJuYmYiOjE3MDAwMDAwMDAsImV4cCI6MTcwMDAwMzYwMCwiaWF0IjoxNzAwMDAwMDAwLCJpc3MiOiJodHRwczovL2xvY2FsaG9zdDo1MDAxIiwiYXVkIjoiaHR0cHM6Ly9sb2NhbGhvc3Q6NTAwMSJ9.YO4x_5vDYPglKdBNRT5ULZ4XAdXsO7f_2fyjhA5iXHA";
const FRACTIONAL_EXP: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiJmIiwiZXhwIjoxNzAwMDAwMDAwLjc1LCJpYXQiOjE2OTk5OTk5OTkuNX0.cuWxpKeQdljEiFddN6pwL-1opfI26vjb7UfJTE30Ujc";
const NODE: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJ1c2VySWQiOjcsImlhdCI6MTcwMDAwMDAwMCwiZXhwIjoxNzAwMDAwOTAwfQ.cZQSnLlO9RBE504t0u6_3ExLYXnVQ3TbHvXsUtvrgO4";

const NOW: u64 = 1_700_000_000;

// ---- Helpers ----

fn key(bytes: &[u8]) -> Key {
    Key::from_secret(bytes).unwrap()
}

fn hs256() -> Validation {
    Validation::new(Algorithm::HS256)
}

/// Signs raw header and payload JSON text, bypassing every check `encode` makes.
fn sign_raw(alg: Algorithm, secret: &[u8], header: &str, payload: &str) -> String {
    let input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header),
        URL_SAFE_NO_PAD.encode(payload)
    );
    let mac = alg.sign(secret, input.as_bytes());
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(mac.as_bytes()))
}

fn parts(token: &str) -> Vec<&str> {
    token.split('.').collect()
}

fn decode_claims(
    token: &str,
    key: &Key,
    v: &Validation,
    now: u64,
) -> Result<TokenData<Claims>, Error> {
    decode::<Claims>(token, key, v, now)
}

// ---- RFC vectors ----

#[test]
fn rfc7515_a1_hs256() {
    let key = Key::from_base64url(RFC7515_A1_KEY).unwrap();
    // exp is 2011-03-22T18:43:00Z.
    let exp = 1_300_819_380;
    let data = decode_claims(RFC7515_A1, &key, &hs256(), exp - 1).unwrap();
    assert_eq!(data.header.alg, Algorithm::HS256);
    assert_eq!(data.header.typ.as_deref(), Some("JWT"));
    assert_eq!(data.claims.iss.as_deref(), Some("joe"));
    assert_eq!(data.claims.exp, Some(exp));
    assert_eq!(data.claims.extra["http://example.com/is_root"], true);
    assert!(
        data.claims
            .get::<bool>("http://example.com/is_root")
            .unwrap()
    );

    // The leeway still accepts it for 60 seconds, then it is expired.
    assert!(decode_claims(RFC7515_A1, &key, &hs256(), exp + 59).is_ok());
    assert_eq!(
        decode_claims(RFC7515_A1, &key, &hs256(), exp + 60).unwrap_err(),
        Error::Expired
    );
    assert_eq!(
        decode_claims(RFC7515_A1, &key, &hs256(), NOW).unwrap_err(),
        Error::Expired
    );

    // Another key fails the signature check.
    let other = key_flip(&key);
    assert_eq!(
        decode_claims(RFC7515_A1, &other, &hs256(), exp).unwrap_err(),
        Error::InvalidSignature
    );
}

fn key_flip(key: &Key) -> Key {
    let mut bytes = key.secret().to_vec();
    bytes[0] ^= 1;
    Key::from_secret(&bytes).unwrap()
}

#[test]
fn rfc7519_unsecured_jwt_is_rejected() {
    let all = Validation {
        algorithms: vec![Algorithm::HS256, Algorithm::HS384, Algorithm::HS512],
        ..hs256()
    };
    let err = decode_claims(RFC7519_UNSECURED, &key(K256), &all, 0).unwrap_err();
    assert_eq!(err, Error::UnsupportedAlgorithm("none".into()));
    assert_eq!(
        decode_header(RFC7519_UNSECURED).unwrap_err(),
        Error::UnsupportedAlgorithm("none".into())
    );

    // Variants of "none" and a "none" header with a real HMAC are rejected too.
    for alg in ["none", "None", "NONE", "nOnE"] {
        let header = format!(r#"{{"alg":"{alg}","typ":"JWT"}}"#);
        let t = sign_raw(Algorithm::HS256, K256, &header, r#"{"exp":9999999999}"#);
        assert_eq!(
            decode_claims(&t, &key(K256), &all, 0).unwrap_err(),
            Error::UnsupportedAlgorithm(alg.into())
        );
    }
}

// ---- Python interop vectors ----

#[test]
fn hs256_vector_decodes_and_reencodes() {
    let v = Validation {
        issuer: Some(vec!["https://auth.example.com".into()]),
        audience: Some(vec!["orders-api".into()]),
        subject: Some("user-42".into()),
        max_age: Some(3600),
        ..hs256()
    };
    let data = decode_claims(HS256_FULL, &key(K256), &v, NOW + 10).unwrap();
    let c = &data.claims;
    assert_eq!(c.iss.as_deref(), Some("https://auth.example.com"));
    assert_eq!(c.sub.as_deref(), Some("user-42"));
    assert_eq!(c.aud, Some(Audience::Single("orders-api".into())));
    assert_eq!(
        (c.exp, c.nbf, c.iat),
        (Some(NOW + 900), Some(NOW), Some(NOW))
    );
    assert_eq!(c.jti.as_deref(), Some("a1b2c3"));
    assert_eq!(c.get::<String>("name").unwrap(), "Nguyễn Văn An");
    assert!(c.get::<bool>("admin").unwrap());

    let claims = Claims::new()
        .issuer("https://auth.example.com")
        .subject("user-42")
        .audience("orders-api")
        .expires_at(NOW + 900)
        .not_before(NOW)
        .issued_at(NOW)
        .jwt_id("a1b2c3")
        .claim("name", "Nguyễn Văn An")
        .claim("admin", true);
    assert_eq!(
        encode(&Header::default(), &claims, &key(K256)).unwrap(),
        HS256_FULL
    );
    // A decoded claims set re-encodes to the same token.
    assert_eq!(
        encode(&data.header, &data.claims, &key(K256)).unwrap(),
        HS256_FULL
    );
}

#[test]
fn hs384_vector_with_kid() {
    let mut keys = KeySet::new();
    keys.insert("2024-05", key(&[9u8; 48]));
    keys.insert("2024-06", key(K384));
    let v = Validation {
        algorithms: vec![Algorithm::HS384],
        audience: Some(vec!["orders-api".into()]),
        ..hs256()
    };
    let data = keys.decode::<Claims>(HS384_KID, &v, NOW).unwrap();
    assert_eq!(data.header.alg, Algorithm::HS384);
    assert_eq!(data.header.kid.as_deref(), Some("2024-06"));
    assert_eq!(
        data.claims.aud,
        Some(Audience::Multiple(vec![
            "billing".into(),
            "orders-api".into()
        ]))
    );

    let header = Header::new(Algorithm::HS384).key_id("2024-06");
    let claims = Claims::new()
        .subject("svc")
        .audiences(["billing", "orders-api"])
        .expires_at(NOW + 3600)
        .issued_at(NOW);
    assert_eq!(encode(&header, &claims, &key(K384)).unwrap(), HS384_KID);
}

#[test]
fn hs512_vector() {
    let v = Validation {
        algorithms: vec![Algorithm::HS512],
        ..hs256()
    };
    let data = decode_claims(HS512, &key(K512), &v, NOW).unwrap();
    assert_eq!(data.claims.get::<String>("scope").unwrap(), "read write");
    let claims = Claims::new()
        .issuer("x")
        .expires_at(NOW + 60)
        .claim("scope", "read write");
    let header = Header::new(Algorithm::HS512);
    assert_eq!(encode(&header, &claims, &key(K512)).unwrap(), HS512);
    assert_eq!(parts(HS512)[2].len(), 86); // 64 bytes
}

/// The claims ASP.NET Core writes for `ClaimTypes.Name`, `NameIdentifier` and `Role`.
#[derive(Debug, PartialEq)]
struct DotNetClaims {
    unique_name: String,
    nameid: String,
    role: Vec<String>,
    nbf: u64,
    exp: u64,
    iat: u64,
    iss: String,
    aud: String,
}

impl FromJson for DotNetClaims {
    fn from_json(v: &Value) -> Result<Self, dl_json::Error> {
        Ok(DotNetClaims {
            unique_name: v.field("unique_name")?,
            nameid: v.field("nameid")?,
            role: v.field("role")?,
            nbf: v.field("nbf")?,
            exp: v.field("exp")?,
            iat: v.field("iat")?,
            iss: v.field("iss")?,
            aud: v.field("aud")?,
        })
    }
}

impl ToJson for DotNetClaims {
    fn to_json(&self) -> Value {
        json!({
            "unique_name": self.unique_name,
            "nameid": self.nameid,
            "role": self.role,
            "nbf": self.nbf,
            "exp": self.exp,
            "iat": self.iat,
            "iss": self.iss,
            "aud": self.aud,
        })
    }
}

#[test]
fn dotnet_style_token() {
    let v = Validation {
        issuer: Some(vec!["https://localhost:5001".into()]),
        audience: Some(vec!["https://localhost:5001".into()]),
        leeway_secs: 300, // ClockSkew default in ASP.NET Core
        ..hs256()
    };
    let data = decode::<DotNetClaims>(DOTNET, &key(KDOTNET), &v, NOW + 3600 + 299).unwrap();
    let expected = DotNetClaims {
        unique_name: "alice".into(),
        nameid: "42".into(),
        role: vec!["Admin".into(), "User".into()],
        nbf: NOW,
        exp: NOW + 3600,
        iat: NOW,
        iss: "https://localhost:5001".into(),
        aud: "https://localhost:5001".into(),
    };
    assert_eq!(data.claims, expected);
    assert_eq!(
        encode(&Header::default(), &expected, &key(KDOTNET)).unwrap(),
        DOTNET
    );
    assert_eq!(
        decode::<DotNetClaims>(DOTNET, &key(KDOTNET), &v, NOW + 3900).unwrap_err(),
        Error::Expired
    );

    // The same token through the generic claims type.
    let c = decode_claims(DOTNET, &key(KDOTNET), &v, NOW)
        .unwrap()
        .claims;
    assert_eq!(c.get::<Vec<String>>("role").unwrap(), ["Admin", "User"]);
    assert_eq!(c.get::<String>("nameid").unwrap(), "42");
    assert_eq!(
        c.extra.keys().map(String::as_str).collect::<Vec<_>>(),
        ["unique_name", "nameid", "role"]
    );
}

#[test]
fn fractional_numeric_dates() {
    let v = Validation {
        leeway_secs: 0,
        ..hs256()
    };
    let data = decode_claims(FRACTIONAL_EXP, &key(K256), &v, NOW - 1).unwrap();
    assert_eq!(data.claims.exp, Some(NOW));
    assert_eq!(data.claims.iat, Some(NOW - 1));
    assert_eq!(
        decode_claims(FRACTIONAL_EXP, &key(K256), &v, NOW).unwrap_err(),
        Error::Expired
    );
}

#[test]
fn node_jsonwebtoken_token() {
    struct Session {
        user_id: u32,
    }
    impl FromJson for Session {
        fn from_json(v: &Value) -> Result<Self, dl_json::Error> {
            Ok(Session {
                user_id: v.field("userId")?,
            })
        }
    }
    let data = decode::<Session>(NODE, &key(K256), &hs256(), NOW + 899).unwrap();
    assert_eq!(data.claims.user_id, 7);
    let claims = Claims::new()
        .claim("userId", 7)
        .issued_at(NOW)
        .expires_at(NOW + 900);
    // Different member order than jsonwebtoken (registered claims first), same claims.
    let ours = encode(&Header::default(), &claims, &key(K256)).unwrap();
    assert_ne!(ours, NODE);
    let a = decode_claims(&ours, &key(K256), &hs256(), NOW)
        .unwrap()
        .claims;
    let b = decode_claims(NODE, &key(K256), &hs256(), NOW)
        .unwrap()
        .claims;
    assert_eq!(a, b);
}

// ---- Round trips ----

#[test]
fn round_trip_all_algorithms() {
    let secret: Vec<u8> = (0u8..64).collect();
    for alg in [Algorithm::HS256, Algorithm::HS384, Algorithm::HS512] {
        let key = key(&secret[..alg.min_key_len()]);
        let header = Header::new(alg)
            .key_id("k")
            .content_type("JWT")
            .param("x-trace", "abc");
        let claims = Claims::new().subject("s").expires_at(NOW + 10).claim(
            "nested",
            json!({"a": [1, 2, {"b": null}], "c": "\u{2028}\"\\"}),
        );
        let token = encode(&header, &claims, &key).unwrap();
        let v = Validation {
            algorithms: vec![alg],
            ..hs256()
        };
        let data = decode_claims(&token, &key, &v, NOW).unwrap();
        assert_eq!(data.header, header);
        assert_eq!(data.claims, claims);
        assert_eq!(
            URL_SAFE_NO_PAD.decode(parts(&token)[2]).unwrap().len(),
            alg.signature_len()
        );
    }
}

#[test]
fn custom_claims_struct() {
    #[derive(Debug, PartialEq)]
    struct AccessClaims {
        sub: String,
        exp: u64,
        tenant: String,
        roles: Vec<String>,
    }
    impl ToJson for AccessClaims {
        fn to_json(&self) -> Value {
            json!({"sub": self.sub, "exp": self.exp, "tenant": self.tenant, "roles": self.roles})
        }
    }
    impl FromJson for AccessClaims {
        fn from_json(v: &Value) -> Result<Self, dl_json::Error> {
            let c = AccessClaims {
                sub: v.field("sub")?,
                exp: v.field("exp")?,
                tenant: v.field("tenant")?,
                roles: v.field("roles")?,
            };
            if c.roles.is_empty() {
                return Err(dl_json::Error::custom("at least one role is required"));
            }
            Ok(c)
        }
    }

    let k = key(K256);
    let claims = AccessClaims {
        sub: "7".into(),
        exp: NOW + 60,
        tenant: "acme".into(),
        roles: vec!["admin".into()],
    };
    let token = encode(&Header::default(), &claims, &k).unwrap();
    let v = Validation {
        required_claims: vec!["exp".into(), "tenant".into()],
        ..hs256()
    };
    assert_eq!(
        decode::<AccessClaims>(&token, &k, &v, NOW).unwrap().claims,
        claims
    );
    // Registered-claim validation applies to custom types too.
    assert_eq!(
        decode::<AccessClaims>(&token, &k, &v, NOW + 120).unwrap_err(),
        Error::Expired
    );

    // Conversion errors from the custom type surface as InvalidJson on the payload.
    let empty = AccessClaims {
        roles: vec![],
        ..claims
    };
    let token = encode(&Header::default(), &empty, &k).unwrap();
    let err = decode::<AccessClaims>(&token, &k, &v, NOW).unwrap_err();
    assert!(
        matches!(
            &err,
            Error::InvalidJson {
                part: Part::Payload,
                ..
            }
        ),
        "{err:?}"
    );
    assert!(err.to_string().contains("at least one role"));
}

// ---- Algorithm checks ----

#[test]
fn algorithm_confusion() {
    // A valid HS512 token is refused when only HS256 is allowed, before any key use.
    let token = encode(
        &Header::new(Algorithm::HS512),
        &Claims::new().expires_at(NOW + 60),
        &key(K512),
    )
    .unwrap();
    assert_eq!(
        decode_claims(&token, &key(K512), &hs256(), NOW).unwrap_err(),
        Error::AlgorithmNotAllowed
    );

    // Rewriting the header's alg breaks the signature.
    let token = encode(
        &Header::default(),
        &Claims::new().expires_at(NOW + 60),
        &key(K512),
    )
    .unwrap();
    let p = parts(&token);
    let forged_header = URL_SAFE_NO_PAD.encode(r#"{"alg":"HS512","typ":"JWT"}"#);
    let forged = format!("{forged_header}.{}.{}", p[1], p[2]);
    let both = Validation {
        algorithms: vec![Algorithm::HS256, Algorithm::HS512],
        ..hs256()
    };
    assert_eq!(
        decode_claims(&forged, &key(K512), &both, NOW).unwrap_err(),
        Error::InvalidSignature
    );

    // An empty allow-list rejects everything.
    let none = Validation {
        algorithms: vec![],
        ..hs256()
    };
    assert_eq!(
        decode_claims(&token, &key(K512), &none, NOW).unwrap_err(),
        Error::AlgorithmNotAllowed
    );
}

#[test]
fn asymmetric_algorithms_are_unsupported() {
    for alg in [
        "RS256", "RS512", "PS256", "ES256", "ES384", "EdDSA", "hs256", "",
    ] {
        let header = format!(r#"{{"alg":"{alg}","typ":"JWT"}}"#);
        let t = sign_raw(Algorithm::HS256, K256, &header, r#"{"exp":9999999999}"#);
        assert_eq!(
            decode_claims(&t, &key(K256), &hs256(), NOW).unwrap_err(),
            Error::UnsupportedAlgorithm(alg.into())
        );
    }
}

#[test]
fn critical_headers_are_rejected() {
    let t = sign_raw(
        Algorithm::HS256,
        K256,
        r#"{"alg":"HS256","crit":["b64"],"b64":false}"#,
        "{}",
    );
    assert_eq!(
        decode_claims(&t, &key(K256), &hs256(), NOW).unwrap_err(),
        Error::UnsupportedCriticalHeader("b64".into())
    );
    let t = sign_raw(Algorithm::HS256, K256, r#"{"alg":"HS256","crit":[]}"#, "{}");
    assert!(matches!(
        decode_claims(&t, &key(K256), &hs256(), NOW).unwrap_err(),
        Error::InvalidJson {
            part: Part::Header,
            ..
        }
    ));
}

// ---- Signature checks ----

#[test]
fn tampered_tokens() {
    let k = key(K256);
    let claims = Claims::new()
        .subject("user")
        .claim("admin", false)
        .expires_at(NOW + 60);
    let token = encode(&Header::default(), &claims, &k).unwrap();
    let p = parts(&token);

    // Payload replaced with an escalated claims set.
    let evil = URL_SAFE_NO_PAD.encode(dl_json::to_string_of(&claims.clone().claim("admin", true)));
    let forged = format!("{}.{evil}.{}", p[0], p[2]);
    assert_eq!(
        decode_claims(&forged, &k, &hs256(), NOW).unwrap_err(),
        Error::InvalidSignature
    );

    // Every single-bit flip of the signature fails.
    let sig = URL_SAFE_NO_PAD.decode(p[2]).unwrap();
    for i in 0..sig.len() * 8 {
        let mut bad = sig.clone();
        bad[i / 8] ^= 1 << (i % 8);
        let t = format!("{}.{}.{}", p[0], p[1], URL_SAFE_NO_PAD.encode(&bad));
        assert_eq!(
            decode_claims(&t, &k, &hs256(), NOW).unwrap_err(),
            Error::InvalidSignature
        );
    }

    // Truncated, extended and empty signatures.
    let extended = [sig.clone(), vec![0]].concat();
    for bad in [&sig[..31], &extended[..], &[][..]] {
        let t = format!("{}.{}.{}", p[0], p[1], URL_SAFE_NO_PAD.encode(bad));
        assert_eq!(
            decode_claims(&t, &k, &hs256(), NOW).unwrap_err(),
            Error::InvalidSignature
        );
    }
    // The signature of another token.
    let other = encode(&Header::default(), &Claims::new().expires_at(NOW + 61), &k).unwrap();
    let t = format!("{}.{}.{}", p[0], p[1], parts(&other)[2]);
    assert_eq!(
        decode_claims(&t, &k, &hs256(), NOW).unwrap_err(),
        Error::InvalidSignature
    );
}

#[test]
fn signature_is_checked_before_claims() {
    let k = key(K256);
    let wrong = key(&[0u8; 32]);
    // Expired, invalid payload JSON and wrong claim types are not reported for forged tokens.
    let expired = encode(&Header::default(), &Claims::new().expires_at(1), &wrong).unwrap();
    assert_eq!(
        decode_claims(&expired, &k, &hs256(), NOW).unwrap_err(),
        Error::InvalidSignature
    );
    for payload in ["not json", "[1]", r#"{"exp":"x"}"#] {
        let t = sign_raw(Algorithm::HS256, &[0u8; 32], r#"{"alg":"HS256"}"#, payload);
        assert_eq!(
            decode_claims(&t, &k, &hs256(), NOW).unwrap_err(),
            Error::InvalidSignature
        );
    }
}

// ---- Structure and encoding ----

#[test]
fn wrong_part_counts() {
    let k = key(K256);
    let cases = [
        ("", Part::Payload),
        ("abc", Part::Payload),
        ("eyJhbGciOiJIUzI1NiJ9.e30", Part::Signature),
        ("a.b.c.d", Part::Signature),
        ("a.b.c.d.e", Part::Signature),
        (".e30.c2ln", Part::Header),
        ("eyJhbGciOiJIUzI1NiJ9..c2ln", Part::Payload),
        ("..", Part::Header),
    ];
    for (token, part) in cases {
        assert_eq!(
            decode_claims(token, &k, &hs256(), NOW).unwrap_err(),
            Error::Malformed { part },
            "{token:?}"
        );
        assert_eq!(
            decode_unverified(token).unwrap_err(),
            Error::Malformed { part },
            "{token:?}"
        );
    }
}

#[test]
fn strict_base64url() {
    let k = key(K256);
    let token = encode(&Header::default(), &Claims::new().expires_at(NOW + 60), &k).unwrap();
    let p = parts(&token);
    assert_eq!(p[0].len() % 4, 0); // the default header has no padding to add
    let cases = [
        (format!("{}=.{}.{}", p[0], p[1], p[2]), Part::Header),
        (format!("{}.{}==.{}", p[0], p[1], p[2]), Part::Payload),
        (format!("{}.{}.{}=", p[0], p[1], p[2]), Part::Signature),
        (format!("{} .{}.{}", p[0], p[1], p[2]), Part::Header),
        (format!("{}.{}\n.{}", p[0], p[1], p[2]), Part::Payload),
        (
            format!(
                "{}.{}.{}",
                p[0],
                p[1],
                p[2].replace('-', "+").replace('_', "/") + "+"
            ),
            Part::Signature,
        ),
        (
            format!("{}.{}.{}", p[0], p[1], &p[2][..p[2].len() - 2]),
            Part::Signature,
        ), // impossible length
        (format!("{}.{}.{}", p[0], "e30xx", p[2]), Part::Payload), // impossible length
        (format!("{}.{}.{}", p[0], "e31", p[2]), Part::Payload),   // non-zero trailing bits
    ];
    for (t, part) in &cases {
        assert_eq!(
            decode_claims(t, &k, &hs256(), NOW).unwrap_err(),
            Error::InvalidBase64 { part: *part },
            "{t}"
        );
    }
}

#[test]
fn invalid_json() {
    let k = key(K256);
    let header_cases = [
        "{",
        "[]",
        r#""HS256""#,
        "{}",
        r#"{"alg":256}"#,
        r#"{"alg":"HS256","kid":1}"#,
    ];
    for header in header_cases {
        let t = sign_raw(Algorithm::HS256, K256, header, "{}");
        let err = decode_claims(&t, &k, &hs256(), NOW).unwrap_err();
        assert!(
            matches!(
                err,
                Error::InvalidJson {
                    part: Part::Header,
                    ..
                }
            ),
            "{header}: {err:?}"
        );
    }
    // Invalid UTF-8 in the header.
    let t = format!("{}.e30.c2ln", URL_SAFE_NO_PAD.encode(b"{\"alg\":\"\xff\"}"));
    assert!(matches!(
        decode_claims(&t, &k, &hs256(), NOW).unwrap_err(),
        Error::InvalidJson {
            part: Part::Header,
            ..
        }
    ));

    // Validly signed payloads that are not claims objects.
    for payload in [
        "",
        " ",
        "[]",
        "null",
        "1",
        "{\"exp\":1",
        r#"{"exp":"soon"}"#,
        r#"{"aud":[1]}"#,
    ] {
        let t = sign_raw(Algorithm::HS256, K256, r#"{"alg":"HS256"}"#, payload);
        let err = decode_claims(&t, &k, &hs256(), NOW).unwrap_err();
        if payload.is_empty() {
            assert_eq!(
                err,
                Error::Malformed {
                    part: Part::Payload
                }
            );
        } else {
            assert!(
                matches!(
                    err,
                    Error::InvalidJson {
                        part: Part::Payload,
                        ..
                    }
                ),
                "{payload}: {err:?}"
            );
        }
    }
    let t = sign_raw(
        Algorithm::HS256,
        K256,
        r#"{"alg":"HS256"}"#,
        r#"{"exp":"soon"}"#,
    );
    let Error::InvalidJson { error, .. } = decode_claims(&t, &k, &hs256(), NOW).unwrap_err() else {
        panic!("expected InvalidJson");
    };
    assert_eq!(error.path(), "/exp");
}

#[test]
fn oversized_tokens() {
    let k = key(K256);
    let big = "x".repeat(70_000);
    let token = encode(
        &Header::default(),
        &Claims::new()
            .expires_at(NOW + 60)
            .claim("blob", big.as_str()),
        &k,
    )
    .unwrap();
    assert!(token.len() > DEFAULT_MAX_TOKEN_LEN);
    assert_eq!(
        decode_claims(&token, &k, &hs256(), NOW).unwrap_err(),
        Error::TokenTooLarge
    );
    assert_eq!(decode_unverified(&token).unwrap_err(), Error::TokenTooLarge);
    assert_eq!(decode_header(&token).unwrap_err(), Error::TokenTooLarge);
    let roomy = Validation {
        max_token_len: 1 << 20,
        ..hs256()
    };
    assert!(decode_claims(&token, &k, &roomy, NOW).is_ok());

    // The limit is checked first, even for garbage, and is inclusive.
    let tight = Validation {
        max_token_len: 10,
        ..hs256()
    };
    assert_eq!(
        decode_claims("...........", &k, &tight, NOW).unwrap_err(),
        Error::TokenTooLarge
    );
    assert_eq!(
        decode_claims("a.b.c.d.e.f", &k, &tight, NOW).unwrap_err(),
        Error::TokenTooLarge
    );
    assert_eq!(
        decode_claims("a.b.c.d.ef", &k, &tight, NOW).unwrap_err(),
        Error::Malformed {
            part: Part::Signature
        }
    );

    // Deeply nested JSON inside the size limit is refused, not a stack overflow.
    let deep = format!("{{\"a\":{}{}}}", "[".repeat(5000), "]".repeat(5000));
    let t = sign_raw(Algorithm::HS256, K256, r#"{"alg":"HS256"}"#, &deep);
    assert!(matches!(
        decode_claims(&t, &k, &hs256(), NOW).unwrap_err(),
        Error::InvalidJson {
            part: Part::Payload,
            ..
        }
    ));
}

// ---- Keys ----

#[test]
fn key_length_per_algorithm() {
    let k32 = key(&[1u8; 32]);
    let claims = Claims::new().expires_at(NOW + 60);
    assert_eq!(
        encode(&Header::new(Algorithm::HS384), &claims, &k32).unwrap_err(),
        Error::KeyTooShort { len: 32, min: 48 }
    );
    assert_eq!(
        encode(&Header::new(Algorithm::HS512), &claims, &k32).unwrap_err(),
        Error::KeyTooShort { len: 32, min: 64 }
    );
    // Decoding an HS512 token with a key too short for it fails the same way.
    let v = Validation {
        algorithms: vec![Algorithm::HS512],
        ..hs256()
    };
    assert_eq!(
        decode_claims(HS512, &k32, &v, NOW).unwrap_err(),
        Error::KeyTooShort { len: 32, min: 64 }
    );
    // Longer keys than the minimum are fine (and hashed first when above the block size).
    let long = key(&[3u8; 200]);
    let token = encode(&Header::new(Algorithm::HS256), &claims, &long).unwrap();
    assert!(decode_claims(&token, &long, &hs256(), NOW).is_ok());
}

#[test]
fn kid_based_key_selection() {
    let mut keys = KeySet::new();
    keys.insert("old", key(&[1u8; 32]));
    keys.insert("new", key(&[2u8; 32]));
    let claims = Claims::new().expires_at(NOW + 60);
    let v = hs256();

    for kid in ["old", "new"] {
        let t = encode(
            &Header::default().key_id(kid),
            &claims,
            keys.get(kid).unwrap(),
        )
        .unwrap();
        assert_eq!(
            keys.decode::<Claims>(&t, &v, NOW)
                .unwrap()
                .header
                .kid
                .as_deref(),
            Some(kid)
        );
    }
    // Unknown kid, no kid.
    let t = encode(&Header::default().key_id("gone"), &claims, &key(&[1u8; 32])).unwrap();
    assert_eq!(
        keys.decode::<Claims>(&t, &v, NOW).unwrap_err(),
        Error::UnknownKey
    );
    let t = encode(&Header::default(), &claims, &key(&[1u8; 32])).unwrap();
    assert_eq!(
        keys.decode::<Claims>(&t, &v, NOW).unwrap_err(),
        Error::UnknownKey
    );
    // A token that names one key but was signed with the other.
    let t = encode(
        &Header::default().key_id("new"),
        &claims,
        keys.get("old").unwrap(),
    )
    .unwrap();
    assert_eq!(
        keys.decode::<Claims>(&t, &v, NOW).unwrap_err(),
        Error::InvalidSignature
    );

    // The selector runs after the algorithm check and sees the parsed header.
    let t = encode(
        &Header::new(Algorithm::HS512).key_id("x"),
        &claims,
        &key(&[5u8; 64]),
    )
    .unwrap();
    let mut called = false;
    let err = decode_with::<Claims, _>(
        &t,
        |_| {
            called = true;
            None
        },
        &v,
        NOW,
    )
    .unwrap_err();
    assert_eq!(err, Error::AlgorithmNotAllowed);
    assert!(!called);
}

// ---- Claim validation through decode ----

#[test]
fn every_validation_error() {
    let k = key(K256);
    let enc = |c: &Claims| encode(&Header::default(), c, &k).unwrap();
    let base = Claims::new().expires_at(NOW + 60);
    let v = hs256();
    let cases: Vec<(Claims, Validation, Error)> = vec![
        (Claims::new(), v.clone(), Error::MissingClaim("exp".into())),
        (
            Claims::new().expires_at(NOW - 61),
            v.clone(),
            Error::Expired,
        ),
        (
            base.clone().not_before(NOW + 61),
            v.clone(),
            Error::NotYetValid,
        ),
        (
            base.clone().issued_at(NOW - 1000),
            Validation {
                max_age: Some(600),
                ..v.clone()
            },
            Error::TooOld,
        ),
        (
            base.clone(),
            Validation {
                max_age: Some(600),
                ..v.clone()
            },
            Error::MissingClaim("iat".into()),
        ),
        (
            base.clone().issuer("evil"),
            Validation {
                issuer: Some(vec!["good".into()]),
                ..v.clone()
            },
            Error::InvalidIssuer,
        ),
        (
            base.clone(),
            Validation {
                issuer: Some(vec!["good".into()]),
                ..v.clone()
            },
            Error::MissingClaim("iss".into()),
        ),
        (
            base.clone().audience("other"),
            Validation {
                audience: Some(vec!["me".into()]),
                ..v.clone()
            },
            Error::InvalidAudience,
        ),
        (
            base.clone().audiences(["a", "b"]),
            Validation {
                audience: Some(vec!["me".into()]),
                ..v.clone()
            },
            Error::InvalidAudience,
        ),
        (
            base.clone().audience("someone"),
            v.clone(),
            Error::InvalidAudience,
        ),
        (
            base.clone(),
            Validation {
                audience: Some(vec!["me".into()]),
                ..v.clone()
            },
            Error::MissingClaim("aud".into()),
        ),
        (
            base.clone().subject("1"),
            Validation {
                subject: Some("2".into()),
                ..v.clone()
            },
            Error::InvalidSubject,
        ),
        (
            base.clone(),
            Validation {
                subject: Some("2".into()),
                ..v.clone()
            },
            Error::MissingClaim("sub".into()),
        ),
        (
            base.clone(),
            Validation {
                required_claims: vec!["jti".into()],
                ..v.clone()
            },
            Error::MissingClaim("jti".into()),
        ),
    ];
    for (claims, validation, expected) in cases {
        assert_eq!(
            decode_claims(&enc(&claims), &k, &validation, NOW).unwrap_err(),
            expected,
            "{claims:?}"
        );
    }
    // And the matching successes.
    let ok = base
        .clone()
        .issuer("good")
        .subject("2")
        .audiences(["x", "me"])
        .issued_at(NOW - 10)
        .not_before(NOW + 59)
        .jwt_id("j");
    let strict = Validation {
        issuer: Some(vec!["good".into()]),
        audience: Some(vec!["me".into()]),
        subject: Some("2".into()),
        max_age: Some(600),
        required_claims: vec!["exp".into(), "jti".into(), "iat".into()],
        ..v
    };
    assert!(decode_claims(&enc(&ok), &k, &strict, NOW).is_ok());
}

#[test]
fn leeway_edges() {
    let k = key(K256);
    let t = encode(
        &Header::default(),
        &Claims::new().not_before(NOW).expires_at(NOW + 100),
        &k,
    )
    .unwrap();
    let v = Validation {
        leeway_secs: 10,
        ..hs256()
    };
    assert_eq!(
        decode_claims(&t, &k, &v, NOW - 11).unwrap_err(),
        Error::NotYetValid
    );
    assert!(decode_claims(&t, &k, &v, NOW - 10).is_ok());
    assert!(decode_claims(&t, &k, &v, NOW + 109).is_ok());
    assert_eq!(
        decode_claims(&t, &k, &v, NOW + 110).unwrap_err(),
        Error::Expired
    );
    let v0 = Validation {
        leeway_secs: 0,
        ..hs256()
    };
    assert_eq!(
        decode_claims(&t, &k, &v0, NOW - 1).unwrap_err(),
        Error::NotYetValid
    );
    assert!(decode_claims(&t, &k, &v0, NOW).is_ok());
    assert!(decode_claims(&t, &k, &v0, NOW + 99).is_ok());
    assert_eq!(
        decode_claims(&t, &k, &v0, NOW + 100).unwrap_err(),
        Error::Expired
    );
}

#[test]
fn audience_string_vs_array() {
    let k = key(K256);
    let v = Validation {
        audience: Some(vec!["api".into()]),
        ..hs256()
    };
    for claims in [
        Claims::new().audience("api"),
        Claims::new().audiences(["api"]),
        Claims::new().audiences(["web", "api"]),
    ] {
        let t = encode(&Header::default(), &claims.expires_at(NOW + 60), &k).unwrap();
        assert!(decode_claims(&t, &k, &v, NOW).is_ok());
    }
    let t = encode(
        &Header::default(),
        &Claims::new()
            .audiences(Vec::<String>::new())
            .expires_at(NOW + 60),
        &k,
    )
    .unwrap();
    assert_eq!(
        decode_claims(&t, &k, &v, NOW).unwrap_err(),
        Error::InvalidAudience
    );
}

// ---- encode errors, unverified decoding ----

#[test]
fn encode_requires_an_object() {
    let k = key(K256);
    for payload in [json!([1]), json!("x"), json!(null), json!(1)] {
        let err = encode(&Header::default(), &payload, &k).unwrap_err();
        assert!(matches!(
            err,
            Error::InvalidJson {
                part: Part::Payload,
                ..
            }
        ));
    }
    assert!(encode(&Header::default(), &json!({}), &k).is_ok());
}

#[test]
fn unverified_decoding() {
    // Any algorithm and any signature: nothing is checked but the format.
    let t = sign_raw(
        Algorithm::HS256,
        K256,
        r#"{"alg":"RS256","kid":"rsa-1"}"#,
        r#"{"sub":"x","exp":1}"#,
    );
    let u = decode_unverified(&t).unwrap();
    assert_eq!((u.alg(), u.kid()), (Some("RS256"), Some("rsa-1")));
    assert_eq!(u.claims.sub.as_deref(), Some("x"));
    assert_eq!(u.claims.exp, Some(1));

    let u = decode_unverified(HS256_FULL).unwrap();
    assert_eq!(u.claims.get::<String>("name").unwrap(), "Nguyễn Văn An");

    // Still strict about structure.
    assert_eq!(
        decode_unverified("e30.e30.c2ln=").unwrap_err(),
        Error::InvalidBase64 {
            part: Part::Signature
        }
    );
    assert!(matches!(
        decode_unverified("W10.e30.").unwrap_err(),
        Error::InvalidJson {
            part: Part::Header,
            ..
        }
    ));
    assert!(matches!(
        decode_unverified("e30.W10.").unwrap_err(),
        Error::InvalidJson {
            part: Part::Payload,
            ..
        }
    ));
    let u = decode_unverified("e30.e30.").unwrap();
    assert_eq!((u.alg(), u.kid()), (None, None));

    let h = decode_header(HS384_KID).unwrap();
    assert_eq!(
        (h.alg, h.kid.as_deref()),
        (Algorithm::HS384, Some("2024-06"))
    );
}

#[cfg(all(
    feature = "std",
    not(all(target_arch = "wasm32", target_os = "unknown"))
))]
#[test]
fn decode_with_system_clock() {
    let k = key(K256);
    let now = crate::now();
    assert!(now > NOW);
    let t = encode(
        &Header::default(),
        &Claims::new().issued_at(now).expires_at(now + 300),
        &k,
    )
    .unwrap();
    assert!(decode_now::<Claims>(&t, &k, &hs256()).is_ok());
    assert_eq!(
        decode_now::<Claims>(
            HS256_FULL,
            &k,
            &Validation {
                audience: Some(vec!["orders-api".into()]),
                ..hs256()
            }
        )
        .unwrap_err(),
        Error::Expired
    );
}

#[test]
fn error_is_send_sync_and_std_error() {
    fn assert_traits<T: Send + Sync + core::error::Error + 'static>() {}
    assert_traits::<Error>();
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Key>();
    assert_send_sync::<KeySet>();
    assert_send_sync::<Validation>();
    assert_send_sync::<TokenData<Claims>>();
}
