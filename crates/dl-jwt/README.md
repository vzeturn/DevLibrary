# dl-jwt

JSON Web Tokens (RFC 7519) signed with HMAC (HS256, HS384, HS512) for Rust: issue,
verify and validate tokens, with no dependencies outside DevLibrary and `no_std`
support.

## Replaces

| Ecosystem | Packages |
|-----------|----------|
| .NET      | `System.IdentityModel.Tokens.Jwt` (`JwtSecurityTokenHandler`), `Microsoft.IdentityModel.Tokens` (`SymmetricSecurityKey`, `TokenValidationParameters`, HS256 path) |
| Node.js   | `jsonwebtoken`, `jose` (HMAC `SignJWT` / `jwtVerify`), `jwt-decode` |
| Go        | `github.com/golang-jwt/jwt/v5` (`SigningMethodHS256`) |
| Python    | `PyJWT`, `python-jose` (HMAC algorithms) |
| Rust      | `jsonwebtoken` (HMAC algorithms) |

## Features

- **Secure defaults**: the accepted algorithms are always chosen by the application
  (`Validation::new(Algorithm::HS256)`), never taken from the token; `none` cannot be
  enabled at all. `exp` is required by default. A token that names an audience is
  rejected unless you configure one (RFC 7519 section 4.1.3).
- **Verify first, parse later**: the signature is checked in constant time before the
  payload is parsed, so a forged token cannot reach the claim checks or your
  `FromJson` code.
- **Strict decoding**: exactly three parts, unpadded canonical base64url, UTF-8 JSON
  objects, typed registered claims, `crit` headers refused, a 64 KiB size limit
  checked before anything else. Every failure is a typed `Error`; nothing panics on
  untrusted input.
- **Key length enforced**: HMAC keys shorter than the hash output (32/48/64 bytes) are
  refused, as RFC 7518 section 3.2 requires. `Debug` never prints a key.
- **Full claim validation**: `exp`, `nbf` with leeway, maximum age from `iat`, issuer
  allow-list, audience as string or array, subject, required custom claims.
- **Key rotation**: `KeySet` picks the key by `kid`; `decode_with` takes your own key
  selector.
- **Your own claim types** through `dl_json::ToJson` / `FromJson`, or the ready-made
  `Claims` with typed registered claims and an ordered map for the rest.
- **Interoperable**: verified against RFC 7515 Appendix A.1 and tokens computed with
  Python's standard library, including the claim shapes ASP.NET Core and npm
  `jsonwebtoken` produce.
- `no_std` + `alloc`, builds for `wasm32-unknown-unknown`, no `unsafe`.

## Installation

The crate lives in the DevLibrary workspace; cargo finds the package by name inside the
repository:

```toml
[dependencies]
dl-jwt = { git = "https://github.com/vzeturn/DevLibrary" }
```

Pin a revision for reproducible builds: `{ git = "...", rev = "<commit>" }`.

With a local checkout, use a path dependency:

```toml
[dependencies]
dl-jwt = { path = "../DevLibrary/crates/dl-jwt" }
```

The default `std` feature only adds `now()` and `decode_now()`, which read the system
clock. Without it the crate is `no_std` (it needs `alloc`) and you pass the current
time to `decode` yourself:

```toml
[dependencies]
dl-jwt = { git = "https://github.com/vzeturn/DevLibrary", default-features = false }
```

The minimum supported Rust version is 1.88.

## Quick start

Issue an access token at login, verify it on every request:

```rust
use dl_jwt::{Algorithm, Claims, Error, Header, Key, Validation};

// At least 32 random bytes, loaded from configuration or a secret store.
let key = Key::from_secret(b"7c1f0d9e6b3a48f2a5e4d3c2b1a09f8e")?;
let now = 1_717_200_000; // dl_jwt::now() with the default `std` feature

// Issue.
let claims = Claims::new()
    .issuer("https://auth.example.com")
    .subject("user-42")
    .audience("orders-api")
    .issued_at(now)
    .expires_at(now + 15 * 60)
    .claim("roles", ["orders:read", "orders:write"]);
let token = dl_jwt::encode(&Header::new(Algorithm::HS256), &claims, &key)?;

// Verify.
let validation = Validation {
    issuer: Some(vec!["https://auth.example.com".into()]),
    audience: Some(vec!["orders-api".into()]),
    ..Validation::new(Algorithm::HS256)
};
let data = dl_jwt::decode::<Claims>(&token, &key, &validation, now + 60)?;
assert_eq!(data.claims.sub.as_deref(), Some("user-42"));
let roles: Vec<String> = data.claims.get("roles")?;
assert_eq!(roles, ["orders:read", "orders:write"]);

// Expired (exp + 60 s of leeway), or signed with another key: rejected.
let later = now + 15 * 60 + 60;
assert_eq!(dl_jwt::decode::<Claims>(&token, &key, &validation, later).unwrap_err(), Error::Expired);
let other = Key::from_secret([0u8; 32])?;
assert_eq!(dl_jwt::decode::<Claims>(&token, &other, &validation, now).unwrap_err(), Error::InvalidSignature);
# Ok::<(), Error>(())
```

## Usage guide

### Your own claims struct

`decode::<T>` converts the payload with `dl_json::FromJson`, and `encode` takes anything
that implements `ToJson`. The registered claims (`exp`, `nbf`, `iat`, `iss`, `aud`,
`sub`) are validated from the raw payload, so the checks apply even if your struct
does not have those fields.

```rust
use dl_json::{Error as JsonError, FromJson, ToJson, Value, json};
use dl_jwt::{Algorithm, Header, Key, Validation};

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
    fn from_json(v: &Value) -> Result<Self, JsonError> {
        Ok(AccessClaims {
            sub: v.field("sub")?,
            exp: v.field("exp")?,
            tenant: v.field("tenant")?,
            roles: v.field("roles")?,
        })
    }
}

let key = Key::from_secret([0x42; 32])?;
let now = 1_717_200_000;
let claims = AccessClaims { sub: "7".into(), exp: now + 300, tenant: "acme".into(), roles: vec!["admin".into()] };
let token = dl_jwt::encode(&Header::default(), &claims, &key)?;

let validation = Validation {
    required_claims: vec!["exp".into(), "tenant".into()],
    ..Validation::new(Algorithm::HS256)
};
let data = dl_jwt::decode::<AccessClaims>(&token, &key, &validation, now)?;
assert_eq!(data.claims, claims);
# Ok::<(), dl_jwt::Error>(())
```

With the generic `Claims` type, registered claims are typed fields and everything else
is in `extra` (a `dl_json::Map` in token order). `Claims::get::<T>(name)` reads either
kind: a missing claim is `Error::MissingClaim` unless `T` is an `Option`, a claim of the
wrong type is `Error::InvalidJson` with a JSON Pointer path such as `/roles/1`.

### Validation options

`Validation` has no `Default`: `Validation::new(alg)` sets the fields below, then
override what you need with struct update syntax (`Validation { leeway_secs: 0, ..Validation::new(alg) }`).

| Field | Default | Effect |
|-------|---------|--------|
| `algorithms` | `[alg]` | `alg` header values accepted; others are `AlgorithmNotAllowed`. |
| `leeway_secs` | `60` | Clock skew allowed in the `exp`, `nbf` and `max_age` checks. |
| `validate_exp` | `true` | Reject when `now >= exp + leeway` (`Expired`). |
| `validate_nbf` | `true` | Reject when `now + leeway < nbf` (`NotYetValid`). |
| `required_claims` | `["exp"]` | Claims that must be present and not `null` (`MissingClaim`); any name. |
| `issuer` | `None` | `Some(list)`: `iss` required and one of `list` (`InvalidIssuer`). |
| `audience` | `None` | `Some(list)`: `aud` required, one of its values in `list` (`InvalidAudience`). |
| `validate_aud` | `true` | With `audience: None`, reject tokens that have an `aud`. |
| `subject` | `None` | `Some(s)`: `sub` required and equal to `s` (`InvalidSubject`). |
| `max_age` | `None` | `Some(secs)`: `iat` required; reject when `now - iat > secs + leeway` (`TooOld`) or `iat > now + leeway` (`NotYetValid`). |
| `max_token_len` | `65536` | Longest token in bytes (`TokenTooLarge`), checked first. |

Times are Unix seconds (`u64`). Fractional NumericDates such as `1700000000.75` are
accepted and truncated to whole seconds.

### Key rotation with `kid`

Sign with the newest key and put its ID in the header. Keep older keys in the
`KeySet` until the tokens they signed have expired, then remove them.

```rust
use dl_jwt::{Algorithm, Claims, Error, Header, Key, KeySet, Validation};

let mut keys = KeySet::new();
keys.insert("2024-05", Key::from_secret([1u8; 32])?);
keys.insert("2024-06", Key::from_secret([2u8; 32])?);
let validation = Validation::new(Algorithm::HS256);
let now = 1_717_200_000;

let header = Header::new(Algorithm::HS256).key_id("2024-06");
let token = dl_jwt::encode(&header, &Claims::new().subject("42").expires_at(now + 900), keys.get("2024-06").unwrap())?;
let data = keys.decode::<Claims>(&token, &validation, now)?;
assert_eq!(data.header.kid.as_deref(), Some("2024-06"));

// After the rotation window the old key is removed; its tokens stop working.
let old = dl_jwt::encode(&Header::default().key_id("2024-05"), &Claims::new().expires_at(now + 900), keys.get("2024-05").unwrap())?;
keys.remove("2024-05");
assert_eq!(keys.decode::<Claims>(&old, &validation, now).unwrap_err(), Error::UnknownKey);
# Ok::<(), Error>(())
```

`decode_with(token, |header| -> Option<&Key>, &validation, now)` does the same with
your own lookup, for example a database of tenant keys. The header passed to the
closure is not authenticated yet: use `kid` only as a lookup key in your own set.

### Refresh-token pattern

Keep access tokens short (5 to 15 minutes) and give the client a long-lived refresh
token to get new ones. Sign refresh tokens with a **different key** so that one can
never be accepted where an access token is expected, give each a unique `jti`, and
store the valid `jti`s on the server so a refresh token can be used once and revoked.

```rust
use std::collections::HashSet;
use dl_jwt::{Algorithm, Claims, Error, Header, Key, Validation};

struct Auth {
    access_key: Key,
    refresh_key: Key,
    live_refresh_ids: HashSet<String>, // a database table in a real service
    next_id: u64,                      // use a random UUID (dl-uuid) in a real service
}

impl Auth {
    fn issue(&mut self, user: &str, now: u64) -> Result<(String, String), Error> {
        let access = Claims::new().subject(user).audience("api").issued_at(now).expires_at(now + 15 * 60);
        self.next_id += 1;
        let jti = format!("r-{}", self.next_id);
        let refresh = Claims::new()
            .subject(user)
            .audience("refresh")
            .issued_at(now)
            .expires_at(now + 14 * 24 * 3600)
            .jwt_id(jti.as_str());
        self.live_refresh_ids.insert(jti);
        Ok((
            dl_jwt::encode(&Header::new(Algorithm::HS256), &access, &self.access_key)?,
            dl_jwt::encode(&Header::new(Algorithm::HS512), &refresh, &self.refresh_key)?,
        ))
    }

    fn refresh(&mut self, refresh_token: &str, now: u64) -> Result<(String, String), Error> {
        let validation = Validation {
            audience: Some(vec!["refresh".into()]),
            required_claims: vec!["exp".into(), "jti".into(), "sub".into()],
            ..Validation::new(Algorithm::HS512)
        };
        let claims = dl_jwt::decode::<Claims>(refresh_token, &self.refresh_key, &validation, now)?.claims;
        // One use only: a second use means the token was copied.
        let jti = claims.jti.unwrap_or_default();
        if !self.live_refresh_ids.remove(&jti) {
            return Err(Error::InvalidSignature); // or your own "revoked" error
        }
        self.issue(claims.sub.as_deref().unwrap_or_default(), now)
    }
}

let mut auth = Auth {
    access_key: Key::from_secret([0x11; 32])?,
    refresh_key: Key::from_secret([0x22; 64])?,
    live_refresh_ids: HashSet::new(),
    next_id: 0,
};
let now = 1_717_200_000;
let (access, refresh) = auth.issue("user-42", now)?;

// The refresh token is not an access token.
let api = Validation { audience: Some(vec!["api".into()]), ..Validation::new(Algorithm::HS256) };
assert!(dl_jwt::decode::<Claims>(&access, &auth.access_key, &api, now).is_ok());
assert!(dl_jwt::decode::<Claims>(&refresh, &auth.access_key, &api, now).is_err());

// An hour later the access token has expired; the refresh token gets a new pair, once.
let later = now + 3600;
let (new_access, _new_refresh) = auth.refresh(&refresh, later)?;
assert!(dl_jwt::decode::<Claims>(&new_access, &auth.access_key, &api, later).is_ok());
assert!(auth.refresh(&refresh, later).is_err());
# Ok::<(), Error>(())
```

### Interop with ASP.NET Core `JwtBearer`

A typical ASP.NET Core API that validates HS256 tokens:

```csharp
builder.Services.AddAuthentication(JwtBearerDefaults.AuthenticationScheme)
    .AddJwtBearer(o => o.TokenValidationParameters = new TokenValidationParameters
    {
        ValidIssuer = "https://auth.example.com",
        ValidAudience = "orders-api",
        IssuerSigningKey = new SymmetricSecurityKey(Encoding.UTF8.GetBytes(secret)),
        ValidAlgorithms = new[] { SecurityAlgorithms.HmacSha256 },
    });
```

| `TokenValidationParameters` | `dl_jwt` |
|-----------------------------|----------|
| `IssuerSigningKey = new SymmetricSecurityKey(Encoding.UTF8.GetBytes(secret))` | `Key::from_secret(secret.as_bytes())` |
| `ValidAlgorithms = { SecurityAlgorithms.HmacSha256 }` | `Validation::new(Algorithm::HS256)` |
| `ValidIssuer` / `ValidIssuers` | `issuer: Some(vec![...])` |
| `ValidAudience` / `ValidAudiences` | `audience: Some(vec![...])` |
| `ValidateIssuer = false` | `issuer: None` |
| `ValidateAudience = false` | `audience: None, validate_aud: false` |
| `ValidateLifetime = false` | `validate_exp: false, validate_nbf: false` |
| `RequireExpirationTime = true` (default) | `required_claims: vec!["exp".into()]` (default) |
| `ClockSkew`, **default 5 minutes** | `leeway_secs`, **default 60 seconds**; set `300` to match |
| `IssuerSigningKeys` with `kid` | `KeySet` |

Both sides need the same secret bytes. Microsoft.IdentityModel also refuses HS256 keys
under 256 bits ([IDX10720](https://github.com/AzureAD/azure-activedirectory-identitymodel-extensions-for-dotnet/wiki/IDX10720)),
so a secret that works there works here.

`JwtSecurityTokenHandler` writes `ClaimTypes.Name` as `unique_name`,
`ClaimTypes.NameIdentifier` as `nameid` and `ClaimTypes.Role` as `role`: a string for
one role, an array for several. When reading, `JwtBearer` maps `sub`, `unique_name`,
`nameid` and `role` back to those `ClaimTypes` (unless `MapInboundClaims = false`), so
tokens issued in Rust with a `role` claim work with `[Authorize(Roles = "Admin")]`.
Decoding a token issued by .NET:

```rust
use dl_json::{FromJson, Value};
use dl_jwt::{Algorithm, Claims, Key, Validation};

let token = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJ1bmlxdWVfbmFtZSI6ImFsaWNlIiwibmFtZWlkIjoiNDIiLCJyb2xlIjpbIkFkbWluIiwiVXNlciJdLCJuYmYiOjE3MDAwMDAwMDAsImV4cCI6MTcwMDAwMzYwMCwiaWF0IjoxNzAwMDAwMDAwLCJpc3MiOiJodHRwczovL2xvY2FsaG9zdDo1MDAxIiwiYXVkIjoiaHR0cHM6Ly9sb2NhbGhvc3Q6NTAwMSJ9.YO4x_5vDYPglKdBNRT5ULZ4XAdXsO7f_2fyjhA5iXHA";
let key = Key::from_secret("ThisIsMyVerySecretKeyForJwtSigning!".as_bytes())?;
let validation = Validation {
    issuer: Some(vec!["https://localhost:5001".into()]),
    audience: Some(vec!["https://localhost:5001".into()]),
    leeway_secs: 300, // same as ClockSkew
    ..Validation::new(Algorithm::HS256)
};
let claims = dl_jwt::decode::<Claims>(token, &key, &validation, 1_700_000_100)?.claims;

assert_eq!(claims.get::<String>("unique_name")?, "alice");
assert_eq!(claims.get::<String>("nameid")?, "42");
// `role` is a string or an array, depending on how many roles the user has.
let roles: Vec<String> = match claims.get::<Value>("role")? {
    Value::Null => vec![],
    Value::String(role) => vec![role],
    other => Vec::from_json(&other)?,
};
assert_eq!(roles, ["Admin", "User"]);
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Interop with Node `jsonwebtoken`

```js
const jwt = require("jsonwebtoken");
const token = jwt.sign({ userId: 7 }, secret, { algorithm: "HS256", expiresIn: "15m" });
const payload = jwt.verify(token, secret, { algorithms: ["HS256"], clockTolerance: 60 });
```

| `jsonwebtoken` | `dl_jwt` |
|----------------|----------|
| `jwt.sign(payload, secret, { algorithm, expiresIn, issuer, audience, subject, keyid })` | `encode(&Header::new(alg).key_id(..), &Claims::new().expires_at(..).issuer(..)..., &key)` |
| `jwt.verify(token, secret, { algorithms })` | `decode::<T>(token, &key, &Validation::new(alg), now)` |
| `issuer`, `audience`, `subject`, `maxAge` options | `issuer`, `audience`, `subject`, `max_age` fields |
| `clockTolerance` (default **0**) | `leeway_secs` (default 60) |
| `ignoreExpiration`, `ignoreNotBefore` | `validate_exp: false`, `validate_nbf: false` |
| `verify(..., { complete: true }).header` | `TokenData::header` |
| `jwt.decode(token)`, `jwt-decode` | `decode_unverified(token)` |

Two behaviors differ. `jsonwebtoken` accepts tokens without `exp`; `dl_jwt` requires
it unless you clear `required_claims`. `jsonwebtoken` ignores `aud` when no `audience`
option is given; `dl_jwt` rejects such tokens unless you set `validate_aud: false`.
Secrets are UTF-8 bytes on both sides, and `sign` adds `iat` automatically.

```rust
use dl_jwt::{Algorithm, Claims, Key, Validation};

// Produced by jwt.sign({ userId: 7 }, secret, { expiresIn: "15m" }).
let token = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJ1c2VySWQiOjcsImlhdCI6MTcwMDAwMDAwMCwiZXhwIjoxNzAwMDAwOTAwfQ.cZQSnLlO9RBE504t0u6_3ExLYXnVQ3TbHvXsUtvrgO4";
let key = Key::from_secret("dl-jwt/HS256/test-key/0123456789".as_bytes())?;
let claims = dl_jwt::decode::<Claims>(token, &key, &Validation::new(Algorithm::HS256), 1_700_000_000)?.claims;
assert_eq!(claims.get::<u32>("userId")?, 7);
assert_eq!(claims.exp, Some(1_700_000_900));
# Ok::<(), dl_jwt::Error>(())
```

## Security notes

- **Always pin the algorithms.** `Validation::new` makes you choose; the `alg` header
  only selects among the algorithms you allowed. `none` is never accepted, and
  asymmetric algorithms are refused instead of being confused with HMAC
  ([RFC 8725 section 2.1](https://www.rfc-editor.org/rfc/rfc8725#section-2.1)).
- **Use real keys.** At least 32 random bytes for HS256 (48 for HS384, 64 for HS512)
  from a cryptographically secure generator, kept in a secret store and rotated with
  `kid`. A password or a sentence is not a key. Everyone who can verify an HMAC token
  can also issue one, so share the secret only with services you trust as issuers.
- **Keep tokens short-lived.** A JWT is valid until `exp` and cannot be revoked by
  itself. Use minutes for access tokens and the refresh-token pattern above, or a
  `jti` deny-list when you need immediate revocation.
- **The payload is not encrypted.** It is only base64url-encoded JSON; anyone holding
  the token can read it. Do not put passwords, secrets or sensitive personal data in
  claims.
- **`decode_unverified` and `decode_header` prove nothing.** Their output is chosen by
  whoever made the token. Use them to display information or to pick a key, never to
  make an authorization decision.
- **Check `iss` and `aud`** when several services or tenants share a key, so a token
  for one cannot be replayed against another.
- Error messages are fine to log, but return a generic 401 to clients rather than the
  exact reason.

## API overview

| Item | Purpose |
|------|---------|
| `encode(&Header, &impl ToJson, &Key) -> Result<String>` | Sign a token |
| `decode::<T>(token, &Key, &Validation, now) -> Result<TokenData<T>>` | Verify and validate |
| `decode_with::<T, _>(token, \|&Header\| Option<&Key>, &Validation, now)` | Verify with a key chosen from the header |
| `decode_now::<T>(token, &Key, &Validation)` | `decode` at `now()` (`std`, not on `wasm32-unknown-unknown`) |
| `now() -> u64` | System time in Unix seconds (`std`, not on `wasm32-unknown-unknown`) |
| `decode_header(token) -> Result<Header>` | Read the header without verifying (for `kid`) |
| `decode_unverified(token) -> Result<Unverified>` | Read header and claims without verifying (like `jwt-decode`) |
| `Algorithm` | `HS256`, `HS384`, `HS512`; `as_str`, `FromStr`, `signature_len`, `min_key_len` |
| `Key` | `from_secret`, `from_base64url` (JWK `k`), `len`; redacted `Debug`, zeroed on drop |
| `KeySet` | `new`, `insert`, `get`, `remove`, `len`, `key_ids`, `decode::<T>` |
| `Header` | `alg`, `typ`, `kid`, `cty`, `extra`; `new`, `key_id`, `content_type`, `param`; `ToJson`, `FromJson` |
| `Claims` | `iss`, `sub`, `aud`, `exp`, `nbf`, `iat`, `jti`, `extra`; builder methods, `get::<T>`, `contains`; `ToJson`, `FromJson` |
| `Audience` | `Single(String)` or `Multiple(Vec<String>)`; `contains`, `iter` |
| `Validation` | The checks `decode` runs, see the table above |
| `TokenData<T>` | `header` and `claims` of a verified token |
| `Unverified` | `header` (raw `Map`), `claims`, `alg()`, `kid()` |
| `Error`, `Part` | Error enum; which token part an error refers to |
| `DEFAULT_LEEWAY_SECS`, `DEFAULT_MAX_TOKEN_LEN`, `MIN_SECRET_LEN` | 60, 65536, 32 |

## Error handling

Every fallible function returns `Result<_, dl_jwt::Error>`. `Error` implements
`Display`, `Debug`, `Clone`, `PartialEq`, `Eq` and `core::error::Error` (with
`source()` for JSON errors). `decode` stops at the first failing step, in the order of
this table, so claim errors are only reported for tokens with a valid signature.

| Variant | Raised when |
|---------|-------------|
| `TokenTooLarge` | The token is longer than `max_token_len` (64 KiB by default) |
| `Malformed { part }` | Not three `.`-separated parts, or an empty header or payload |
| `InvalidBase64 { part }` | A part is not unpadded, canonical base64url |
| `InvalidJson { part, error }` | Header or payload is not a UTF-8 JSON object, a registered claim has the wrong type (`"exp": "soon"`), or your `FromJson` rejected the payload |
| `UnsupportedAlgorithm(name)` | `alg` is `none`, `RS256`, `ES256`, `EdDSA` or anything but `HS256`/`HS384`/`HS512` |
| `UnsupportedCriticalHeader(name)` | The header has a `crit` parameter |
| `AlgorithmNotAllowed` | `alg` is supported but not in `Validation::algorithms` |
| `UnknownKey` | The key selector found no key (`KeySet`: no `kid` or an unknown one) |
| `KeyTooShort { len, min }` | The key is shorter than the algorithm requires |
| `InvalidSignature` | The HMAC does not match |
| `MissingClaim(name)` | A required claim is missing, or `iss`/`aud`/`sub`/`iat` needed by a configured check |
| `Expired` | `now >= exp + leeway` |
| `NotYetValid` | `now + leeway < nbf`, or `iat` in the future when `max_age` is set |
| `TooOld` | `now - iat > max_age + leeway` |
| `InvalidIssuer` | `iss` not in `Validation::issuer` |
| `InvalidAudience` | No `aud` value in `Validation::audience`, or an `aud` with no audience configured |
| `InvalidSubject` | `sub` differs from `Validation::subject` |
| `InvalidKeyEncoding` | `Key::from_base64url` got invalid base64url |

`Part` is `Header`, `Payload` or `Signature`. `Error` is `#[non_exhaustive]`: match
with a `_` arm.

## Platform support

- Linux, macOS and Windows, and `wasm32-unknown-unknown`.
- With `default-features = false` the crate is `#![no_std]` and uses only `core` and
  `alloc`. With the default `std` feature it adds `now()` and `decode_now()`, except on
  `wasm32-unknown-unknown`, where `std::time::SystemTime` is not available; in the
  browser pass `Math.floor(Date.now() / 1000)` to `decode`.
- No `unsafe` code, no platform-specific code, no build script.

## Performance

`decode` makes one pass per part: three base64url decodes, one HMAC over the signing
input, and one JSON parse each for the header and the payload. Tokens with a bad
signature are rejected before the payload is parsed.

Measured with `cargo bench -p dl-jwt` on an Intel Core i7-9850H (2.6 GHz), Windows 11,
Rust 1.100, release profile with fat LTO. The token is a typical 395-byte access token
(`iss`, `sub`, `aud`, `iat`, `nbf`, `exp`, `jti`, a Vietnamese name, two roles and a
tenant); validation checks issuer, audience, `exp` and `nbf`.

| Case | Median |
|------|-------:|
| `encode`, HS256 | 6.8 µs |
| `decode` + validate, HS256 | 8.8 µs |
| `encode`, HS512 | 6.0 µs |
| `decode` + validate, HS512 | 8.1 µs |
| Reject a forged HS256 signature | 4.1 µs |
| `decode_unverified` | 5.1 µs |
| Baseline: HMAC-SHA256 of the 351-byte signing input (`dl-hash`) | 2.8 µs |
| Baseline: `dl_json::parse` of the 235-byte payload | 2.4 µs |

The machine was running other builds at the same time and repeated runs varied by up
to 1.4x; the table is the quietest run. HS512 is not slower than HS256 here because
SHA-512 processes 128-byte blocks with 64-bit arithmetic. HMAC and JSON parsing, both
in other DevLibrary crates, take most of the time; the rest is base64url decoding,
building the `Claims` value, serializing on `encode`, and the checks.

Reproduce with:

```sh
cargo bench -p dl-jwt
cargo bench -p dl-jwt -- decode   # only cases whose name contains "decode"
```

## Standards and test vectors

- [RFC 7519](https://www.rfc-editor.org/rfc/rfc7519): JSON Web Token. The section 3.1
  example token is a unit test, and the section 6.1 unsecured token must be rejected.
- [RFC 7515](https://www.rfc-editor.org/rfc/rfc7515): JSON Web Signature, compact
  serialization. The Appendix A.1 HS256 token is verified with the key from its JWK.
- [RFC 7518](https://www.rfc-editor.org/rfc/rfc7518) section 3.2: HMAC with SHA-2 and
  the minimum key sizes.
- [RFC 8725](https://www.rfc-editor.org/rfc/rfc8725): JWT Best Current Practices
  (algorithm pinning, `none` rejection, issuer and audience validation).
- HS256, HS384 and HS512 tokens, a .NET-style claim set, an npm `jsonwebtoken`-style
  token and fractional NumericDates were computed independently with the Python 3.13
  standard library (`hmac`, `hashlib`, `base64`, `json`) by `testdata/gen_vectors.py`;
  `encode` reproduces them byte for byte.

## Limitations and roadmap

- Only HMAC (`HS256`, `HS384`, `HS512`). `RS256`, `PS256`, `ES256` and `EdDSA` are on
  the roadmap; until then tokens that use them fail with `UnsupportedAlgorithm`, and
  `decode_unverified` can still read them.
- No JWE (encrypted tokens) and no JWKS (fetching or parsing JSON Web Key Sets);
  `Key::from_base64url` reads the `k` member of a symmetric JWK.
- Secrets shorter than 32 bytes are always refused, with no opt-out. Systems that still
  use a short secret must rotate to a longer one before switching.
- The `typ` header is not validated (RFC 8725 section 3.11 explicit typing); compare
  `TokenData::header.typ` yourself if you use distinct types such as `at+jwt`.
- No `crit` extensions (such as RFC 7797 unencoded payloads) and no nested-JWT
  processing for `cty: "JWT"`.
- NumericDates are whole seconds: fractions are truncated, negative values and values
  above `u64::MAX` are rejected.
- Duplicate JSON member names are accepted and the last one wins, which RFC 7519
  section 4 allows.
- Keys are zeroed on drop on a best-effort basis; copies inside the allocator or the
  hash state are not tracked.

## License

Licensed under either of [MIT](https://opensource.org/licenses/MIT) or
[Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0) at your option.
