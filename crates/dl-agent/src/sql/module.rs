//! The built-in `sql` module: watches every declared database and answers `sql.ping` and
//! `sql.status`.
//!
//! It is added only when the site declares databases. A database that stops answering
//! turns the module `degraded` (so the canary gate sees it) and files one `sql_unreachable`
//! report per outage; recovery files `sql_recovered`.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dl_json::{Map, Value, json};

use super::{Connector, Pools, SqlPools};
use crate::Error;
use crate::report::Severity;
use crate::supervisor::{Factory, Module, ModuleContext};

/// How often every database is checked.
pub const CHECK_EVERY: Duration = Duration::from_secs(30);

/// The `sql` module.
pub struct SqlModule<C: Connector> {
    pools: Pools<C>,
    down: BTreeSet<String>,
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

    fn handle_commands(&self, ctx: &ModuleContext) {
        while let Some(cmd) = ctx.next_command() {
            let (ok, output) = match cmd.kind.as_str() {
                "sql.ping" => self.ping(&cmd.args),
                "sql.status" => (true, dl_json::to_string(&self.pools.stats())),
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
}
