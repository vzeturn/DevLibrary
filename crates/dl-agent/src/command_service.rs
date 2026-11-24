//! Signed remote service operations (RT-1054 part D): stop, start or change the start type
//! of a service the agent manages, with nobody at the machine.
//!
//! Wire form: `{id, kind: "service_control", args: {agent_id, service, action, start_type?,
//! nonce, issued_at, expires_at, key_id, signature}}`. `action` is `stop`, `start` or
//! `set_start_type` (`start_type`: `automatic`, `manual` or `disabled`). `signature` is the
//! base64 Ed25519 signature, by an operations key the agent trusts, over
//! [`ServiceRequest::signing_bytes`]:
//!
//! ```text
//! DLSVC1\n{agent_id}\n{command id}\n{service}\n{action}\n{start_type or ""}\n{nonce}\n{issued_at}\n{expires_at}\n{key_id}
//! ```
//!
//! A command is refused, before anything is touched, when it is unsigned, signed by an
//! unknown key or badly, addressed to another agent, expired, issued in the future, valid for
//! longer than [`MAX_LIFETIME`], a replay of a nonce already seen, or names a service outside
//! the [`ServiceAllowlist`]. Stopping or disabling the agent's own service is refused too:
//! it would cut the only remote way back in.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use dl_ed25519::{SIGNATURE_LEN, SigningKey};
use dl_encoding::base64::STANDARD as B64;
use dl_json::{Map, Value, json};
use dl_winsvc::control::ServiceControl;
use dl_winsvc::scm::StartType;

use crate::command::{Command, CommandResult};
use crate::manifest::TrustedKeys;

/// The command kind.
pub const KIND: &str = "service_control";
/// Longest validity window a signed command may carry (`expires_at - issued_at`).
pub const MAX_LIFETIME: u64 = 15 * 60;
/// How far `issued_at` may be ahead of the agent's clock.
pub const MAX_CLOCK_SKEW: u64 = 5 * 60;
/// Most nonces remembered at once; a full store refuses new commands instead of forgetting
/// a nonce that could still be replayed.
pub const MAX_NONCES: usize = 10_000;
/// How long a stop or start may take before it is reported as a timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

const DOMAIN: &str = "DLSVC1";

/// Why a service command was refused or failed. [`ServiceOpError::code`] is the stable
/// machine-readable part sent back to the hub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceOpError {
    /// No signature, or no key id.
    Unsigned,
    /// The agent trusts no operations key, so it accepts no service command.
    NoTrustedKeys,
    /// `key_id` is not a trusted key.
    UnknownKey(String),
    /// The signature does not match.
    BadSignature,
    /// The command was signed for another agent.
    WrongAgent(String),
    /// `now` is past `expires_at`.
    Expired {
        /// Signed expiry (Unix seconds).
        expires_at: u64,
        /// Agent clock (Unix seconds).
        now: u64,
    },
    /// `issued_at` is further in the future than [`MAX_CLOCK_SKEW`].
    NotYetValid {
        /// Signed issue time (Unix seconds).
        issued_at: u64,
        /// Agent clock (Unix seconds).
        now: u64,
    },
    /// `expires_at - issued_at` exceeds [`MAX_LIFETIME`] (or is negative).
    BadLifetime(u64, u64),
    /// The nonce was already used.
    Replayed(String),
    /// The service is not one this agent manages.
    UnknownService(String),
    /// Stopping or disabling the agent's own service.
    SelfLockout(String),
    /// A missing or malformed field.
    BadRequest(String),
    /// The nonce store cannot be read, written, or is full.
    NonceStore(String),
    /// The service manager call failed.
    Service(dl_winsvc::Error),
}

impl ServiceOpError {
    /// Stable code for the hub and dashboards.
    pub fn code(&self) -> &'static str {
        match self {
            ServiceOpError::Unsigned => "unsigned",
            ServiceOpError::NoTrustedKeys => "no_trusted_keys",
            ServiceOpError::UnknownKey(_) => "unknown_key",
            ServiceOpError::BadSignature => "bad_signature",
            ServiceOpError::WrongAgent(_) => "wrong_agent",
            ServiceOpError::Expired { .. } => "expired",
            ServiceOpError::NotYetValid { .. } => "not_yet_valid",
            ServiceOpError::BadLifetime(..) => "bad_lifetime",
            ServiceOpError::Replayed(_) => "replayed",
            ServiceOpError::UnknownService(_) => "unknown_service",
            ServiceOpError::SelfLockout(_) => "self_lockout",
            ServiceOpError::BadRequest(_) => "bad_request",
            ServiceOpError::NonceStore(_) => "nonce_store",
            ServiceOpError::Service(_) => "service_failed",
        }
    }
}

impl fmt::Display for ServiceOpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = self.code();
        match self {
            ServiceOpError::Unsigned => write!(f, "{code}: the command carries no signature"),
            ServiceOpError::NoTrustedKeys => {
                write!(f, "{code}: no operations key is configured on this agent")
            }
            ServiceOpError::UnknownKey(k) => write!(f, "{code}: key {k:?} is not trusted"),
            ServiceOpError::BadSignature => write!(f, "{code}: signature does not verify"),
            ServiceOpError::WrongAgent(a) => write!(f, "{code}: signed for agent {a:?}"),
            ServiceOpError::Expired { expires_at, now } => {
                write!(f, "{code}: expired at {expires_at}, now {now}")
            }
            ServiceOpError::NotYetValid { issued_at, now } => {
                write!(f, "{code}: issued at {issued_at}, now {now}")
            }
            ServiceOpError::BadLifetime(i, e) => write!(
                f,
                "{code}: issued_at {i} / expires_at {e} is not a window of at most {MAX_LIFETIME} s"
            ),
            ServiceOpError::Replayed(n) => write!(f, "{code}: nonce {n:?} was already used"),
            ServiceOpError::UnknownService(s) => {
                write!(f, "{code}: {s:?} is not a service this agent manages")
            }
            ServiceOpError::SelfLockout(s) => write!(
                f,
                "{code}: refusing to stop or disable the agent's own service {s:?}"
            ),
            ServiceOpError::BadRequest(m) => write!(f, "{code}: {m}"),
            ServiceOpError::NonceStore(m) => write!(f, "{code}: {m}"),
            ServiceOpError::Service(e) => write!(f, "{code}: {e}"),
        }
    }
}

impl std::error::Error for ServiceOpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ServiceOpError::Service(e) => Some(e),
            _ => None,
        }
    }
}

/// What to do to the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAction {
    /// Stop and wait.
    Stop,
    /// Start and wait.
    Start,
    /// Change the start type only.
    SetStartType(StartType),
}

impl ServiceAction {
    /// Wire name of the action.
    pub fn name(self) -> &'static str {
        match self {
            ServiceAction::Stop => "stop",
            ServiceAction::Start => "start",
            ServiceAction::SetStartType(_) => "set_start_type",
        }
    }

    /// Wire name of the start type, empty for stop and start.
    pub fn start_type_name(self) -> &'static str {
        match self {
            ServiceAction::SetStartType(StartType::Automatic) => "automatic",
            ServiceAction::SetStartType(StartType::Manual) => "manual",
            ServiceAction::SetStartType(StartType::Disabled) => "disabled",
            _ => "",
        }
    }

    fn parse(action: &str, start_type: Option<&str>) -> Result<ServiceAction, ServiceOpError> {
        let bad = |m: String| Err(ServiceOpError::BadRequest(m));
        match (action, start_type) {
            ("stop", None) => Ok(ServiceAction::Stop),
            ("start", None) => Ok(ServiceAction::Start),
            ("set_start_type", Some(t)) => Ok(ServiceAction::SetStartType(match t {
                "automatic" => StartType::Automatic,
                "manual" => StartType::Manual,
                "disabled" => StartType::Disabled,
                other => return bad(format!("unknown start_type {other:?}")),
            })),
            ("set_start_type", None) => bad("set_start_type needs start_type".into()),
            ("stop" | "start", Some(_)) => bad(format!("{action} takes no start_type")),
            (other, _) => bad(format!("unknown action {other:?}")),
        }
    }
}

/// One signed service command, as carried in the command's `args`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRequest {
    /// Hub command id (signed, so a signature cannot be moved to another command).
    pub command_id: String,
    /// Agent the command is for.
    pub agent_id: String,
    /// Service name.
    pub service: String,
    /// What to do.
    pub action: ServiceAction,
    /// Single-use random value.
    pub nonce: String,
    /// Unix seconds.
    pub issued_at: u64,
    /// Unix seconds; refused after.
    pub expires_at: u64,
    /// Operations key id.
    pub key_id: String,
    /// Ed25519 signature over [`ServiceRequest::signing_bytes`]; all zero when unsigned.
    pub signature: [u8; SIGNATURE_LEN],
}

/// A field that ends up in the signed text: 1 to 256 printable characters without newline.
fn field(v: &Value, k: &str) -> Result<String, ServiceOpError> {
    let s = v
        .get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| ServiceOpError::BadRequest(format!("missing {k}")))?;
    if s.is_empty() || s.len() > 256 || s.chars().any(char::is_control) {
        return Err(ServiceOpError::BadRequest(format!("invalid {k}")));
    }
    Ok(s.to_string())
}

impl ServiceRequest {
    /// Parses a [`KIND`] command. Does not verify it; [`ServiceOps::handle`] does.
    ///
    /// # Errors
    /// [`ServiceOpError::Unsigned`] without `signature`/`key_id`, otherwise
    /// [`ServiceOpError::BadRequest`] for a wrong kind or shape.
    pub fn from_command(cmd: &Command) -> Result<ServiceRequest, ServiceOpError> {
        if cmd.kind != KIND {
            return Err(ServiceOpError::BadRequest(format!(
                "not a {KIND} command: {}",
                cmd.kind
            )));
        }
        let a = &cmd.args;
        let sig = a.get("signature").and_then(Value::as_str).unwrap_or("");
        let key_id = a.get("key_id").and_then(Value::as_str).unwrap_or("");
        if sig.is_empty() || key_id.is_empty() {
            return Err(ServiceOpError::Unsigned);
        }
        let signature: [u8; SIGNATURE_LEN] = B64
            .decode(sig)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or(ServiceOpError::BadSignature)?;
        let num = |k: &str| {
            a.get(k)
                .and_then(Value::as_u64)
                .ok_or_else(|| ServiceOpError::BadRequest(format!("missing {k}")))
        };
        let start_type = a.get("start_type").and_then(Value::as_str);
        let nonce = field(a, "nonce")?;
        if nonce.len() < 16 {
            return Err(ServiceOpError::BadRequest(
                "nonce shorter than 16 characters".into(),
            ));
        }
        Ok(ServiceRequest {
            command_id: cmd.id.clone(),
            agent_id: field(a, "agent_id")?,
            service: field(a, "service")?,
            action: ServiceAction::parse(&field(a, "action")?, start_type)?,
            nonce,
            issued_at: num("issued_at")?,
            expires_at: num("expires_at")?,
            key_id: field(a, "key_id")?,
            signature,
        })
    }

    /// The exact bytes signed (see the module docs).
    pub fn signing_bytes(&self) -> Vec<u8> {
        format!(
            "{DOMAIN}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
            self.agent_id,
            self.command_id,
            self.service,
            self.action.name(),
            self.action.start_type_name(),
            self.nonce,
            self.issued_at,
            self.expires_at,
            self.key_id
        )
        .into_bytes()
    }

    /// Signs with an operations key (tooling and tests; the hub signs with the same text).
    pub fn sign(&mut self, key_id: &str, key: &SigningKey) {
        self.key_id = key_id.to_string();
        self.signature = key.sign(&self.signing_bytes());
    }

    /// The wire command carrying this request.
    pub fn to_command(&self) -> Command {
        let mut args = json!({
            "agent_id": self.agent_id, "service": self.service, "action": self.action.name(),
            "nonce": self.nonce, "issued_at": self.issued_at, "expires_at": self.expires_at,
            "key_id": self.key_id, "signature": B64.encode(self.signature),
        });
        if let (ServiceAction::SetStartType(_), Some(o)) = (self.action, args.as_object_mut()) {
            o.insert("start_type", self.action.start_type_name());
        }
        Command {
            id: self.command_id.clone(),
            kind: KIND.into(),
            module: None,
            args,
            expires_at: self.expires_at,
        }
    }

    /// Checks the signature against `trusted`.
    ///
    /// # Errors
    /// [`ServiceOpError::NoTrustedKeys`], [`ServiceOpError::UnknownKey`] or
    /// [`ServiceOpError::BadSignature`].
    pub fn verify(&self, trusted: &TrustedKeys) -> Result<(), ServiceOpError> {
        if trusted.is_empty() {
            return Err(ServiceOpError::NoTrustedKeys);
        }
        let key = trusted
            .get(&self.key_id)
            .ok_or_else(|| ServiceOpError::UnknownKey(self.key_id.clone()))?;
        key.verify(&self.signing_bytes(), &self.signature)
            .map_err(|_| ServiceOpError::BadSignature)
    }

    /// Checks the validity window against `now`.
    ///
    /// # Errors
    /// [`ServiceOpError::BadLifetime`], [`ServiceOpError::NotYetValid`] or
    /// [`ServiceOpError::Expired`].
    pub fn check_time(&self, now: u64) -> Result<(), ServiceOpError> {
        let (i, e) = (self.issued_at, self.expires_at);
        if e < i || e - i > MAX_LIFETIME {
            return Err(ServiceOpError::BadLifetime(i, e));
        }
        if i > now.saturating_add(MAX_CLOCK_SKEW) {
            return Err(ServiceOpError::NotYetValid { issued_at: i, now });
        }
        if now > e {
            return Err(ServiceOpError::Expired { expires_at: e, now });
        }
        Ok(())
    }
}

/// The services a service command may touch: the agent's own service plus the other
/// services it installed and manages. Names compare ASCII case-insensitively, as the SCM
/// does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceAllowlist {
    own: String,
    managed: Vec<String>,
}

impl ServiceAllowlist {
    /// The agent's own service (`App::service`) and the others it manages.
    pub fn new(own: &str, managed: &[&str]) -> ServiceAllowlist {
        ServiceAllowlist {
            own: own.to_string(),
            managed: managed.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// The canonical name of `service` and whether it is the agent's own.
    ///
    /// # Errors
    /// [`ServiceOpError::UnknownService`] when it is not listed.
    pub fn resolve(&self, service: &str) -> Result<(&str, bool), ServiceOpError> {
        if self.own.eq_ignore_ascii_case(service) {
            return Ok((&self.own, true));
        }
        self.managed
            .iter()
            .find(|m| m.eq_ignore_ascii_case(service))
            .map(|m| (m.as_str(), false))
            .ok_or_else(|| ServiceOpError::UnknownService(service.to_string()))
    }
}

/// Nonces of accepted commands until they expire, optionally kept in a file so a restart
/// does not reopen a replay window.
#[derive(Debug, Default)]
pub struct NonceStore {
    seen: BTreeMap<String, u64>,
    path: Option<PathBuf>,
}

impl NonceStore {
    /// A store that lives only in memory.
    pub fn in_memory() -> NonceStore {
        NonceStore::default()
    }

    /// A store kept in `path` (JSON `{nonce: expires_at}`); a missing file is empty.
    ///
    /// # Errors
    /// [`ServiceOpError::NonceStore`] when the file cannot be read or parsed: the agent then
    /// cannot prove a command is fresh, so it refuses rather than starting empty.
    pub fn open(path: impl Into<PathBuf>) -> Result<NonceStore, ServiceOpError> {
        let path = path.into();
        let err = |e: crate::Error| ServiceOpError::NonceStore(e.to_string());
        let mut seen = BTreeMap::new();
        if let Some(v) = crate::store::read_json(&path).map_err(err)? {
            let o = v.as_object().ok_or_else(|| {
                ServiceOpError::NonceStore(format!("{}: not an object", path.display()))
            })?;
            for (n, exp) in o {
                let exp = exp.as_u64().ok_or_else(|| {
                    ServiceOpError::NonceStore(format!("{}: bad expiry", path.display()))
                })?;
                seen.insert(n.to_string(), exp);
            }
        }
        Ok(NonceStore {
            seen,
            path: Some(path),
        })
    }

    /// Number of nonces remembered.
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// Whether no nonce is remembered.
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// Records `nonce` (valid until `expires_at`), forgetting expired ones first.
    ///
    /// # Errors
    /// [`ServiceOpError::Replayed`] when it was seen, [`ServiceOpError::NonceStore`] when
    /// the store is full or cannot be saved (the nonce is then not recorded).
    pub fn record(&mut self, nonce: &str, expires_at: u64, now: u64) -> Result<(), ServiceOpError> {
        self.seen.retain(|_, exp| *exp >= now);
        if self.seen.contains_key(nonce) {
            return Err(ServiceOpError::Replayed(nonce.to_string()));
        }
        if self.seen.len() >= MAX_NONCES {
            return Err(ServiceOpError::NonceStore(format!(
                "{MAX_NONCES} live nonces; refusing until some expire"
            )));
        }
        self.seen.insert(nonce.to_string(), expires_at);
        if let Err(e) = self.save() {
            self.seen.remove(nonce);
            return Err(e);
        }
        Ok(())
    }

    fn save(&self) -> Result<(), ServiceOpError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let mut m = Map::new();
        for (n, exp) in &self.seen {
            m.insert(n.as_str(), *exp);
        }
        crate::store::write_json(path, &Value::Object(m))
            .map_err(|e| ServiceOpError::NonceStore(e.to_string()))
    }
}

/// Verifies and carries out [`KIND`] commands.
pub struct ServiceOps {
    agent_id: String,
    trusted: TrustedKeys,
    allowlist: ServiceAllowlist,
    nonces: NonceStore,
    control: Box<dyn ServiceControl>,
    timeout: Duration,
}

impl fmt::Debug for ServiceOps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceOps")
            .field("agent_id", &self.agent_id)
            .field("allowlist", &self.allowlist)
            .field("nonces", &self.nonces.len())
            .finish_non_exhaustive()
    }
}

impl ServiceOps {
    /// An executor for agent `agent_id`, trusting `trusted` operations keys and acting
    /// through `control` (use [`dl_winsvc::control::system`] in the service).
    pub fn new(
        agent_id: &str,
        trusted: TrustedKeys,
        allowlist: ServiceAllowlist,
        nonces: NonceStore,
        control: Box<dyn ServiceControl>,
    ) -> ServiceOps {
        ServiceOps {
            agent_id: agent_id.to_string(),
            trusted,
            allowlist,
            nonces,
            control,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Changes how long stop and start may take.
    pub fn with_timeout(mut self, timeout: Duration) -> ServiceOps {
        self.timeout = timeout;
        self
    }

    /// Verifies `cmd` and carries it out; returns a one-line summary.
    ///
    /// Order: shape, signature, agent, time window, allowlist, nonce (recorded before the
    /// call, so a command that failed half-way cannot be replayed), service call.
    ///
    /// # Errors
    /// The first check that fails, or [`ServiceOpError::Service`] from the service manager.
    pub fn handle(&mut self, cmd: &Command, now: u64) -> Result<String, ServiceOpError> {
        let req = ServiceRequest::from_command(cmd)?;
        req.verify(&self.trusted)?;
        if req.agent_id != self.agent_id {
            return Err(ServiceOpError::WrongAgent(req.agent_id));
        }
        req.check_time(now)?;
        let (name, own) = self.allowlist.resolve(&req.service)?;
        let name = name.to_string();
        let locks_out = matches!(
            req.action,
            ServiceAction::Stop | ServiceAction::SetStartType(StartType::Disabled)
        );
        if own && locks_out {
            return Err(ServiceOpError::SelfLockout(name));
        }
        self.nonces.record(&req.nonce, req.expires_at, now)?;
        let c = &self.control;
        match req.action {
            ServiceAction::Stop => c.stop(&name, self.timeout),
            ServiceAction::Start => c.start(&name, self.timeout),
            ServiceAction::SetStartType(t) => c.set_start_type(&name, t),
        }
        .map_err(ServiceOpError::Service)?;
        Ok(match req.action {
            ServiceAction::SetStartType(_) => {
                format!("{name}: start type {}", req.action.start_type_name())
            }
            a => format!(
                "{name}: {}",
                if a == ServiceAction::Stop {
                    "stopped"
                } else {
                    "running"
                }
            ),
        })
    }

    /// [`ServiceOps::handle`] as the command's single result; an error becomes
    /// `{ok: false, output: "<code>: <detail>"}`.
    pub fn answer(&mut self, cmd: &Command, now: u64) -> CommandResult {
        match self.handle(cmd, now) {
            Ok(out) => CommandResult::new(&cmd.id, true, out, now),
            Err(e) => CommandResult::new(&cmd.id, false, e.to_string(), now),
        }
    }
}

#[cfg(test)]
#[path = "command_service_tests.rs"]
mod tests;
