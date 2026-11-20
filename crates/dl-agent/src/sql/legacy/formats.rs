//! How each older agent stores its SQL connection. Every reader cites the product source it
//! mirrors; a missing file is silence, an unreadable one a warning (never its content).

use std::path::{Path, PathBuf};

use dl_json::Value;

use super::{Unseal, open};

type Out = Vec<(PathBuf, String)>;

/// A file's text: `None` when it does not exist; any other failure is a warning.
fn text(file: &Path, warnings: &mut Vec<String>) -> Option<String> {
    bytes(file, warnings).map(|b| {
        String::from_utf8_lossy(&b)
            .trim_start_matches('\u{feff}')
            .to_string()
    })
}

fn bytes(file: &Path, warnings: &mut Vec<String>) -> Option<Vec<u8>> {
    match std::fs::read(file) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            warnings.push(format!("{}: not readable: {e}", file.display()));
            None
        }
    }
}

/// JSON with `//` and `/* */` comments and trailing commas (.NET appsettings files).
pub(super) fn parse_jsonc(s: &str) -> Result<Value, String> {
    let mut out = String::with_capacity(s.len());
    let b: Vec<char> = s.chars().collect();
    let (mut i, mut in_str) = (0, false);
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < b.len() {
                out.push(b[i + 1]);
                i += 1;
            } else if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
            out.push(c);
        } else if c == '/' && b.get(i + 1) == Some(&'/') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
            continue;
        } else if c == '/' && b.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            continue;
        } else if c == ',' {
            let next = b[i + 1..].iter().find(|c| !c.is_whitespace());
            if !matches!(next, Some('}' | ']')) {
                out.push(c);
            }
        } else {
            out.push(c);
        }
        i += 1;
    }
    dl_json::parse(&out).map_err(|e| e.to_string())
}

fn json_file(file: &Path, warnings: &mut Vec<String>) -> Option<Value> {
    let t = text(file, warnings)?;
    match parse_jsonc(&t) {
        Ok(v) => Some(v),
        Err(e) => {
            warnings.push(format!("{}: not valid JSON: {e}", file.display()));
            None
        }
    }
}

/// Every non-empty string under a key named one of `names` (any depth, any case).
pub(super) fn strings(v: &Value, names: &[&str], out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, child) in m.iter() {
                if names.iter().any(|n| k.eq_ignore_ascii_case(n))
                    && let Some(s) = child.as_str().map(str::trim).filter(|s| !s.is_empty())
                    && !out.iter().any(|o| o == s)
                {
                    out.push(s.to_string());
                }
                strings(child, names, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|c| strings(c, names, out)),
        _ => {}
    }
}

fn from_json_files(dir: &Path, files: &[&str], keys: &[&str], warnings: &mut Vec<String>) -> Out {
    let mut out = Out::new();
    for f in files {
        let path = dir.join(f);
        if let Some(v) = json_file(&path, warnings) {
            let mut found = Vec::new();
            strings(&v, keys, &mut found);
            out.extend(found.into_iter().map(|c| (path.clone(), c)));
        }
    }
    out
}

/// An ADO value, quoted when it needs to be.
fn ado(v: &str) -> String {
    format!("\"{}\"", v.replace('"', "\"\""))
}

/// Check-In: `appsettings.Production.json` (over `appsettings.json`), `Agent:PmsConnectionString`
/// (SmileCheckin deploy/agent/Install.ps1; dl-agent-arrival `agent::settings`).
pub(super) fn checkin(dir: &Path, _: &dyn Unseal, w: &mut Vec<String>) -> Out {
    from_json_files(
        dir,
        &["appsettings.Production.json", "appsettings.json"],
        &["PmsConnectionString"],
        w,
    )
}

/// Membership: `config.dpapi`, a raw DPAPI LocalMachine blob (entropy `MM.Membership.Agent.v1`)
/// of `{"SmileConnectionString": …}`; before enrolment, `appsettings*.json`
/// `Agent:SmileConnectionString` or `ConnectionStrings:SmileFo` (MM-MEMBERSHIP
/// AgentSecureConfig.cs; dl-agent-membership `config`, `enroll::legacy`).
pub(super) fn membership(dir: &Path, unseal: &dyn Unseal, w: &mut Vec<String>) -> Out {
    let mut out = Out::new();
    let sealed = dir.join("config.dpapi");
    if let Some(blob) = bytes(&sealed, w)
        && let Some(plain) = open(unseal, &blob, b"MM.Membership.Agent.v1", &sealed, w)
    {
        match parse_jsonc(&String::from_utf8_lossy(&plain)) {
            Ok(v) => {
                let mut found = Vec::new();
                strings(&v, &["SmileConnectionString"], &mut found);
                out.extend(found.into_iter().map(|c| (sealed.clone(), c)));
            }
            Err(e) => w.push(format!(
                "{}: decrypted, but not JSON: {e}",
                sealed.display()
            )),
        }
    }
    out.extend(from_json_files(
        dir,
        &[
            "appsettings.hotel.json",
            "appsettings.Production.json",
            "appsettings.json",
        ],
        &["SmileConnectionString", "SmileFo"],
        w,
    ));
    out
}

/// PDA SyncAgent: `agent.json` (`sqlInstance`, `sqlDatabase`) and the fixed `pda_sync` login,
/// whose password is `secrets/pda-sync-sql-password.bin`, DPAPI LocalMachine with entropy
/// `MM.PdaOrder.SyncAgent.v1` (SML-PDAOrder apps/sql-sync-agent; dl-agent-pda-sync
/// `config::sql`, `security`).
pub(super) fn pda_sync(dir: &Path, unseal: &dyn Unseal, w: &mut Vec<String>) -> Out {
    let file = dir.join("agent.json");
    let Some(v) = json_file(&file, w) else {
        return Out::new();
    };
    let s = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let Some(database) = s("sqlDatabase") else {
        w.push(format!(
            "{}: no sqlDatabase (never enrolled?)",
            file.display()
        ));
        return Out::new();
    };
    let instance = s("sqlInstance").unwrap_or(".");
    let pw_file = dir.join("secrets").join("pda-sync-sql-password.bin");
    let Some(blob) = bytes(&pw_file, w) else {
        w.push(format!(
            "{}: missing, the pda_sync password is unknown",
            pw_file.display()
        ));
        return Out::new();
    };
    let Some(pw) = open(unseal, &blob, b"MM.PdaOrder.SyncAgent.v1", &pw_file, w) else {
        return Out::new();
    };
    let conn = format!(
        "Server={};Database={};User ID=pda_sync;Password={}",
        ado(instance),
        ado(database),
        ado(&String::from_utf8_lossy(&pw))
    );
    vec![(pw_file, conn)]
}

/// SMILE-PMS site agent: `state/databases.json`, `{"SMILE_FO": "<connection>", …}` (its
/// `config.json` `state_dir` when set) (SMILE-PMS apps/site-agent; dl-agent-pms `db`).
pub(super) fn pms(dir: &Path, _: &dyn Unseal, w: &mut Vec<String>) -> Out {
    let mut files = vec![dir.join("state").join("databases.json")];
    if let Some(cfg) = json_file(&dir.join("config.json"), w)
        && let Some(state) = cfg
            .get("state_dir")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    {
        files.push(Path::new(state).join("databases.json"));
    }
    let mut out = Out::new();
    for f in files {
        if out.iter().any(|(p, _)| p == &f) {
            continue;
        }
        let Some(Value::Object(m)) = json_file(&f, w) else {
            continue;
        };
        for (db, conn) in m.iter() {
            if db.to_ascii_uppercase().starts_with("SMILE_")
                && let Some(c) = conn.as_str().filter(|c| !c.trim().is_empty())
            {
                out.push((f.clone(), c.trim().to_string()));
            }
        }
    }
    out
}

/// A HotelLink secret: `enc:` + base64 of a DPAPI LocalMachine blob, or plain text.
fn hl_secret(v: &str, unseal: &dyn Unseal, file: &Path, w: &mut Vec<String>) -> Option<String> {
    let Some(b64) = v.strip_prefix("enc:") else {
        return Some(v.to_string());
    };
    let blob = match dl_encoding::base64::STANDARD.decode(b64.trim()) {
        Ok(b) => b,
        Err(e) => {
            w.push(format!(
                "{}: a sealed value is not base64: {e}",
                file.display()
            ));
            return None;
        }
    };
    open(unseal, &blob, b"", file, w).map(|p| String::from_utf8_lossy(&p).into_owned())
}

/// HotelLink: `config.json` `sql` {`server`, `database`, `trustedConnection`, `user`,
/// `password`, `connectionStringOverride`}, secrets `enc:`+DPAPI LocalMachine (SMILE-HLAPI
/// go/internal/agent config; dl-agent-hotellink `config`).
pub(super) fn hotellink(dir: &Path, unseal: &dyn Unseal, w: &mut Vec<String>) -> Out {
    let file = dir.join("config.json");
    let Some(v) = json_file(&file, w) else {
        return Out::new();
    };
    let Some(sql) = v.get("sql") else {
        w.push(format!("{}: no sql section", file.display()));
        return Out::new();
    };
    let s = |k: &str| {
        sql.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let over = s("connectionStringOverride");
    if !over.is_empty() {
        return hl_secret(&over, unseal, &file, w)
            .map(|c| vec![(file.clone(), c)])
            .unwrap_or_default();
    }
    let server = s("server");
    if server.is_empty() {
        return Out::new();
    }
    let trusted = sql.get("trustedConnection").and_then(Value::as_bool) == Some(true);
    let login = if trusted {
        "Integrated Security=SSPI".to_string()
    } else {
        let Some(pw) = hl_secret(&s("password"), unseal, &file, w) else {
            return Out::new();
        };
        format!("User ID={};Password={}", ado(&s("user")), ado(&pw))
    };
    vec![(file, format!("Server={};{login}", ado(&server)))]
}

/// E-Invoice: `config.enc`, the whole file DPAPI LocalMachine of `BE32(len) || JSON` with
/// `sql_dsn` (FO) and `pos_sql_dsn` as go-mssqldb URLs (MM-EINVOICE cmd/agent;
/// dl-agent-einvoice `agent_config`).
pub(super) fn einvoice(dir: &Path, unseal: &dyn Unseal, w: &mut Vec<String>) -> Out {
    let file = dir.join("config.enc");
    let Some(blob) = bytes(&file, w) else {
        return Out::new();
    };
    let Some(plain) = open(unseal, &blob, b"", &file, w) else {
        return Out::new();
    };
    let json = match plain.split_at_checked(4) {
        Some((len, rest))
            if u32::from_be_bytes([len[0], len[1], len[2], len[3]]) as usize == rest.len() =>
        {
            rest
        }
        _ => {
            w.push(format!(
                "{}: decrypted, but its frame is wrong",
                file.display()
            ));
            return Out::new();
        }
    };
    match parse_jsonc(&String::from_utf8_lossy(json)) {
        Ok(v) => {
            let mut found = Vec::new();
            strings(&v, &["sql_dsn", "pos_sql_dsn"], &mut found);
            found.into_iter().map(|c| (file.clone(), c)).collect()
        }
        Err(e) => {
            w.push(format!("{}: decrypted, but not JSON: {e}", file.display()));
            Out::new()
        }
    }
}

/// SRBackup: `agent.json` holds the hub URL and keys; its SQL servers and passwords come from
/// the SRBackup hub (SRBackup agent/scripts/install.ps1; dl-agent-backup `enroll::goagent`).
/// A connection key, should a build ever keep one, is still read.
pub(super) fn srbackup(dir: &Path, _: &dyn Unseal, w: &mut Vec<String>) -> Out {
    let file = dir.join("agent.json");
    let Some(v) = json_file(&file, w) else {
        return Out::new();
    };
    let mut found = Vec::new();
    strings(
        &v,
        &["connection_string", "connectionString", "sql_dsn"],
        &mut found,
    );
    if found.is_empty() {
        w.push(format!(
            "{}: the SRBackup agent keeps no SQL connection on this machine (its hub holds them)",
            file.display()
        ));
    }
    found.into_iter().map(|c| (file.clone(), c)).collect()
}
