//! Agent identity: Ed25519 key sealed on disk and request signing (PROTOCOL.md §1).

use std::path::{Path, PathBuf};

use dl_ed25519::{SEED_LEN, SigningKey};
use dl_encoding::base64::STANDARD as B64;
use dl_hash::{Digest, Sha256};
use dl_json::{Value, json};

use crate::Error;
use crate::store;

/// DPAPI entropy: binds the sealed seed to this use.
const ENTROPY: &[u8] = b"dl-agent identity v1";

/// Who this agent is, once enrolled.
#[derive(Clone)]
pub struct Identity {
    /// Hub-assigned id.
    pub agent_id: String,
    /// Tenant code.
    pub tenant: String,
    /// Site code inside the tenant.
    pub site: String,
    /// Hub base URLs, primary first (fallback hosts after).
    pub hub_urls: Vec<String>,
    key: SigningKey,
}

impl core::fmt::Debug for Identity {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Never print the key.
        f.debug_struct("Identity")
            .field("agent_id", &self.agent_id)
            .field("tenant", &self.tenant)
            .field("site", &self.site)
            .field("hub_urls", &self.hub_urls)
            .finish_non_exhaustive()
    }
}

/// A fresh key pair for enrolment.
pub fn generate_key() -> Result<SigningKey, Error> {
    let seed: [u8; SEED_LEN] =
        dl_rand::secure_bytes().map_err(|e| Error::Config(format!("no OS randomness: {e}")))?;
    Ok(SigningKey::from_seed(&seed))
}

/// Base64 public key, as sent at enrolment.
pub fn public_key_b64(key: &SigningKey) -> String {
    B64.encode(key.verifying_key().as_bytes())
}

/// The string the signature covers.
pub fn canonical(
    method: &str,
    path_and_query: &str,
    time: u64,
    nonce: &str,
    body: &[u8],
) -> String {
    let body_hash = dl_encoding::hex::encode(Sha256::digest(body));
    format!("DLAGENT1\n{method}\n{path_and_query}\n{time}\n{nonce}\n{body_hash}")
}

impl Identity {
    /// Builds an identity (after enrolment or in tests).
    pub fn new(
        agent_id: &str,
        tenant: &str,
        site: &str,
        hub_urls: Vec<String>,
        key: SigningKey,
    ) -> Identity {
        Identity {
            agent_id: agent_id.into(),
            tenant: tenant.into(),
            site: site.into(),
            hub_urls,
            key,
        }
    }

    /// The signing headers for one request.
    pub fn sign(
        &self,
        method: &str,
        path_and_query: &str,
        body: &[u8],
        time: u64,
    ) -> Result<Vec<(&'static str, String)>, Error> {
        let nonce_bytes: [u8; 16] =
            dl_rand::secure_bytes().map_err(|e| Error::Config(format!("no OS randomness: {e}")))?;
        let nonce = dl_encoding::hex::encode(nonce_bytes);
        let sig = self
            .key
            .sign(canonical(method, path_and_query, time, &nonce, body).as_bytes());
        Ok(vec![
            ("X-Agent-Id", self.agent_id.clone()),
            ("X-Agent-Time", time.to_string()),
            ("X-Agent-Nonce", nonce),
            ("X-Agent-Signature", B64.encode(sig)),
        ])
    }

    /// Base64 public key.
    pub fn public_key(&self) -> String {
        public_key_b64(&self.key)
    }

    fn paths(dir: &Path) -> (PathBuf, PathBuf) {
        (dir.join("identity.json"), dir.join("identity.key"))
    }

    /// Whether `dir` holds an identity.
    pub fn exists(dir: &Path) -> bool {
        Identity::paths(dir).0.exists()
    }

    /// Saves the identity: public facts as JSON, the seed sealed (DPAPI machine scope on
    /// Windows, mode 0600 elsewhere).
    pub fn save(&self, dir: &Path) -> Result<(), Error> {
        let (meta, key) = Identity::paths(dir);
        store::write_bytes(&key, &seal(self.key.seed())?)?;
        restrict_file(&key)?;
        let v = json!({"agent_id": self.agent_id, "tenant": self.tenant, "site": self.site,
                       "hub_urls": self.hub_urls.clone(), "public_key": self.public_key()});
        store::write_json(&meta, &v)
    }

    /// Loads the identity; [`Error::NotEnrolled`] when there is none.
    pub fn load(dir: &Path) -> Result<Identity, Error> {
        let (meta, key) = Identity::paths(dir);
        let Some(v) = store::read_json(&meta)? else {
            return Err(Error::NotEnrolled);
        };
        let s = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| Error::Config(format!("identity.json: missing {k}")))
        };
        let blob = std::fs::read(&key).map_err(|e| Error::Io("read identity key", e))?;
        let seed: [u8; SEED_LEN] = unseal(&blob)?
            .try_into()
            .map_err(|_| Error::Config("identity key has the wrong length".into()))?;
        let hub_urls = v
            .get("hub_urls")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .filter(|u| !u.is_empty())
            .ok_or_else(|| Error::Config("identity.json: missing hub_urls".into()))?;
        let id = Identity::new(
            &s("agent_id")?,
            &s("tenant")?,
            &s("site")?,
            hub_urls,
            SigningKey::from_seed(&seed),
        );
        if v.get("public_key").and_then(Value::as_str) != Some(id.public_key().as_str()) {
            return Err(Error::Config(
                "identity key does not match identity.json".into(),
            ));
        }
        Ok(id)
    }

    /// Removes the identity (command `revoke`, or `uninstall --purge`).
    pub fn wipe(dir: &Path) -> Result<(), Error> {
        let (meta, key) = Identity::paths(dir);
        for p in [key, meta] {
            match std::fs::remove_file(&p) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(Error::Io("remove identity", e));
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
fn seal(seed: &[u8]) -> Result<Vec<u8>, Error> {
    use dl_winsvc::dpapi::{Scope, protect_with_entropy};
    protect_with_entropy(seed, Scope::LocalMachine, ENTROPY)
        .map_err(|e| Error::Config(format!("DPAPI protect: {e}")))
}

#[cfg(windows)]
fn unseal(blob: &[u8]) -> Result<Vec<u8>, Error> {
    use dl_winsvc::dpapi::{Scope, unprotect_with_entropy};
    unprotect_with_entropy(blob, Scope::LocalMachine, ENTROPY)
        .map_err(|e| Error::Config(format!("DPAPI unprotect: {e}")))
}

#[cfg(windows)]
pub(crate) fn restrict_file(path: &Path) -> Result<(), Error> {
    dl_winsvc::acl::restrict(path)
        .map_err(|e| Error::Config(format!("restrict {}: {e}", path.display())))
}

// Outside Windows the seed is stored as-is with mode 0600 (the data dir is root-owned).
#[cfg(not(windows))]
fn seal(seed: &[u8]) -> Result<Vec<u8>, Error> {
    let _ = ENTROPY;
    Ok(seed.to_vec())
}

#[cfg(not(windows))]
fn unseal(blob: &[u8]) -> Result<Vec<u8>, Error> {
    Ok(blob.to_vec())
}

#[cfg(unix)]
pub(crate) fn restrict_file(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| Error::Io("restrict private file", e))
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn restrict_file(_path: &Path) -> Result<(), Error> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testdir::TestDir;

    fn sample() -> Identity {
        Identity::new(
            "ag-1",
            "demo",
            "hq",
            vec!["https://hub.test".into()],
            SigningKey::from_seed(&[3; 32]),
        )
    }

    #[test]
    fn signature_verifies_over_canonical_string() {
        let id = sample();
        let h = id
            .sign("POST", "/v1/heartbeat", b"{}", 1_763_460_000)
            .unwrap();
        let get = |n: &str| {
            h.iter()
                .find(|(k, _)| *k == n)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert_eq!(get("X-Agent-Nonce").len(), 32);
        let msg = canonical(
            "POST",
            "/v1/heartbeat",
            1_763_460_000,
            &get("X-Agent-Nonce"),
            b"{}",
        );
        let pk = B64.decode(id.public_key()).unwrap();
        let sig = B64.decode(get("X-Agent-Signature")).unwrap();
        assert!(dl_ed25519::verify(&pk, msg.as_bytes(), &sig));
        assert!(!dl_ed25519::verify(
            &pk,
            msg.replace("POST", "GET").as_bytes(),
            &sig
        ));
    }

    #[test]
    fn canonical_matches_protocol_layout() {
        let c = canonical("GET", "/v1/releases/dl-agent/canary", 5, "00ff", b"");
        assert_eq!(
            c,
            "DLAGENT1\nGET\n/v1/releases/dl-agent/canary\n5\n00ff\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn save_load_wipe() {
        let d = TestDir::new("identity");
        assert!(matches!(Identity::load(&d.0), Err(Error::NotEnrolled)));
        let id = sample();
        id.save(&d.0).unwrap();
        let back = Identity::load(&d.0).unwrap();
        assert_eq!(
            (back.agent_id.as_str(), back.public_key()),
            ("ag-1", id.public_key())
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(d.0.join("identity.key"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        Identity::wipe(&d.0).unwrap();
        assert!(!Identity::exists(&d.0));
    }
}
