//! The SRBackup hub's agent API (`<origin>/api/agent/...`), the port of the Go
//! `hubclient` package: self-registration, config, the per-agent key, reports, discovery and
//! logs. Every call carries `Authorization: Bearer <agent token>` except self-registration,
//! which carries the tenant's enroll key.

pub mod types;

use std::sync::Arc;
use std::time::Duration;

use dl_http::{Method, Request, Url};
use dl_json::{Value, json};

pub use types::*;

use crate::Error;
use crate::crypto::envelope::{DataKey, check_key_id};

/// How long one hub call may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// A client for one hub origin.
#[derive(Clone)]
pub struct HubClient {
    origin: String,
    token: String,
    http: Arc<dl_http::Client>,
}

/// What `self-register` answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// The agent's id at the hub.
    pub agent_id: String,
    /// The agent's API token (a secret).
    pub token: String,
}

impl std::fmt::Debug for HubClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubClient")
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

/// Checks a hub origin: `https://host[:port]`, or `http://` for a loopback host only.
pub fn validate_origin(origin: &str) -> Result<String, Error> {
    let trimmed = origin.trim().trim_end_matches('/');
    let url = Url::parse(trimmed).map_err(|e| Error::Hub(format!("hub url {trimmed:?}: {e}")))?;
    let local = matches!(url.host(), "localhost" | "127.0.0.1" | "::1" | "[::1]");
    if url.scheme() == dl_http::Scheme::Http && !local {
        return Err(Error::Hub(format!(
            "hub url {trimmed:?} must use https (http is allowed for localhost only)"
        )));
    }
    if url.path() != "/" && !url.path().is_empty() {
        return Err(Error::Hub(format!(
            "hub url {trimmed:?} must be an origin without a path"
        )));
    }
    Ok(trimmed.to_owned())
}

impl HubClient {
    /// A client for `origin` (already validated) holding the agent token.
    pub fn new(origin: &str, token: &str) -> HubClient {
        let http = dl_http::Client::with_options(dl_http::ClientOptions {
            timeout: Some(CALL_TIMEOUT),
            connect_timeout: Some(Duration::from_secs(10)),
            user_agent: Some(format!("dl-agent-backup/{}", env!("CARGO_PKG_VERSION"))),
            ..Default::default()
        });
        HubClient {
            origin: origin.trim_end_matches('/').to_owned(),
            token: token.to_owned(),
            http: Arc::new(http),
        }
    }

    /// The origin this client talks to.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    fn send(
        &self,
        method: Method,
        path: &str,
        bearer: &str,
        body: Option<&Value>,
    ) -> Result<dl_http::Response, Error> {
        let url = format!("{}/api/agent{path}", self.origin);
        let parsed = Url::parse(&url).map_err(|e| Error::Hub(format!("{path}: {e}")))?;
        let mut req = Request::new(method, parsed)
            .with_header("Authorization", &format!("Bearer {bearer}"))
            .and_then(|r| r.with_header("Accept", "application/json"))
            .map_err(|e| Error::Hub(format!("{path}: {e}")))?;
        if let Some(b) = body {
            req = req
                .with_header("Content-Type", "application/json")
                .map_err(|e| Error::Hub(format!("{path}: {e}")))?
                .with_body(dl_json::to_string(b).into_bytes());
        }
        self.http
            .request(&req)
            .map_err(|e| Error::Hub(format!("{path}: {e}")))
    }

    fn call(
        &self,
        method: Method,
        path: &str,
        bearer: &str,
        body: Option<&Value>,
    ) -> Result<Value, Error> {
        let resp = self.send(method, path, bearer, body)?;
        decode(path, &resp)
    }

    /// `POST /api/agent/self-register` with the tenant enroll key.
    pub fn self_register(
        &self,
        enroll_key: &str,
        machine_name: &str,
        hwid: &str,
    ) -> Result<Registration, Error> {
        let body = json!({"machineName": machine_name, "hwid": hwid});
        let v = self.call(Method::Post, "/self-register", enroll_key, Some(&body))?;
        let field = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let reg = Registration {
            agent_id: field("agentId"),
            token: field("token"),
        };
        if reg.agent_id.is_empty() || reg.token.is_empty() {
            return Err(Error::Hub(
                "self-register: response has no agentId/token".into(),
            ));
        }
        Ok(reg)
    }

    /// `GET /api/agent/config`.
    pub fn fetch_config(&self) -> Result<ConfigResponse, Error> {
        let v = self.call(Method::Get, "/config", &self.token, None)?;
        ConfigResponse::from_json(&v)
    }

    /// `GET /api/agent/config` only to learn whether the hub accepts this client's token
    /// (any 2xx); the body is not interpreted. 401/403 is [`Error::Auth`].
    pub fn check_token(&self) -> Result<(), Error> {
        self.call(Method::Get, "/config", &self.token, None)
            .map(|_| ())
    }

    /// `GET /api/agent/master-key`: the per-agent key the config's credentials are sealed under.
    pub fn master_key(&self) -> Result<String, Error> {
        let v = self.call(Method::Get, "/master-key", &self.token, None)?;
        match v.get("masterKey").and_then(Value::as_str) {
            Some(k) if !k.is_empty() => Ok(k.to_owned()),
            _ => Err(Error::Hub("master-key: response has no masterKey".into())),
        }
    }

    /// `GET /api/agent/backup-key`: the tenant's current backup data key. `None` when the
    /// hub answers 404 (a hub without per-tenant backup keys: backups go up unencrypted).
    pub fn backup_key(&self) -> Result<Option<DataKey>, Error> {
        let path = "/backup-key";
        let resp = self.send(Method::Get, path, &self.token, None)?;
        if resp.status.as_u16() == 404 {
            return Ok(None);
        }
        data_key(path, &decode(path, &resp)?).map(Some)
    }

    /// `GET /api/agent/backup-key?id=<id>`: the key a sealed backup names. The hub answers
    /// only for its own tenant's keys; 404 (or 403) for any other id is
    /// [`Error::Credential`], not an agent rejection.
    pub fn backup_key_by_id(&self, id: &str) -> Result<DataKey, Error> {
        check_key_id(id)?;
        let path = format!("/backup-key?id={id}");
        let resp = self.send(Method::Get, &path, &self.token, None)?;
        if matches!(resp.status.as_u16(), 403 | 404) {
            return Err(Error::Credential(format!(
                "the backup is encrypted under key {id}, which the hub does not give this agent \
                 (a backup of another tenant, or a key the hub no longer has)"
            )));
        }
        let key = data_key("/backup-key", &decode("/backup-key", &resp)?)?;
        if key.id != id {
            return Err(Error::Hub(format!(
                "/backup-key: asked for key {id}, the hub sent {}",
                key.id
            )));
        }
        Ok(key)
    }

    /// `POST /api/agent/report` with a backup, restore or command outcome.
    pub fn report(&self, body: &Value) -> Result<(), Error> {
        self.call(Method::Post, "/report", &self.token, Some(body))
            .map(drop)
    }

    /// `POST /api/agent/discover`: the user databases found on a server.
    pub fn discover(&self, server_id: &str, databases: &[String]) -> Result<(), Error> {
        let dbs: Vec<Value> = databases
            .iter()
            .map(|d| json!({"databaseName": d.as_str()}))
            .collect();
        let body = json!({"serverId": server_id, "databases": dbs});
        self.call(Method::Post, "/discover", &self.token, Some(&body))
            .map(drop)
    }

    /// `POST /api/agent/logs`: one line for the hub's agent log.
    pub fn log(&self, level: &str, message: &str) -> Result<(), Error> {
        let body = json!({"level": level, "message": message});
        self.call(Method::Post, "/logs", &self.token, Some(&body))
            .map(drop)
    }
}

/// Turns a hub answer into its JSON body; 401/403 is [`Error::Auth`], other failures
/// [`Error::Hub`].
fn decode(path: &str, resp: &dl_http::Response) -> Result<Value, Error> {
    let code = resp.status.as_u16();
    if code == 401 || code == 403 {
        return Err(Error::Auth(format!("{path}: hub answered HTTP {code}")));
    }
    if !resp.status.is_success() {
        let text = String::from_utf8_lossy(&resp.body);
        let snippet: String = text.chars().take(200).collect();
        return Err(Error::Hub(format!(
            "{path}: hub answered HTTP {code}: {snippet}"
        )));
    }
    if resp.body.is_empty() {
        return Ok(Value::Null);
    }
    dl_json::parse_slice(&resp.body).map_err(|e| Error::Hub(format!("{path}: bad JSON: {e}")))
}

fn data_key(path: &str, v: &Value) -> Result<DataKey, Error> {
    let field = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default();
    if field("keyId").is_empty() || field("key").is_empty() {
        return Err(Error::Hub(format!("{path}: response has no keyId/key")));
    }
    DataKey::from_hub(field("keyId"), field("key"))
}

/// The report body for a finished backup, the Go `BackupReport` on the wire.
pub fn backup_report(
    schedule_id: &str,
    history_id: &str,
    ok: bool,
    steps: Vec<Value>,
    checksum: &str,
    cloud_file_path: &str,
) -> Value {
    json!({
        "type": "backup",
        "scheduleId": schedule_id,
        "historyId": history_id,
        "status": if ok { "Succeeded" } else { "Failed" },
        "steps": steps,
        "checksum": checksum,
        "cloudFilePath": cloud_file_path,
    })
}

/// Adds the tenant backup key id a successful backup's copy is sealed under
/// (`encryptionKeyId`); an unencrypted copy leaves the body as it is.
pub fn with_key_id(mut body: Value, key_id: &str) -> Value {
    if !key_id.is_empty()
        && let Some(m) = body.as_object_mut()
    {
        m.insert("encryptionKeyId".to_owned(), Value::from(key_id));
    }
    body
}

/// The report body for a restore request.
pub fn restore_report(request_id: &str, ok: bool, error: &str) -> Value {
    json!({
        "type": "restore",
        "requestId": request_id,
        "status": if ok { "Succeeded" } else { "Failed" },
        "error": error,
    })
}

/// The report body for a command.
pub fn command_report(command_id: &str, ok: bool, result: Value) -> Value {
    json!({
        "type": "command",
        "commandId": command_id,
        "status": if ok { "Succeeded" } else { "Failed" },
        "result": result,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_must_be_https_or_loopback() {
        assert_eq!(
            validate_origin("https://backupapi.sr.io.vn/").unwrap(),
            "https://backupapi.sr.io.vn"
        );
        assert!(validate_origin("http://backupapi.sr.io.vn").is_err());
        assert!(validate_origin("http://127.0.0.1:8787").is_ok());
        assert!(validate_origin("https://h/api").is_err());
        assert!(validate_origin("not a url").is_err());
    }

    #[test]
    fn report_bodies_match_the_hub_contract() {
        let b = backup_report("s", "h", false, vec![json!({"step": "job"})], "", "");
        assert_eq!(b.get("status").and_then(Value::as_str), Some("Failed"));
        assert_eq!(b.get("type").and_then(Value::as_str), Some("backup"));
        let r = restore_report("r", true, "");
        assert_eq!(r.get("status").and_then(Value::as_str), Some("Succeeded"));
        let c = command_report("c", true, json!({"latencyMs": 3}));
        assert_eq!(c.get("commandId").and_then(Value::as_str), Some("c"));
    }
}
