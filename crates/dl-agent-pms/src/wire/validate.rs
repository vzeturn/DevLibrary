//! Per-frame validation rules, shared by the decoder and the encoder.

use super::*;

// ---- validation ------------------------------------------------------------------

pub(super) fn str_len(field: &str, v: &str, lo: usize, hi: usize) -> Result<(), String> {
    let n = json::rune_count(v);
    if n < lo || n > hi {
        return Err(format!("{field}: length must be {lo}..{hi}"));
    }
    Ok(())
}

pub(super) fn positive(field: &str, v: i64) -> Result<(), String> {
    if v <= 0 {
        return Err(format!("{field}: must be a positive integer"));
    }
    Ok(())
}

pub(super) fn non_negative(field: &str, v: i64) -> Result<(), String> {
    if v < 0 {
        return Err(format!("{field}: must be a non-negative integer"));
    }
    Ok(())
}

/// Go's `url.Parse(u)` succeeding with a host, for an `https://` URL.
pub(super) fn https_url_ok(u: &str) -> bool {
    let Some(rest) = u.strip_prefix("https://") else {
        return false;
    };
    if u.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return false;
    }
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let host_port = match authority.rfind('@') {
        Some(i) => &authority[i + 1..],
        None => authority,
    };
    let host = if let Some(stripped) = host_port.strip_prefix('[') {
        match stripped.find(']') {
            Some(i) => &stripped[..i],
            None => return false,
        }
    } else {
        match host_port.rfind(':') {
            Some(i) => {
                let port = &host_port[i + 1..];
                if !port.bytes().all(|b| b.is_ascii_digit()) {
                    return false;
                }
                &host_port[..i]
            }
            None => host_port,
        }
    };
    !host.is_empty() && !host.contains(' ')
}

pub(super) fn validate(frame: &Frame) -> Result<(), String> {
    match frame {
        Frame::Hello(f) => {
            positive("protocol", f.protocol)?;
            str_len("agent_version", &f.agent_version, 1, 32)?;
            non_negative("schema_version", f.schema_version)?;
            positive("contract_max", f.contract_max)
        }
        Frame::Op(f) => {
            if !pat::is_uuid(&f.id) {
                return Err("id: must be a uuid".into());
            }
            str_len("op", &f.op, 1, 100)?;
            positive("contract_ver", f.contract_ver)?;
            if !f.args.is_object() {
                return Err("args: must be an object".into());
            }
            if let Some(k) = &f.request_key {
                str_len("request_key", k, 1, 100)?;
            }
            if let Some(s) = &f.expected_stamp {
                str_len("expected_stamp", s, 1, 100)?;
            }
            positive("deadline_ms", f.deadline_ms)
        }
        Frame::Result(f) => {
            if !pat::is_uuid(&f.id) {
                return Err("id: must be a uuid".into());
            }
            if ![STATUS_APPLIED, STATUS_REJECTED, STATUS_FAILED].contains(&f.status.as_str()) {
                return Err("status: unknown value".into());
            }
            for (i, n) in f.rowcounts.iter().enumerate() {
                if *n < 0 {
                    return Err(format!("rowcounts.{i}: must be non-negative"));
                }
            }
            let applied = f.status == STATUS_APPLIED;
            match (&f.error, applied) {
                (Some(_), true) => return Err("error: applied carries no error".into()),
                (None, false) => return Err(format!("error: {} needs an error", f.status)),
                _ => {}
            }
            if let Some(e) = &f.error {
                str_len("error.code", &e.code, 1, 64)?;
                str_len("error.message", &e.message, 0, 4000)?;
            }
            Ok(())
        }
        Frame::Ping(f) => non_negative("ts", f.ts),
        Frame::Pong(f) => {
            str_len("agent_version", &f.agent_version, 0, 32)?;
            non_negative("ts", f.ts)?;
            non_negative("schema_version", f.schema_version)?;
            non_negative("uptime_s", f.uptime_s)
        }
        Frame::Cfg(f) => {
            if f.protocol != PROTOCOL_VERSION {
                return Err(format!("protocol: must be {PROTOCOL_VERSION}"));
            }
            positive("poll.interval_ms", f.poll_interval_ms)?;
            positive("limits.max_frame_bytes", f.max_frame_bytes)?;
            positive("limits.gzip_threshold_bytes", f.gzip_threshold_bytes)?;
            positive("limits.inflight_cap", f.inflight_cap)?;
            positive("limits.heartbeat_ms", f.heartbeat_ms)?;
            if let Some(h) = &f.allowlist_hash
                && !pat::is_hex64(h)
            {
                return Err("allowlist_hash: must be 64 lowercase hex characters".into());
            }
            Ok(())
        }
        Frame::Update(f) => {
            str_len("version", &f.version, 1, 32)?;
            if !https_url_ok(&f.url) {
                return Err("url: must be an https url".into());
            }
            if !pat::is_hex64(&f.sha256) {
                return Err("sha256: must be 64 lowercase hex characters".into());
            }
            if !pat::is_sig_b64(&f.sig) {
                return Err("sig: must be a base64 Ed25519 signature".into());
            }
            Ok(())
        }
        Frame::Migrate(f) => {
            if !pat::is_uuid(&f.id) {
                return Err("id: must be a UUID".into());
            }
            if !pat::is_bundle_id(&f.bundle_id) {
                return Err("bundle_id: bad shape".into());
            }
            positive("target_version", f.target_version)?;
            non_negative("rerun_partial", f.rerun_partial)
        }
        Frame::MigrateProgress(f) => {
            if !pat::is_uuid(&f.id) {
                return Err("id: must be a UUID".into());
            }
            if !["verified", "adopt", "backup", "start", "applied", "failed"]
                .contains(&f.phase.as_str())
            {
                return Err("phase: unknown".into());
            }
            if let Some(n) = &f.name
                && pat::migration_file(n).is_none()
            {
                return Err("name: must be a migration file name".into());
            }
            non_negative("seq", f.seq)?;
            non_negative("duration_ms", f.duration_ms)
        }
        Frame::MigrateResult(f) => validate_migrate_result(f),
    }
}

pub(super) fn validate_migrate_result(f: &MigrateResult) -> Result<(), String> {
    if !pat::is_uuid(&f.id) {
        return Err("id: must be a UUID".into());
    }
    if ![
        MIGRATE_APPLIED,
        MIGRATE_DRY_RUN,
        MIGRATE_REFUSED,
        MIGRATE_FAILED,
    ]
    .contains(&f.status.as_str())
    {
        return Err("status: unknown".into());
    }
    if !pat::is_bundle_id(&f.bundle_id) {
        return Err("bundle_id: bad shape".into());
    }
    for (name, v) in [
        ("head", f.head),
        ("applied", f.applied),
        ("adopted", f.adopted),
        ("pending", f.pending),
        ("duration_ms", f.duration_ms),
    ] {
        non_negative(name, v)?;
    }
    if f.drift.len() > 500 {
        return Err("drift: at most 500 findings".into());
    }
    for d in &f.drift {
        if !["ledger", "proc", "table"].contains(&d.kind.as_str()) || !pat::is_migrate_code(&d.code)
        {
            return Err("drift: bad finding".into());
        }
        str_len("drift.object", &d.object, 1, 128)?;
    }
    let ok = f.status == MIGRATE_APPLIED || f.status == MIGRATE_DRY_RUN;
    match (&f.error, ok) {
        (Some(_), true) => return Err(format!("error: {} carries no error", f.status)),
        (None, false) => return Err(format!("error: {} needs an error", f.status)),
        _ => {}
    }
    if let Some(e) = &f.error {
        if !pat::is_migrate_code(&e.code) {
            return Err("error.code: bad shape".into());
        }
        if e.seq.is_some_and(|s| s < 0) {
            return Err("error.seq: must be a non-negative integer".into());
        }
    }
    Ok(())
}
