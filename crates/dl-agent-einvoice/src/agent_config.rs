//! The sealed local configuration `config.enc` (a port of `config.go` and
//! `internal/kernel/agentcrypto`).
//!
//! On Windows the file is the DPAPI machine-scope blob of `BE32(len) || JSON`, exactly as
//! the Go agent writes it, so either agent reads the other's file. Field names, order and
//! encodings (durations as nanosecond integers, `omitempty`) follow the Go struct, so a
//! file saved by this agent stays loadable by the Go agent.

use std::path::{Path, PathBuf};
use std::time::Duration;

use dl_json::Value;
use dl_time::OffsetDateTime;
use dl_uuid::Uuid;

use crate::config::Rules;
use crate::gojson::{self, Obj, array};
use crate::sql::SpNames;

const CONFIG_FILE: &str = "config.enc";

/// `AgentConfig`.
#[derive(Debug, Clone, PartialEq)]
#[allow(missing_docs)] // fields mirror the Go struct one for one; see its doc
pub struct AgentConfig {
    pub platform_base_url: String,
    pub api_key: String,
    pub device_id: Uuid,
    pub sql_dsn: String,
    pub pos_sql_dsn: String,
    pub select_sp: String,
    pub header_sp_fo: String,
    pub header_sp_pos: String,
    pub details_sp_fo: String,
    pub details_sp_pos: String,
    pub custom_sp_fo: String,
    pub mark_sp: String,
    pub writeback_sp: String,
    pub select_fo_sp: String,
    pub fetch_fo_sp: String,
    pub select_pos_sp: String,
    pub fetch_pos_sp: String,
    pub mark_collected_sp: String,
    pub list_trn_types_sp: String,
    pub probe_fo_schema_sp: String,
    /// `None` is a nil Go slice.
    pub fo_charge_codes: Option<Vec<String>>,
    pub fo_exempt_codes: Vec<String>,
    pub poll_interval: Duration,
    pub batch_size: i64,
    pub cycle_timeout: Duration,
    pub collect_from: Option<OffsetDateTime>,
    pub collect_sources: String,
    pub recalc_tax_into_sc: bool,
    pub standard_service_charge_rate: f64,
    pub outbox_path: String,
    /// Not persisted: where config.enc lives.
    pub config_dir: PathBuf,
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

impl Default for AgentConfig {
    /// `Defaults()`.
    fn default() -> AgentConfig {
        AgentConfig {
            platform_base_url: String::new(),
            api_key: String::new(),
            device_id: Uuid::NIL,
            sql_dsn: String::new(),
            pos_sql_dsn: String::new(),
            select_sp: "sp_GetPendingInvoices".into(),
            header_sp_fo: "sp_getbillheader_FO".into(),
            header_sp_pos: "sp_getbillheader_POS".into(),
            details_sp_fo: "sp_getbilldetails_FO".into(),
            details_sp_pos: "sp_getbilldetails_POS".into(),
            custom_sp_fo: "sp_getbillcustomdata_FO".into(),
            mark_sp: "sp_MarkInvoiceProcessed".into(),
            writeback_sp: "sp_ApplyInvoiceWriteback".into(),
            select_fo_sp: "sp_SelectPendingFO".into(),
            fetch_fo_sp: "sp_FetchFO".into(),
            select_pos_sp: "sp_SelectPendingPOS".into(),
            fetch_pos_sp: "sp_FetchPOS".into(),
            mark_collected_sp: "sp_MarkCollected".into(),
            list_trn_types_sp: "sp_ListTrnTypes".into(),
            probe_fo_schema_sp: "sp_ProbeFOSchema".into(),
            fo_charge_codes: Some(strings(&[
                "110", "150", "160", "170", "200", "201", "210", "211", "220", "221", "230", "240",
            ])),
            fo_exempt_codes: Vec::new(),
            poll_interval: Duration::from_secs(30),
            batch_size: 50,
            cycle_timeout: Duration::from_secs(120),
            collect_from: None,
            collect_sources: String::new(),
            recalc_tax_into_sc: false,
            standard_service_charge_rate: 0.05,
            outbox_path: String::new(),
            config_dir: PathBuf::new(),
        }
    }
}

fn dur_ns(d: Duration) -> i64 {
    i64::try_from(d.as_nanos()).unwrap_or(i64::MAX)
}

fn ns_dur(v: i64) -> Duration {
    Duration::from_nanos(u64::try_from(v).unwrap_or(0))
}

impl AgentConfig {
    /// The money rules the collection pipeline applies.
    pub fn rules(&self) -> Rules {
        Rules {
            recalc_tax_into_sc: self.recalc_tax_into_sc,
            fo_exempt_codes: self.fo_exempt_codes.clone(),
            standard_service_charge_rate: self.standard_service_charge_rate,
        }
    }

    /// The SP names the SQL layer uses.
    pub fn sp_names(&self) -> SpNames {
        SpNames {
            select_fo: self.select_fo_sp.clone(),
            fetch_fo: self.fetch_fo_sp.clone(),
            select_pos: self.select_pos_sp.clone(),
            fetch_pos: self.fetch_pos_sp.clone(),
            mark_collected: self.mark_collected_sp.clone(),
            list_trn_types: self.list_trn_types_sp.clone(),
            probe_fo_schema: self.probe_fo_schema_sp.clone(),
            writeback: self.writeback_sp.clone(),
        }
    }

    /// The charge codes the drift check compares against.
    pub fn charge_codes(&self) -> Vec<String> {
        self.fo_charge_codes.clone().unwrap_or_default()
    }

    /// `json.Marshal(AgentConfig)`.
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        let mut o = Obj::new(&mut out);
        o.str("platform_base_url", &self.platform_base_url)
            .str("api_key", &self.api_key)
            .str("device_id", &self.device_id.to_string())
            .str("sql_dsn", &self.sql_dsn)
            .str("pos_sql_dsn", &self.pos_sql_dsn)
            .str("select_sp", &self.select_sp)
            .str("header_sp_fo", &self.header_sp_fo)
            .str("header_sp_pos", &self.header_sp_pos)
            .str("details_sp_fo", &self.details_sp_fo)
            .str("details_sp_pos", &self.details_sp_pos)
            .str("custom_sp_fo", &self.custom_sp_fo)
            .str("mark_sp", &self.mark_sp)
            .str("writeback_sp", &self.writeback_sp)
            .str("select_fo_sp", &self.select_fo_sp)
            .str("fetch_fo_sp", &self.fetch_fo_sp)
            .str("select_pos_sp", &self.select_pos_sp)
            .str("fetch_pos_sp", &self.fetch_pos_sp)
            .str("mark_collected_sp", &self.mark_collected_sp)
            .str_omit("list_trn_types_sp", &self.list_trn_types_sp)
            .str_omit("probe_fo_schema_sp", &self.probe_fo_schema_sp)
            .raw("fo_charge_codes", |o| {
                array(o, self.fo_charge_codes.as_deref(), |o, s| {
                    gojson::string(o, s)
                })
            });
        if !self.fo_exempt_codes.is_empty() {
            o.raw("fo_exempt_codes", |o| {
                array(o, Some(&self.fo_exempt_codes), |o, s| gojson::string(o, s))
            });
        }
        o.int("poll_interval", dur_ns(self.poll_interval))
            .int("batch_size", self.batch_size)
            .int_omit("cycle_timeout", dur_ns(self.cycle_timeout));
        if let Some(t) = self.collect_from {
            o.str("collect_from", &gojson::rfc3339_nano_offset(t));
        }
        o.str_omit("collect_sources", &self.collect_sources)
            .bool("recalc_tax_into_sc", self.recalc_tax_into_sc)
            .float_omit(
                "standard_service_charge_rate",
                self.standard_service_charge_rate,
            )
            .str("outbox_path", &self.outbox_path)
            .end();
        out
    }

    /// Parses the JSON of config.enc. Unknown fields are ignored and missing ones keep
    /// their zero value, as `json.Unmarshal` does; then `applyDefaultsForZero` runs.
    pub fn from_json(text: &str, dir: &Path) -> Result<AgentConfig, String> {
        let v = dl_json::parse(text).map_err(|e| format!("agent: parse config: {e}"))?;
        let s = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let list = |k: &str| -> Option<Vec<String>> {
            v.get(k)?.as_array().map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
        };
        let device_id = match v.get("device_id").and_then(Value::as_str) {
            Some(t) => t
                .parse()
                .map_err(|e| format!("agent: parse config: device_id: {e:?}"))?,
            None => Uuid::NIL,
        };
        let collect_from = match v.get("collect_from").and_then(Value::as_str) {
            Some(t) => Some(
                OffsetDateTime::parse_rfc3339(t)
                    .map_err(|e| format!("agent: parse config: collect_from: {e}"))?,
            ),
            None => None,
        };
        let mut c = AgentConfig {
            platform_base_url: s("platform_base_url"),
            api_key: s("api_key"),
            device_id,
            sql_dsn: s("sql_dsn"),
            pos_sql_dsn: s("pos_sql_dsn"),
            select_sp: s("select_sp"),
            header_sp_fo: s("header_sp_fo"),
            header_sp_pos: s("header_sp_pos"),
            details_sp_fo: s("details_sp_fo"),
            details_sp_pos: s("details_sp_pos"),
            custom_sp_fo: s("custom_sp_fo"),
            mark_sp: s("mark_sp"),
            writeback_sp: s("writeback_sp"),
            select_fo_sp: s("select_fo_sp"),
            fetch_fo_sp: s("fetch_fo_sp"),
            select_pos_sp: s("select_pos_sp"),
            fetch_pos_sp: s("fetch_pos_sp"),
            mark_collected_sp: s("mark_collected_sp"),
            list_trn_types_sp: s("list_trn_types_sp"),
            probe_fo_schema_sp: s("probe_fo_schema_sp"),
            fo_charge_codes: list("fo_charge_codes"),
            fo_exempt_codes: list("fo_exempt_codes").unwrap_or_default(),
            poll_interval: ns_dur(v.get("poll_interval").and_then(Value::as_i64).unwrap_or(0)),
            batch_size: v.get("batch_size").and_then(Value::as_i64).unwrap_or(0),
            cycle_timeout: ns_dur(v.get("cycle_timeout").and_then(Value::as_i64).unwrap_or(0)),
            collect_from,
            collect_sources: s("collect_sources"),
            recalc_tax_into_sc: v
                .get("recalc_tax_into_sc")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            standard_service_charge_rate: v
                .get("standard_service_charge_rate")
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
            outbox_path: s("outbox_path"),
            config_dir: dir.to_owned(),
        };
        c.apply_defaults_for_zero();
        Ok(c)
    }

    /// `applyDefaultsForZero`.
    pub fn apply_defaults_for_zero(&mut self) {
        let d = AgentConfig::default();
        for (dst, def) in [
            (&mut self.select_sp, d.select_sp),
            (&mut self.header_sp_fo, d.header_sp_fo),
            (&mut self.header_sp_pos, d.header_sp_pos),
            (&mut self.details_sp_fo, d.details_sp_fo),
            (&mut self.details_sp_pos, d.details_sp_pos),
            (&mut self.custom_sp_fo, d.custom_sp_fo),
            (&mut self.mark_sp, d.mark_sp),
            (&mut self.writeback_sp, d.writeback_sp),
            (&mut self.select_fo_sp, d.select_fo_sp),
            (&mut self.fetch_fo_sp, d.fetch_fo_sp),
            (&mut self.select_pos_sp, d.select_pos_sp),
            (&mut self.fetch_pos_sp, d.fetch_pos_sp),
            (&mut self.mark_collected_sp, d.mark_collected_sp),
        ] {
            if dst.is_empty() {
                *dst = def;
            }
        }
        if self.fo_charge_codes.as_ref().is_none_or(Vec::is_empty) {
            self.fo_charge_codes = d.fo_charge_codes;
        }
        if self.poll_interval.is_zero() {
            self.poll_interval = d.poll_interval;
        }
        if self.batch_size == 0 {
            self.batch_size = d.batch_size;
        }
        if self.cycle_timeout.is_zero() {
            self.cycle_timeout = d.cycle_timeout;
        }
        if self.outbox_path.is_empty() {
            self.outbox_path = self
                .config_dir
                .join("outbox.db")
                .to_string_lossy()
                .into_owned();
        }
    }

    /// A copy with secrets masked, for logs.
    pub fn redacted(&self) -> AgentConfig {
        let mut r = self.clone();
        for f in [&mut r.api_key, &mut r.sql_dsn, &mut r.pos_sql_dsn] {
            if !f.is_empty() {
                *f = "***".into();
            }
        }
        r
    }

    /// Where config.enc lives.
    pub fn path(&self) -> PathBuf {
        self.config_dir.join(CONFIG_FILE)
    }

    /// `Load` without the seeding: decrypts `config.enc` in `dir`. `Ok(None)` when the file
    /// does not exist (the site is not enrolled); enrolling is not this crate's job.
    pub fn load(dir: &Path) -> Result<Option<AgentConfig>, String> {
        let path = dir.join(CONFIG_FILE);
        match std::fs::read(&path) {
            Ok(blob) => {
                let plain = open(&blob).map_err(|e| format!("agent: decrypt config: {e}"))?;
                let text =
                    String::from_utf8(plain).map_err(|e| format!("agent: parse config: {e}"))?;
                AgentConfig::from_json(&text, dir).map(Some)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("agent: read config: {e}")),
        }
    }

    /// `Save`: seals and writes config.enc (0600-equivalent: the directory is created
    /// if missing; on Windows the DPAPI blob is readable by this machine only).
    pub fn save(&self) -> Result<(), String> {
        if self.config_dir.as_os_str().is_empty() {
            return Err("agent: ConfigDir is empty".into());
        }
        std::fs::create_dir_all(&self.config_dir)
            .map_err(|e| format!("agent: create config dir: {e}"))?;
        let blob =
            seal(self.to_json().as_bytes()).map_err(|e| format!("agent: encrypt config: {e}"))?;
        write_atomic(&self.path(), &blob).map_err(|e| format!("agent: write config: {e}"))
    }
}

/// Writes through a temporary file and renames it over `path`. On Unix the file is
/// owner-only (0600): off Windows `config.enc` is not encrypted and holds the API key and
/// the SQL password, and the outbox holds invoice data.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        #[cfg(unix)]
        {
            // An older tmp file keeps its mode through `create`; tighten it explicitly.
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Go's `time.ParseDuration` for the forms an operator types ("30s", "1m30s", "500ms").
pub fn parse_go_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s == "0" {
        return Some(Duration::ZERO);
    }
    let mut total = 0f64;
    let mut rest = s;
    if rest.is_empty() {
        return None;
    }
    while !rest.is_empty() {
        let num_len = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(rest.len());
        if num_len == 0 {
            return None;
        }
        let n: f64 = rest[..num_len].parse().ok()?;
        rest = &rest[num_len..];
        let unit_len = rest
            .find(|c: char| c.is_ascii_digit() || c == '.')
            .unwrap_or(rest.len());
        let mult = match &rest[..unit_len] {
            "ns" => 1e-9,
            "us" | "µs" | "μs" => 1e-6,
            "ms" => 1e-3,
            "s" => 1.0,
            "m" => 60.0,
            "h" => 3600.0,
            _ => return None,
        };
        rest = &rest[unit_len..];
        total += n * mult;
    }
    Some(Duration::from_secs_f64(total))
}

/// The Go framing around the sealed JSON: a 4-byte big-endian length prefix.
fn wrap(p: &[u8]) -> Vec<u8> {
    let mut out = (p.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(p);
    out
}

fn unwrap(b: &[u8]) -> Result<Vec<u8>, String> {
    let (len, rest) = b
        .split_at_checked(4)
        .ok_or("agentcrypto: decrypted data too short")?;
    let n = u32::from_be_bytes([len[0], len[1], len[2], len[3]]) as usize;
    if n != rest.len() {
        return Err("agentcrypto: decrypted length prefix mismatch".into());
    }
    Ok(rest.to_vec())
}

#[cfg(windows)]
fn seal(p: &[u8]) -> Result<Vec<u8>, String> {
    dl_winsvc::dpapi::protect(&wrap(p), dl_winsvc::dpapi::Scope::LocalMachine)
        .map_err(|e| e.to_string())
}

#[cfg(windows)]
fn open(blob: &[u8]) -> Result<Vec<u8>, String> {
    if blob.is_empty() {
        return Err("agentcrypto: empty ciphertext".into());
    }
    let plain = dl_winsvc::dpapi::unprotect(blob, dl_winsvc::dpapi::Scope::LocalMachine)
        .map_err(|e| e.to_string())?;
    unwrap(&plain)
}

// Development builds on Linux/macOS keep the framed JSON unencrypted (the Go agent uses a
// hostname-derived AES key there, which is not a secret either). Production is Windows.
#[cfg(not(windows))]
fn seal(p: &[u8]) -> Result<Vec<u8>, String> {
    Ok(wrap(p))
}

#[cfg(not(windows))]
fn open(blob: &[u8]) -> Result<Vec<u8>, String> {
    unwrap(blob)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_serialise_like_go() {
        // json.Marshal(Defaults()) with ConfigDir/OutboxPath empty.
        let want = r#"{"platform_base_url":"","api_key":"","device_id":"00000000-0000-0000-0000-000000000000","sql_dsn":"","pos_sql_dsn":"","select_sp":"sp_GetPendingInvoices","header_sp_fo":"sp_getbillheader_FO","header_sp_pos":"sp_getbillheader_POS","details_sp_fo":"sp_getbilldetails_FO","details_sp_pos":"sp_getbilldetails_POS","custom_sp_fo":"sp_getbillcustomdata_FO","mark_sp":"sp_MarkInvoiceProcessed","writeback_sp":"sp_ApplyInvoiceWriteback","select_fo_sp":"sp_SelectPendingFO","fetch_fo_sp":"sp_FetchFO","select_pos_sp":"sp_SelectPendingPOS","fetch_pos_sp":"sp_FetchPOS","mark_collected_sp":"sp_MarkCollected","list_trn_types_sp":"sp_ListTrnTypes","probe_fo_schema_sp":"sp_ProbeFOSchema","fo_charge_codes":["110","150","160","170","200","201","210","211","220","221","230","240"],"poll_interval":30000000000,"batch_size":50,"cycle_timeout":120000000000,"recalc_tax_into_sc":false,"standard_service_charge_rate":0.05,"outbox_path":""}"#;
        assert_eq!(AgentConfig::default().to_json(), want);
    }

    #[test]
    fn round_trip_and_defaults_for_zero() {
        let mut c = AgentConfig {
            config_dir: PathBuf::from("C:/x"),
            ..Default::default()
        };
        c.api_key = "k".into();
        c.device_id = "6f9619ff-8b86-d011-b42d-00c04fc964ff".parse().unwrap();
        c.collect_from = Some(OffsetDateTime::parse_rfc3339("2026-07-01T00:00:00+07:00").unwrap());
        c.fo_exempt_codes = vec!["150".into()];
        let back = AgentConfig::from_json(&c.to_json(), Path::new("C:/x")).unwrap();
        assert_eq!(back.to_json(), {
            let mut d = c.clone();
            d.outbox_path = Path::new("C:/x")
                .join("outbox.db")
                .to_string_lossy()
                .into_owned();
            d.to_json()
        });
        assert!(
            c.to_json()
                .contains(r#""collect_from":"2026-07-01T00:00:00+07:00""#)
        );
        let sparse =
            AgentConfig::from_json(r#"{"api_key":"k","fo_charge_codes":[]}"#, Path::new("d"))
                .unwrap();
        assert_eq!(sparse.fetch_fo_sp, "sp_FetchFO");
        assert_eq!(sparse.batch_size, 50);
        assert_eq!(sparse.charge_codes().len(), 12);
        assert_eq!(
            sparse.list_trn_types_sp, "",
            "Go's applyDefaultsForZero leaves the optional SPs empty"
        );
    }

    #[test]
    fn load_is_none_without_config_enc_and_reads_a_saved_one() {
        let dir = std::env::temp_dir().join(format!("einv-cfg-load-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(AgentConfig::load(&dir).unwrap(), None);
        let c = AgentConfig {
            config_dir: dir.clone(),
            api_key: "k".into(),
            ..Default::default()
        };
        c.save().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(c.path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "config.enc is owner-only");
        }
        let back = AgentConfig::load(&dir).unwrap().expect("saved");
        assert_eq!(back.api_key, "k");
        std::fs::write(c.path(), b"\x00\x00").unwrap();
        assert!(
            AgentConfig::load(&dir).is_err(),
            "a corrupt file is an error, not None"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn durations_and_framing() {
        assert_eq!(parse_go_duration("1m30s"), Some(Duration::from_secs(90)));
        assert_eq!(parse_go_duration("500ms"), Some(Duration::from_millis(500)));
        assert_eq!(parse_go_duration("30"), None);
        assert_eq!(unwrap(&wrap(b"abc")).unwrap(), b"abc");
        assert!(unwrap(&[0, 0, 0, 9, 1]).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn sealed_file_round_trip() {
        let dir = std::env::temp_dir().join(format!("mmei-cfg-{}", std::process::id()));
        let mut c = AgentConfig {
            config_dir: dir.clone(),
            ..Default::default()
        };
        c.api_key = "secret".into();
        c.save().unwrap();
        let raw = std::fs::read(c.path()).unwrap();
        assert!(
            !raw.windows(6).any(|w| w == b"secret"),
            "config.enc must not hold the key in clear"
        );
        let back = AgentConfig::load(&dir).unwrap().expect("config.enc exists");
        assert_eq!(back.api_key, "secret");
        let _ = std::fs::remove_dir_all(dir);
    }
}
