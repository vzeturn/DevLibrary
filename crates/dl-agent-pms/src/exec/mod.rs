//! Runs ops on the site database: parameterized calls of allow-listed procedures only,
//! one transaction per op, results encoded to JSON exactly as the Go agent does
//! (`internal/exec`). Args, results, request keys and connection strings are never
//! logged.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use dl_json::{Map, Number, Value as J};
use dl_tds::{Param, ParamValue, Value as V};
use dl_time::{Date, DateTime, OffsetDateTime};

use crate::attrs;
use crate::ctx::Ctx;
use crate::db::{self, DbError, Pool, Pools};
use crate::json;
use crate::log::Logger;
use crate::ops::{self, Manifest};
use crate::wire::{self, Op, OpError, OpResult, Row};

/// Rejected: the database refused (THROW, constraint); rolled back.
pub const CODE_SQL_ERROR: &str = "SQL_ERROR";
/// Failed: deadlock victim (1205) after the allowed retries.
pub const CODE_DEADLOCK: &str = "DEADLOCK";
/// Failed: SET LOCK_TIMEOUT ran out (1222).
pub const CODE_LOCK_TIMEOUT: &str = "LOCK_TIMEOUT";
/// Failed: no connection, or the commit's outcome is unknown.
pub const CODE_DB_UNAVAILABLE: &str = "DB_UNAVAILABLE";
/// Rejected: past the row or byte cap; a write is rolled back.
pub const CODE_RESULT_TOO_LARGE: &str = "RESULT_TOO_LARGE";

/// The stub's rejection message.
pub const NOT_IMPLEMENTED_MESSAGE: &str = "not_implemented";

/// Rows per result set.
pub const DEFAULT_MAX_ROWS_PER_SET: usize = 10_000;
/// Encoded bytes per op (one frame is at most 8 MiB).
pub const DEFAULT_MAX_RESULT_BYTES: usize = 7 * 1024 * 1024;

const SQL_DEADLOCK: i32 = 1205;
const SQL_LOCK_TIMEOUT: i32 = 1222;
const LOCK_TIMEOUT_SQL: &str = "SET LOCK_TIMEOUT 10000";
const PROC_GET_LOG: &str = "MM_Op_GetLog";
const PROC_CREATE_LOG: &str = "MM_Op_CreateLog";
const AGENT_ENVELOPE: &str = "agent/1";

/// Runs one op; must honour the context (cancelled at the op's deadline) and never log
/// args or results.
pub trait Executor: Send + Sync {
    /// The outcome; the caller fills in the id.
    fn execute(&self, ctx: &Ctx, op: &Op) -> OpResult;
    /// The highest op contract version implemented.
    fn contract_max(&self) -> i64;
}

/// Reports the highest MM_PMS migration applied (0: cannot tell).
pub trait SchemaVersion: Send + Sync {
    /// The applied version.
    fn schema_version(&self, ctx: &Ctx) -> Result<i64, String>;
}

/// Always [`wire::SCHEMA_VERSION_UNKNOWN`].
pub struct UnknownSchema;

impl SchemaVersion for UnknownSchema {
    fn schema_version(&self, _: &Ctx) -> Result<i64, String> {
        Ok(wire::SCHEMA_VERSION_UNKNOWN)
    }
}

/// Refuses every op; nothing touches a database.
pub struct Stub;

impl Executor for Stub {
    fn execute(&self, _: &Ctx, op: &Op) -> OpResult {
        rejected(&op.id, wire::ERR_OP_REJECTED, NOT_IMPLEMENTED_MESSAGE)
    }
    fn contract_max(&self) -> i64 {
        1
    }
}

fn outcome(id: &str, status: &str, code: &str, message: &str) -> OpResult {
    OpResult {
        id: id.to_owned(),
        status: status.to_owned(),
        sets: Vec::new(),
        rowcounts: Vec::new(),
        outputs: BTreeMap::new(),
        error: Some(OpError {
            code: code.to_owned(),
            sql_number: None,
            message: message.to_owned(),
        }),
        replayed: None,
    }
}

/// A final "nothing changed" result.
pub fn rejected(id: &str, code: &str, message: &str) -> OpResult {
    outcome(id, wire::STATUS_REJECTED, code, message)
}

/// A "could not run" result the cloud may retry.
pub fn failed(id: &str, code: &str, message: &str) -> OpResult {
    outcome(id, wire::STATUS_FAILED, code, message)
}

fn applied(
    sets: Vec<Vec<Row>>,
    rowcounts: Vec<i64>,
    outputs: BTreeMap<String, J>,
    replayed: bool,
) -> OpResult {
    OpResult {
        id: String::new(),
        status: wire::STATUS_APPLIED.to_owned(),
        sets,
        rowcounts,
        outputs,
        error: None,
        replayed: Some(replayed),
    }
}

/// The database whose pool runs an op target (BO is read through MM_PMS synonyms).
pub fn target_db(target: &str) -> &'static str {
    match target {
        "FO" => db::DB_FO,
        "POS" => db::DB_POS,
        _ => db::DB_PMS,
    }
}

/// `^[A-Za-z0-9][A-Za-z0-9._:-]{0,99}$` (REQUEST_KEY_RE, packages/shared ops/types.ts).
fn valid_request_key(k: &str) -> bool {
    let b = k.as_bytes();
    (1..=100).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b[1..]
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b':' | b'-'))
}

// ---- errors ----------------------------------------------------------------------

#[derive(Debug)]
enum ExecErr {
    Db(DbError),
    Commit,
    TooLarge,
    ReplayConflict,
    LogCorrupt,
    NotAllowed,
    Encode(String),
}

impl From<DbError> for ExecErr {
    fn from(e: DbError) -> ExecErr {
        ExecErr::Db(e)
    }
}

impl ExecErr {
    fn sql_number(&self) -> Option<i32> {
        match self {
            ExecErr::Db(e) => e.number(),
            _ => None,
        }
    }
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn with_number(mut r: OpResult, n: i32) -> OpResult {
    if let Some(e) = &mut r.error {
        e.sql_number = Some(i64::from(n));
    }
    r
}

/// The error of a finished attempt as a result frame. Driver text is never forwarded
/// (it can carry host names); only SQL Server's own message for a refused statement is.
fn classify(id: &str, ctx: &Ctx, err: &ExecErr) -> OpResult {
    match err {
        ExecErr::Commit => return failed(id, CODE_DB_UNAVAILABLE, "commit_outcome_unknown"),
        ExecErr::TooLarge => return rejected(id, CODE_RESULT_TOO_LARGE, "result_too_large"),
        ExecErr::ReplayConflict => {
            return rejected(id, wire::ERR_OP_REJECTED, "request_key_conflict");
        }
        ExecErr::LogCorrupt => return rejected(id, wire::ERR_OP_REJECTED, "op_log_corrupt"),
        ExecErr::NotAllowed => return rejected(id, wire::ERR_OP_REJECTED, "proc_not_allowed"),
        ExecErr::Encode(m) => {
            return rejected(id, wire::ERR_OP_REJECTED, &format!("result_encoding: {m}"));
        }
        ExecErr::Db(_) => {}
    }
    if ctx.is_done() {
        return failed(
            id,
            wire::ERR_OP_TIMEOUT,
            "deadline passed before the op finished",
        );
    }
    let ExecErr::Db(DbError::Tds(dl_tds::Error::Server {
        number,
        class,
        message,
        ..
    })) = err
    else {
        return failed(id, CODE_DB_UNAVAILABLE, "database_unavailable");
    };
    match *number {
        SQL_DEADLOCK => with_number(failed(id, CODE_DEADLOCK, "deadlock_victim"), *number),
        SQL_LOCK_TIMEOUT => with_number(failed(id, CODE_LOCK_TIMEOUT, "lock_timeout"), *number),
        n if *class >= 20 => {
            with_number(failed(id, CODE_DB_UNAVAILABLE, "database_unavailable"), n)
        }
        n => with_number(rejected(id, CODE_SQL_ERROR, &truncate(message, 4000)), n),
    }
}

mod bind;
mod encode;
mod replay;

use bind::*;
pub use encode::*;
use replay::*;

// ---- the executor ------------------------------------------------------------------

/// Limits of one op.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Rows per set.
    pub max_rows_per_set: usize,
    /// Encoded bytes per op.
    pub max_result_bytes: usize,
    /// Total tries of a retry-safe op on deadlock (1205).
    pub deadlock_attempts: u32,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_rows_per_set: DEFAULT_MAX_ROWS_PER_SET,
            max_result_bytes: DEFAULT_MAX_RESULT_BYTES,
            deadlock_attempts: 3,
        }
    }
}

/// Runs manifest ops on the site's SQL Server.
pub struct SqlExecutor {
    /// The op manifest.
    pub manifest: Arc<Manifest>,
    /// One pool per database; a missing one fails its ops with DB_UNAVAILABLE.
    pub dbs: Pools,
    /// Limits.
    pub limits: Limits,
    /// Logger.
    pub log: Logger,
}

/// 50 ms doubled per try with ±50 % jitter, so two victims do not collide again.
fn backoff(try_no: u32) -> Duration {
    let base = 50u64 << (try_no - 1).min(10);
    Duration::from_millis(base / 2 + dl_rand::range(0..base))
}

fn rollback(conn: &mut db::Conn<'_>) {
    let ctx = Ctx::background().with_timeout(Duration::from_secs(10));
    let _ = conn.batch(&ctx, "IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION");
}

impl SqlExecutor {
    fn call(
        &self,
        ctx: &Ctx,
        conn: &mut db::Conn<'_>,
        c: &BoundCall,
        b: &mut SetBuilder,
        outputs: &mut BTreeMap<String, J>,
    ) -> Result<(), ExecErr> {
        if !self.manifest.allowed(&c.proc_name) {
            return Err(ExecErr::NotAllowed);
        }
        let r = conn.call(ctx, &c.proc_name, &c.params)?;
        b.read_sets(&r)?;
        for o in &c.outputs {
            let v = r
                .output_params()
                .iter()
                .find(|(n, _)| n.trim_start_matches('@').eq_ignore_ascii_case(&o.name))
                .map(|(_, v)| v);
            outputs.insert(o.name.clone(), encode_output(o, v)?);
        }
        outputs.insert(
            "ReturnValue".to_owned(),
            num(r.return_status().unwrap_or(0).to_string()),
        );
        Ok(())
    }

    fn get_log(
        &self,
        ctx: &Ctx,
        conn: &mut db::Conn<'_>,
        key: &str,
    ) -> Result<Option<LogRow>, ExecErr> {
        let r = conn.call(
            ctx,
            PROC_GET_LOG,
            &[Param::new(
                "RequestKey",
                ParamValue::VarChar(Some(key.into())),
            )],
        )?;
        let Some(row) = r.sets().iter().find_map(|s| s.rows().first()) else {
            return Ok(None);
        };
        let s = |i: usize| -> Result<String, ExecErr> {
            row.get::<String>(i)
                .map_err(|e| ExecErr::Db(DbError::Tds(e)))
        };
        Ok(Some(LogRow {
            op: s(1)?,
            status: s(2)?,
            hash: s(3)?,
            json: s(4)?,
        }))
    }

    /// One try: one transaction, every call in order, commit or roll back.
    fn attempt(
        &self,
        ctx: &Ctx,
        pool: &Pool,
        spec: &ops::Op,
        calls: &[BoundCall],
        key: &str,
    ) -> Result<OpResult, ExecErr> {
        let mut conn = pool.get(ctx)?;
        let res = self.attempt_on(ctx, &mut conn, spec, calls, key);
        match &res {
            Ok(_) => {}
            Err(ExecErr::Commit) => {}
            Err(_) => rollback(&mut conn),
        }
        res
    }

    fn attempt_on(
        &self,
        ctx: &Ctx,
        conn: &mut db::Conn<'_>,
        spec: &ops::Op,
        calls: &[BoundCall],
        key: &str,
    ) -> Result<OpResult, ExecErr> {
        conn.batch(ctx, &format!("BEGIN TRANSACTION; {LOCK_TIMEOUT_SQL}"))?;
        if !key.is_empty()
            && let Some(prior) = self.get_log(ctx, conn, key)?
        {
            let s = replay(&prior, &spec.name)?;
            rollback(conn);
            return Ok(applied(s.sets, s.rowcounts, s.outputs, true));
        }
        let mut b = SetBuilder {
            max_rows: self.limits.max_rows_per_set,
            max_bytes: self.limits.max_result_bytes,
            bytes: 0,
            sets: Vec::new(),
            rowcounts: Vec::new(),
        };
        let mut outputs = BTreeMap::new();
        for c in calls {
            self.call(ctx, conn, c, &mut b, &mut outputs)?;
        }
        if spec.kind == ops::KIND_READ {
            rollback(conn); // ends the read
            return Ok(applied(b.sets, b.rowcounts, outputs, false));
        }
        if !key.is_empty() {
            let stored = store_json(&b.sets, &b.rowcounts, &outputs);
            conn.call(
                ctx,
                PROC_CREATE_LOG,
                &[
                    Param::new("RequestKey", ParamValue::VarChar(Some(key.into()))),
                    Param::new("Op", ParamValue::VarChar(Some(spec.name.clone()))),
                    Param::new("Status", ParamValue::VarChar(Some("applied".into()))),
                    Param::new(
                        "ResultHash",
                        ParamValue::VarChar(Some(crate::sha256_hex(stored.as_bytes()))),
                    ),
                    Param::new("ResultJson", ParamValue::String(Some(stored))),
                ],
            )?;
        }
        if conn.batch(ctx, "COMMIT TRANSACTION").is_err() {
            return Err(ExecErr::Commit);
        }
        Ok(applied(b.sets, b.rowcounts, outputs, false))
    }
}

impl Executor for SqlExecutor {
    fn contract_max(&self) -> i64 {
        self.manifest.contract_max()
    }

    fn execute(&self, ctx: &Ctx, op: &Op) -> OpResult {
        let Some(spec) = self.manifest.lookup(&op.op) else {
            return rejected(&op.id, wire::ERR_OP_REJECTED, "unknown_op");
        };
        if spec.calls.is_empty() {
            return rejected(&op.id, wire::ERR_OP_REJECTED, "in_process_only");
        }
        if op.contract_ver != spec.contract_ver {
            let rel = if op.contract_ver > spec.contract_ver {
                '>'
            } else {
                '<'
            };
            return rejected(
                &op.id,
                wire::ERR_CONTRACT_UNSUPPORTED,
                &format!(
                    "contract_ver {} {rel} {}",
                    op.contract_ver, spec.contract_ver
                ),
            );
        }
        let key = op.request_key.clone().unwrap_or_default();
        if op.request_key.is_some() {
            if spec.kind == ops::KIND_READ {
                return rejected(&op.id, wire::ERR_OP_REJECTED, "request_key_on_read");
            }
            if !valid_request_key(&key) {
                return rejected(&op.id, wire::ERR_OP_REJECTED, "invalid_request_key");
            }
            if target_db(&spec.target) != db::DB_PMS {
                return rejected(&op.id, wire::ERR_OP_REJECTED, "request_key_needs_pms");
            }
        }
        let calls = match bind_args(spec, &op.args) {
            Ok(c) => c,
            Err(e) => return rejected(&op.id, wire::ERR_OP_REJECTED, &e),
        };
        let dbname = target_db(&spec.target);
        let Some(pool) = self.dbs.get(dbname) else {
            return failed(
                &op.id,
                CODE_DB_UNAVAILABLE,
                &format!("no connection configured for {dbname}"),
            );
        };
        let ctx = ctx.with_timeout(Duration::from_millis(spec.timeout_ms.max(0) as u64));
        let attempts = if spec.kind == ops::KIND_READ || spec.idempotent || !key.is_empty() {
            self.limits.deadlock_attempts.max(1)
        } else {
            1
        };
        let mut try_no = 1;
        loop {
            let err = match self.attempt(&ctx, pool, spec, &calls, &key) {
                Ok(mut res) => {
                    res.id = op.id.clone();
                    return res;
                }
                Err(e) => e,
            };
            if err.sql_number() == Some(SQL_DEADLOCK) && try_no < attempts && !ctx.is_done() {
                self.log.debug(
                    "exec.deadlock.retry",
                    attrs!["op_id" => &op.id, "op" => &op.op, "attempt" => try_no],
                );
                if ctx.sleep(backoff(try_no)) {
                    try_no += 1;
                    continue;
                }
            }
            return classify(&op.id, &ctx, &err);
        }
    }
}

#[cfg(test)]
mod tests;
