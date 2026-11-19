//! SQLite [`CaptureState`] (`State/SqliteCaptureState.cs`): `capture.db` in the agent data
//! directory. Same schema, pragmas and statements as the C# agent, so a `capture.db` the
//! C# agent wrote is read and continued unchanged (and the other way round).
//!
//! ```text
//! stream_seq (stream TEXT PRIMARY KEY, next_seq INTEGER NOT NULL)
//! cursor     (name TEXT PRIMARY KEY, value TEXT NOT NULL)
//! shadow     (tbl, key, hash, bucket TEXT NOT NULL, ref TEXT, PRIMARY KEY (tbl, key)) WITHOUT ROWID
//! ix_shadow_bucket ON shadow (tbl, bucket)
//! ```

use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use dl_sqlite::{Connection, OpenFlags, ToSql, TransactionBehavior};

use super::error::Result;
use super::state::{CaptureState, Shadow, ShadowEntry, StateCommit};
use crate::util::ctx::Ctx;

/// Keys per `IN (...)` lookup.
const KEY_CHUNK: usize = 400;

/// The pragmas the C# constructor runs.
pub const PRAGMAS: &str = "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;";

/// The schema script the C# constructor runs.
pub const SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS stream_seq (stream TEXT PRIMARY KEY, next_seq INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS cursor (name TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS shadow (
    tbl TEXT NOT NULL, key TEXT NOT NULL, hash TEXT NOT NULL, bucket TEXT NOT NULL, ref TEXT,
    PRIMARY KEY (tbl, key)) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS ix_shadow_bucket ON shadow (tbl, bucket);";

/// Microsoft.Data.Sqlite retries a busy database for the command timeout (30 s).
const BUSY_TIMEOUT: Duration = Duration::from_secs(30);

/// SQLite [`CaptureState`]. WAL journal so a crash never leaves a half-applied commit; all
/// writes of one commit share a transaction.
pub struct SqliteCaptureState {
    db: Mutex<Connection>,
}

impl std::fmt::Debug for SqliteCaptureState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteCaptureState").finish_non_exhaustive()
    }
}

fn check(ctx: &Ctx) -> Result<()> {
    if ctx.is_done() {
        return Err(super::error::CaptureError::cancelled());
    }
    Ok(())
}

impl SqliteCaptureState {
    /// Opens (creating when absent) the state database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<SqliteCaptureState> {
        let db = Connection::open(path, OpenFlags::READ_WRITE_CREATE)?;
        db.busy_timeout(BUSY_TIMEOUT)?;
        db.execute_batch(PRAGMAS)?;
        db.execute_batch(SCHEMA)?;
        Ok(SqliteCaptureState { db: Mutex::new(db) })
    }

    fn with<T>(&self, ctx: &Ctx, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        check(ctx)?;
        let mut db = self.db.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut db)
    }

    fn pairs(db: &Connection, sql: &str, params: &[&dyn ToSql]) -> Result<Vec<(String, String)>> {
        Ok(db.query(sql, params, |r| {
            Ok((r.get::<String>(0)?, r.get::<String>(1)?))
        })?)
    }
}

impl CaptureState for SqliteCaptureState {
    fn peek_next_seq(&self, stream: &str, ctx: &Ctx) -> Result<i64> {
        self.with(ctx, |db| {
            let rows = db.query(
                "SELECT next_seq FROM stream_seq WHERE stream = $s",
                &[&stream],
                |r| r.get::<dl_sqlite::Value>(0),
            )?;
            Ok(match rows.first() {
                Some(dl_sqlite::Value::Integer(n)) => *n,
                _ => 1,
            })
        })
    }

    fn get_cursor(&self, name: &str, ctx: &Ctx) -> Result<Option<String>> {
        self.with(ctx, |db| {
            let rows = db.query("SELECT value FROM cursor WHERE name = $n", &[&name], |r| {
                r.get::<dl_sqlite::Value>(0)
            })?;
            Ok(match rows.into_iter().next() {
                Some(dl_sqlite::Value::Text(s)) => Some(s),
                _ => None,
            })
        })
    }

    fn get_shadow(&self, table: &str, bucket: Option<&str>, ctx: &Ctx) -> Result<Shadow> {
        self.with(ctx, |db| {
            let rows = match bucket {
                None => Self::pairs(db, "SELECT key, hash FROM shadow WHERE tbl = $t", &[&table])?,
                Some(b) => Self::pairs(
                    db,
                    "SELECT key, hash FROM shadow WHERE tbl = $t AND bucket = $b",
                    &[&table, &b],
                )?,
            };
            Ok(rows.into_iter().collect())
        })
    }

    fn get_shadow_entries(&self, table: &str, bucket: &str, ctx: &Ctx) -> Result<Vec<ShadowEntry>> {
        self.with(ctx, |db| {
            Ok(db.query(
                "SELECT key, hash, bucket, ref FROM shadow WHERE tbl = $t AND bucket = $b ORDER BY key",
                &[&table, &bucket],
                |r| {
                    Ok(ShadowEntry {
                        key: r.get(0)?,
                        hash: r.get(1)?,
                        bucket: r.get(2)?,
                        ref_: r.get(3)?,
                    })
                },
            )?)
        })
    }

    fn get_shadow_by_keys(&self, table: &str, keys: &[String], ctx: &Ctx) -> Result<Shadow> {
        self.with(ctx, |db| {
            let mut distinct: Vec<&str> = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for k in keys {
                if seen.insert(k.as_str()) {
                    distinct.push(k);
                }
            }
            let mut result = Shadow::new();
            for chunk in distinct.chunks(KEY_CHUNK) {
                let names: Vec<String> = (0..chunk.len()).map(|i| format!("$k{i}")).collect();
                let sql = format!(
                    "SELECT key, hash FROM shadow WHERE tbl = $t AND key IN ({})",
                    names.join(",")
                );
                let mut params: Vec<&dyn ToSql> = Vec::with_capacity(chunk.len() + 1);
                params.push(&table);
                for k in chunk {
                    params.push(k);
                }
                for (k, h) in Self::pairs(db, &sql, &params)? {
                    result.insert(k, h);
                }
            }
            Ok(result)
        })
    }

    fn get_buckets(&self, table: &str, ctx: &Ctx) -> Result<Vec<String>> {
        self.with(ctx, |db| {
            Ok(db.query(
                "SELECT DISTINCT bucket FROM shadow WHERE tbl = $t ORDER BY bucket",
                &[&table],
                |r| r.get::<String>(0),
            )?)
        })
    }

    fn count_shadow(&self, table: &str, ctx: &Ctx) -> Result<usize> {
        self.with(ctx, |db| {
            let rows = db.query(
                "SELECT COUNT(*) FROM shadow WHERE tbl = $t",
                &[&table],
                |r| r.get::<i64>(0),
            )?;
            Ok(rows.first().map_or(0, |&n| usize::try_from(n).unwrap_or(0)))
        })
    }

    fn scan_shadow(&self, table: &str, visit: &mut dyn FnMut(&str, &str), ctx: &Ctx) -> Result<()> {
        self.with(ctx, |db| {
            // One statement: SQLite reads it from a single WAL snapshot, so a concurrent
            // capture commit is either wholly in or out.
            db.query(
                "SELECT key, hash FROM shadow WHERE tbl = $t",
                &[&table],
                |r| {
                    visit(&r.get::<String>(0)?, &r.get::<String>(1)?);
                    Ok(())
                },
            )?;
            Ok(())
        })
    }

    fn commit(&self, commit: &StateCommit, ctx: &Ctx) -> Result<()> {
        self.with(ctx, |db| {
            let tx = db.transaction_with(TransactionBehavior::Immediate)?;
            if let (Some(stream), Some(seq)) = (&commit.stream, commit.seq) {
                tx.execute(
                    "INSERT INTO stream_seq (stream, next_seq) VALUES ($s, $n) ON CONFLICT(stream) DO UPDATE SET next_seq = $n",
                    &[stream, &(seq + 1)],
                )?;
            }
            for w in &commit.shadow {
                for key in &w.delete_keys {
                    tx.execute(
                        "DELETE FROM shadow WHERE tbl = $t AND key = $k",
                        &[&w.table, key],
                    )?;
                }
                for e in &w.upserts {
                    tx.execute(
                        "INSERT INTO shadow (tbl, key, hash, bucket, ref) VALUES ($t, $k, $h, $b, $r) ON CONFLICT(tbl, key) DO UPDATE SET hash = $h, bucket = $b, ref = $r",
                        &[&w.table, &e.key, &e.hash, &e.bucket, &e.ref_],
                    )?;
                }
            }
            for (name, value) in commit.cursors.iter() {
                match value {
                    None => {
                        tx.execute("DELETE FROM cursor WHERE name = $n", &[&name])?;
                    }
                    Some(v) => {
                        tx.execute(
                            "INSERT INTO cursor (name, value) VALUES ($n, $v) ON CONFLICT(name) DO UPDATE SET value = $v",
                            &[&name, v],
                        )?;
                    }
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    fn reset_table(&self, table: &str, ctx: &Ctx) -> Result<()> {
        self.with(ctx, |db| {
            db.execute("DELETE FROM shadow WHERE tbl = $t", &[&table])?;
            Ok(())
        })
    }
}
