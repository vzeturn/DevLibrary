use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::*;
use crate::identity::{canonical, generate_key};

/// One request the fake hub saw.
#[derive(Clone, Debug)]
pub(crate) struct Seen {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// A scripted hub: answers are popped in order (`Err` = transport failure); every request
/// is recorded. When the script is empty it answers `default`.
/// One scripted transport answer: `(status, body)` or a transport error.
pub(crate) type Scripted = Result<(u16, String), String>;

#[derive(Clone, Default)]
pub(crate) struct FakeHub {
    pub seen: Arc<Mutex<Vec<Seen>>>,
    pub script: Arc<Mutex<VecDeque<Scripted>>>,
    pub default: Arc<Mutex<Option<(u16, String)>>>,
}

impl FakeHub {
    pub fn push(&self, code: u16, body: &str) -> &Self {
        self.script
            .lock()
            .unwrap()
            .push_back(Ok((code, body.into())));
        self
    }

    pub fn push_down(&self) -> &Self {
        self.script
            .lock()
            .unwrap()
            .push_back(Err("connection refused".into()));
        self
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

impl Transport for FakeHub {
    fn send(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, String)],
        body: &[u8],
    ) -> Result<(u16, Vec<u8>), Error> {
        self.seen.lock().unwrap().push(Seen {
            method: method.into(),
            url: url.into(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            body: body.to_vec(),
        });
        let next = self.script.lock().unwrap().pop_front();
        match next {
            Some(Ok((c, b))) => Ok((c, b.into_bytes())),
            Some(Err(e)) => Err(Error::Transport(e)),
            None => match self.default.lock().unwrap().clone() {
                Some((c, b)) => Ok((c, b.into_bytes())),
                None => Err(Error::Transport("script empty".into())),
            },
        }
    }
}

pub(crate) fn identity(hubs: &[&str]) -> Identity {
    Identity::new(
        "agt_1",
        "demo",
        "site-1",
        hubs.iter().map(|s| s.to_string()).collect(),
        generate_key().unwrap(),
    )
}

fn header<'a>(s: &'a Seen, k: &str) -> &'a str {
    s.headers
        .iter()
        .find(|(n, _)| n == k)
        .map_or("", |(_, v)| v.as_str())
}

/// Checks a request signature the way the hub does.
fn verify(id: &Identity, s: &Seen) -> bool {
    let key = dl_encoding::base64::STANDARD
        .decode(id.public_key())
        .unwrap();
    let sig = dl_encoding::base64::STANDARD
        .decode(header(s, "X-Agent-Signature"))
        .unwrap();
    let time: u64 = header(s, "X-Agent-Time").parse().unwrap();
    let msg = canonical(
        &s.method,
        &path_and_query(&s.url).unwrap(),
        time,
        header(s, "X-Agent-Nonce"),
        &s.body,
    );
    dl_ed25519::verify(&key, msg.as_bytes(), &sig)
}

#[test]
fn heartbeat_is_signed_and_parsed() {
    let hub = FakeHub::default();
    hub.push(
        200,
        r#"{"server_time": 1, "channel": "canary", "heartbeat_seconds": 5,
        "config": {"version": 3, "body": {"log_level": "debug"}},
        "commands": [{"id": "c1", "kind": "restart"}, {"kind": "no-id"}], "update_check": true}"#,
    );
    let id = identity(&["https://hub.example/"]);
    let c = HubClient::new(Box::new(hub.clone()), id.clone());
    let a = c.heartbeat(&json!({"agent_id": "agt_1"})).unwrap();
    assert_eq!(a.channel, "canary");
    assert_eq!(a.heartbeat_seconds, 10, "clamped to the minimum");
    assert_eq!(a.config.unwrap().version, 3);
    assert_eq!(a.commands.len(), 1);
    assert!(a.update_check);
    let seen = hub.seen();
    assert_eq!(seen[0].url, "https://hub.example/v1/heartbeat");
    assert_eq!(header(&seen[0], "X-Agent-Id"), "agt_1");
    assert!(verify(&id, &seen[0]));
    assert!(c.skew() < 0, "server_time 1 is far in the past");
}

#[test]
fn clock_skew_is_corrected_and_retried_once() {
    let hub = FakeHub::default();
    let server = now() + 3600;
    hub.push(
        401,
        &format!(r#"{{"error": "clock_skew", "server_time": {server}}}"#),
    )
    .push(200, r#"{"accepted": 2}"#);
    let c = HubClient::new(Box::new(hub.clone()), identity(&["https://hub"]));
    assert_eq!(c.reports(&[], "h").unwrap(), 2);
    let seen = hub.seen();
    assert_eq!(seen.len(), 2);
    let t: u64 = header(&seen[1], "X-Agent-Time").parse().unwrap();
    assert!(t + 5 >= server, "second try uses the hub clock");
    assert!((c.skew() - 3600).abs() <= 2);
}

#[test]
fn other_401_is_not_retried_and_exposes_code() {
    let hub = FakeHub::default();
    hub.push(401, r#"{"error": "revoked"}"#);
    let c = HubClient::new(Box::new(hub.clone()), identity(&["https://a", "https://b"]));
    let e = c.heartbeat(&json!({})).unwrap_err();
    assert_eq!(error_code(&e).as_deref(), Some("revoked"));
    assert_eq!(
        hub.seen().len(),
        1,
        "4xx does not fall back to the next hub"
    );
}

#[test]
fn falls_back_to_next_hub_and_remembers_it() {
    let hub = FakeHub::default();
    hub.push_down()
        .push(200, "{}")
        .push(503, "busy")
        .push(200, "{}");
    let c = HubClient::new(Box::new(hub.clone()), identity(&["https://a", "https://b"]));
    c.command_result(&CommandResult::new("c/1", true, "ok", 1))
        .unwrap();
    c.command_result(&CommandResult::new("c2", true, "ok", 1))
        .unwrap();
    let urls: Vec<String> = hub.seen().into_iter().map(|s| s.url).collect();
    assert_eq!(
        urls,
        [
            "https://a/v1/commands/c%2F1/result",
            "https://b/v1/commands/c%2F1/result",
            "https://b/v1/commands/c2/result",
            "https://a/v1/commands/c2/result"
        ]
    );
}

#[test]
fn manifest_404_means_no_release() {
    let hub = FakeHub::default();
    hub.push(404, r#"{"error": "no_release"}"#)
        .push(200, "{\"product\":\"dl-agent\"}");
    let c = HubClient::new(Box::new(hub.clone()), identity(&["https://a"]));
    assert_eq!(c.manifest("dl-agent", "canary").unwrap(), None);
    assert!(
        c.manifest("dl-agent", "canary")
            .unwrap()
            .unwrap()
            .contains("dl-agent")
    );
    assert_eq!(hub.seen()[0].url, "https://a/v1/releases/dl-agent/canary");
}

#[test]
fn download_signs_hub_urls_only() {
    let hub = FakeHub::default();
    hub.push(200, "bin").push(200, "cdn");
    let id = identity(&["https://a"]);
    let c = HubClient::new(Box::new(hub.clone()), id.clone());
    assert_eq!(c.download("https://a/v1/artifacts/k1").unwrap(), b"bin");
    assert_eq!(c.download("https://cdn.example/k1").unwrap(), b"cdn");
    let seen = hub.seen();
    assert!(verify(&id, &seen[0]));
    assert!(seen[1].headers.is_empty());
}

#[test]
fn enroll_tries_hubs_and_builds_identity() {
    let hub = FakeHub::default();
    hub.push_down().push(
        200,
        r#"{"agent_id": "agt_9", "tenant": "t", "site": "s", "channel": "canary",
        "heartbeat_seconds": 60, "config": {"version": 0, "body": {}}}"#,
    );
    let hubs = vec!["https://a".to_string(), "https://b".to_string()];
    let e = enroll(&hub, &hubs, "CODE", &["sys".into()]).unwrap();
    assert_eq!(e.identity.agent_id, "agt_9");
    assert_eq!(e.identity.hub_urls, hubs);
    assert_eq!(e.channel, "canary");
    assert!(e.config.is_none(), "version 0 is no config");
    let seen = hub.seen();
    assert_eq!(seen[1].url, "https://b/v1/enroll");
    assert!(seen[1].headers.is_empty(), "enrolment is unsigned");
    let body = dl_json::parse_slice(&seen[1].body).unwrap();
    assert_eq!(
        body.get("public_key").and_then(Value::as_str),
        Some(e.identity.public_key().as_str())
    );
    let refused = FakeHub::default();
    refused.push(400, r#"{"error": "bad_code"}"#);
    assert_eq!(
        error_code(&enroll(&refused, &hubs, "x", &[]).unwrap_err()).as_deref(),
        Some("bad_code")
    );
}

#[test]
fn path_and_query_strips_host_and_fragment() {
    assert_eq!(
        path_and_query("https://h:8443/v1/x?a=1#f").unwrap(),
        "/v1/x?a=1"
    );
    assert!(path_and_query("https://h").is_err());
}
