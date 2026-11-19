//! The per-request Ed25519 signature (port of `Transport/RequestSigner.cs`, S2
//! verification format): the agent signs
//! `MM-SYNC1 \n METHOD \n path+query \n unix-ts \n nonce \n sha256hex(body)`. Every
//! attempt, including a retry, gets a fresh nonce because the server remembers nonces.

use crate::config::time::SharedClock;
use crate::security::AgentKey;
use dl_hash::Digest;
use std::sync::Arc;

/// Header with the Unix timestamp (seconds).
pub const TIMESTAMP_HEADER: &str = "X-Sync-Timestamp";
/// Header with the nonce (32 lowercase hex digits).
pub const NONCE_HEADER: &str = "X-Sync-Nonce";
/// Header with the base64 signature.
pub const SIGNATURE_HEADER: &str = "X-Sync-Signature";
const VERSION: &str = "MM-SYNC1";

/// The three signature headers of one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedHeaders {
    /// `X-Sync-Timestamp`.
    pub timestamp: String,
    /// `X-Sync-Nonce`.
    pub nonce: String,
    /// `X-Sync-Signature`.
    pub signature: String,
}

/// Signs requests with the agent key at the clock's time.
#[derive(Clone)]
pub struct RequestSigner {
    key: Arc<AgentKey>,
    clock: SharedClock,
}

impl std::fmt::Debug for RequestSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestSigner")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

/// Lowercase hex SHA-256 of `body`.
pub fn body_hash(body: &[u8]) -> String {
    dl_encoding::hex::encode(dl_hash::Sha256::digest(body))
}

/// The signed bytes.
pub fn canonical(
    method: &str,
    path_and_query: &str,
    timestamp: &str,
    nonce: &str,
    body: &[u8],
) -> Vec<u8> {
    canonical_with_body_hash(method, path_and_query, timestamp, nonce, &body_hash(body))
}

/// The signed bytes with a body hash computed elsewhere.
pub fn canonical_with_body_hash(
    method: &str,
    path_and_query: &str,
    timestamp: &str,
    nonce: &str,
    body_sha256: &str,
) -> Vec<u8> {
    format!(
        "{VERSION}\n{}\n{path_and_query}\n{timestamp}\n{nonce}\n{body_sha256}",
        method.to_uppercase()
    )
    .into_bytes()
}

impl RequestSigner {
    /// A signer with `key` and `clock`.
    pub fn new(key: Arc<AgentKey>, clock: SharedClock) -> RequestSigner {
        RequestSigner { key, clock }
    }

    /// The key.
    pub fn key(&self) -> &AgentKey {
        &self.key
    }

    /// Signs a request with its body.
    pub fn sign(
        &self,
        method: &str,
        path_and_query: &str,
        body: &[u8],
    ) -> Result<SignedHeaders, dl_rand::Error> {
        self.sign_body_hash(method, path_and_query, &body_hash(body))
    }

    /// Signs with a body hash computed elsewhere (a streamed upload whose path names its
    /// SHA-256, ADR 0095).
    pub fn sign_body_hash(
        &self,
        method: &str,
        path_and_query: &str,
        body_sha256: &str,
    ) -> Result<SignedHeaders, dl_rand::Error> {
        let nonce = dl_encoding::hex::encode(dl_rand::secure_bytes::<16>()?);
        Ok(self.sign_with(method, path_and_query, body_sha256, &nonce))
    }

    /// Signs with an explicit nonce (tests and cross-checks).
    pub fn sign_with(
        &self,
        method: &str,
        path_and_query: &str,
        body_sha256: &str,
        nonce: &str,
    ) -> SignedHeaders {
        let timestamp = self.clock.now().unix_seconds().to_string();
        let signature = self.key.sign(&canonical_with_body_hash(
            method,
            path_and_query,
            &timestamp,
            nonce,
            body_sha256,
        ));
        SignedHeaders {
            timestamp,
            nonce: nonce.to_owned(),
            signature: dl_encoding::base64::STANDARD.encode(signature),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::time::{ManualClock, Timestamp};

    fn b64(bytes: &[u8]) -> String {
        dl_encoding::base64::STANDARD.encode(bytes)
    }

    /// Expected values: the C# `RequestSigner` and `AgentKey` (BouncyCastle) on .NET 10,
    /// `%TEMP%\kx\sa-core\check.cs sign`.
    #[test]
    fn signatures_match_the_csharp_signer() {
        let seed: Vec<u8> = (1..=32).collect();
        let key = Arc::new(AgentKey::from_seed(&seed).unwrap());
        assert_eq!(
            key.public_key_base64(),
            "ebVWLo/mVPlAeLES6KmLp5AfhTrmlb7X4OORC60ElmQ="
        );
        let body = "{\"agentToken\":\"tok-ơn\"}".as_bytes();
        let canon = canonical(
            "post",
            "/sync/v1/token?x=1&y=%C3%A9",
            "1793761200",
            "00112233445566778899aabbccddeeff",
            body,
        );
        assert_eq!(
            b64(&canon),
            "TU0tU1lOQzEKUE9TVAovc3luYy92MS90b2tlbj94PTEmeT0lQzMlQTkKMTc5Mzc2MTIwMAowMDExMjIzMzQ0NTU2Njc3ODg5OWFhYmJjY2RkZWVmZgo4ZmRlM2UxNmFjMjUyMTJmYmNmODAzNWEyMjczOTg1MGY0NzU1OTJjMTVhNGM2NTNlN2U0MGY4YjY2MjgyZDI3"
        );
        assert_eq!(
            b64(&key.sign(&canon)),
            "6a9QdIiYS8hzRi07WlGEAQfP8Di1WUBXtZB74rpJk5qLVwofdrH5c+w/rQrmTLCZymoTevY77cC2VCHVTrUgAA=="
        );
        let empty = canonical(
            "GET",
            "/sync/v1/commands/poll",
            "1793761200",
            "ffeeddccbbaa99887766554433221100",
            &[],
        );
        assert_eq!(
            b64(&empty),
            "TU0tU1lOQzEKR0VUCi9zeW5jL3YxL2NvbW1hbmRzL3BvbGwKMTc5Mzc2MTIwMApmZmVlZGRjY2JiYWE5OTg4Nzc2NjU1NDQzMzIyMTEwMAplM2IwYzQ0Mjk4ZmMxYzE0OWFmYmY0Yzg5OTZmYjkyNDI3YWU0MWU0NjQ5YjkzNGNhNDk1OTkxYjc4NTJiODU1"
        );
        assert_eq!(
            b64(&key.sign(&empty)),
            "RMc7lXyAMjKiWtI5niVkJlZ6+o3lGi+zDci/VRRpmFbcFRZz+ZWEI2XOvx4ktjInuKAeFVRzkI4lXY7vc+xpDg=="
        );
        // The headers of a fresh signature: whole seconds, 32 lowercase hex digits, 88 base64 characters.
        let clock = Arc::new(ManualClock::new(
            Timestamp::parse_iso("2026-11-04T03:00:00.999Z").unwrap(),
        ));
        let signer = RequestSigner::new(key.clone(), clock);
        let h = signer.sign("GET", "/x", &[]).unwrap();
        assert_eq!(h.timestamp, "1793761200");
        assert_eq!(h.nonce.len(), 32);
        assert!(
            h.nonce
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert_eq!(h.signature.len(), 88);
        assert_ne!(signer.sign("GET", "/x", &[]).unwrap().nonce, h.nonce);
        let put = canonical_with_body_hash(
            "PUT",
            "/sync/v1/legacy/import/abc?x=1",
            "1793761201",
            "0123456789abcdef0123456789abcdef",
            &format!("ab{}", "c".repeat(62)),
        );
        assert_eq!(
            b64(&key.sign(&put)),
            "ovzKe9edE0yXcDYzfeEuIjZYka3P4L5g36FhD1gnLM/QAopyoZ6fIkb1xwxzP7hWyL1h1fQzA2rIow7ykpdOAg=="
        );
        let fixed = signer.sign_with(
            "post",
            "/sync/v1/token?x=1&y=%C3%A9",
            &body_hash(body),
            "00112233445566778899aabbccddeeff",
        );
        assert_eq!(
            fixed.signature,
            "6a9QdIiYS8hzRi07WlGEAQfP8Di1WUBXtZB74rpJk5qLVwofdrH5c+w/rQrmTLCZymoTevY77cC2VCHVTrUgAA=="
        );
    }
}
