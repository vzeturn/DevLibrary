//! The request-key log: stores a write's result and replays it for a retried request.

use super::*;

pub(super) struct LogRow {
    pub(super) op: String,
    pub(super) status: String,
    pub(super) hash: String,
    pub(super) json: String,
}

pub(super) struct Stored {
    pub(super) sets: Vec<Vec<Row>>,
    pub(super) rowcounts: Vec<i64>,
    pub(super) outputs: BTreeMap<String, J>,
}

pub(super) fn store_json(
    sets: &[Vec<Row>],
    rowcounts: &[i64],
    outputs: &BTreeMap<String, J>,
) -> String {
    // Same text as Go's json.Marshal(storedResult{...}).
    let frame = wire::to_json(&wire::Frame::Result(OpResult {
        id: String::new(),
        status: String::new(),
        sets: sets.to_vec(),
        rowcounts: rowcounts.to_vec(),
        outputs: outputs.clone(),
        error: None,
        replayed: None,
    }));
    // {"type":"result","id":"","status":"","sets":…,"rowcounts":…,"outputs":…,"error":null}
    let start = frame.find(",\"sets\":").unwrap_or(0);
    let end = frame.rfind(",\"error\":").unwrap_or(frame.len());
    format!(
        "{{\"v\":{}{}}}",
        json::quote(AGENT_ENVELOPE),
        &frame[start..end]
    )
}

pub(super) fn replay(r: &LogRow, op: &str) -> Result<Stored, ExecErr> {
    if r.op != op || r.status != "applied" {
        return Err(ExecErr::ReplayConflict);
    }
    if crate::sha256_hex(r.json.as_bytes()) != r.hash.trim().to_lowercase() {
        return Err(ExecErr::LogCorrupt);
    }
    let Some(o) = json::parse_object(r.json.as_bytes()) else {
        return Err(ExecErr::ReplayConflict);
    };
    if json::field_ci(&o, "v").and_then(J::as_str) != Some(AGENT_ENVELOPE) {
        return Err(ExecErr::ReplayConflict);
    }
    let sets = match json::field_ci(&o, "sets") {
        None | Some(J::Null) => return Err(ExecErr::LogCorrupt),
        Some(J::Array(items)) => {
            let mut sets = Vec::new();
            for s in items {
                let J::Array(rows) = s else {
                    return Err(ExecErr::ReplayConflict);
                };
                let mut out = Vec::new();
                for row in rows {
                    let J::Object(m) = row else {
                        return Err(ExecErr::ReplayConflict);
                    };
                    out.push(m.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
                }
                sets.push(out);
            }
            sets
        }
        Some(_) => return Err(ExecErr::ReplayConflict),
    };
    let rowcounts = match json::field_ci(&o, "rowcounts") {
        None | Some(J::Null) => return Err(ExecErr::LogCorrupt),
        Some(J::Array(items)) => items
            .iter()
            .map(|v| match v {
                J::Null => Ok(0),
                v => json::as_int(v).ok_or(ExecErr::ReplayConflict),
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(ExecErr::ReplayConflict),
    };
    let outputs = match json::field_ci(&o, "outputs") {
        None | Some(J::Null) => return Err(ExecErr::LogCorrupt),
        Some(J::Object(m)) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        Some(_) => return Err(ExecErr::ReplayConflict),
    };
    Ok(Stored {
        sets,
        rowcounts,
        outputs,
    })
}
