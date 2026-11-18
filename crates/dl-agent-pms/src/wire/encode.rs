//! Frame encoding: JSON, with gzip for large binary results.

use super::*;

/// One encoded WebSocket message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// The payload.
    pub data: Vec<u8>,
    /// Binary (gzip) rather than text.
    pub binary: bool,
}

/// Validates an agent -> cloud frame and encodes it: JSON text up to the gzip threshold,
/// a binary gzip of that JSON above it.
pub fn encode(frame: &Frame) -> Result<Message, WireError> {
    if frame.sent_by_cloud() {
        return Err(frame_err(format!(
            "frame type {} is not sent by the agent",
            frame.frame_type()
        )));
    }
    let js = marshal_frame(frame)?;
    if js.len() > MAX_FRAME_BYTES {
        return Err(WireError::TooLarge);
    }
    if js.len() <= GZIP_THRESHOLD_BYTES {
        return Ok(Message {
            data: js.into_bytes(),
            binary: false,
        });
    }
    Ok(Message {
        data: dl_flate::gzip::compress(js.as_bytes(), dl_flate::Level::Default),
        binary: true,
    })
}

/// The plain JSON of any frame, validated first (tests and the fake cloud use it).
pub fn marshal_frame(frame: &Frame) -> Result<String, WireError> {
    validate(frame).map_err(|e| frame_err(format!("bad {} frame: {e}", frame.frame_type())))?;
    Ok(to_json(frame))
}

pub(super) fn op_error_json(e: &OpError) -> String {
    Obj::new()
        .str("code", &e.code)
        .opt_int("sqlNumber", e.sql_number)
        .str("message", &e.message)
        .finish()
}

pub(super) fn sets_json(sets: &[Vec<Row>]) -> String {
    let mut out = String::from("[");
    for (i, set) in sets.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('[');
        for (j, row) in set.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            json::write_map(&mut out, row);
        }
        out.push(']');
    }
    out.push(']');
    out
}

pub(super) fn ints_json(v: &[i64]) -> String {
    let items: Vec<String> = v.iter().map(i64::to_string).collect();
    format!("[{}]", items.join(","))
}

/// The JSON of a frame, as Go's `json.Marshal` writes the struct (no validation).
pub fn to_json(frame: &Frame) -> String {
    let o = Obj::new().str("type", frame.frame_type());
    match frame {
        Frame::Hello(f) => o
            .int("protocol", f.protocol)
            .str("agent_version", &f.agent_version)
            .int("schema_version", f.schema_version)
            .int("contract_max", f.contract_max)
            .finish(),
        Frame::Op(f) => {
            let mut o = o
                .str("id", &f.id)
                .str("op", &f.op)
                .int("contract_ver", f.contract_ver)
                .value("args", &f.args)
                .opt_str("request_key", f.request_key.as_deref());
            if let Some(s) = &f.expected_stamp {
                o = o.str("expected_stamp", s);
            }
            o.int("deadline_ms", f.deadline_ms).finish()
        }
        Frame::Result(f) => {
            let mut outputs = String::new();
            json::write_map(&mut outputs, &f.outputs);
            let mut o = o
                .str("id", &f.id)
                .str("status", &f.status)
                .raw("sets", &sets_json(&f.sets))
                .raw("rowcounts", &ints_json(&f.rowcounts))
                .raw("outputs", &outputs)
                .raw(
                    "error",
                    &f.error.as_ref().map_or("null".into(), op_error_json),
                );
            if let Some(r) = f.replayed {
                o = o.bool("replayed", r);
            }
            o.finish()
        }
        Frame::Ping(f) => o.int("ts", f.ts).finish(),
        Frame::Pong(f) => {
            let mut o = o.int("ts", f.ts);
            if !f.agent_version.is_empty() {
                o = o.str("agent_version", &f.agent_version);
            }
            if f.schema_version != 0 {
                o = o.int("schema_version", f.schema_version);
            }
            if f.uptime_s != 0 {
                o = o.int("uptime_s", f.uptime_s);
            }
            o.finish()
        }
        Frame::Cfg(f) => o
            .int("protocol", f.protocol)
            .raw(
                "poll",
                &Obj::new().int("interval_ms", f.poll_interval_ms).finish(),
            )
            .raw(
                "limits",
                &Obj::new()
                    .int("max_frame_bytes", f.max_frame_bytes)
                    .int("gzip_threshold_bytes", f.gzip_threshold_bytes)
                    .int("inflight_cap", f.inflight_cap)
                    .int("heartbeat_ms", f.heartbeat_ms)
                    .finish(),
            )
            .opt_str("allowlist_hash", f.allowlist_hash.as_deref())
            .finish(),
        Frame::Update(f) => o
            .str("version", &f.version)
            .str("url", &f.url)
            .str("sha256", &f.sha256)
            .str("sig", &f.sig)
            .finish(),
        Frame::Migrate(f) => {
            let mut o = o
                .str("id", &f.id)
                .str("bundle_id", &f.bundle_id)
                .int("target_version", f.target_version)
                .bool("dry_run", f.dry_run);
            if f.rerun_partial != 0 {
                o = o.int("rerun_partial", f.rerun_partial);
            }
            if f.check_drift {
                o = o.bool("check_drift", true);
            }
            o.finish()
        }
        Frame::MigrateProgress(f) => o
            .str("id", &f.id)
            .str("phase", &f.phase)
            .int("seq", f.seq)
            .opt_str("name", f.name.as_deref())
            .int("duration_ms", f.duration_ms)
            .finish(),
        Frame::MigrateResult(f) => {
            let mut o = o
                .str("id", &f.id)
                .str("status", &f.status)
                .str("bundle_id", &f.bundle_id)
                .int("head", f.head)
                .int("applied", f.applied)
                .int("adopted", f.adopted)
                .int("pending", f.pending)
                .int("duration_ms", f.duration_ms);
            if !f.drift.is_empty() {
                let items: Vec<String> = f
                    .drift
                    .iter()
                    .map(|d| {
                        Obj::new()
                            .str("kind", &d.kind)
                            .str("object", &d.object)
                            .str("code", &d.code)
                            .finish()
                    })
                    .collect();
                o = o.raw("drift", &format!("[{}]", items.join(",")));
            }
            let err = f.error.as_ref().map_or("null".into(), |e| {
                Obj::new()
                    .str("code", &e.code)
                    .opt_int("seq", e.seq)
                    .opt_int("sql_number", e.sql_number.map(i64::from))
                    .opt_int("line", e.line.map(i64::from))
                    .finish()
            });
            o.raw("error", &err).finish()
        }
    }
}
