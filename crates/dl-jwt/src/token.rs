//! Encoding and decoding compact JWS tokens.

use alloc::string::String;
use alloc::vec::Vec;

use dl_encoding::base64::URL_SAFE_NO_PAD;
use dl_hash::ct_eq;
use dl_json::{Error as JsonError, FromJson, Map, ToJson, Value};

use crate::claims::Registered;
use crate::header::HeaderError;
use crate::validation::DEFAULT_MAX_TOKEN_LEN;
use crate::{Claims, Error, Header, Key, Part, Validation};

/// A verified and validated token.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenData<T> {
    /// The token header.
    pub header: Header,
    /// The claims, converted to `T` with [`FromJson`].
    pub claims: T,
}

/// A token decoded without any verification, from [`decode_unverified`].
#[derive(Debug, Clone, PartialEq)]
pub struct Unverified {
    /// The header as raw JSON members. It may name any algorithm, including ones this
    /// crate cannot verify.
    pub header: Map,
    /// The claims. Registered claims are type-checked; nothing is validated.
    pub claims: Claims,
}

impl Unverified {
    /// The `alg` header parameter, if it is a string.
    pub fn alg(&self) -> Option<&str> {
        self.header.get("alg").and_then(Value::as_str)
    }

    /// The `kid` header parameter, if it is a string.
    pub fn kid(&self) -> Option<&str> {
        self.header.get("kid").and_then(Value::as_str)
    }
}

/// Creates a signed token: `base64url(header) . base64url(claims) . base64url(HMAC)`.
///
/// `claims` can be a [`Claims`] or any type that implements [`ToJson`] and produces a
/// JSON object. JSON is written compactly, with members in the order the types define.
///
/// # Errors
///
/// - [`Error::KeyTooShort`] if the key is shorter than the algorithm needs.
/// - [`Error::InvalidJson`] with [`Part::Payload`] if `claims` is not a JSON object.
///
/// ```
/// use dl_jwt::{Claims, Header, Key};
///
/// let key = Key::from_secret(b"an example secret of 32+ bytes!!")?;
/// let claims = Claims::new().subject("42").expires_at(1_700_000_900);
/// let token = dl_jwt::encode(&Header::default(), &claims, &key)?;
/// assert!(token.starts_with("eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9."));
/// # Ok::<(), dl_jwt::Error>(())
/// ```
pub fn encode<T: ToJson + ?Sized>(header: &Header, claims: &T, key: &Key) -> Result<String, Error> {
    key.check_len(header.alg)?;
    let payload = claims.to_json();
    if !payload.is_object() {
        let error = JsonError::type_mismatch("object", payload.type_name());
        return Err(Error::InvalidJson {
            part: Part::Payload,
            error,
        });
    }
    let header_json = dl_json::to_string(&header.to_json());
    let payload_json = dl_json::to_string(&payload);
    let alg = header.alg;
    let len = URL_SAFE_NO_PAD.encoded_len(header_json.len())
        + URL_SAFE_NO_PAD.encoded_len(payload_json.len())
        + URL_SAFE_NO_PAD.encoded_len(alg.signature_len())
        + 2;
    let mut out = Vec::with_capacity(len);
    URL_SAFE_NO_PAD.encode_into(header_json, &mut out);
    out.push(b'.');
    URL_SAFE_NO_PAD.encode_into(payload_json, &mut out);
    let mac = alg.sign(key.secret(), &out);
    out.push(b'.');
    URL_SAFE_NO_PAD.encode_into(mac.as_bytes(), &mut out);
    // Only base64url characters and '.' were written.
    Ok(String::from_utf8(out).expect("token is ASCII"))
}

/// Verifies `token` with `key` and validates its claims at time `now` (Unix seconds).
///
/// Steps, in order; the first failure is returned:
///
/// 1. Length at most [`Validation::max_token_len`], else [`Error::TokenTooLarge`].
/// 2. Exactly three `.`-separated parts with a non-empty header and payload, else
///    [`Error::Malformed`].
/// 3. Each part is strict base64url without padding, else [`Error::InvalidBase64`].
/// 4. The header is a JSON object with a supported `alg` and no `crit`, else
///    [`Error::InvalidJson`], [`Error::UnsupportedAlgorithm`] (also for `none`) or
///    [`Error::UnsupportedCriticalHeader`].
/// 5. `alg` is in [`Validation::algorithms`], else [`Error::AlgorithmNotAllowed`].
/// 6. The key is long enough for `alg`, else [`Error::KeyTooShort`].
/// 7. The HMAC matches, compared in constant time, else [`Error::InvalidSignature`].
/// 8. Only now is the payload parsed: a JSON object whose registered claims have the
///    right types, else [`Error::InvalidJson`].
/// 9. The claim checks of [`Validation`].
/// 10. Conversion to `T`, else [`Error::InvalidJson`].
///
/// Use [`decode_now`] to read the clock, [`decode_with`] or [`KeySet`](crate::KeySet)
/// to choose a key by `kid`.
///
/// ```
/// use dl_jwt::{Algorithm, Claims, Error, Header, Key, Validation};
///
/// let key = Key::from_secret(b"an example secret of 32+ bytes!!")?;
/// let now = 1_700_000_000;
/// let token = dl_jwt::encode(&Header::default(), &Claims::new().subject("42").expires_at(now + 900), &key)?;
///
/// let validation = Validation::new(Algorithm::HS256);
/// let data = dl_jwt::decode::<Claims>(&token, &key, &validation, now)?;
/// assert_eq!(data.claims.sub.as_deref(), Some("42"));
///
/// let later = now + 900 + 60;
/// assert_eq!(dl_jwt::decode::<Claims>(&token, &key, &validation, later).unwrap_err(), Error::Expired);
/// # Ok::<(), Error>(())
/// ```
pub fn decode<T: FromJson>(
    token: &str,
    key: &Key,
    validation: &Validation,
    now: u64,
) -> Result<TokenData<T>, Error> {
    decode_with(token, |_| Some(key), validation, now)
}

/// Like [`decode`], but `select_key` picks the key after the header has been parsed and
/// `alg` has been checked against [`Validation::algorithms`]. Return `None` to reject
/// the token with [`Error::UnknownKey`].
///
/// The header is not authenticated yet when `select_key` runs: use `kid` only to look
/// a key up in a fixed set of your own keys, never as a file path, URL or SQL fragment.
///
/// ```
/// use dl_jwt::{Algorithm, Claims, Header, Key, Validation};
///
/// let current = Key::from_secret([7u8; 32])?;
/// let previous = Key::from_secret([6u8; 32])?;
/// let now = 1_700_000_000;
/// let header = Header::default().key_id("v2");
/// let token = dl_jwt::encode(&header, &Claims::new().expires_at(now + 60), &current)?;
///
/// let data = dl_jwt::decode_with::<Claims, _>(
///     &token,
///     |h| match h.kid.as_deref() {
///         Some("v2") => Some(&current),
///         Some("v1") => Some(&previous),
///         _ => None,
///     },
///     &Validation::new(Algorithm::HS256),
///     now,
/// )?;
/// assert_eq!(data.header.kid.as_deref(), Some("v2"));
/// # Ok::<(), dl_jwt::Error>(())
/// ```
pub fn decode_with<'k, T, F>(
    token: &str,
    select_key: F,
    validation: &Validation,
    now: u64,
) -> Result<TokenData<T>, Error>
where
    T: FromJson,
    F: FnOnce(&Header) -> Option<&'k Key>,
{
    let raw = Raw::split(token, validation.max_token_len)?;
    let header = parse_header(&raw.header)?;
    if !validation.algorithms.contains(&header.alg) {
        return Err(Error::AlgorithmNotAllowed);
    }
    let key = select_key(&header).ok_or(Error::UnknownKey)?;
    key.check_len(header.alg)?;
    let expected = header.alg.sign(key.secret(), raw.signing_input.as_bytes());
    if !ct_eq(expected.as_bytes(), &raw.signature) {
        return Err(Error::InvalidSignature);
    }

    let payload = parse_json(&raw.payload, Part::Payload)?;
    let invalid = |error| Error::InvalidJson {
        part: Part::Payload,
        error,
    };
    let registered = Registered::from_value(&payload).map_err(invalid)?;
    let Some(obj) = payload.as_object() else {
        // Unreachable: `Registered::from_value` rejects non-objects.
        return Err(invalid(JsonError::type_mismatch(
            "object",
            payload.type_name(),
        )));
    };
    validation.check(&registered, obj, now)?;
    let claims = T::from_json(&payload).map_err(invalid)?;
    Ok(TokenData { header, claims })
}

/// [`decode`] at the current system time.
///
/// Only with the `std` feature (on by default) and not on `wasm32-unknown-unknown`,
/// which has no clock; pass the time to [`decode`] there (for example
/// `Date.now() / 1000` from JavaScript).
#[cfg(all(
    feature = "std",
    not(all(target_arch = "wasm32", target_os = "unknown"))
))]
pub fn decode_now<T: FromJson>(
    token: &str,
    key: &Key,
    validation: &Validation,
) -> Result<TokenData<T>, Error> {
    decode(token, key, validation, crate::now())
}

/// Reads the header of a token without verifying anything, for example to look at
/// `kid` before choosing a key. Fails like steps 1 to 4 of [`decode`], with the
/// default 64 KiB size limit.
///
/// The result is attacker-controlled until the signature has been verified.
///
/// ```
/// let token = "eyJhbGciOiJIUzI1NiIsImtpZCI6InYyIn0.e30.c2ln";
/// let header = dl_jwt::decode_header(token)?;
/// assert_eq!(header.kid.as_deref(), Some("v2"));
/// # Ok::<(), dl_jwt::Error>(())
/// ```
pub fn decode_header(token: &str) -> Result<Header, Error> {
    let raw = Raw::split(token, DEFAULT_MAX_TOKEN_LEN)?;
    parse_header(&raw.header)
}

/// Decodes a token **without verifying the signature or validating any claim**, like
/// npm `jwt-decode` or `jsonwebtoken.decode`.
///
/// Never use the result for authentication or authorization: anyone can create a
/// token with any content. It is meant for clients that display what is inside their
/// own token (user name, expiry time) and for debugging. It accepts any `alg`,
/// including `RS256` and `none`, and an empty signature.
///
/// The token must still be well-formed: three parts, strict base64url, a JSON object
/// header and a JSON object payload whose registered claims have the right types; the
/// default 64 KiB size limit applies.
///
/// ```
/// // The unsecured JWT from RFC 7519 section 6.1.
/// let token = "eyJhbGciOiJub25lIn0\
///     .eyJpc3MiOiJqb2UiLA0KICJleHAiOjEzMDA4MTkzODAsDQogImh0dHA6Ly9leGFtcGxlLmNvbS9pc19yb290Ijp0cnVlfQ.";
/// let t = dl_jwt::decode_unverified(token)?;
/// assert_eq!(t.alg(), Some("none"));
/// assert_eq!(t.claims.iss.as_deref(), Some("joe"));
/// assert_eq!(t.claims.exp, Some(1300819380));
/// assert_eq!(t.claims.extra["http://example.com/is_root"], true);
/// # Ok::<(), dl_jwt::Error>(())
/// ```
pub fn decode_unverified(token: &str) -> Result<Unverified, Error> {
    let raw = Raw::split(token, DEFAULT_MAX_TOKEN_LEN)?;
    let header = match parse_json(&raw.header, Part::Header)? {
        Value::Object(m) => m,
        other => {
            let error = JsonError::type_mismatch("object", other.type_name());
            return Err(Error::InvalidJson {
                part: Part::Header,
                error,
            });
        }
    };
    let payload = parse_json(&raw.payload, Part::Payload)?;
    let claims = Claims::from_json(&payload).map_err(|error| Error::InvalidJson {
        part: Part::Payload,
        error,
    })?;
    Ok(Unverified { header, claims })
}

/// The three decoded parts of a token.
struct Raw<'a> {
    /// `header.payload`, the bytes the signature covers.
    signing_input: &'a str,
    header: Vec<u8>,
    payload: Vec<u8>,
    signature: Vec<u8>,
}

impl<'a> Raw<'a> {
    fn split(token: &'a str, max_len: usize) -> Result<Raw<'a>, Error> {
        if token.len() > max_len {
            return Err(Error::TokenTooLarge);
        }
        let (header, rest) = token.split_once('.').ok_or(Error::Malformed {
            part: Part::Payload,
        })?;
        let (payload, signature) = rest.split_once('.').ok_or(Error::Malformed {
            part: Part::Signature,
        })?;
        if signature.contains('.') {
            // Five parts would be a JWE, which is not supported.
            return Err(Error::Malformed {
                part: Part::Signature,
            });
        }
        if header.is_empty() {
            return Err(Error::Malformed { part: Part::Header });
        }
        if payload.is_empty() {
            return Err(Error::Malformed {
                part: Part::Payload,
            });
        }
        let b64 = |text: &str, part| {
            URL_SAFE_NO_PAD
                .decode(text)
                .map_err(|_| Error::InvalidBase64 { part })
        };
        Ok(Raw {
            signing_input: &token[..header.len() + 1 + payload.len()],
            header: b64(header, Part::Header)?,
            payload: b64(payload, Part::Payload)?,
            signature: b64(signature, Part::Signature)?,
        })
    }
}

fn parse_json(bytes: &[u8], part: Part) -> Result<Value, Error> {
    dl_json::parse_slice(bytes).map_err(|error| Error::InvalidJson { part, error })
}

fn parse_header(bytes: &[u8]) -> Result<Header, Error> {
    let value = parse_json(bytes, Part::Header)?;
    Header::from_value(&value).map_err(|e| match e {
        HeaderError::Json(error) => Error::InvalidJson {
            part: Part::Header,
            error,
        },
        HeaderError::Alg(alg) => Error::UnsupportedAlgorithm(alg),
        HeaderError::Crit(name) => Error::UnsupportedCriticalHeader(name),
    })
}
