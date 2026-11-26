//! Schema dictionary upload (RT-1215): once a day the agent reads the structure of every
//! declared database through the catalog-only collector (`dl-agent-schema`) and sends it to
//! the hub, one database per request so each body stays under the hub cap. No table row is
//! ever read: the collector refuses any statement that is not one of its fixed `sys.*` queries.

use dl_agent_schema::{Database, MetaSource, Snapshot, collect_database, upload_body};
use dl_json::Value;

use super::Agent;
use crate::Error;
use crate::report::Severity;
use crate::sql::{Connection, Connector, Pools};

/// A successful round is repeated after this long; the hub only moves `last_seen` when the
/// structure is unchanged.
pub const SCHEMA_EVERY_SECONDS: u64 = 24 * 3600;
/// A round where nothing reached the hub is tried again after this long.
pub const SCHEMA_RETRY_SECONDS: u64 = 3600;
/// The first round waits this long after start, so boot, probation and the SQL survey come first.
pub const SCHEMA_FIRST_DELAY_SECONDS: u64 = 600;

/// Runs the collector's catalog queries on one pooled session and hands back positional text.
struct Rows<'a, C: Connection>(&'a mut C);

impl<C: Connection> MetaSource for Rows<'_, C> {
    fn rows(&mut self, sql: &str) -> Result<Vec<Vec<String>>, dl_agent_schema::Error> {
        let rows = self
            .0
            .query_json(sql, &[])
            .map_err(|e| dl_agent_schema::Error::Sql(e.to_string()))?;
        rows.iter().map(positional).collect()
    }
}

/// `{"c0": …, "c1": …}` → `["…", "…"]`; `NULL` becomes an empty string.
fn positional(row: &Value) -> Result<Vec<String>, dl_agent_schema::Error> {
    let obj = row
        .as_object()
        .ok_or_else(|| dl_agent_schema::Error::Row("catalog row is not an object".into()))?;
    let mut out = Vec::new();
    while let Some(v) = obj.get(&format!("c{}", out.len())) {
        out.push(match v {
            Value::Null => String::new(),
            Value::String(s) => s.clone(),
            other => dl_json::to_string(other),
        });
    }
    Ok(out)
}

/// The structure of every declared database; a database that fails is named with its error
/// and does not stop the others.
pub fn collect_all<C: Connector>(pools: &Pools<C>) -> (Vec<Database>, Vec<(String, String)>) {
    let mut done = Vec::new();
    let mut failed = Vec::new();
    for pool in pools.all() {
        let name = pool.name().to_string();
        let got = pool.with(|conn| {
            collect_database(&name, &mut Rows(conn)).map_err(|e| Error::Sql(e.to_string()))
        });
        match got {
            Ok(db) => done.push(db),
            Err(e) => failed.push((name, e.to_string())),
        }
    }
    (done, failed)
}

/// `engine.version` of a database's survey, empty when it was not surveyed yet.
fn server_version(surveys: &Value, database: &str) -> String {
    surveys
        .get(database)
        .and_then(|s| s.get("engine"))
        .and_then(|e| e.get("version"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

impl Agent {
    /// Sends the schema dictionary when a round is due and the hub answers.
    pub(super) fn schema_upload(&mut self, now: u64) {
        let due = *self
            .schema_next
            .get_or_insert(now + SCHEMA_FIRST_DELAY_SECONDS);
        if now < due || !self.hub_ok || self.sql.pools.is_empty() {
            return;
        }
        let sent = self.send_schema();
        self.schema_next = Some(
            now + if sent {
                SCHEMA_EVERY_SECONDS
            } else {
                SCHEMA_RETRY_SECONDS
            },
        );
    }

    /// One round; `true` when at least one database reached the hub (or the hub has no route).
    fn send_schema(&self) -> bool {
        let (databases, failed) = collect_all(&self.sql.pools);
        for (db, e) in &failed {
            self.report(
                Severity::Warning,
                "schema_collect_failed",
                &format!("{db}: {e}"),
            );
        }
        let surveys = self.sql.pools.surveys();
        let version = self.opts.version.to_string();
        let mut sent = false;
        for db in databases {
            let name = db.name.clone();
            let snapshot = Snapshot {
                server_version: server_version(&surveys, &name),
                databases: vec![db],
            };
            let result = upload_body(&version, &snapshot)
                .map_err(|e| e.to_string())
                .and_then(|body| self.hub.schema_snapshot(&body).map_err(|e| e.to_string()));
            match result {
                Ok(true) => {
                    dl_log::info!("schema snapshot sent"; database = %name);
                    sent = true;
                }
                Ok(false) => {
                    dl_log::info!("the hub has no schema snapshot route yet"; database = %name);
                    return true;
                }
                Err(e) => self.report(
                    Severity::Warning,
                    "schema_upload_failed",
                    &format!("{name}: {e}"),
                ),
            }
        }
        sent
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use dl_json::json;

    use super::*;
    use crate::sql::{Pool, PoolOptions};

    struct Fake;
    struct Catalog {
        seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl Connector for Catalog {
        type Conn = Session;
        fn connect(&self) -> Result<Session, Error> {
            Ok(Session(std::sync::Arc::clone(&self.seen)))
        }
    }

    struct Session(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

    impl Connection for Session {
        fn ping(&mut self) -> Result<(), Error> {
            Ok(())
        }
        fn is_broken(&self) -> bool {
            false
        }
        fn in_transaction(&self) -> bool {
            false
        }
        fn set_query_timeout(&mut self, _: Option<Duration>) -> Result<(), Error> {
            Ok(())
        }
        fn query_json(&mut self, sql: &str, params: &[(&str, &str)]) -> Result<Vec<Value>, Error> {
            assert!(params.is_empty());
            self.0.lock().unwrap().push(sql.to_string());
            use dl_agent_schema::queries::*;
            Ok(match sql {
                s if s == TABLES => vec![json!({"c0": "dbo", "c1": "Guest", "c2": "5"})],
                s if s == COLUMNS => vec![
                    json!({"c0": "dbo", "c1": "Guest", "c2": "Id", "c3": "int(10,0)",
                    "c4": "4", "c5": "0", "c6": "1", "c7": "1"}),
                ],
                s if s == INDEXES => vec![
                    json!({"c0": "dbo", "c1": "Guest", "c2": "PK_Guest", "c3": "1", "c4": "1", "c5": "Id"}),
                ],
                s if s == FOREIGN_KEYS => vec![],
                s if s == ROUTINES => vec![json!({"c0": "dbo", "c1": "usp_CheckIn", "c2": "P"})],
                _ => return Err(Error::Sql(format!("unexpected query: {sql}"))),
            })
        }
    }

    impl Fake {
        fn pools(
            names: &[&str],
        ) -> (
            Pools<Catalog>,
            std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        ) {
            let seen = std::sync::Arc::default();
            let pools = Pools::default();
            for n in names {
                let c = Catalog {
                    seen: std::sync::Arc::clone(&seen),
                };
                pools.insert(Pool::new(n, c, PoolOptions::default()));
            }
            (pools, seen)
        }
    }

    #[test]
    fn collects_every_database_with_catalog_queries_only() {
        let (pools, seen) = Fake::pools(&["SMILE_BO", "SMILE_FO"]);
        let (dbs, failed) = collect_all(&pools);
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(dbs.len(), 2);
        assert_eq!(dbs[0].tables[0].columns[0].name, "Id");
        assert_eq!(dbs[1].routines[0].name, "usp_CheckIn");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 10);
        assert!(
            seen.iter()
                .all(|q| dl_agent_schema::queries::ensure_catalog_only(q).is_ok())
        );
        let body = upload_body(
            "1.0.0",
            &Snapshot {
                server_version: String::new(),
                databases: dbs,
            },
        )
        .unwrap();
        assert!(body.contains("\"schema_hash\""));
    }

    #[test]
    fn positional_rows_keep_order_and_blank_nulls() {
        let row = json!({"c1": "b", "c0": "a", "c2": null, "x": "ignored"});
        assert_eq!(positional(&row).unwrap(), ["a", "b", ""]);
        assert!(positional(&json!([1])).is_err());
    }

    #[test]
    fn server_version_comes_from_the_survey() {
        let s = json!({"SMILE_FO": {"engine": {"version": "10.50.1600.1"}}});
        assert_eq!(server_version(&s, "SMILE_FO"), "10.50.1600.1");
        assert_eq!(server_version(&s, "SMILE_BO"), "");
    }
}
