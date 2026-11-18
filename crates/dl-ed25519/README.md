# dl-ed25519

Ed25519 signatures (RFC 8032, pure Ed25519): a key from a 32-byte seed, deterministic
signing and verification. `no_std`, no `unsafe`, no dependencies outside DevLibrary
(SHA-512 from `dl-hash`).

## Replaces

| Ecosystem | Packages |
|-----------|----------|
| Go        | `crypto/ed25519` |
| .NET      | `NSec`, BouncyCastle `Ed25519Signer` |
| Node.js   | `crypto.sign(null, ...)` with Ed25519 keys, `tweetnacl` |
| Rust      | `ed25519-dalek` |

## Usage

```rust
use dl_ed25519::{SigningKey, VerifyingKey};

// In an application, take the seed from the OS: dl_rand::secure_bytes::<32>()?.
let seed = [42u8; 32];
let key = SigningKey::from_seed(&seed);
let sig = key.sign(b"POST|/agent/v1/token|1767225600|nonce|e3b0c442");

let public: [u8; 32] = *key.verifying_key().as_bytes();
let vk = VerifyingKey::from_bytes(&public)?;
assert!(vk.verify(b"POST|/agent/v1/token|1767225600|nonce|e3b0c442", &sig).is_ok());
assert!(!dl_ed25519::verify(&public, b"tampered", &sig));
# Ok::<(), dl_ed25519::Error>(())
```

## Behaviour

- Signing and key derivation follow RFC 8032 section 5.1 and give the same bytes as Go's
  `crypto/ed25519` (checked against the RFC 8032 section 7.1 vectors and 120 keys and
  messages signed by Go).
- Verification matches Go's `Verify`: S must be below the group order, the key is decoded
  the lenient way most libraries use (a y ≥ p is read modulo p), and the check is the
  cofactorless `[S]B - [k]A == R` on encodings. About 870 valid and altered inputs give
  the same verdict as Go (`testdata/go_vectors.txt`, made by
  `testdata/gen_go_vectors.go`).
- Field arithmetic uses five 51-bit limbs; scalar multiplication is a Montgomery ladder
  and scalar reduction is bit-serial, so signing has no secret-dependent branches or
  table lookups. It favours simplicity over speed (a signature takes well under a
  millisecond, enough for request signing and manifest checks).
- Secret material is not wiped from memory on drop.
- No Ed25519ph/ctx variants, no batch verification, no X25519.
