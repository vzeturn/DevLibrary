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
            positive("contract_max", f.contract_max)?;
            if let Some(ops) = &f.ops {
                if ops.len() > MAX_HELLO_OPS {
                    return Err(format!("ops: at most {MAX_HELLO_OPS} entries"));
                }
                if !ops.iter().all(|o| pat::is_op_cap(o)) {
                    return Err("ops: every entry must be name@contract_ver".into());
                }
            }
            Ok(())
        }
        Frame::Op(f) => {
            if !pat::is_uuid(&f.id) {
                return Err("id: must be a uuid".into());
            }
            str_len("op", &f.op, 1, 100)?;
            positive("contract_ver", f.contract_ver)?;
            if !crate::ops::TARGET_DBS.contains(&f.target_db.as_str()) {
                return Err("target_db: must be SMILE_FO, SMILE_POS or SMILE_BO".into());
            }
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
    }
}
