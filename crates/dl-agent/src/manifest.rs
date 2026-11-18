//! Signed release manifests (PROTOCOL.md §5).

use dl_ed25519::{SIGNATURE_LEN, SigningKey, VerifyingKey};
use dl_encoding::base64::STANDARD as B64;
use dl_json::{Value, json};

use crate::Error;
use crate::channel::Channel;
use crate::version::Version;

/// One downloadable build for one target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artifact {
    /// `"windows-x86_64"`, `"linux-x86_64"` … see [`crate::current_target`].
    pub target: String,
    /// Download URL.
    pub url: String,
    /// SHA-256 of the file.
    pub sha256: [u8; 32],
    /// File size in bytes.
    pub size: u64,
}

/// A release of one product on one channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    /// Product name, `"dl-agent"`.
    pub product: String,
    /// Channel the release is published on.
    pub channel: Channel,
    /// Released version.
    pub version: Version,
    /// Publication time (Unix seconds); agents refuse older manifests than the last accepted.
    pub published_at: u64,
    /// Share of stable agents (0–100) that install it.
    pub rollout_percent: u8,
    /// Oldest version that may update straight to this one.
    pub min_version: Option<Version>,
    /// Allows installing a lower version than the running one (emergency rollback releases).
    pub allow_downgrade: bool,
    /// Installs at once, outside the site's update window (a fix that cannot wait).
    pub urgent: bool,
    /// Installs even where the site turned automatic updates off.
    pub required: bool,
    /// Release notes.
    pub notes: String,
    /// Builds per target.
    pub artifacts: Vec<Artifact>,
    /// Id of the release key that signed it.
    pub key_id: String,
    /// Ed25519 signature over [`Manifest::signing_bytes`]; all zero when unsigned.
    pub signature: [u8; SIGNATURE_LEN],
}

/// Public keys an agent accepts release signatures from, by key id (supports rotation).
#[derive(Clone, Debug, Default)]
pub struct TrustedKeys {
    keys: Vec<(String, VerifyingKey)>,
}

impl TrustedKeys {
    /// An empty set.
    pub fn new() -> TrustedKeys {
        TrustedKeys::default()
    }

    /// Adds a key from its base64 form.
    pub fn with_key(mut self, key_id: &str, public_key_b64: &str) -> Result<TrustedKeys, Error> {
        let raw = B64
            .decode(public_key_b64)
            .map_err(|e| Error::Config(format!("release key {key_id}: {e}")))?;
        let arr: [u8; 32] = raw
            .try_into()
            .map_err(|_| Error::Config(format!("release key {key_id}: not 32 bytes")))?;
        let key = VerifyingKey::from_bytes(&arr)
            .map_err(|e| Error::Config(format!("release key {key_id}: {e}")))?;
        self.keys.push((key_id.to_string(), key));
        Ok(self)
    }

    pub(crate) fn get(&self, key_id: &str) -> Option<&VerifyingKey> {
        self.keys
            .iter()
            .find(|(id, _)| id == key_id)
            .map(|(_, k)| k)
    }
}

impl Manifest {
    fn body(&self) -> Value {
        let artifacts: Vec<Value> = self
            .artifacts
            .iter()
            .map(|a| json!({"target": a.target, "url": a.url, "sha256": dl_encoding::hex::encode(a.sha256), "size": a.size}))
            .collect();
        let min = self
            .min_version
            .as_ref()
            .map(|v| Value::String(v.to_string()))
            .unwrap_or(Value::Null);
        let mut body = json!({
            "product": self.product,
            "channel": self.channel.as_str(),
            "version": self.version.to_string(),
            "published_at": self.published_at,
            "rollout_percent": u64::from(self.rollout_percent),
            "min_version": min,
            "allow_downgrade": self.allow_downgrade,
            "notes": self.notes,
            "artifacts": artifacts,
            "key_id": self.key_id,
        });
        // Written only when set, so manifests signed before these flags still verify.
        if let Some(o) = body.as_object_mut() {
            if self.urgent {
                o.insert("urgent", true);
            }
            if self.required {
                o.insert("required", true);
            }
        }
        body
    }

    /// The canonical bytes the signature covers: compact JSON of every field but `signature`.
    pub fn signing_bytes(&self) -> Vec<u8> {
        dl_json::to_string(&self.body()).into_bytes()
    }

    /// Signs the manifest with a release key.
    pub fn sign(&mut self, key_id: &str, key: &SigningKey) {
        self.key_id = key_id.to_string();
        self.signature = key.sign(&self.signing_bytes());
    }

    /// Checks the signature against the trusted keys.
    pub fn verify(&self, trusted: &TrustedKeys) -> Result<(), Error> {
        let key = trusted
            .get(&self.key_id)
            .ok_or_else(|| Error::Manifest(format!("unknown release key {:?}", self.key_id)))?;
        key.verify(&self.signing_bytes(), &self.signature)
            .map_err(|_| Error::Manifest("bad signature".into()))
    }

    /// The manifest as JSON, signature included.
    pub fn to_json(&self) -> String {
        let mut v = self.body();
        if let Some(o) = v.as_object_mut() {
            o.insert("signature", B64.encode(self.signature));
        }
        dl_json::to_string_pretty(&v)
    }

    /// Parses a manifest. Does not verify it; call [`Manifest::verify`].
    pub fn parse(text: &str) -> Result<Manifest, Error> {
        let v = dl_json::parse(text)?;
        let s = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Manifest(format!("missing {k}")))
        };
        let n = |k: &str| {
            v.get(k)
                .and_then(Value::as_u64)
                .ok_or_else(|| Error::Manifest(format!("missing {k}")))
        };
        let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        let channel: Channel = s("channel")?.parse()?;
        let rollout = n("rollout_percent")?;
        if rollout > 100 {
            return Err(Error::Manifest("rollout_percent above 100".into()));
        }
        let min_version = match v.get("min_version").and_then(Value::as_str) {
            Some(m) => Some(m.parse()?),
            None => None,
        };
        let mut artifacts = Vec::new();
        for a in v
            .get("artifacts")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Manifest("missing artifacts".into()))?
        {
            let f = |k: &str| {
                a.get(k)
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::Manifest(format!("artifact missing {k}")))
            };
            let sha = dl_encoding::hex::decode_array::<32>(f("sha256")?)
                .map_err(|_| Error::Manifest("artifact sha256".into()))?;
            let size = a
                .get("size")
                .and_then(Value::as_u64)
                .ok_or_else(|| Error::Manifest("artifact missing size".into()))?;
            artifacts.push(Artifact {
                target: f("target")?.into(),
                url: f("url")?.into(),
                sha256: sha,
                size,
            });
        }
        let sig = B64
            .decode(s("signature")?)
            .map_err(|_| Error::Manifest("signature is not base64".into()))?;
        let signature: [u8; SIGNATURE_LEN] = sig
            .try_into()
            .map_err(|_| Error::Manifest("signature length".into()))?;
        Ok(Manifest {
            product: s("product")?.into(),
            channel,
            version: s("version")?.parse()?,
            published_at: n("published_at")?,
            rollout_percent: rollout as u8, // checked ≤ 100 above
            min_version,
            allow_downgrade: v
                .get("allow_downgrade")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            urgent: flag("urgent"),
            required: flag("required"),
            notes: v.get("notes").and_then(Value::as_str).unwrap_or("").into(),
            artifacts,
            key_id: s("key_id")?.into(),
            signature,
        })
    }

    /// The build for `target`, if the release has one.
    pub fn artifact_for(&self, target: &str) -> Option<&Artifact> {
        self.artifacts.iter().find(|a| a.target == target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn sample(channel: Channel, version: &str, published_at: u64) -> Manifest {
        Manifest {
            product: "dl-agent".into(),
            channel,
            version: version.parse().unwrap(),
            published_at,
            rollout_percent: 100,
            min_version: None,
            allow_downgrade: false,
            urgent: false,
            required: false,
            notes: "Bản thử".into(),
            artifacts: vec![Artifact {
                target: "windows-x86_64".into(),
                url: "https://example.test/a.exe".into(),
                sha256: [7; 32],
                size: 3,
            }],
            key_id: String::new(),
            signature: [0; SIGNATURE_LEN],
        }
    }

    fn keys() -> (SigningKey, TrustedKeys) {
        let k = SigningKey::from_seed(&[9; 32]);
        let t = TrustedKeys::new()
            .with_key("rel-1", &B64.encode(k.verifying_key().as_bytes()))
            .unwrap();
        (k, t)
    }

    #[test]
    fn sign_round_trip_verify() {
        let (k, t) = keys();
        let mut m = sample(Channel::Canary, "1.4.0-canary.1", 100);
        m.sign("rel-1", &k);
        let back = Manifest::parse(&m.to_json()).unwrap();
        assert_eq!(back, m);
        back.verify(&t).unwrap();
    }

    #[test]
    fn tampering_and_unknown_keys_are_rejected() {
        let (k, t) = keys();
        let mut m = sample(Channel::Stable, "1.4.0", 100);
        m.sign("rel-1", &k);
        let mut evil = m.clone();
        evil.artifacts[0].url = "https://evil.test/a.exe".into();
        assert!(evil.verify(&t).is_err());
        let mut other = m.clone();
        other.key_id = "rel-9".into();
        assert!(other.verify(&t).is_err());
        let unsigned = sample(Channel::Stable, "1.4.0", 100);
        assert!(unsigned.verify(&t).is_err());
    }

    #[test]
    fn urgent_and_required_are_signed_and_absent_when_off() {
        let (k, t) = keys();
        let plain = sample(Channel::Stable, "1.4.0", 100);
        let text = dl_json::to_string(&plain.body());
        assert!(!text.contains("urgent") && !text.contains("required"));

        let mut m = sample(Channel::Stable, "1.4.1", 100);
        m.urgent = true;
        m.required = true;
        m.sign("rel-1", &k);
        let back = Manifest::parse(&m.to_json()).unwrap();
        assert!(back.urgent && back.required);
        back.verify(&t).unwrap();
        let mut stripped = back.clone();
        stripped.urgent = false;
        assert!(
            stripped.verify(&t).is_err(),
            "the flag is covered by the signature"
        );
    }
}
