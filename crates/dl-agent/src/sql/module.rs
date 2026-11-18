//! The built-in `sql` module: watches every declared database and answers `sql.ping`,
//! `sql.status` and `sql.survey`.
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

use super::{Connector, Pools, SqlPools, survey};
use crate::Error;
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
        }
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
                        ctx.report(Severity::Error, "sql_unreachable", &msg, db_context(&name));
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
                other => (false, format!("sql: unknown command {other}")),
            };
            ctx.complete(&cmd.id, ok, &output);
        }
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
}
