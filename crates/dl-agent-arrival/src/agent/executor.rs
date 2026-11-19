//! `IPmsExecutor`: the only thing that touches the PMS. It calls stored procedures by their
//! catalog name (never SQL text from the wire), binds only catalog parameters, returns only
//! catalog-projected columns and caps rows, so nothing beyond what the cloud needs leaves the
//! hotel.

use std::sync::{Arc, Mutex, PoisonError};

use dl_json::Value;

use crate::contracts::projection::{self, Projection};
use crate::contracts::{Operation, Params, PmsRow};

/// Rows of each result set, in catalog order.
pub type Sets = Vec<Vec<PmsRow>>;

/// Why a PMS call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecError {
    /// SQL Server answered with error `number` (`-2` = the command timed out).
    Sql {
        /// The error number.
        number: i32,
    },
    /// The database cannot be reached (connect failed, connection lost). Sent as error `-1`.
    Unavailable(String),
    /// Anything else (answered as "Agent error").
    Other(String),
}

impl ExecError {
    /// The SQL error number the cloud sees.
    pub fn wire_number(&self) -> Option<i32> {
        match self {
            ExecError::Sql { number } => Some(*number),
            ExecError::Unavailable(_) => Some(-1),
            ExecError::Other(_) => None,
        }
    }
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecError::Sql { number } => write!(f, "SQL error {number}"),
            ExecError::Unavailable(m) => write!(f, "PMS unavailable: {m}"),
            ExecError::Other(m) => f.write_str(m),
        }
    }
}

/// Runs catalog operations against the PMS.
pub trait Executor: Send + Sync {
    /// `ExecuteAsync`: runs a validated operation; `params` use the catalog spelling (absent =
    /// the procedure default, `None` = SQL NULL).
    ///
    /// # Errors
    /// [`ExecError`].
    fn execute(
        &self,
        op: &Operation,
        params: &Params,
        projection: Option<&Projection>,
    ) -> Result<Sets, ExecError>;

    /// `GetSchemaFingerprintAsync`: a hash of the catalog procedures' parameter signatures, so
    /// the cloud spots a PMS upgrade (`None` when it cannot be read).
    fn schema_fingerprint(&self) -> Option<String>;

    /// `DescribeLockHoldersAsync`: for the log after a lock timeout, the sessions holding the
    /// oldest open transactions (`None` when the login may not read server state).
    fn describe_lock_holders(&self) -> Option<String> {
        None
    }
}

/// `Project`: a row restricted to `columns` (case-insensitive; `None` = all), duplicates
/// collapsing like the .NET dictionaries (last value, first position).
pub fn project(raw: Vec<(String, Value)>, columns: Option<&[String]>) -> PmsRow {
    let mut row = PmsRow::new();
    let mut ordinal: Vec<(String, Value)> = Vec::with_capacity(raw.len());
    for (name, value) in raw {
        match ordinal.iter_mut().find(|(n, _)| *n == name) {
            Some(slot) => slot.1 = value,
            None => ordinal.push((name, value)),
        }
    }
    for (name, value) in ordinal {
        if columns.is_some_and(|cols| !cols.iter().any(|c| c.eq_ignore_ascii_case(&name))) {
            continue;
        }
        row.insert(name, value);
    }
    row
}

/// The effective projection of each result set of `op`.
pub fn effective_columns(
    op: &Operation,
    projection: Option<&Projection>,
    index: usize,
) -> Option<Vec<String>> {
    projection::effective(op, projection, index)
}

/// `Printable`: `?` for null, trimmed, at most 48 characters, control characters as `?`.
pub fn printable(value: Option<&str>) -> String {
    value
        .unwrap_or("?")
        .trim()
        .chars()
        .take(48)
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// `PmsConnectionProbe.EngineName`: the SQL Server release of a `ProductVersion` (`2008 R2`).
pub fn engine_name(version: &str) -> &'static str {
    match version.split('.').next().unwrap_or("") {
        "9" => "2005",
        "10" if version.starts_with("10.5") => "2008 R2",
        "10" => "2008",
        "11" => "2012",
        "12" => "2014",
        "13" => "2016",
        "14" => "2017",
        "15" => "2019",
        "16" => "2022",
        "17" => "2025",
        _ => "unknown",
    }
}

/// A scripted executor for tests: answers from a queue, records calls.
#[derive(Default)]
pub struct FakeExecutor {
    /// Answers handed out in order (the last one repeats).
    pub answers: Mutex<Vec<Result<Sets, ExecError>>>,
    /// `(operation, parameters)` of every call.
    pub calls: Mutex<Vec<(String, Params)>>,
    /// What [`Executor::schema_fingerprint`] returns.
    pub fingerprint: Option<String>,
    /// Delay of each call.
    pub delay: std::time::Duration,
}

impl std::fmt::Debug for FakeExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeExecutor").finish_non_exhaustive()
    }
}

impl FakeExecutor {
    /// An executor answering `answers` in order.
    pub fn new(answers: Vec<Result<Sets, ExecError>>) -> Arc<FakeExecutor> {
        Arc::new(FakeExecutor {
            answers: Mutex::new(answers),
            ..FakeExecutor::default()
        })
    }

    /// Number of calls so far.
    pub fn call_count(&self) -> usize {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

impl Executor for FakeExecutor {
    fn execute(
        &self,
        op: &Operation,
        params: &Params,
        projection: Option<&Projection>,
    ) -> Result<Sets, ExecError> {
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((op.name.clone(), params.clone()));
        let mut answers = self.answers.lock().unwrap_or_else(PoisonError::into_inner);
        let answer = match answers.len() {
            0 => Ok(Vec::new()),
            1 => answers[0].clone(),
            _ => answers.remove(0),
        }?;
        Ok(answer
            .into_iter()
            .enumerate()
            .map(|(i, rows)| {
                let cols = effective_columns(op, projection, i);
                rows.into_iter()
                    .map(|r| {
                        let raw = r.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
                        project(raw, cols.as_deref())
                    })
                    .collect()
            })
            .collect())
    }

    fn schema_fingerprint(&self) -> Option<String> {
        self.fingerprint.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers_match_the_dotnet_ones() {
        let raw = vec![
            ("A".to_string(), Value::from(1)),
            ("b".to_string(), Value::from(2)),
            ("A".to_string(), Value::from(3)),
        ];
        let row = project(raw.clone(), Some(&["a".to_string()]));
        assert_eq!(row.len(), 1);
        assert_eq!(row.get("a"), Some(&Value::from(3)));
        assert_eq!(project(raw, None).len(), 2);
        assert_eq!(printable(None), "?");
        assert_eq!(printable(Some("  SMILE\u{1}FO  ")), "SMILE?FO");
        assert_eq!(printable(Some(&"x".repeat(60))).len(), 48);
        assert_eq!(engine_name("10.50.6000.34"), "2008 R2");
        assert_eq!(engine_name("16.0.1000.6"), "2022");
        assert_eq!(engine_name("8.0"), "unknown");
        assert_eq!(ExecError::Unavailable("x".into()).wire_number(), Some(-1));
    }
}
