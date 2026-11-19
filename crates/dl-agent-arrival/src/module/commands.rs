//! Commands the hub sends to the `arrival` module (kind `<name>` or `arrival.<name>`). Each
//! answers structured JSON without secrets (no connection string, password, token or key).
//!
//! - `status`: the install, identity, settings summary, tenant key fingerprint and source,
//!   bundle and enrol record;
//! - `vault_check`: the tenant key against every sealed legal vault file;
//! - `selftest`: the module's self-test rows;
//! - `diagnostics`: `status` + `vault_check` + `selftest` in one answer;
//! - `picture_probe`: see [`super::picture`];
//! - `op_sweep`: see [`super::sweep`];
//! - `install_procedures` (`force`): installs the procedure bundle now.
//!
//! Every command but `status` reads the disk or the PMS at length (`vault_check` reads each
//! sealed file's header), so they run on their own thread, one at a time (a second gets
//! `busy`), and the link keeps polling. Unknown commands are answered `false`.

use std::sync::atomic::{AtomicBool, Ordering};

use dl_agent::command::Command;
use dl_agent::supervisor::ModuleContext;
use dl_json::{Map, Value};
use dl_time::OffsetDateTime;

use super::{ModuleConfig, NAME, picture, selftest, sweep};
use crate::agent::installer;
use crate::agent::layout::Layout;
use crate::agent::secrets::{Identity, TenantKeyStore};
use crate::agent::settings::Settings;
use crate::contracts::catalog::catalog;
use crate::contracts::key_vault::TenantKeyVault;
use crate::legacy::sqlauth::{self, ConnStr};
use crate::legacy::tenant_key;
use crate::tools::files;

/// One slow command at a time.
static BUSY: AtomicBool = AtomicBool::new(false);

/// The commands answered on their own thread (all but `status`).
const SLOW: &[&str] = &[
    "vault_check",
    "selftest",
    "diagnostics",
    "picture_probe",
    "op_sweep",
    "install_procedures",
];

const SERVER_KEYS: &[&str] = &[
    "server",
    "data source",
    "address",
    "addr",
    "network address",
];

/// Clears [`BUSY`] even when the command panics.
struct Slot;

impl Drop for Slot {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::SeqCst);
    }
}

/// Answers every queued command.
pub fn answer_all(ctx: &ModuleContext) {
    while let Some(c) = ctx.next_command() {
        answer(ctx, &c);
    }
}

fn answer(ctx: &ModuleContext, cmd: &Command) {
    let kind = cmd
        .kind
        .strip_prefix("arrival.")
        .unwrap_or(&cmd.kind)
        .to_string();
    let fast = match kind.as_str() {
        "status" => Some(Install::open(ctx).and_then(|i| i.status())),
        _ => None,
    };
    if let Some(result) = fast {
        finish(ctx, &cmd.id, result);
        return;
    }
    if !SLOW.contains(&kind.as_str()) {
        ctx.complete(
            &cmd.id,
            false,
            &format!("{NAME}: unknown command {}", cmd.kind),
        );
        return;
    }
    if BUSY.swap(true, Ordering::SeqCst) {
        ctx.complete(
            &cmd.id,
            false,
            &format!("{NAME}: busy with another command; send {kind} again when it has answered"),
        );
        return;
    }
    let ctx2 = ctx.clone();
    let id = cmd.id.clone();
    let args = cmd.args.clone();
    let spawned = std::thread::Builder::new()
        .name(format!("arrival-{kind}"))
        .spawn(move || {
            let _slot = Slot;
            let result = slow(&ctx2, &kind, &args);
            finish(&ctx2, &id, result);
        });
    if let Err(e) = spawned {
        BUSY.store(false, Ordering::SeqCst);
        ctx.complete(
            &cmd.id,
            false,
            &format!("{NAME}: cannot start the command: {e}"),
        );
    }
}

fn finish(ctx: &ModuleContext, id: &str, result: Result<Value, String>) {
    match result {
        Ok(v) => ctx.complete(id, true, &dl_json::to_string(&v)),
        Err(e) => {
            dl_log::warn!("arrival command failed"; error = %e);
            ctx.complete(id, false, &format!("{NAME}: {e}"));
        }
    }
}

fn slow(ctx: &ModuleContext, kind: &str, args: &Value) -> Result<Value, String> {
    let install = Install::open(ctx)?;
    match kind {
        "vault_check" => install.vault_check(),
        "selftest" => Ok(selftest_json(ctx)),
        "diagnostics" => {
            let mut m = Map::new();
            m.insert("status", install.status()?);
            m.insert(
                "vault",
                install.vault_check().unwrap_or_else(|e| {
                    let mut err = Map::new();
                    err.insert("error", e);
                    Value::Object(err)
                }),
            );
            m.insert("selftest", selftest_json(ctx));
            Ok(Value::Object(m))
        }
        "picture_probe" => {
            let (f, a) = picture::probe(&install.settings.pms_connection_string)?;
            ctx.set_metric("picture_phase", a.state);
            Ok(picture::to_json(&f, &a))
        }
        "op_sweep" => {
            let opts = sweep::SweepOptions::from_args(args, &install.today(ctx))?;
            let out = sweep::sweep(&install.settings.pms_connection_string, &opts)?;
            if let Some(c) = out.get("counts") {
                ctx.set_metric("last_sweep", c.clone());
            }
            Ok(out)
        }
        "install_procedures" => {
            let force = matches!(args.get("force"), Some(Value::Bool(true)));
            let ran = installer::ensure_installed(&install.settings.pms_connection_string, force)
                .map_err(|e| e.0)?;
            let mut m = Map::new();
            m.insert("ran", ran);
            m.insert("bundle", installer::bundle().hash.as_str());
            Ok(Value::Object(m))
        }
        other => Err(format!("unknown command {other}")),
    }
}

fn selftest_json(ctx: &ModuleContext) -> Value {
    let rows: Vec<Value> = selftest::run(ctx)
        .into_iter()
        .map(|c| {
            let mut m = Map::new();
            m.insert("id", c.id.as_str());
            m.insert("title", c.title.as_str());
            m.insert("status", c.status.as_str());
            m.insert("detail", c.detail.as_str());
            if let Some(r) = &c.remedy {
                m.insert("remedy", r.as_str());
            }
            m.insert("ms", c.duration_ms);
            Value::Object(m)
        })
        .collect();
    Value::Array(rows)
}

/// The module's install as the commands read it.
pub struct Install {
    config: ModuleConfig,
    layout: Layout,
    settings: Settings,
}

impl Install {
    /// Reads the config section and the settings.
    ///
    /// # Errors
    /// The config or the settings do not read.
    pub fn open(ctx: &ModuleContext) -> Result<Install, String> {
        let config = ModuleConfig::from_value(ctx.config()).map_err(|e| e.to_string())?;
        let layout = Layout::detect(config.install_dir.as_deref(), ctx.data_dir());
        let settings = Settings::load(layout.install_dir()).map_err(|e| e.to_string())?;
        Ok(Install {
            config,
            layout,
            settings,
        })
    }

    fn today(&self, _ctx: &ModuleContext) -> String {
        let offset = self.config.offset().unwrap_or(dl_time::Offset::UTC);
        let local = OffsetDateTime::now_utc().to_offset(offset);
        format!(
            "{:04}-{:02}-{:02}",
            local.year(),
            local.month().number(),
            local.day()
        )
    }

    /// The tenant key's fingerprint, read without creating one (`None`: no key yet).
    ///
    /// # Errors
    /// The key exists but does not open here.
    pub fn key_fingerprint(&self) -> Result<Option<String>, String> {
        let store = TenantKeyStore::new(
            &self.settings.tenant_key,
            self.layout.resolve(&self.settings.tenant_key_file),
        );
        match store.load_existing().map_err(|e| e.to_string())? {
            None => Ok(None),
            Some(k) => Ok(Some(
                TenantKeyVault::new(&k)
                    .map_err(|e| e.to_string())?
                    .fingerprint(),
            )),
        }
    }

    /// The `status` answer.
    ///
    /// # Errors
    /// Never for a readable install; kept fallible for the identity.
    pub fn status(&self) -> Result<Value, String> {
        let mut m = Map::new();
        m.insert("install_dir", files::path_text(self.layout.install_dir()));
        let identity_path = self.layout.resolve(&self.settings.identity_file);
        match Identity::load(&identity_path) {
            Ok(Some(i)) => {
                m.insert("enrolled", true);
                m.insert("tenant_id", i.tenant_id.as_str());
                m.insert("agent_id", i.agent_id.as_str());
                m.insert("kid", i.kid.as_str());
            }
            Ok(None) => {
                m.insert("enrolled", false);
            }
            Err(e) => {
                m.insert("enrolled", false);
                m.insert("identity_error", e.to_string());
            }
        }
        let record = files::read_record(self.layout.install_dir());
        m.insert("identity_source", identity_source(record.as_ref()));
        let conn = &self.settings.pms_connection_string;
        let parsed = ConnStr::parse(conn);
        let mut pms = Map::new();
        pms.insert("server", parsed.get(SERVER_KEYS).unwrap_or(""));
        pms.insert("database", sqlauth::database(conn).unwrap_or_default());
        pms.insert("sql_auth", sqlauth::detect(conn).as_str());
        if let Some(u) = sqlauth::user(conn) {
            pms.insert("sql_user", u);
        }
        m.insert("pms", Value::Object(pms));
        m.insert("cloud_base_url", self.settings.cloud_base_url.as_str());
        let mut key = Map::new();
        match self.key_fingerprint() {
            Ok(fp) => {
                key.insert("present", fp.is_some());
                key.insert("fingerprint", fp.map_or(Value::Null, Value::from));
            }
            Err(e) => {
                key.insert("present", true);
                key.insert("error", e);
            }
        }
        key.insert(
            "file",
            files::path_text(&self.layout.resolve(&self.settings.tenant_key_file)),
        );
        if let Some(src) = record
            .as_ref()
            .and_then(|r| r.get("tenant_key"))
            .and_then(|k| k.get("source"))
        {
            key.insert("source", src.clone());
        }
        m.insert("tenant_key", Value::Object(key));
        let b = installer::bundle();
        let mut bundle = Map::new();
        bundle.insert("hash", b.hash.as_str());
        bundle.insert("picture_hash", b.picture_hash.as_str());
        if let Ok(c) = catalog() {
            bundle.insert("catalog_version", i64::from(c.version));
            bundle.insert("operations", c.operations().len() as u64);
        }
        m.insert("bundle", Value::Object(bundle));
        if let Some(r) = record {
            m.insert("enroll", r);
        }
        Ok(Value::Object(m))
    }

    /// The `vault_check` answer: every sealed file must carry the key's fingerprint.
    ///
    /// # Errors
    /// The vault folder cannot be read.
    pub fn vault_check(&self) -> Result<Value, String> {
        let dir = self.layout.resolve(&self.settings.vault_directory);
        let fp = self.key_fingerprint();
        let scan = if dir.exists() {
            Some(tenant_key::scan_vault(&dir)?)
        } else {
            None
        };
        let mut m = Map::new();
        m.insert("directory", files::path_text(&dir));
        let (key_fp, key_error) = match fp {
            Ok(f) => (f, None),
            Err(e) => (None, Some(e)),
        };
        m.insert(
            "key_fingerprint",
            key_fp.clone().map_or(Value::Null, Value::from),
        );
        if let Some(e) = &key_error {
            m.insert("key_error", e.as_str());
        }
        let mut files_n = 0usize;
        let mut foreign = 0usize;
        let mut malformed = 0usize;
        let mut by = Map::new();
        if let Some(s) = &scan {
            files_n = s.files;
            malformed = s.malformed;
            for (f, n) in &s.by_fingerprint {
                by.insert(f.as_str(), *n as u64);
                if Some(f) != key_fp.as_ref() {
                    foreign += n;
                }
            }
        }
        m.insert("files", files_n as u64);
        m.insert("by_fingerprint", Value::Object(by));
        m.insert("malformed", malformed as u64);
        m.insert("unreadable", foreign as u64);
        m.insert("ok", key_error.is_none() && foreign == 0 && malformed == 0);
        Ok(Value::Object(m))
    }
}

/// Where the identity came from: the enrol record, else an install the .NET tools made.
pub fn identity_source(record: Option<&Value>) -> String {
    match record
        .and_then(|r| r.get("identity_source"))
        .and_then(Value::as_str)
    {
        Some(s) => {
            let at = record
                .and_then(|r| r.get("enrolled_at"))
                .and_then(Value::as_str)
                .unwrap_or("?");
            format!("{s} {at}")
        }
        None => "no enrol record (Install.ps1 or an older dl-agent)".into(),
    }
}

/// Publishes the cheap facts as module metrics (core `collect_diagnostics` carries them).
pub fn publish_metrics(ctx: &ModuleContext) {
    let install = match Install::open(ctx) {
        Ok(i) => i,
        Err(e) => {
            // The module's state (degraded: config invalid / not enrolled) already says why.
            dl_log::debug!("arrival metrics not published"; error = %e);
            return;
        }
    };
    let record = files::read_record(install.layout.install_dir());
    ctx.set_metric("identity_source", identity_source(record.as_ref()));
    match install.key_fingerprint() {
        Ok(Some(fp)) => ctx.set_metric("tenant_key_fingerprint", fp),
        Ok(None) => ctx.set_metric("tenant_key_fingerprint", "none yet"),
        Err(e) => ctx.set_metric("tenant_key_fingerprint", format!("unreadable: {e}")),
    }
    if let Some(src) = record
        .as_ref()
        .and_then(|r| r.get("tenant_key"))
        .and_then(|k| k.get("source"))
        .and_then(Value::as_str)
    {
        ctx.set_metric("tenant_key_source", src);
    }
    ctx.set_metric("bundle_hash", installer::bundle().hash.as_str());
    if let Ok(c) = catalog() {
        ctx.set_metric("catalog_version", i64::from(c.version));
    }
    ctx.set_metric(
        "sql_auth",
        sqlauth::detect(&install.settings.pms_connection_string).as_str(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use dl_json::json;

    fn temp(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "arr-cmd-{tag}-{}-{:016x}",
            std::process::id(),
            dl_rand::u64()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn status_and_vault_check_name_no_secret_and_create_nothing() {
        let dir = temp("status");
        std::fs::write(
            dir.join("appsettings.Production.json"),
            r#"{"Agent":{"PmsConnectionString":"Server=HOTEL\\SQL;Database=SMILE_FO;User Id=ci;Password=Top$ecret","CloudBaseUrl":"https://a.example"}}"#,
        )
        .unwrap();
        let ctx = ModuleContext::standalone(
            NAME,
            json!({"install_dir": dir.display().to_string()}),
            dir.clone(),
        );
        let i = Install::open(&ctx).unwrap();
        let s = dl_json::to_string(&i.status().unwrap());
        assert!(!s.contains("Top$ecret"), "{s}");
        assert!(
            s.contains(r#""sql_auth":"sql""#) && s.contains(r#""sql_user":"ci""#),
            "{s}"
        );
        assert!(s.contains(r#""database":"SMILE_FO""#), "{s}");
        assert!(s.contains(r#""present":false"#), "{s}");
        let v = i.vault_check().unwrap();
        assert_eq!(v.get("ok").and_then(Value::as_bool), Some(true));
        assert!(
            !dir.join("agent-tenant-key.bin").exists(),
            "no key was created"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn vault_check_counts_files_another_key_sealed() {
        let dir = temp("vault");
        // The key file as the agent writes it (protected), not raw bytes.
        let key = TenantKeyStore::new("", dir.join("agent-tenant-key.bin"))
            .create(&dir.join("vault"))
            .unwrap();
        let foreign = temp("vault-other");
        let other_key = TenantKeyStore::new("", foreign.join("agent-tenant-key.bin"))
            .create(&foreign.join("vault"))
            .unwrap();
        let vault = dir.join("vault").join("guests");
        std::fs::create_dir_all(&vault).unwrap();
        let mine = TenantKeyVault::new(&key).unwrap();
        let other = TenantKeyVault::new(&other_key).unwrap();
        std::fs::write(vault.join("a"), mine.wrap(b"a", "guests/a").unwrap()).unwrap();
        std::fs::write(vault.join("b"), other.wrap(b"b", "guests/b").unwrap()).unwrap();
        let ctx = ModuleContext::standalone(
            NAME,
            json!({"install_dir": dir.display().to_string()}),
            dir.clone(),
        );
        let v = Install::open(&ctx).unwrap().vault_check().unwrap();
        assert_eq!(v.get("files").and_then(Value::as_u64), Some(2));
        assert_eq!(v.get("unreadable").and_then(Value::as_u64), Some(1));
        assert_eq!(v.get("ok").and_then(Value::as_bool), Some(false));
        assert_eq!(
            v.get("key_fingerprint").and_then(Value::as_str),
            Some(mine.fingerprint().as_str())
        );
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&foreign).unwrap();
    }

    #[test]
    fn only_status_answers_on_the_polling_thread() {
        for kind in ["vault_check", "selftest", "diagnostics", "op_sweep"] {
            assert!(SLOW.contains(&kind), "{kind} must not hold the poll loop");
        }
        assert!(!SLOW.contains(&"status"));
    }

    #[test]
    fn identity_source_reads_the_enrol_record() {
        let r = json!({"identity_source":"fresh_enrol","enrolled_at":"2026-11-19T08:00:00Z"});
        assert_eq!(
            identity_source(Some(&r)),
            "fresh_enrol 2026-11-19T08:00:00Z"
        );
        assert!(identity_source(None).contains("no enrol record"));
    }
}
