//! Frame decoding: the key specs and the strict decoders.

use super::*;

// ---- key specs -------------------------------------------------------------------

pub(super) struct KeySpec {
    pub(super) required: &'static [&'static str],
    pub(super) nullable: &'static [&'static str],
    pub(super) optional: &'static [&'static str],
}

pub(super) fn spec(typ: &str) -> Option<KeySpec> {
    let s = |required, nullable, optional| {
        Some(KeySpec {
            required,
            nullable,
            optional,
        })
    };
    match typ {
        "hello" => s(
            &[
                "type",
                "protocol",
                "agent_version",
                "schema_version",
                "contract_max",
            ],
            &[],
            &[],
        ),
        "op" => s(
            &[
                "type",
                "id",
                "op",
                "contract_ver",
                "args",
                "request_key",
                "deadline_ms",
            ],
            &["request_key"],
            &["expected_stamp"],
        ),
        "result" => s(
            &[
                "type",
                "id",
                "status",
                "sets",
                "rowcounts",
                "outputs",
                "error",
            ],
            &["error"],
            &["replayed"],
        ),
        "ping" => s(&["type", "ts"], &[], &[]),
        "pong" => s(
            &["type", "ts"],
            &[],
            &["agent_version", "schema_version", "uptime_s"],
        ),
        "cfg" => s(
            &["type", "protocol", "poll", "limits", "allowlist_hash"],
            &["allowlist_hash"],
            &[],
        ),
        "update" => s(&["type", "version", "url", "sha256", "sig"], &[], &[]),
        "migrate" => s(
            &["type", "id", "bundle_id", "target_version", "dry_run"],
            &[],
            &["rerun_partial", "check_drift"],
        ),
        "migrate_progress" => s(
            &["type", "id", "phase", "seq", "name", "duration_ms"],
            &["name"],
            &[],
        ),
        "migrate_result" => s(
            &[
                "type",
                "id",
                "status",
                "bundle_id",
                "head",
                "applied",
                "adopted",
                "pending",
                "duration_ms",
                "error",
            ],
            &["error"],
            &["drift"],
        ),
        _ => None,
    }
}

pub(super) const ERROR_SPEC: KeySpec = KeySpec {
    required: &["code", "sqlNumber", "message"],
    nullable: &["sqlNumber"],
    optional: &[],
};
pub(super) const POLL_SPEC: KeySpec = KeySpec {
    required: &["interval_ms"],
    nullable: &[],
    optional: &[],
};
pub(super) const LIMITS_SPEC: KeySpec = KeySpec {
    required: &[
        "max_frame_bytes",
        "gzip_threshold_bytes",
        "inflight_cap",
        "heartbeat_ms",
    ],
    nullable: &[],
    optional: &[],
};
pub(super) const MIG_ERR_SPEC: KeySpec = KeySpec {
    required: &["code", "seq", "sql_number", "line"],
    nullable: &["seq", "sql_number", "line"],
    optional: &[],
};

pub(super) fn check_keys(prefix: &str, obj: &Map, spec: &KeySpec) -> Result<(), WireError> {
    for (k, v) in obj.iter() {
        if !spec.required.contains(&k.as_str()) && !spec.optional.contains(&k.as_str()) {
            return Err(frame_err(format!("{prefix}unknown key {}", json::quote(k))));
        }
        if v.is_null() && !spec.nullable.contains(&k.as_str()) {
            return Err(frame_err(format!("{prefix}{k}: must not be null")));
        }
    }
    for k in spec.required {
        if !obj.contains_key(k) {
            return Err(frame_err(format!("{prefix}{k}: required")));
        }
    }
    Ok(())
}

pub(super) fn check_nested(obj: &Map, key: &str, spec: &KeySpec) -> Result<(), WireError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(()),
        Some(Value::Object(m)) => check_keys(&format!("{key}."), m, spec),
        Some(_) => Err(frame_err(format!("{key}: must be an object"))),
    }
}

// ---- decoding ----------------------------------------------------------------------

/// A Go struct decode failure (type mismatch or unknown nested field).
pub(super) enum DecodeFail {
    Type(String),
    Other,
}

impl From<TypeError> for DecodeFail {
    fn from(e: TypeError) -> DecodeFail {
        DecodeFail::Type(e.field)
    }
}

pub(super) type D<T> = Result<T, DecodeFail>;

pub(super) fn nested<'a>(obj: &'a Map, key: &str) -> D<Option<&'a Map>> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(m)) => Ok(Some(m)),
        Some(_) => Err(DecodeFail::Type(key.to_owned())),
    }
}

pub(super) fn decode_hello(o: &Map) -> D<Hello> {
    Ok(Hello {
        protocol: json::int(o, "protocol", "protocol")?,
        agent_version: json::string(o, "agent_version", "agent_version")?,
        schema_version: json::int(o, "schema_version", "schema_version")?,
        contract_max: json::int(o, "contract_max", "contract_max")?,
    })
}

pub(super) fn decode_op(o: &Map) -> D<Op> {
    Ok(Op {
        id: json::string(o, "id", "id")?,
        op: json::string(o, "op", "op")?,
        contract_ver: json::int(o, "contract_ver", "contract_ver")?,
        // Raw JSON, as in Go; validate() refuses anything but an object.
        args: o.get("args").cloned().unwrap_or(Value::Null),
        request_key: json::opt_string(o, "request_key", "request_key")?,
        expected_stamp: json::opt_string(o, "expected_stamp", "expected_stamp")?,
        deadline_ms: json::int(o, "deadline_ms", "deadline_ms")?,
    })
}

pub(super) fn decode_op_error(o: &Map) -> D<OpError> {
    Ok(OpError {
        code: json::string(o, "code", "error.code")?,
        sql_number: json::opt_int(o, "sqlNumber", "error.sqlNumber")?,
        message: json::string(o, "message", "error.message")?,
    })
}

pub(super) fn decode_result(o: &Map) -> D<OpResult> {
    let mut sets = Vec::new();
    match o.get("sets") {
        Some(Value::Array(items)) => {
            for set in items {
                match set {
                    Value::Array(rows) => {
                        let mut out = Vec::with_capacity(rows.len());
                        for row in rows {
                            match row {
                                Value::Object(m) => out
                                    .push(m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
                                Value::Null => return Err(DecodeFail::Other),
                                _ => return Err(DecodeFail::Type("sets".into())),
                            }
                        }
                        sets.push(out);
                    }
                    Value::Null => return Err(DecodeFail::Other),
                    _ => return Err(DecodeFail::Type("sets".into())),
                }
            }
        }
        _ => return Err(DecodeFail::Type("sets".into())),
    }
    let rowcounts = match o.get("rowcounts") {
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| match v {
                Value::Null => Ok(0),
                v => json::as_int(v).ok_or(DecodeFail::Type("rowcounts".into())),
            })
            .collect::<D<Vec<i64>>>()?,
        _ => return Err(DecodeFail::Type("rowcounts".into())),
    };
    let outputs = match o.get("outputs") {
        Some(Value::Object(m)) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        _ => return Err(DecodeFail::Type("outputs".into())),
    };
    Ok(OpResult {
        id: json::string(o, "id", "id")?,
        status: json::string(o, "status", "status")?,
        sets,
        rowcounts,
        outputs,
        error: nested(o, "error")?.map(decode_op_error).transpose()?,
        replayed: json::opt_bool(o, "replayed", "replayed")?,
    })
}

pub(super) fn decode_pong(o: &Map) -> D<Pong> {
    Ok(Pong {
        ts: json::int(o, "ts", "ts")?,
        agent_version: json::string(o, "agent_version", "agent_version")?,
        schema_version: json::int(o, "schema_version", "schema_version")?,
        uptime_s: json::int(o, "uptime_s", "uptime_s")?,
    })
}

pub(super) fn decode_cfg(o: &Map) -> D<Cfg> {
    let empty = Map::new();
    let poll = nested(o, "poll")?.unwrap_or(&empty);
    let limits = nested(o, "limits")?.unwrap_or(&empty);
    Ok(Cfg {
        protocol: json::int(o, "protocol", "protocol")?,
        poll_interval_ms: json::int(poll, "interval_ms", "poll.interval_ms")?,
        max_frame_bytes: json::int(limits, "max_frame_bytes", "limits.max_frame_bytes")?,
        gzip_threshold_bytes: json::int(
            limits,
            "gzip_threshold_bytes",
            "limits.gzip_threshold_bytes",
        )?,
        inflight_cap: json::int(limits, "inflight_cap", "limits.inflight_cap")?,
        heartbeat_ms: json::int(limits, "heartbeat_ms", "limits.heartbeat_ms")?,
        allowlist_hash: json::opt_string(o, "allowlist_hash", "allowlist_hash")?,
    })
}

pub(super) fn decode_update(o: &Map) -> D<Update> {
    Ok(Update {
        version: json::string(o, "version", "version")?,
        url: json::string(o, "url", "url")?,
        sha256: json::string(o, "sha256", "sha256")?,
        sig: json::string(o, "sig", "sig")?,
    })
}

pub(super) fn decode_migrate(o: &Map) -> D<Migrate> {
    Ok(Migrate {
        id: json::string(o, "id", "id")?,
        bundle_id: json::string(o, "bundle_id", "bundle_id")?,
        target_version: json::int(o, "target_version", "target_version")?,
        dry_run: json::boolean(o, "dry_run", "dry_run")?,
        rerun_partial: json::int(o, "rerun_partial", "rerun_partial")?,
        check_drift: json::boolean(o, "check_drift", "check_drift")?,
    })
}

pub(super) fn decode_progress(o: &Map) -> D<MigrateProgress> {
    Ok(MigrateProgress {
        id: json::string(o, "id", "id")?,
        phase: json::string(o, "phase", "phase")?,
        seq: json::int(o, "seq", "seq")?,
        name: json::opt_string(o, "name", "name")?,
        duration_ms: json::int(o, "duration_ms", "duration_ms")?,
    })
}

/// A Go struct field of `DriftFinding`: case-insensitive key match, unknown keys refused.
pub(super) fn decode_drift(v: &Value) -> D<DriftFinding> {
    let o = match v {
        Value::Object(m) => m,
        Value::Null => {
            return Ok(DriftFinding {
                kind: String::new(),
                object: String::new(),
                code: String::new(),
            });
        }
        _ => return Err(DecodeFail::Type("drift".into())),
    };
    for (k, _) in o.iter() {
        if !["kind", "object", "code"]
            .iter()
            .any(|f| k.eq_ignore_ascii_case(f))
        {
            return Err(DecodeFail::Other);
        }
    }
    let s = |name: &str| -> D<String> {
        match json::field_ci(o, name) {
            None | Some(Value::Null) => Ok(String::new()),
            Some(Value::String(s)) => Ok(s.clone()),
            Some(_) => Err(DecodeFail::Type(format!("drift.{name}"))),
        }
    };
    Ok(DriftFinding {
        kind: s("kind")?,
        object: s("object")?,
        code: s("code")?,
    })
}

pub(super) fn decode_migrate_result(o: &Map) -> D<MigrateResult> {
    let drift = match o.get("drift") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.iter().map(decode_drift).collect::<D<Vec<_>>>()?,
        Some(_) => return Err(DecodeFail::Type("drift".into())),
    };
    let error = match nested(o, "error")? {
        None => None,
        Some(e) => Some(MigrateError {
            code: json::string(e, "code", "error.code")?,
            seq: json::opt_int(e, "seq", "error.seq")?,
            sql_number: json::opt_i32(e, "sql_number", "error.sql_number")?,
            line: json::opt_i32(e, "line", "error.line")?,
        }),
    };
    Ok(MigrateResult {
        id: json::string(o, "id", "id")?,
        status: json::string(o, "status", "status")?,
        bundle_id: json::string(o, "bundle_id", "bundle_id")?,
        head: json::int(o, "head", "head")?,
        applied: json::int(o, "applied", "applied")?,
        adopted: json::int(o, "adopted", "adopted")?,
        pending: json::int(o, "pending", "pending")?,
        duration_ms: json::int(o, "duration_ms", "duration_ms")?,
        drift,
        error,
    })
}

/// Parses and validates one frame of any type.
pub fn decode_json(data: &[u8]) -> Result<Frame, WireError> {
    let Some(obj) = json::parse_object(data) else {
        return Err(frame_err("frame is not a JSON object"));
    };
    let typ = match obj.get("type") {
        Some(Value::String(s)) => s.clone(),
        _ => return Err(frame_err("type: must be a string")),
    };
    let Some(sp) = spec(&typ) else {
        return Err(frame_err("unknown frame type"));
    };
    check_keys("", &obj, &sp)?;
    match typ.as_str() {
        "result" => check_nested(&obj, "error", &ERROR_SPEC)?,
        "cfg" => {
            check_nested(&obj, "poll", &POLL_SPEC)?;
            check_nested(&obj, "limits", &LIMITS_SPEC)?;
        }
        "migrate_result" => check_nested(&obj, "error", &MIG_ERR_SPEC)?,
        _ => {}
    }
    let decoded: D<Frame> = match typ.as_str() {
        "hello" => decode_hello(&obj).map(Frame::Hello),
        "op" => decode_op(&obj).map(Frame::Op),
        "result" => decode_result(&obj).map(Frame::Result),
        "ping" => json::int(&obj, "ts", "ts")
            .map(|ts| Frame::Ping(Ping { ts }))
            .map_err(DecodeFail::from),
        "pong" => decode_pong(&obj).map(Frame::Pong),
        "cfg" => decode_cfg(&obj).map(Frame::Cfg),
        "update" => decode_update(&obj).map(Frame::Update),
        "migrate" => decode_migrate(&obj).map(Frame::Migrate),
        "migrate_progress" => decode_progress(&obj).map(Frame::MigrateProgress),
        _ => decode_migrate_result(&obj).map(Frame::MigrateResult),
    };
    let frame = match decoded {
        Ok(f) => f,
        Err(DecodeFail::Type(field)) => {
            return Err(frame_err(format!("bad {typ} frame at {field}")));
        }
        Err(DecodeFail::Other) => {
            // Go reaches validate() with the zero value here and reports its rule.
            return Err(frame_err(format!(
                "bad {typ} frame: {}",
                zero_value_rule(&typ)
            )));
        }
    };
    validate(&frame).map_err(|e| frame_err(format!("bad {typ} frame: {e}")))?;
    Ok(frame)
}

pub(super) fn zero_value_rule(typ: &str) -> &'static str {
    match typ {
        "result" => "sets: every set must be an array of objects",
        _ => "unknown field",
    }
}

/// Decodes one cloud -> agent message (text JSON or binary gzip JSON).
pub fn decode_cloud_frame(data: &[u8], binary: bool) -> Result<Frame, WireError> {
    if data.len() > MAX_FRAME_BYTES {
        return Err(WireError::TooLarge);
    }
    let plain;
    let data = if binary {
        plain = match dl_flate::gzip::decompress(data, MAX_FRAME_BYTES) {
            Ok(p) => p,
            Err(e) if matches!(e.kind(), dl_flate::ErrorKind::TooLarge { .. }) => {
                return Err(WireError::TooLarge);
            }
            Err(_) => return Err(frame_err("bad gzip frame")),
        };
        &plain[..]
    } else {
        data
    };
    let frame = decode_json(data)?;
    if !frame.sent_by_cloud() {
        return Err(frame_err(format!(
            "frame type {} is not sent by the cloud",
            frame.frame_type()
        )));
    }
    Ok(frame)
}
