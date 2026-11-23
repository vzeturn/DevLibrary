//! The built-in `sql` module: watches every declared database and answers `sql.ping`,
//! `sql.status`, `sql.survey`, `sql.probe`, `sql.install` and `sql.inventory`.
//!
//! It is added only when the site declares databases. A database that stops answering
//! turns the module `degraded` (so the canary gate sees it) and files one `sql_unreachable`
//! report per outage; recovery files `sql_recovered`. Each reachable database is surveyed
//! (engine, compat level, collations, grants) on first contact and daily; `sql.survey`
//! forces one.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dl_json::{Map, Value, json};

use super::bundle::Bundle;
use super::{Connector, Pools, SqlPools, install, inventory, probe, survey};
use crate::Error;
use crate::manifest::TrustedKeys;
use crate::report::Severity;
use crate::supervisor::{Factory, Module, ModuleContext};

/// How often every database is checked.
pub const CHECK_EVERY: Duration = Duration::from_secs(30);
/// How often a reachable database is surveyed again (grants and compat levels change).
pub const SURVEY_EVERY: Duration = Duration::from_secs(24 * 3600);

/// The `sql` module.
pub struct SqlModule<C: Connector> {
    pools: Pools<C>,
    down: BTreeSet<String>,
    unencrypted: BTreeSet<String>,
    surveyed: BTreeMap<String, Instant>,
    trusted: TrustedKeys,
}

/// Factory for the module over the site's pools.
pub fn factory(pools: SqlPools) -> Factory {
    Arc::new(move || Box::new(SqlModule::new(pools.clone())))
}

impl<C: Connector> SqlModule<C> {
    /// A module watching `pools`.
    pub fn new(pools: Pools<C>) -> SqlModule<C> {
        SqlModule {
            pools,
            down: BTreeSet::new(),
            unencrypted: BTreeSet::new(),
            surveyed: BTreeMap::new(),
            trusted: crate::agent::release_keys(),
        }
    }

    /// Replaces the keys SQL bundles must be signed with (tests).
    pub fn with_trusted_keys(mut self, trusted: TrustedKeys) -> SqlModule<C> {
        self.trusted = trusted;
        self
    }

    /// Checks every database once and updates state, metrics and reports.
    pub fn check(&mut self, ctx: &ModuleContext) {
        let mut failing = Vec::new();
        for pool in self.pools.all() {
            pool.reap();
            let name = pool.name().to_string();
            match pool.check() {
                Ok(_) => {
                    if self.down.remove(&name) {
                        let msg = format!("{name}: reachable again");
                        ctx.report(Severity::Info, "sql_recovered", &msg, db_context(&name));
                    }
                    let rung = pool
                        .stats()
                        .get("transport")
                        .and_then(|t| t.get("rung"))
                        .cloned();
                    if rung.as_ref().and_then(Value::as_str) == Some("unencrypted")
                        && self.unencrypted.insert(name.clone())
                    {
                        let msg = format!(
                            "{name}: the server accepts only unencrypted sessions (no TLS 1.0+ \
                             it can finish); login travels in clear on the LAN. Install the \
                             SQL Server TLS 1.2 update or service pack."
                        );
                        ctx.report(
                            Severity::Warning,
                            "sql_unencrypted",
                            &msg,
                            db_context(&name),
                        );
                    }
                    if self
                        .surveyed
                        .get(&name)
                        .is_none_or(|at| at.elapsed() >= SURVEY_EVERY)
                    {
                        // A failure is already filed as `sql_survey_failed`; retried tomorrow.
                        let _ = self.survey_one(ctx, &name);
                    }
                }
                Err(e) => {
                    let msg = e.to_string();
                    if self.down.insert(name.clone()) {
                        // A wrong password reads as `sql_access_denied`, not as a dead server.
                        let code = super::problem_code(&e, "sql_unreachable");
                        ctx.report(Severity::Error, code, &msg, db_context(&name));
                    }
                    failing.push(msg);
                }
            }
        }
        ctx.set_metric("databases", self.pools.stats());
        if failing.is_empty() {
            let n = self.pools.names().len();
            ctx.set_running(&format!("{n} database(s) reachable"));
        } else {
            ctx.set_degraded(&failing.join("; "));
        }
    }

    /// Surveys `name` and stores the result; a failure is reported once per day, never fatal.
    fn survey_one(&mut self, ctx: &ModuleContext, name: &str) -> Result<Value, Error> {
        self.surveyed.insert(name.to_string(), Instant::now());
        let pool = self.pools.get(name)?;
        let rung = pool
            .stats()
            .get("transport")
            .and_then(|t| t.get("rung"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let result = pool
            .get()
            .and_then(|mut conn| survey::survey(&mut *conn, rung.as_deref()));
        match result {
            Ok(s) => {
                self.pools.set_survey(name, s.clone());
                Ok(s)
            }
            Err(e) => {
                let msg = format!("{name}: site survey failed: {e}");
                ctx.report(
                    Severity::Warning,
                    "sql_survey_failed",
                    &msg,
                    db_context(name),
                );
                Err(e)
            }
        }
    }

    /// `sql.survey`: surveys every (or one) database now and answers the results.
    fn survey_now(&mut self, ctx: &ModuleContext, args: &Value) -> (bool, String) {
        let only = args.get("database").and_then(Value::as_str);
        let mut out = Map::new();
        let mut ok = true;
        for name in self.pools.names() {
            if only.is_some_and(|o| o != name) {
                continue;
            }
            let answer = match self.survey_one(ctx, &name) {
                Ok(s) => s,
                Err(e) => {
                    ok = false;
                    json!({"error": e.to_string()})
                }
            };
            out.insert(name, answer);
        }
        if out.is_empty() {
            return (
                false,
                format!("sql: no database {}", only.unwrap_or("declared")),
            );
        }
        let body = json!({"host": crate::host::info(), "databases": Value::Object(out)});
        (ok, dl_json::to_string(&body))
    }

    /// `sql.probe {requirement, database?}`: which tables, columns, views, procedures,
    /// functions and configuration rows the requirement names are missing (§9.6). The result
    /// is kept in the database's survey for the hub gate.
    fn probe_now(&self, args: &Value) -> (bool, String) {
        let req = match args
            .get("requirement")
            .ok_or_else(|| Error::Config("probe: requirement is required".into()))
            .and_then(probe::Requirement::from_json)
        {
            Ok(r) => r,
            Err(e) => return (false, format!("sql: {e}")),
        };
        let only = args.get("database").and_then(Value::as_str);
        let mut out = Map::new();
        let mut ok = true;
        for name in self.pools.names() {
            if only.is_some_and(|o| o != name) {
                continue;
            }
            let answer = match self
                .pools
                .get(&name)
                .and_then(|p| p.with(|conn| probe::probe(conn, &req)))
            {
                Ok(r) => {
                    ok &= r["ok"].as_bool() == Some(true);
                    self.pools.set_probe(&name, r.clone());
                    r
                }
                Err(e) => {
                    ok = false;
                    json!({"ok": false, "code": super::problem_code(&e, "sql_error"), "error": e.to_string()})
                }
            };
            out.insert(name, answer);
        }
        if out.is_empty() {
            return (
                false,
                format!("sql: no database {}", only.unwrap_or("declared")),
            );
        }
        (ok, dl_json::to_string(&Value::Object(out)))
    }

    /// `sql.install {bundle, database?, dry_run?, force?}`: verifies the bundle against the
    /// release keys and installs it into each database (§9.7). Replaced definitions are
    /// saved under `sql-backups/<database>/<product>-<version>-<time>/`, every real install
    /// is appended to `sql-installs.jsonl`, and the bundle's probe is kept in the survey.
    fn install_now(&self, ctx: &ModuleContext, args: &Value) -> (bool, String) {
        let bundle = match args
            .get("bundle")
            .ok_or_else(|| Error::Config("install: bundle is required".into()))
            .and_then(Bundle::from_json)
            .and_then(|b| b.verify(&self.trusted).map(|()| b))
        {
            Ok(b) => b,
            Err(e) => return (false, format!("sql: {e}")),
        };
        let flag = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(false);
        let only = args.get("database").and_then(Value::as_str);
        let at = crate::now();
        let mut out = Map::new();
        let mut ok = true;
        for name in self.pools.names() {
            if only.is_some_and(|o| o != name) {
                continue;
            }
            let opts = install::Options {
                dry_run: flag("dry_run"),
                force: flag("force"),
                backup_dir: Some(
                    ctx.data_dir()
                        .join("sql-backups")
                        .join(&name)
                        .join(format!("{}-{}-{at}", bundle.product, bundle.version)),
                ),
            };
            let answer = match self
                .pools
                .get(&name)
                .and_then(|p| p.with(|conn| install::install(conn, &bundle, &opts)))
            {
                Ok(r) => {
                    if let Some(p) = r.get("probe") {
                        self.pools.set_probe(&name, p.clone());
                    }
                    if !opts.dry_run {
                        ledger(ctx, &name, &bundle, at, &r);
                    }
                    r
                }
                Err(e) => {
                    json!({"ok": false, "code": super::problem_code(&e, "sql_error"), "error": e.to_string()})
                }
            };
            ok &= answer["ok"].as_bool() == Some(true);
            out.insert(name, answer);
        }
        if out.is_empty() {
            return (
                false,
                format!("sql: no database {}", only.unwrap_or("declared")),
            );
        }
        (ok, dl_json::to_string(&Value::Object(out)))
    }

    /// `sql.inventory {database?, checksum?, section?, after?, limit?}`: row counts, column-layout and definition
    /// hashes per object, plus data checksums when asked (§9.8). No row leaves the server.
    fn inventory_now(&self, args: &Value) -> (bool, String) {
        let checksum = match inventory::Checksum::from_arg(args.get("checksum")) {
            Ok(c) => c,
            Err(e) => return (false, format!("sql: {e}")),
        };
        let page = match inventory::Page::from_args(args) {
            Ok(p) => p,
            Err(e) => return (false, format!("sql: {e}")),
        };
        let only = args.get("database").and_then(Value::as_str);
        let mut out = Map::new();
        let mut ok = true;
        for name in self.pools.names() {
            if only.is_some_and(|o| o != name) {
                continue;
            }
            let answer = match self
                .pools
                .get(&name)
                .and_then(|p| p.with(|conn| inventory::inventory(conn, &checksum, &page)))
            {
                Ok(v) => v,
                Err(e) => {
                    ok = false;
                    json!({"code": super::problem_code(&e, "sql_error"), "error": e.to_string()})
                }
            };
            out.insert(name, answer);
        }
        if out.is_empty() {
            return (
                false,
                format!("sql: no database {}", only.unwrap_or("declared")),
            );
        }
        let text = dl_json::to_string(&Value::Object(out));
        if text.len() > crate::command::MAX_OUTPUT {
            // A cut answer is not JSON any more: refuse it whole and say how to page.
            return (
                false,
                format!(
                    "sql: inventory_too_large ({} bytes): ask one database and one section \
                     with limit (sql.inventory {{database, section, limit, after}})",
                    text.len()
                ),
            );
        }
        (ok, text)
    }

    fn ping(&self, args: &Value) -> (bool, String) {
        let only = args.get("database").and_then(Value::as_str);
        let mut out = Map::new();
        let mut ok = true;
        for name in self.pools.names() {
            if only.is_some_and(|o| o != name) {
                continue;
            }
            let answer = match self.pools.get(&name).and_then(|p| p.check()) {
                Ok(d) => json!({"ok": true, "ms": d.as_millis() as u64}),
                Err(e) => {
                    ok = false;
                    json!({"ok": false, "error": e.to_string()})
                }
            };
            out.insert(name, answer);
        }
        if out.is_empty() {
            return (
                false,
                format!("sql: no database {}", only.unwrap_or("declared")),
            );
        }
        (ok, dl_json::to_string(&Value::Object(out)))
    }

    fn handle_commands(&mut self, ctx: &ModuleContext) {
        while let Some(cmd) = ctx.next_command() {
            let (ok, output) = match cmd.kind.as_str() {
                "sql.ping" => self.ping(&cmd.args),
                "sql.status" => (true, dl_json::to_string(&self.pools.stats())),
                "sql.survey" => self.survey_now(ctx, &cmd.args),
                "sql.probe" => self.probe_now(&cmd.args),
                "sql.install" => self.install_now(ctx, &cmd.args),
                "sql.inventory" => self.inventory_now(&cmd.args),
                other => (false, format!("sql: unknown command {other}")),
            };
            ctx.complete(&cmd.id, ok, &output);
        }
    }
}

/// Appends one install to `sql-installs.jsonl` (what was applied where, by which bundle);
/// a failure to write it is filed as a report, the install itself already happened.
fn ledger(ctx: &ModuleContext, database: &str, b: &Bundle, at: u64, result: &Value) {
    use std::io::Write;
    let line = dl_json::to_string(&json!({
        "at": at, "database": database, "product": b.product, "version": b.version.to_string(),
        "key_id": b.key_id, "ok": result["ok"].clone(), "applied": result["applied"].clone(),
        "backups": result["backups"].clone(),
    }));
    let file = ctx.data_dir().join("sql-installs.jsonl");
    let written = std::fs::create_dir_all(ctx.data_dir()).and_then(|()| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file)?
            .write_all(format!("{line}\n").as_bytes())
    });
    if let Err(e) = written {
        let msg = format!(
            "could not record the SQL install in {}: {e}",
            file.display()
        );
        ctx.report(
            Severity::Warning,
            "sql_ledger_unwritable",
            &msg,
            db_context(database),
        );
    }
}

fn db_context(name: &str) -> Map {
    let mut m = Map::new();
    m.insert("database", name);
    m
}

impl<C: Connector> Module for SqlModule<C> {
    fn name(&self) -> &str {
        "sql"
    }

    fn run(&mut self, ctx: &ModuleContext) -> Result<(), Error> {
        self.check(ctx);
        let mut last = Instant::now();
        while !ctx.stop().wait_timeout(Duration::from_millis(500)) {
            self.handle_commands(ctx);
            if last.elapsed() >= CHECK_EVERY {
                self.check(ctx);
                last = Instant::now();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::tests::{Fake, fake_pools};
    use crate::store::testdir::TestDir;

    #[test]
    fn outage_degrades_once_and_recovery_reports() {
        let d = TestDir::new("sql-module");
        let (pools, server) = fake_pools(&["SMILE_FO"]);
        let ctx = ModuleContext::standalone("sql", Value::Null, d.0.clone());
        let mut m = SqlModule::new(pools.clone());
        m.check(&ctx);
        assert_eq!(ctx.status().state.as_str(), "running");

        server.set_up(false);
        // Idle sessions die with the server; the ping on checkout must notice.
        Fake::break_all(&server);
        m.check(&ctx);
        m.check(&ctx);
        assert_eq!(ctx.status().state.as_str(), "degraded");
        assert!(ctx.status().detail.contains("SMILE_FO"));

        server.set_up(true);
        std::thread::sleep(Duration::from_millis(20)); // past the test backoff
        m.check(&ctx);
        assert_eq!(ctx.status().state.as_str(), "running");
        let (ok, out) = m.ping(&json!({"database": "SMILE_FO"}));
        assert!(ok, "{out}");
        assert!(out.contains("\"ok\":true"));
        let (ok, out) = m.ping(&json!({"database": "NOPE"}));
        assert!(!ok && out.contains("NOPE"));
    }

    #[test]
    fn first_reachable_check_surveys_and_the_command_refreshes() {
        let d = TestDir::new("sql-survey");
        let (pools, server) = fake_pools(&["SMILE_FO"]);
        let ctx = ModuleContext::standalone("sql", Value::Null, d.0.clone());
        let mut m = SqlModule::new(pools.clone());
        server.set_up(false);
        m.check(&ctx);
        assert_eq!(pools.surveys().as_object().map(|o| o.len()), Some(0));

        server.set_up(true);
        std::thread::sleep(Duration::from_millis(20));
        m.check(&ctx);
        let s = &pools.surveys()["SMILE_FO"];
        assert_eq!(s["engine"]["name"].as_str(), Some("SQL Server 2008 R2"));
        assert_eq!(s["syntax_2012"].as_bool(), Some(false));

        let (ok, out) = m.survey_now(&ctx, &json!({}));
        assert!(ok, "{out}");
        let v = dl_json::parse(&out).unwrap();
        assert!(v["host"]["target"].is_string());
        assert_eq!(
            v["databases"]["SMILE_FO"]["database"]["compatibility_level"].as_u64(),
            Some(100)
        );
        let (ok, out) = m.survey_now(&ctx, &json!({"database": "NOPE"}));
        assert!(!ok && out.contains("NOPE"));
    }

    #[test]
    fn install_command_needs_a_release_signature_and_records_the_install() {
        use dl_ed25519::SigningKey;
        use dl_encoding::base64::STANDARD as B64;
        let d = TestDir::new("sql-install");
        let (pools, _server) = fake_pools(&["SMILE_FO"]);
        let ctx = ModuleContext::standalone("sql", Value::Null, d.0.clone());
        let key = SigningKey::from_seed(&[5u8; 32]);
        let trusted = TrustedKeys::new()
            .with_key("rel", &B64.encode(key.verifying_key().as_bytes()))
            .unwrap();
        let m = SqlModule::new(pools.clone()).with_trusted_keys(trusted);
        let mut b = Bundle::from_json(&json!({
            "product": "einvoice", "version": "1.4.0",
            "objects": [{"kind": "procedure", "name": "dbo.usp_GetBill",
                         "sql": "CREATE OR ALTER PROCEDURE dbo.usp_GetBill AS SELECT 1 AS x"}],
            "requires": {"procedures": ["dbo.usp_GetBill"]}
        }))
        .unwrap();
        let (ok, out) = m.install_now(&ctx, &json!({"bundle": b.to_json()}));
        assert!(!ok && out.contains("unknown key"), "{out}");

        b.sign("rel", &key);
        let (ok, out) = m.install_now(&ctx, &json!({"bundle": b.to_json(), "dry_run": true}));
        assert!(ok, "{out}");
        assert!(
            !ctx.data_dir().join("sql-installs.jsonl").exists(),
            "dry run"
        );
        let (ok, out) = m.install_now(&ctx, &json!({"bundle": b.to_json()}));
        assert!(ok, "{out}");
        let v = dl_json::parse(&out).unwrap();
        assert_eq!(
            v["SMILE_FO"]["applied"][0]["action"].as_str(),
            Some("create")
        );
        assert_eq!(
            pools.surveys()["SMILE_FO"]["probe"]["ok"].as_bool(),
            Some(true)
        );
        let ledger = std::fs::read_to_string(ctx.data_dir().join("sql-installs.jsonl")).unwrap();
        assert!(ledger.contains("\"version\":\"1.4.0\"") && ledger.ends_with('\n'));
        let (ok, out) = m.install_now(&ctx, &json!({"bundle": b.to_json(), "database": "NOPE"}));
        assert!(!ok && out.contains("NOPE"));
    }

    #[test]
    fn probe_command_lists_what_is_missing_and_keeps_it_in_the_survey() {
        let d = TestDir::new("sql-probe");
        let (pools, _server) = fake_pools(&["SMILE_FO"]);
        let ctx = ModuleContext::standalone("sql", Value::Null, d.0.clone());
        let mut m = SqlModule::new(pools.clone());
        m.check(&ctx);
        let req = json!({
            "tables": {"dbo.HoaDon": ["Id", "SoHD", "MaCQT"], "dbo.Config": ["Key"]},
            "procedures": ["dbo.usp_GetBill", "dbo.usp_IssueInvoice"],
            "rows": [{"table": "dbo.Config", "column": "Key", "values": ["EINV_URL", "EINV_KEY"]}]
        });
        let (ok, out) = m.probe_now(&json!({"requirement": req}));
        assert!(!ok, "{out}");
        let v = dl_json::parse(&out).unwrap();
        let missing: Vec<String> = v["SMILE_FO"]["missing"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            missing,
            ["dbo.usp_IssueInvoice", "dbo.HoaDon.MaCQT", "dbo.Config.Key"]
        );
        assert_eq!(v["SMILE_FO"]["checked"].as_u64(), Some(10));
        // Kept in the survey for the heartbeat, and a later survey does not drop it.
        assert_eq!(
            pools.surveys()["SMILE_FO"]["probe"]["ok"].as_bool(),
            Some(false)
        );
        m.survey_now(&ctx, &json!({}));
        assert!(pools.surveys()["SMILE_FO"]["probe"]["missing"].is_array());
        assert!(pools.surveys()["SMILE_FO"]["engine"].is_object());

        let (ok, out) = m.probe_now(&json!({"requirement": {"tables": {"dbo.HoaDon": ["Id"]}}}));
        assert!(ok, "{out}");
        let (ok, out) = m.probe_now(&json!({}));
        assert!(!ok && out.contains("requirement"), "{out}");
    }

    #[test]
    fn problem_codes_come_from_tagged_sql_errors() {
        let e = Error::Sql("sql_access_denied: Login failed for user 'smile'.".into());
        assert_eq!(
            super::super::problem_code(&e, "sql_unreachable"),
            "sql_access_denied"
        );
        let e = Error::Sql("connection refused".into());
        assert_eq!(
            super::super::problem_code(&e, "sql_unreachable"),
            "sql_unreachable"
        );
        // Through the pool: `<db>: connect failed: <tagged error>`.
        let e = Error::Sql(
            "SMILE_FO: connect failed: sql_tls_unavailable: tls handshake failed: …".into(),
        );
        assert_eq!(
            super::super::problem_code(&e, "sql_unreachable"),
            "sql_tls_unavailable"
        );
        let e = Error::Sql("SMILE_FO: connect failed: sql_access_denied: Login failed".into());
        assert_eq!(
            super::super::problem_code(&e, "sql_unreachable"),
            "sql_access_denied"
        );
    }
}
