//! `SyncMessages`: what travels inside the envelopes, written as STJ's web defaults write
//! it (camelCase, nulls included, enums as numbers) and read case-insensitively.

use dl_json::Value;

use super::projection::Projection;
use super::row::PmsRow;
use crate::stj::{self, escape_into};

/// Envelope type of an agent poll.
pub const TYPE_POLL: &str = "agent.poll";
/// Envelope type of the cloud's answer.
pub const TYPE_POLL_RESPONSE: &str = "cloud.poll-response";
/// Header naming why the cloud refused this agent.
pub const HEADER_REFUSED: &str = "X-Agent-Refused";
/// `X-Agent-Refused` value for a wrong machine clock.
pub const REFUSED_CLOCK: &str = "clock";

/// `PmsResponseStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Full result.
    Ok = 0,
    /// Same ETag as the cloud's base.
    NotModified = 1,
    /// Changes against the cloud's base.
    Delta = 2,
    /// Refused before reaching the PMS.
    Rejected = 3,
    /// The PMS failed.
    Failed = 4,
}

impl Status {
    fn from_i64(n: i64) -> Option<Status> {
        Some(match n {
            0 => Status::Ok,
            1 => Status::NotModified,
            2 => Status::Delta,
            3 => Status::Rejected,
            4 => Status::Failed,
            _ => return None,
        })
    }
}

/// Request parameters: names compared ordinally, last duplicate wins in place.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Params(Vec<(String, Option<String>)>);

impl Params {
    /// No parameters.
    pub fn new() -> Params {
        Params::default()
    }

    /// Sets `name` (exact spelling).
    pub fn insert(&mut self, name: impl Into<String>, value: Option<String>) {
        let name = name.into();
        match self.0.iter_mut().find(|(k, _)| *k == name) {
            Some(slot) => slot.1 = value,
            None => self.0.push((name, value)),
        }
    }

    /// `Some(value)` when `name` is present exactly (`Some(None)` for an explicit null).
    pub fn get(&self, name: &str) -> Option<Option<&str>> {
        self.0
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_deref())
    }

    /// Parameters in order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, Option<&str>)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_deref()))
    }

    /// Count.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` when empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// `PmsRequest`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PmsRequest {
    /// Correlates the response.
    pub request_id: String,
    /// Catalog operation, or a vault / key operation.
    pub operation: String,
    /// Parameters as text.
    pub parameters: Params,
    /// ETag of the snapshot the cloud holds.
    pub base_etag: Option<String>,
    /// Required for writes.
    pub idempotency_key: Option<String>,
    /// Columns to keep per set.
    pub projection: Option<Projection>,
}

/// `PmsResultSet`.
#[derive(Debug, Clone, PartialEq)]
pub struct PmsResultSet {
    /// Position in the procedure's output.
    pub index: i32,
    /// `HashRows` of the full set.
    pub hash: String,
    /// Rows (upserts only in a delta).
    pub rows: Vec<PmsRow>,
    /// Delta: keys gone since the base.
    pub removed_keys: Option<Vec<String>>,
    /// Delta: every key in the new order.
    pub key_order: Option<Vec<String>>,
}

/// `PmsResponse`.
#[derive(Debug, Clone, PartialEq)]
pub struct PmsResponse {
    /// From the request.
    pub request_id: String,
    /// Outcome.
    pub status: Status,
    /// `CombineETag` of the full result.
    pub etag: Option<String>,
    /// Sets (empty for NotModified, Rejected, Failed).
    pub result_sets: Vec<PmsResultSet>,
    /// Why it was rejected or failed.
    pub error: Option<String>,
    /// Time spent answering.
    pub elapsed_ms: i32,
}

impl PmsResponse {
    /// A response without sets.
    pub fn bare(request_id: &str, status: Status, error: Option<String>) -> PmsResponse {
        PmsResponse {
            request_id: request_id.to_string(),
            status,
            etag: None,
            result_sets: Vec::new(),
            error,
            elapsed_ms: 0,
        }
    }
}

/// `PmsChangeNotice`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmsChangeNotice {
    /// Watched read.
    pub operation: String,
    /// `HashParameters` of the watch's parameters.
    pub parameters_hash: String,
    /// New ETag.
    pub new_etag: String,
    /// Rows in the new result.
    pub changed_rows: i32,
}

/// `AgentHello` (problems and installations are not reported by this agent: dl-agent's
/// own reports carry them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentHello {
    /// Agent version.
    pub agent_version: String,
    /// `PmsOperationCatalog.Version`.
    pub catalog_version: i32,
    /// Machine name.
    pub machine_name: String,
    /// `{bundleHash}.{schemaFingerprint}`.
    pub pms_schema_fingerprint: Option<String>,
    /// `procedures: ...` when the bundle failed to install.
    pub procedures_report: Option<String>,
}

/// `AgentPollRequest`.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentPollRequest {
    /// Sent until the first successful poll.
    pub hello: Option<AgentHello>,
    /// Answers.
    pub responses: Vec<PmsResponse>,
    /// Change notices.
    pub notices: Vec<PmsChangeNotice>,
}

/// `AgentPollResponse` (update fields ignored: dl-agent updates itself).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentPollResponse {
    /// Work for the agent.
    pub requests: Vec<PmsRequest>,
    /// Wait before the next poll.
    pub next_poll_delay_ms: i32,
}

fn opt_string(out: &mut String, v: Option<&str>) {
    match v {
        Some(s) => escape_into(out, s),
        None => out.push_str("null"),
    }
}

fn string_list(out: &mut String, v: Option<&[String]>) {
    let Some(list) = v else {
        out.push_str("null");
        return;
    };
    out.push('[');
    for (i, s) in list.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        escape_into(out, s);
    }
    out.push(']');
}

impl PmsResultSet {
    /// Appends the JSON text.
    pub fn write(&self, out: &mut String) {
        out.push_str("{\"index\":");
        out.push_str(&self.index.to_string());
        out.push_str(",\"hash\":");
        escape_into(out, &self.hash);
        out.push_str(",\"rows\":[");
        for (i, r) in self.rows.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            r.write(out);
        }
        out.push_str("],\"removedKeys\":");
        string_list(out, self.removed_keys.as_deref());
        out.push_str(",\"keyOrder\":");
        string_list(out, self.key_order.as_deref());
        out.push('}');
    }

    /// Reads one set.
    ///
    /// # Errors
    /// A description of the first malformed field.
    pub fn from_json(v: &Value) -> Result<PmsResultSet, String> {
        let m = v.as_object().ok_or("a result set must be an object")?;
        let rows = match stj::get_ci(m, "rows") {
            Some(Value::Array(a)) => a.iter().map(PmsRow::from_json).collect::<Result<_, _>>()?,
            None | Some(Value::Null) => Vec::new(),
            Some(_) => return Err("'rows' must be a list".into()),
        };
        Ok(PmsResultSet {
            index: stj::opt_i64(m, "index")?
                .and_then(|n| i32::try_from(n).ok())
                .unwrap_or(0),
            hash: stj::opt_str(m, "hash")?.unwrap_or_default(),
            rows,
            removed_keys: opt_string_list(m, "removedKeys")?,
            key_order: opt_string_list(m, "keyOrder")?,
        })
    }
}

fn opt_string_list(m: &dl_json::Map, name: &str) -> Result<Option<Vec<String>>, String> {
    match stj::get_ci(m, name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => a
            .iter()
            .map(|s| {
                s.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("'{name}' must list strings"))
            })
            .collect::<Result<_, _>>()
            .map(Some),
        Some(_) => Err(format!("'{name}' must be a list")),
    }
}

impl PmsResponse {
    /// Appends the JSON text.
    pub fn write(&self, out: &mut String) {
        out.push_str("{\"requestId\":");
        escape_into(out, &self.request_id);
        out.push_str(",\"status\":");
        out.push_str(&(self.status as i32).to_string());
        out.push_str(",\"eTag\":");
        opt_string(out, self.etag.as_deref());
        out.push_str(",\"resultSets\":[");
        for (i, s) in self.result_sets.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            s.write(out);
        }
        out.push_str("],\"error\":");
        opt_string(out, self.error.as_deref());
        out.push_str(",\"elapsedMs\":");
        out.push_str(&self.elapsed_ms.to_string());
        out.push('}');
    }

    /// The JSON text.
    pub fn to_json_string(&self) -> String {
        let mut s = String::new();
        self.write(&mut s);
        s
    }

    /// Reads a response (the idempotency journal stores them).
    ///
    /// # Errors
    /// A description of the first malformed field.
    pub fn from_json(v: &Value) -> Result<PmsResponse, String> {
        let m = v.as_object().ok_or("a response must be an object")?;
        let status = stj::opt_i64(m, "status")?.unwrap_or(0);
        let result_sets = match stj::get_ci(m, "resultSets") {
            Some(Value::Array(a)) => a
                .iter()
                .map(PmsResultSet::from_json)
                .collect::<Result<_, _>>()?,
            None | Some(Value::Null) => Vec::new(),
            Some(_) => return Err("'resultSets' must be a list".into()),
        };
        Ok(PmsResponse {
            request_id: stj::opt_str(m, "requestId")?.unwrap_or_default(),
            status: Status::from_i64(status).ok_or("unknown status")?,
            etag: stj::opt_str(m, "eTag")?,
            result_sets,
            error: stj::opt_str(m, "error")?,
            elapsed_ms: stj::opt_i64(m, "elapsedMs")?
                .and_then(|n| i32::try_from(n).ok())
                .unwrap_or(0),
        })
    }
}

impl PmsChangeNotice {
    /// Appends the JSON text.
    pub fn write(&self, out: &mut String) {
        out.push_str("{\"operation\":");
        escape_into(out, &self.operation);
        out.push_str(",\"parametersHash\":");
        escape_into(out, &self.parameters_hash);
        out.push_str(",\"newETag\":");
        escape_into(out, &self.new_etag);
        out.push_str(",\"changedRows\":");
        out.push_str(&self.changed_rows.to_string());
        out.push('}');
    }
}

impl AgentHello {
    /// Appends the JSON text.
    pub fn write(&self, out: &mut String) {
        out.push_str("{\"agentVersion\":");
        escape_into(out, &self.agent_version);
        out.push_str(",\"catalogVersion\":");
        out.push_str(&self.catalog_version.to_string());
        out.push_str(",\"machineName\":");
        escape_into(out, &self.machine_name);
        out.push_str(",\"pmsSchemaFingerprint\":");
        opt_string(out, self.pms_schema_fingerprint.as_deref());
        out.push_str(",\"proceduresReport\":");
        opt_string(out, self.procedures_report.as_deref());
        out.push_str(",\"recentProblems\":null,\"installations\":null}");
    }
}

impl AgentPollRequest {
    /// The JSON text sealed into an `agent.poll` envelope.
    pub fn to_json_string(&self) -> String {
        let mut out = String::from("{\"hello\":");
        match &self.hello {
            Some(h) => h.write(&mut out),
            None => out.push_str("null"),
        }
        out.push_str(",\"responses\":[");
        for (i, r) in self.responses.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            r.write(&mut out);
        }
        out.push_str("],\"notices\":[");
        for (i, n) in self.notices.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            n.write(&mut out);
        }
        out.push_str("],\"updateCheck\":null}");
        out
    }
}

impl PmsRequest {
    /// Appends the JSON text (the cloud's side; used by tests and the fake cloud).
    pub fn write(&self, out: &mut String) {
        out.push_str("{\"requestId\":");
        escape_into(out, &self.request_id);
        out.push_str(",\"operation\":");
        escape_into(out, &self.operation);
        out.push_str(",\"parameters\":{");
        for (i, (k, v)) in self.parameters.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            escape_into(out, k);
            out.push(':');
            opt_string(out, v);
        }
        out.push_str("},\"baseETag\":");
        opt_string(out, self.base_etag.as_deref());
        out.push_str(",\"idempotencyKey\":");
        opt_string(out, self.idempotency_key.as_deref());
        out.push_str(",\"projection\":");
        match &self.projection {
            None => out.push_str("null"),
            Some(p) => {
                out.push('[');
                for (i, set) in p.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    string_list(out, set.as_deref());
                }
                out.push(']');
            }
        }
        out.push('}');
    }

    /// Reads a request from the cloud.
    ///
    /// # Errors
    /// A description of the first malformed field.
    pub fn from_json(v: &Value) -> Result<PmsRequest, String> {
        let m = v.as_object().ok_or("a request must be an object")?;
        let mut parameters = Params::new();
        match stj::get_ci(m, "parameters") {
            None | Some(Value::Null) => {}
            Some(Value::Object(p)) => {
                for (k, val) in p.iter() {
                    let val = match val {
                        Value::Null => None,
                        Value::String(s) => Some(s.clone()),
                        _ => return Err(format!("parameter '{k}' must be a string")),
                    };
                    parameters.insert(k.clone(), val);
                }
            }
            Some(_) => return Err("'parameters' must be an object".into()),
        }
        let projection = match stj::get_ci(m, "projection") {
            None | Some(Value::Null) => None,
            Some(Value::Array(sets)) => {
                let mut p = Vec::with_capacity(sets.len());
                for set in sets {
                    p.push(match set {
                        Value::Null => None,
                        Value::Array(cols) => Some(
                            cols.iter()
                                .map(|c| c.as_str().map(str::to_string))
                                .collect::<Option<Vec<_>>>()
                                .ok_or("projection columns must be strings")?,
                        ),
                        _ => return Err("projection sets must be lists".into()),
                    });
                }
                Some(p)
            }
            Some(_) => return Err("'projection' must be a list".into()),
        };
        Ok(PmsRequest {
            request_id: stj::opt_str(m, "requestId")?.unwrap_or_default(),
            operation: stj::opt_str(m, "operation")?.unwrap_or_default(),
            parameters,
            base_etag: stj::opt_str(m, "baseETag")?,
            idempotency_key: stj::opt_str(m, "idempotencyKey")?,
            projection,
        })
    }
}

impl AgentPollResponse {
    /// Reads the cloud's answer.
    ///
    /// # Errors
    /// A description of the first malformed field.
    pub fn from_json(v: &Value) -> Result<AgentPollResponse, String> {
        let m = v.as_object().ok_or("a poll response must be an object")?;
        let requests = match stj::get_ci(m, "requests") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(a)) => a
                .iter()
                .map(PmsRequest::from_json)
                .collect::<Result<_, _>>()?,
            Some(_) => return Err("'requests' must be a list".into()),
        };
        Ok(AgentPollResponse {
            requests,
            next_poll_delay_ms: stj::opt_i64(m, "nextPollDelayMs")?
                .map(|n| n.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32)
                .unwrap_or(0),
        })
    }

    /// The JSON text (the cloud's side; used by tests and the fake cloud).
    pub fn to_json_string(&self) -> String {
        let mut out = String::from("{\"requests\":[");
        for (i, r) in self.requests.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            r.write(&mut out);
        }
        out.push_str("],\"nextPollDelayMs\":");
        out.push_str(&self.next_poll_delay_ms.to_string());
        out.push_str(",\"update\":null,\"updateHint\":null}");
        out
    }
}
