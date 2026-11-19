//! `SyncEnvelope`: every message between agent and cloud is AES-256-GCM sealed under a
//! per-direction key derived (HKDF-SHA256) from the agent secret, with the clear header
//! bound in as associated data. `ReplayGuard` refuses stale and repeated envelopes.

use std::collections::HashMap;
use std::sync::Mutex;

use dl_json::Value;

use crate::crypto::{self, AesGcm, NONCE_LEN, TAG_LEN};
use crate::error::Error;
use crate::stj::{self, escape_into};

/// `SyncEnvelopeCipher.Version`.
pub const VERSION: i32 = 1;

/// The clear header plus the sealed payload, as it travels over HTTP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncEnvelope {
    /// Format version.
    pub v: i32,
    /// Tenant id (GUID, `N` format).
    pub tenant_id: String,
    /// Agent id.
    pub agent_id: String,
    /// Key id of the agent secret.
    pub kid: String,
    /// Unique per message (GUID, `N` format).
    pub message_id: String,
    /// Unix milliseconds when sealed.
    pub timestamp: i64,
    /// Message type.
    pub ty: String,
    /// Base64 GCM nonce.
    pub nonce: String,
    /// Base64 ciphertext followed by the 16-byte tag.
    pub ciphertext: String,
}

impl SyncEnvelope {
    fn aad(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{}",
            self.v,
            self.tenant_id,
            self.agent_id,
            self.kid,
            self.message_id,
            self.timestamp,
            self.ty,
            self.nonce
        )
    }

    /// The JSON body (STJ web defaults).
    pub fn to_json_string(&self) -> String {
        let mut out = String::from("{\"v\":");
        out.push_str(&self.v.to_string());
        for (name, value) in [
            ("tenantId", &self.tenant_id),
            ("agentId", &self.agent_id),
            ("kid", &self.kid),
            ("messageId", &self.message_id),
        ] {
            out.push_str(",\"");
            out.push_str(name);
            out.push_str("\":");
            escape_into(&mut out, value);
        }
        out.push_str(",\"timestamp\":");
        out.push_str(&self.timestamp.to_string());
        for (name, value) in [
            ("type", &self.ty),
            ("nonce", &self.nonce),
            ("ciphertext", &self.ciphertext),
        ] {
            out.push_str(",\"");
            out.push_str(name);
            out.push_str("\":");
            escape_into(&mut out, value);
        }
        out.push('}');
        out
    }

    /// Reads an envelope body (property names case-insensitive, missing strings empty).
    ///
    /// # Errors
    /// [`Error::Crypto`] when the body is not an envelope.
    pub fn parse(text: &str) -> Result<SyncEnvelope, Error> {
        let bad = |m: String| Error::Crypto(format!("Malformed envelope: {m}."));
        let v = stj::parse(text).map_err(bad)?;
        let Value::Object(m) = v else {
            return Err(Error::Crypto("Empty reply from cloud.".into()));
        };
        let s = |name: &str| stj::opt_str(&m, name).map(Option::unwrap_or_default);
        let n = |name: &str| stj::opt_i64(&m, name).map(|v| v.unwrap_or(0));
        Ok(SyncEnvelope {
            v: i32::try_from(n("v").map_err(bad)?).map_err(|_| bad("v".into()))?,
            tenant_id: s("tenantId").map_err(bad)?,
            agent_id: s("agentId").map_err(bad)?,
            kid: s("kid").map_err(bad)?,
            message_id: s("messageId").map_err(bad)?,
            timestamp: n("timestamp").map_err(bad)?,
            ty: s("type").map_err(bad)?,
            nonce: s("nonce").map_err(bad)?,
            ciphertext: s("ciphertext").map_err(bad)?,
        })
    }
}

/// Which way a message travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Agent to cloud.
    AgentToCloud,
    /// Cloud to agent.
    CloudToAgent,
}

/// `SyncEnvelopeCipher`: seals and opens envelopes for one agent.
pub struct EnvelopeCipher {
    tenant_id: String,
    agent_id: String,
    kid: String,
    agent_to_cloud: AesGcm,
    cloud_to_agent: AesGcm,
}

impl std::fmt::Debug for EnvelopeCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvelopeCipher")
            .field("tenant_id", &self.tenant_id)
            .field("agent_id", &self.agent_id)
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

impl EnvelopeCipher {
    /// Derives both direction keys from the 32-byte agent secret.
    ///
    /// # Errors
    /// [`Error::Crypto`] when the secret is not 32 bytes.
    pub fn new(tenant_id: &str, agent_id: &str, kid: &str, secret: &[u8]) -> Result<Self, Error> {
        if secret.len() != 32 {
            return Err(Error::Crypto("Agent secret must be 32 bytes.".into()));
        }
        let salt = format!("mm-arrival/sync/{tenant_id}/{agent_id}");
        let a2c = crypto::hkdf_sha256(secret, salt.as_bytes(), b"v1 agent->cloud");
        let c2a = crypto::hkdf_sha256(secret, salt.as_bytes(), b"v1 cloud->agent");
        Ok(EnvelopeCipher {
            tenant_id: tenant_id.to_string(),
            agent_id: agent_id.to_string(),
            kid: kid.to_string(),
            agent_to_cloud: AesGcm::new(&a2c),
            cloud_to_agent: AesGcm::new(&c2a),
        })
    }

    /// Agent id.
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Key id.
    pub fn kid(&self) -> &str {
        &self.kid
    }

    fn key(&self, d: Direction) -> &AesGcm {
        match d {
            Direction::AgentToCloud => &self.agent_to_cloud,
            Direction::CloudToAgent => &self.cloud_to_agent,
        }
    }

    /// Seals `plain` (JSON text) with a fresh nonce and message id.
    ///
    /// # Errors
    /// [`Error::Crypto`] when the system random source fails.
    pub fn seal(
        &self,
        d: Direction,
        ty: &str,
        plain: &str,
        now_ms: i64,
    ) -> Result<SyncEnvelope, Error> {
        let nonce = crypto::random::<NONCE_LEN>()?;
        let id =
            dl_uuid::Uuid::try_new_v4().map_err(|e| Error::Crypto(format!("message id: {e}")))?;
        Ok(self.seal_with(d, ty, plain, now_ms, &id.simple().to_string(), nonce))
    }

    /// Seals with the given message id and nonce (fixtures).
    pub fn seal_with(
        &self,
        d: Direction,
        ty: &str,
        plain: &str,
        now_ms: i64,
        message_id: &str,
        nonce: [u8; NONCE_LEN],
    ) -> SyncEnvelope {
        let mut env = SyncEnvelope {
            v: VERSION,
            tenant_id: self.tenant_id.clone(),
            agent_id: self.agent_id.clone(),
            kid: self.kid.clone(),
            message_id: message_id.to_string(),
            timestamp: now_ms,
            ty: ty.to_string(),
            nonce: dl_encoding::base64::STANDARD.encode(nonce),
            ciphertext: String::new(),
        };
        let sealed = self
            .key(d)
            .seal(&nonce, plain.as_bytes(), env.aad().as_bytes());
        env.ciphertext = dl_encoding::base64::STANDARD.encode(sealed);
        env
    }

    /// Checks the header, decrypts and returns the JSON text.
    ///
    /// # Errors
    /// [`Error::Crypto`] with the .NET message for the first problem.
    pub fn open_text(
        &self,
        d: Direction,
        expected_type: &str,
        env: &SyncEnvelope,
    ) -> Result<String, Error> {
        if env.v != VERSION
            || env.tenant_id != self.tenant_id
            || env.agent_id != self.agent_id
            || env.kid != self.kid
            || env.ty != expected_type
        {
            return Err(Error::Crypto(
                "Envelope header does not match this channel.".into(),
            ));
        }
        let malformed = || Error::Crypto("Malformed envelope.".into());
        let nonce = crate::contracts::values::parse_base64(&env.nonce).ok_or_else(malformed)?;
        let data = crate::contracts::values::parse_base64(&env.ciphertext).ok_or_else(malformed)?;
        let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| malformed())?;
        if data.len() < TAG_LEN {
            return Err(malformed());
        }
        let plain = self
            .key(d)
            .open(&nonce, &data, env.aad().as_bytes())
            .ok_or_else(|| Error::Crypto("The envelope failed authentication.".into()))?;
        String::from_utf8(plain).map_err(|_| Error::Crypto("Envelope payload is not UTF-8.".into()))
    }

    /// [`Self::open_text`] parsed as JSON; a `null` payload is refused.
    ///
    /// # Errors
    /// [`Error::Crypto`] as [`Self::open_text`], or for a payload that is not JSON.
    pub fn open(
        &self,
        d: Direction,
        expected_type: &str,
        env: &SyncEnvelope,
    ) -> Result<Value, Error> {
        let text = self.open_text(d, expected_type, env)?;
        match stj::parse(&text) {
            Ok(Value::Null) => Err(Error::Crypto("Empty envelope payload.".into())),
            Ok(v) => Ok(v),
            Err(e) => Err(Error::Crypto(format!("Envelope payload is not JSON: {e}."))),
        }
    }
}

/// `SyncReplayGuard`: envelopes outside the skew window, or seen before, are refused.
#[derive(Debug)]
pub struct ReplayGuard {
    skew_ms: i64,
    state: Mutex<ReplayState>,
}

#[derive(Debug, Default)]
struct ReplayState {
    seen: HashMap<String, i64>,
    last_sweep_ms: i64,
}

const SWEEP_INTERVAL_MS: i64 = 1_000;
const SWEEP_ABOVE: usize = 1024;

impl ReplayGuard {
    /// A guard with the given window either side of now.
    pub fn new(skew_ms: i64) -> ReplayGuard {
        ReplayGuard {
            skew_ms,
            state: Mutex::new(ReplayState::default()),
        }
    }

    /// `TryAccept`.
    pub fn try_accept(&self, env: &SyncEnvelope, now_ms: i64) -> bool {
        if now_ms.saturating_sub(env.timestamp).saturating_abs() > self.skew_ms {
            return false;
        }
        let key = format!("{}/{}/{}", env.tenant_id, env.agent_id, env.message_id);
        let mut st = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if st.seen.len() > SWEEP_ABOVE
            && now_ms.saturating_sub(st.last_sweep_ms).saturating_abs() >= SWEEP_INTERVAL_MS
        {
            let skew = self.skew_ms;
            st.seen.retain(|_, ts| now_ms.saturating_sub(*ts) <= skew);
            st.last_sweep_ms = now_ms;
        }
        if st.seen.contains_key(&key) {
            return false;
        }
        st.seen.insert(key, env.timestamp);
        true
    }

    /// Envelopes currently remembered.
    pub fn tracked(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .seen
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cipher() -> EnvelopeCipher {
        EnvelopeCipher::new("t", "a", "k", &[7u8; 32]).unwrap()
    }

    #[test]
    fn round_trips_and_binds_the_header() {
        let c = cipher();
        let env = c.seal_with(Direction::CloudToAgent, "x", "{\"a\":1}", 5, "m", [1; 12]);
        assert_eq!(
            c.open_text(Direction::CloudToAgent, "x", &env).unwrap(),
            "{\"a\":1}"
        );
        let mut tampered = env.clone();
        tampered.timestamp = 6;
        assert!(
            c.open_text(Direction::CloudToAgent, "x", &tampered)
                .is_err()
        );
        assert!(c.open_text(Direction::AgentToCloud, "x", &env).is_err());
        assert!(c.open_text(Direction::CloudToAgent, "y", &env).is_err());
        let parsed = SyncEnvelope::parse(&env.to_json_string()).unwrap();
        assert_eq!(parsed, env);
    }

    #[test]
    fn replay_guard_refuses_repeats_and_skew() {
        let g = ReplayGuard::new(120_000);
        let c = cipher();
        let env = c.seal_with(Direction::CloudToAgent, "x", "{}", 1_000_000, "m", [1; 12]);
        assert!(g.try_accept(&env, 1_000_000));
        assert!(!g.try_accept(&env, 1_000_000));
        let old = c.seal_with(Direction::CloudToAgent, "x", "{}", 1_000_000, "n", [1; 12]);
        assert!(!g.try_accept(&old, 1_000_000 + 120_001));
        assert!(g.try_accept(&old, 1_000_000 - 120_000));
    }
}
