//! The HL hub API (Go `hubclient.go`) on `dl-http`: Bearer token, `X-Hotel-Id`,
//! `X-Agent-Version`, `X-Instance-Id` (single-active), `X-Mapping-Hash`, and hub URL
//! fallbacks with a sticky active base. The self-update endpoints are not ported.

use std::sync::Mutex;
use std::time::Duration;

use dl_http::{ClientOptions, Method, Request, Url};
use dl_json::Value;

use crate::error::{Error, Kind, Result};
use crate::gojson::{Obj, array, string};
use crate::jobmodel::{
    AuditSnapshot, AvailRow, CAP_PMS_AUDIT, CancelPolicy, MappingRow, PmsResult, Poll, encode,
    policy_from_value, poll_from_value,
};

/// `DefaultHubFallback`.
pub const DEFAULT_HUB_FALLBACK: &str = "https://smile-hl-hub.fly.dev";
/// `AgentCapabilities`.
pub const CAPABILITIES: [&str; 3] = ["hub-policy", "actions-v2", CAP_PMS_AUDIT];
/// The request timeout (longer than the 25 s long-poll).
const TIMEOUT: Duration = Duration::from_secs(60);
/// The response body limit.
const MAX_BODY: usize = 10 << 20;

/// `AgentConfig`: what `GET /api/agent/config` returns.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HubConfig {
    /// `enabled`: availability push on.
    pub enabled: bool,
    /// `availabilityIntervalMin`.
    pub availability_interval_min: i64,
    /// `availabilityDays`.
    pub availability_days: i64,
    /// `policy` (agents with the `hub-policy` capability).
    pub policy: Option<CancelPolicy>,
}

/// `Heartbeat` (the update field `pendingVer` is never set).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Heartbeat {
    /// `pmsOk`.
    pub pms_ok: bool,
    /// `breakerState`.
    pub breaker_state: String,
    /// `queueDepth`.
    pub queue_depth: i64,
    /// `recentErrors` (null when empty, as Go's nil slice).
    pub recent_errors: Vec<String>,
    /// `loopsStalled` (omitted when empty).
    pub loops_stalled: Vec<String>,
}

struct Bases {
    all: Vec<String>,
    active: usize,
    mapping_hash: String,
}

/// `HubClient`.
pub struct HubClient {
    http: dl_http::Client,
    /// Ends the call in flight (the 25 s long poll included) when the module stops.
    cancel: dl_http::Cancel,
    hotel_id: String,
    token: String,
    version: String,
    instance_id: String,
    st: Mutex<Bases>,
}

impl std::fmt::Debug for HubClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubClient")
            .field("hotel_id", &self.hotel_id)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

fn trim(u: &str) -> String {
    u.trim().trim_end_matches('/').to_owned()
}

/// `null` for an empty list (Go's nil slice), else the strings.
fn strings_or_null(out: &mut String, items: &[String]) {
    let items = if items.is_empty() { None } else { Some(items) };
    array(out, items, |out, s| string(out, s));
}

/// `truncate`: at most 300 bytes (on a char boundary) plus an ellipsis.
fn truncate(s: &str) -> String {
    if s.len() <= 300 {
        return s.to_owned();
    }
    let mut end = 300;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

impl HubClient {
    /// `NewHubClient`; `version` is sent as `X-Agent-Version`.
    pub fn new(
        base_url: &str,
        hotel_id: &str,
        token: &str,
        version: &str,
        instance_id: &str,
    ) -> HubClient {
        let http = dl_http::Client::with_options(ClientOptions {
            timeout: Some(TIMEOUT),
            max_body: MAX_BODY,
            ..Default::default()
        });
        HubClient {
            http,
            cancel: dl_http::Cancel::new(),
            hotel_id: hotel_id.to_owned(),
            token: token.to_owned(),
            version: version.to_owned(),
            instance_id: instance_id.to_owned(),
            st: Mutex::new(Bases {
                all: vec![trim(base_url)],
                active: 0,
                mapping_hash: String::new(),
            }),
        }
    }

    fn st(&self) -> std::sync::MutexGuard<'_, Bases> {
        self.st.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// `SetFallbacks`: the main URL first, then these (deduplicated), active reset to main.
    pub fn set_fallbacks<S: AsRef<str>>(&self, urls: &[S]) {
        let mut st = self.st();
        st.all.truncate(1);
        for u in urls {
            let u = trim(u.as_ref());
            if !u.is_empty() && !st.all.contains(&u) {
                st.all.push(u);
            }
        }
        st.active = 0;
    }

    /// The bases starting at the active one.
    fn bases(&self) -> Vec<String> {
        let st = self.st();
        let n = st.all.len();
        (0..n)
            .map(|i| st.all[(st.active + i) % n].clone())
            .collect()
    }

    fn mark_active(&self, base: &str) {
        let mut st = self.st();
        if let Some(i) = st.all.iter().position(|u| u == base) {
            st.active = i;
        }
    }

    /// Ends the call in flight at once and fails every later call (Go: the runner's
    /// context is cancelled on stop). One-way: a stopped runner is never restarted.
    pub fn cancel(&self) {
        if let Err(e) = self.cancel.cancel() {
            crate::log::warn(
                "closing the hub connection on stop failed",
                &[("error", crate::log::V::S(&e.to_string()))],
            );
        }
    }

    /// `SetMappingHash`: sent as `X-Mapping-Hash` from now on.
    pub fn set_mapping_hash(&self, h: &str) {
        self.st().mapping_hash = h.to_owned();
    }

    fn build(
        &self,
        base: &str,
        method: Method,
        path: &str,
        body: Option<&str>,
        extra: &[(&str, &str)],
        hash: &str,
    ) -> Result<Request> {
        let bad =
            |e: &dyn std::fmt::Display| Error::new(Kind::Hub, format!("hub request {path}: {e}"));
        let url = Url::parse(&format!("{base}{path}")).map_err(|e| bad(&e))?;
        let mut headers: Vec<(&str, String)> = vec![
            ("Content-Type", "application/json".into()),
            ("Authorization", format!("Bearer {}", self.token)),
            ("X-Hotel-Id", self.hotel_id.clone()),
            ("X-Agent-Version", self.version.clone()),
        ];
        if !self.instance_id.is_empty() {
            headers.push(("X-Instance-Id", self.instance_id.clone()));
        }
        if !hash.is_empty() {
            headers.push(("X-Mapping-Hash", hash.to_owned()));
        }
        for (k, v) in extra {
            headers.push((k, (*v).to_owned()));
        }
        let mut req = Request::new(method, url);
        for (k, v) in &headers {
            req = req.with_header(k, v).map_err(|e| bad(&e))?;
        }
        if let Some(b) = body {
            req = req.with_body(b.as_bytes().to_vec());
        }
        Ok(req.with_timeout(TIMEOUT))
    }

    /// `do`: tries each base on a network error; 401, 403 and other non-200 answers are
    /// errors of their own kinds. Returns the body of a 200.
    fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<&str>,
        extra: &[(&str, &str)],
    ) -> Result<Vec<u8>> {
        let m = if method == Method::Get { "GET" } else { "POST" };
        let hash = self.st().mapping_hash.clone();
        let mut errs: Vec<String> = Vec::new();
        for base in self.bases() {
            let req = self.build(&base, method, path, body, extra, &hash)?;
            match self.http.request_with_cancel(&req, &self.cancel) {
                // Stopping: no fallback base, the caller is on its way out.
                Err(dl_http::Error::Cancelled) => {
                    return Err(Error::new(
                        Kind::Hub,
                        format!("hub {m} {path}: cancelled (module stopping)"),
                    ));
                }
                Err(e) => errs.push(format!("{m} {base}{path}: {e}")),
                Ok(resp) => {
                    self.mark_active(&base);
                    let code = resp.status.as_u16();
                    return match code {
                        200 => Ok(resp.body),
                        403 => Err(Error::new(
                            Kind::Superseded,
                            "agent superseded: a newer agent install took over this hotel",
                        )),
                        _ => {
                            let text = truncate(&String::from_utf8_lossy(&resp.body));
                            let kind = if code == 401 {
                                Kind::Unauthorized
                            } else {
                                Kind::Hub
                            };
                            Err(Error::new(
                                kind,
                                format!("hub {m} {path}: HTTP {code}: {text}"),
                            ))
                        }
                    };
                }
            }
        }
        Err(Error::new(Kind::Hub, errs.join("\n")))
    }

    fn call_json(&self, method: Method, path: &str) -> Result<Value> {
        let raw = self.call(method, path, None, &[])?;
        dl_json::parse_slice(&raw)
            .map_err(|e| Error::new(Kind::Hub, format!("hub {path}: bad JSON: {e}")))
    }

    /// `PollJobs`: long-polls for up to `wait_sec` seconds.
    pub fn poll_jobs(&self, wait_sec: u32) -> Result<Poll> {
        let v = self.call_json(Method::Get, &format!("/api/agent/jobs?wait={wait_sec}"))?;
        Ok(poll_from_value(&v))
    }

    /// `AckMappingCmd`.
    pub fn ack_mapping_cmd(&self, cmd_id: &str, ok: bool, err: &str) -> Result<()> {
        let mut out = String::new();
        Obj::new(&mut out).str("error", err).bool("ok", ok).end();
        let path = format!(
            "/api/agent/mapping-cmds/{}/ack",
            dl_http::encode_path_segment(cmd_id)
        );
        self.call(Method::Post, &path, Some(&out), &[]).map(drop)
    }

    /// `Ack`: the job's result; `pms_result` is omitted when `None`.
    pub fn ack(
        &self,
        job_id: &str,
        ok: bool,
        err: &str,
        recent: &[String],
        pms_result: Option<&PmsResult>,
    ) -> Result<()> {
        let mut out = String::new();
        let mut o = Obj::new(&mut out);
        o.str("error", err).bool("ok", ok);
        if let Some(r) = pms_result {
            o.raw("pmsResult", |out| r.write_json(out));
        }
        o.raw("recentErrors", |out| strings_or_null(out, recent))
            .end();
        let path = format!(
            "/api/agent/jobs/{}/ack",
            dl_http::encode_path_segment(job_id)
        );
        self.call(Method::Post, &path, Some(&out), &[]).map(drop)
    }

    /// `PostPMSAudit`: the read-only PMS snapshot the hub asked for.
    pub fn post_pms_audit(&self, snap: &AuditSnapshot) -> Result<()> {
        self.call(
            Method::Post,
            "/api/agent/pms-audit",
            Some(&snap.to_json()),
            &[],
        )
        .map(drop)
    }

    /// `PushMapping`.
    pub fn push_mapping(&self, rows: &[MappingRow], hash: &str) -> Result<()> {
        let mut out = String::new();
        Obj::new(&mut out)
            .str("hash", hash)
            .raw("rows", |out| encode::mapping_rows(out, rows))
            .end();
        self.call(Method::Post, "/api/agent/mapping", Some(&out), &[])
            .map(drop)
    }

    /// `PushAvailability`.
    pub fn push_availability(&self, rows: &[AvailRow]) -> Result<()> {
        let mut out = String::new();
        Obj::new(&mut out)
            .raw("rows", |out| encode::avail_rows(out, rows))
            .end();
        self.call(Method::Post, "/api/agent/availability", Some(&out), &[])
            .map(drop)
    }

    /// `PushAvailabilityDisabled`: tells the hub the PMS switched inventory sync off.
    pub fn push_availability_disabled(&self) -> Result<()> {
        self.call(
            Method::Post,
            "/api/agent/availability",
            Some(r#"{"rows":[]}"#),
            &[("X-Inventory-Sync", "0")],
        )
        .map(drop)
    }

    /// `GetConfig`.
    pub fn get_config(&self) -> Result<HubConfig> {
        let v = self.call_json(Method::Get, "/api/agent/config")?;
        let int = |k: &str| v.get(k).and_then(Value::as_i64).unwrap_or(0);
        Ok(HubConfig {
            enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(false),
            availability_interval_min: int("availabilityIntervalMin"),
            availability_days: int("availabilityDays"),
            policy: v
                .get("policy")
                .filter(|p| !p.is_null())
                .map(policy_from_value),
        })
    }

    /// `Heartbeat`.
    pub fn heartbeat(&self, hb: &Heartbeat) -> Result<()> {
        let mut out = String::new();
        let mut o = Obj::new(&mut out);
        o.str("version", &self.version)
            .bool("pmsOk", hb.pms_ok)
            .str("breakerState", &hb.breaker_state)
            .int("queueDepth", hb.queue_depth)
            .raw("recentErrors", |out| {
                strings_or_null(out, &hb.recent_errors)
            })
            .raw("capabilities", |out| {
                let caps: Vec<String> = CAPABILITIES.iter().map(|s| (*s).to_owned()).collect();
                strings_or_null(out, &caps);
            });
        if !hb.loops_stalled.is_empty() {
            o.raw("loopsStalled", |out| {
                strings_or_null(out, &hb.loops_stalled)
            });
        }
        o.end();
        self.call(Method::Post, "/api/agent/heartbeat", Some(&out), &[])
            .map(drop)
    }
}

#[cfg(test)]
mod tests;
