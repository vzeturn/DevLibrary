//! The `op_sweep` command: runs every operation of the catalog against the hotel's real PMS
//! and reports, per operation, whether the procedure ran (`ok`), failed with a SQL error
//! (classified) or was skipped, and why.
//!
//! Every call runs inside a transaction that is rolled back, reads included (some read
//! procedures write, e.g. `group.auto-assign`), so a sweep leaves the PMS as it was. Only
//! `commit: true` together with an explicit `only` list commits. Parameters are type-valid
//! placeholders (required ones a neutral value, optional ones SQL NULL) unless the command
//! gives them (`params: {"<op>": {"<name>": "<value>" | null}}`). Messages are kept only for
//! the classes that point at the install (schema, permission, other), never for business or
//! data errors, which may echo PMS values.

use std::time::{Duration, Instant};

use dl_json::{Map, Value};
use dl_tds::{Client, Error as TdsError};

use crate::agent::tds::{self, TdsExecutor};
use crate::contracts::catalog::{Operation, OperationKind, ParamType, catalog};
use crate::contracts::messages::Params;

/// The per-operation timeout when the command names none.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);
/// The longest message kept per operation (characters).
const MESSAGE_CHARS: usize = 240;
/// Lock waits inside a sweep: a busy PMS must not hold the sweep.
const LOCK_TIMEOUT_MS: u32 = 5000;

/// What the command asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct SweepOptions {
    /// Commit instead of rolling back (needs `only`).
    pub commit: bool,
    /// Only these operations (all when empty).
    pub only: Vec<String>,
    /// Operations to leave out.
    pub skip: Vec<String>,
    /// Parameter values per operation, over the placeholders.
    pub params: Map,
    /// Per-operation timeout.
    pub timeout: Duration,
    /// The hotel's date, `yyyy-MM-dd` (the placeholder for dates).
    pub today: String,
}

impl SweepOptions {
    /// Reads the command arguments.
    ///
    /// # Errors
    /// A wrong type, an unknown operation, or `commit` without `only`.
    pub fn from_args(args: &Value, today: &str) -> Result<SweepOptions, String> {
        let names = |key: &str| -> Result<Vec<String>, String> {
            match args.get(key) {
                None | Some(Value::Null) => Ok(Vec::new()),
                Some(Value::Array(a)) => a
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_string)
                            .ok_or_else(|| format!("{key} must list operation names"))
                    })
                    .collect(),
                Some(_) => Err(format!("{key} must be an array of operation names")),
            }
        };
        let commit = match args.get("commit") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err("commit must be true or false".into()),
        };
        let only = names("only")?;
        let skip = names("skip")?;
        let params = match args.get("params") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(_) => return Err("params must be an object of operation -> parameters".into()),
        };
        let timeout = match args.get("timeout_seconds") {
            None | Some(Value::Null) => DEFAULT_TIMEOUT,
            Some(v) => v
                .as_u64()
                .filter(|n| (1..=300).contains(n))
                .map(Duration::from_secs)
                .ok_or("timeout_seconds must be 1..300")?,
        };
        let cat = catalog().map_err(|e| e.to_string())?;
        let known = |n: &str| cat.operations().iter().any(|o| o.name == n);
        let params_ops: Vec<String> = params.iter().map(|(k, _)| k.to_string()).collect();
        for n in only.iter().chain(&skip).chain(&params_ops) {
            if !known(n) {
                return Err(format!("unknown operation {n}"));
            }
        }
        if commit && only.is_empty() {
            return Err("commit: true needs an explicit only list".into());
        }
        Ok(SweepOptions {
            commit,
            only,
            skip,
            params,
            timeout,
            today: today.to_string(),
        })
    }
}

/// A neutral, type-valid value for a required parameter.
fn placeholder(ty: ParamType, today: &str) -> String {
    match ty {
        ParamType::String | ParamType::Int | ParamType::Long | ParamType::Decimal => "0".into(),
        ParamType::Date => today.to_string(),
        ParamType::DateTime => format!("{today}T00:00:00"),
        ParamType::Bool => "false".into(),
        ParamType::Xml => "<root/>".into(),
        ParamType::Binary => "AA==".into(),
    }
}

/// The parameters the sweep sends for `op`.
///
/// # Errors
/// A given value that is not a string or null.
pub fn sweep_params(op: &Operation, opts: &SweepOptions) -> Result<Params, String> {
    let given = match opts.params.get(&op.name) {
        None | Some(Value::Null) => None,
        Some(Value::Object(m)) => Some(m),
        Some(_) => return Err(format!("params.{} must be an object", op.name)),
    };
    let mut out = Params::new();
    for p in &op.parameters {
        let value = match given.and_then(|m| m.get(&p.name)) {
            Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            Some(Value::Bool(b)) => Some(b.to_string()),
            Some(_) => return Err(format!("params.{}.{} must be a string", op.name, p.name)),
            None if p.required => Some(placeholder(p.ty, &opts.today)),
            None => None,
        };
        out.insert(p.name.clone(), value);
    }
    Ok(out)
}

/// The class of a SQL error number: `business` (raised by the procedure), `schema` (an
/// object, column or parameter the procedure needs is missing), `permission`, `data` (the
/// placeholder values broke a rule), `timeout` or `other`.
pub fn classify(number: i32) -> &'static str {
    match number {
        n if n >= 50000 => "business",
        207 | 208 | 213 | 201 | 2812 | 4121 | 8144 | 8145 | 4104 | 1911 => "schema",
        229 | 230 | 262 | 297 | 300 | 916 | 15247 | 4064 => "permission",
        245 | 241 | 242 | 515 | 547 | 2601 | 2627 | 8114 | 8115 | 8152 | 2628 | 9436 | 6602 => {
            "data"
        }
        -2 | 1222 => "timeout",
        _ => "other",
    }
}

fn clip(s: &str) -> String {
    match s.char_indices().nth(MESSAGE_CHARS) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// Runs the sweep on a fresh session of `connection`.
///
/// # Errors
/// The PMS cannot be reached (a message without the connection string).
pub fn sweep(connection: &str, opts: &SweepOptions) -> Result<Value, String> {
    let open = || {
        tds::connect(connection, None)
            .map_err(|e| format!("cannot connect: {}", tds::exec_error(&e)))
    };
    let mut client = open()?;
    let cat = catalog().map_err(|e| e.to_string())?;
    let started = Instant::now();
    let mut results = Vec::new();
    let mut counts = Map::new();
    for op in cat.operations() {
        if !opts.only.is_empty() && !opts.only.contains(&op.name) {
            continue;
        }
        let row = if opts.skip.contains(&op.name) {
            skipped(op, "left out by the command")
        } else {
            let row = run_one(&mut client, op, opts);
            if client.is_broken() {
                // A dropped session rolls its transaction back on the server.
                client = open()?;
            }
            row
        };
        let result = row.get("class").or_else(|| row.get("result"));
        if let Some(k) = result.and_then(Value::as_str) {
            let n = counts.get(k).and_then(Value::as_u64).unwrap_or(0) + 1;
            counts.insert(k.to_string(), n);
        }
        results.push(Value::Object(row));
    }
    let mut out = Map::new();
    out.insert("catalog_version", i64::from(cat.version));
    out.insert("commit", opts.commit);
    out.insert("total", results.len() as u64);
    out.insert("counts", Value::Object(counts));
    out.insert("ms", started.elapsed().as_millis() as u64);
    out.insert("ops", Value::Array(results));
    Ok(Value::Object(out))
}

fn head(op: &Operation) -> Map {
    let mut m = Map::new();
    m.insert("op", op.name.as_str());
    m.insert(
        "kind",
        match op.kind {
            OperationKind::Read => "read",
            OperationKind::Write => "write",
        },
    );
    m.insert("procedure", op.procedure.as_str());
    m
}

fn skipped(op: &Operation, why: &str) -> Map {
    let mut m = head(op);
    m.insert("result", "skipped");
    m.insert("why", why);
    m
}

/// One operation inside its own transaction.
fn run_one(client: &mut Client, op: &Operation, opts: &SweepOptions) -> Map {
    let params = match sweep_params(op, opts) {
        Ok(p) => p,
        Err(e) => return skipped(op, &e),
    };
    let sql_params = match TdsExecutor::params(op, &params) {
        Ok(p) => p,
        Err(e) => return skipped(op, &e.to_string()),
    };
    let mut m = head(op);
    let started = Instant::now();
    let begin = client.set_timeout(Some(opts.timeout)).and_then(|()| {
        client
            .batch(&format!(
                "SET LOCK_TIMEOUT {LOCK_TIMEOUT_MS}; BEGIN TRANSACTION"
            ))
            .map(|_| ())
    });
    if let Err(e) = begin {
        m.insert("result", "error");
        m.insert(
            "why",
            format!("could not open the transaction: {}", tds::exec_error(&e)),
        );
        return m;
    }
    let called = client.call(&op.procedure, &sql_params);
    let end = if opts.commit {
        "IF @@TRANCOUNT > 0 COMMIT TRANSACTION"
    } else {
        "IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION"
    };
    let closed = client.batch(end);
    m.insert("ms", started.elapsed().as_millis() as u64);
    match called {
        Ok(results) => {
            let sets: Vec<_> = results
                .sets()
                .iter()
                .filter(|s| !s.columns().is_empty())
                .collect();
            m.insert("result", "ok");
            m.insert("sets", sets.len() as u64);
            m.insert("rows", sets.iter().map(|s| s.len() as u64).sum::<u64>());
        }
        Err(e) => sql_error(&mut m, &e),
    }
    match closed {
        Ok(_) => {
            m.insert(
                "transaction",
                if opts.commit {
                    "committed"
                } else {
                    "rolled_back"
                },
            );
        }
        Err(e) => {
            // The session is dropped by the caller when broken; otherwise make sure.
            dl_log::warn!("op_sweep could not end its transaction"; op = %op.name, error = %e);
            m.insert("transaction", "dropped");
            if !client.is_broken()
                && let Err(e) = client.batch("IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION")
            {
                dl_log::warn!("op_sweep rollback retry failed"; op = %op.name, error = %e);
            }
        }
    }
    m
}

fn sql_error(m: &mut Map, e: &TdsError) {
    m.insert("result", "sql_error");
    match e {
        TdsError::Server {
            number, message, ..
        } => {
            let class = classify(*number);
            m.insert("class", class);
            m.insert("number", i64::from(*number));
            if matches!(class, "schema" | "permission" | "other") {
                m.insert("message", clip(message));
            }
        }
        TdsError::Timeout => {
            m.insert("class", "timeout");
        }
        other => {
            m.insert("class", "other");
            m.insert("message", clip(&tds::exec_error(other).to_string()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(name: &str) -> &'static Operation {
        catalog()
            .unwrap()
            .operations()
            .iter()
            .find(|o| o.name == name)
            .unwrap()
    }

    #[test]
    fn options_need_only_to_commit_and_known_names() {
        let o = SweepOptions::from_args(&Value::Null, "2026-11-19").unwrap();
        assert!(!o.commit && o.only.is_empty());
        assert_eq!(o.timeout, DEFAULT_TIMEOUT);
        let bad = [
            r#"{"commit":true}"#,
            r#"{"only":["nope.op"]}"#,
            r#"{"params":{"nope":{}}}"#,
            r#"{"timeout_seconds":0}"#,
            r#"{"commit":"yes"}"#,
        ];
        for b in bad {
            assert!(
                SweepOptions::from_args(&dl_json::parse(b).unwrap(), "2026-11-19").is_err(),
                "{b}"
            );
        }
        let ok = dl_json::parse(r#"{"commit":true,"only":["checkin.online"]}"#).unwrap();
        assert!(SweepOptions::from_args(&ok, "2026-11-19").unwrap().commit);
    }

    #[test]
    fn placeholders_fill_required_parameters_and_null_the_rest() {
        let args =
            dl_json::parse(r#"{"params":{"folio.room-status":{"RoomCode":"101"}}}"#).unwrap();
        let o = SweepOptions::from_args(&args, "2026-11-19").unwrap();
        let p = sweep_params(op("folio.room-status"), &o).unwrap();
        assert_eq!(p.get("FolioNum"), Some(Some("0")));
        assert_eq!(p.get("RoomCode"), Some(Some("101")));
        let p = sweep_params(op("report.occupancy"), &o).unwrap();
        assert_eq!(p.get("from"), Some(Some("2026-11-19")));
        let p = sweep_params(op("folio.search"), &o).unwrap();
        assert_eq!(
            p.get("sInfo"),
            Some(None),
            "optional parameters are SQL NULL"
        );
        for o2 in catalog().unwrap().operations() {
            let p = sweep_params(o2, &o).unwrap();
            TdsExecutor::params(o2, &p).unwrap_or_else(|e| panic!("{}: {e}", o2.name));
        }
    }

    #[test]
    fn sql_errors_are_classified() {
        assert_eq!(classify(50001), "business");
        assert_eq!(classify(208), "schema");
        assert_eq!(classify(2812), "schema");
        assert_eq!(classify(916), "permission");
        assert_eq!(classify(229), "permission");
        assert_eq!(classify(2627), "data");
        assert_eq!(classify(1222), "timeout");
        assert_eq!(classify(102), "other");
    }
}
