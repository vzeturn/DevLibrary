//! One Change Tracking round (RT-1318): the enable plan run as the bootstrap identities of
//! ADR 0010, in their order (the service's Windows identity, then the SMILE default SQL
//! login), and the site event that reports it.
//!
//! The next identity is tried for what the previous one was refused (an access refusal at
//! connect, a permission error, a table it cannot see); a lock timeout or any other failure
//! leaves that work for the next round. Every step is idempotent, so a round can always be
//! run again. Outcomes name the identity and an error code or SQL error number, never a
//! password, and nothing alters a login or a password.

use std::collections::VecDeque;

use crate::bootstrap::admin::SqlAdminCredential;
use crate::bootstrap::change_tracking::{
    CtPlan, CtProbe, CtSkip, CtStep, LOCK_TIMEOUT_ERRORS, REFUSAL_NUMBERS, StepState, disable_plan,
    reasons,
};
use crate::bootstrap::change_tracking_sql::{CtExecutor, CtSession};
use crate::bootstrap::error::Error;
use crate::config::codes;
use crate::contracts::wire::site_event_levels::{ERROR, INFO, WARN};
use crate::hosting::enroll_attempts::{Attempt, is_access_refusal};
use crate::reporting::{Logger, Value};
use crate::util::ctx::Ctx;

/// Category of the round's log lines.
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.Commands.ChangeTracking";

/// How a failed step or connection may be cured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The identity lacks a permission: the next identity may hold it.
    Refused,
    /// The `ALTER` waited past the lock timeout: a later round tries again.
    LockTimeout,
    /// Anything else: a later round tries again.
    Failed,
}

/// The outcome text of a failure and how it may be cured: an agent code (`SQL_NO_ACCESS`),
/// `refused (SQL error 5011)`, `lock_timeout (SQL error 1222)`, `SQL error <n>` or an
/// error type; never a password.
pub fn classify(e: &Error) -> (String, FailureKind) {
    if let Some(code) = e.code() {
        let kind = if is_access_refusal(code) {
            FailureKind::Refused
        } else {
            FailureKind::Failed
        };
        return (code.to_owned(), kind);
    }
    let Some(sql) = e.sql() else {
        return (e.to_exception().type_name, FailureKind::Failed);
    };
    let numbers = || sql.errors.iter().map(|i| i.number);
    if let Some(n) = numbers().find(|n| LOCK_TIMEOUT_ERRORS.contains(n)) {
        return (
            format!("{} (SQL error {n})", reasons::LOCK_TIMEOUT),
            FailureKind::LockTimeout,
        );
    }
    if let Some(n) = numbers().find(|n| REFUSAL_NUMBERS.contains(n)) {
        return (
            format!("{} (SQL error {n})", reasons::REFUSED),
            FailureKind::Refused,
        );
    }
    (format!("SQL error {}", sql.number()), FailureKind::Failed)
}

/// What one identity did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CtAttempt {
    /// `windows` or `sql:<login>`.
    pub identity: String,
    /// The steps it ran.
    pub applied: usize,
    /// `applied 3 step(s)`, `skipped: <why>`, a failure of [`classify`], or
    /// `applied 1 step(s), then <failure>`; never a password.
    pub outcome: String,
}

/// What a round did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CtOutcome {
    /// One entry per identity tried or skipped, in order.
    pub attempts: Vec<CtAttempt>,
    /// Change Tracking is on for the database after the round.
    pub database_enabled: bool,
    /// The captured tables tracked and readable by `pda_sync` after the round.
    pub ready: Vec<String>,
    /// The tables left out and why (the plan's, then the round's).
    pub skipped: Vec<CtSkip>,
    /// Work is left that a later round may finish (a refusal, a lock timeout, a failure).
    pub pending: bool,
}

impl CtOutcome {
    /// The steps the round ran.
    pub fn applied(&self) -> usize {
        self.attempts.iter().map(|a| a.applied).sum()
    }

    /// The identities that ran a step, joined with `, `.
    pub fn applied_by(&self) -> String {
        self.attempts
            .iter()
            .filter(|a| a.applied > 0)
            .map(|a| a.identity.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// `windows: refused (SQL error 5011); sql:smile: applied 5 step(s)`.
    pub fn summary(&self) -> String {
        self.attempts
            .iter()
            .map(|a| format!("{}: {}", a.identity, a.outcome))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// `CashierRVC (no_pk), Trn (lock_timeout (SQL error 1222))`.
    pub fn skipped_text(&self) -> String {
        self.skipped
            .iter()
            .map(|s| format!("{} ({})", s.table, s.reason))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// A site event of a round: level, code, message and detail.
#[derive(Debug, Clone, PartialEq)]
pub struct CtReport {
    /// [`INFO`], [`WARN`] or [`ERROR`].
    pub level: &'static str,
    /// [`codes::CAPTURE_CT_ENABLED`], [`codes::CAPTURE_CT_PARTIAL`] or
    /// [`codes::CAPTURE_CT_FAILED`].
    pub code: &'static str,
    /// The operator's text.
    pub message: String,
    /// `database`, `database_enabled`, `ready`, `skipped`, `identities`, `pending`.
    pub detail: dl_json::Map,
}

fn strings(items: impl Iterator<Item = String>) -> dl_json::Value {
    dl_json::Value::Array(items.map(dl_json::Value::String).collect())
}

/// The event of `outcome` on `database`: `CAPTURE_CT_FAILED` when the round applied
/// nothing and work is left, `CAPTURE_CT_PARTIAL` when a table is left out,
/// `CAPTURE_CT_ENABLED` otherwise. The detail names identities and outcomes, never a
/// password.
pub fn report(outcome: &CtOutcome, database: &str) -> CtReport {
    let mut detail = dl_json::Map::new();
    detail.insert("database", database);
    detail.insert("database_enabled", outcome.database_enabled);
    detail.insert("ready", strings(outcome.ready.iter().cloned()));
    detail.insert(
        "skipped",
        dl_json::Value::Array(
            outcome
                .skipped
                .iter()
                .map(|s| {
                    let mut m = dl_json::Map::new();
                    m.insert("table", s.table.as_str());
                    m.insert("reason", s.reason.as_str());
                    dl_json::Value::Object(m)
                })
                .collect(),
        ),
    );
    detail.insert(
        "identities",
        dl_json::Value::Array(
            outcome
                .attempts
                .iter()
                .map(|a| {
                    let mut m = dl_json::Map::new();
                    m.insert("identity", a.identity.as_str());
                    m.insert("outcome", a.outcome.as_str());
                    m.insert("applied", a.applied);
                    dl_json::Value::Object(m)
                })
                .collect(),
        ),
    );
    detail.insert("pending", outcome.pending);
    let by = match outcome.applied_by() {
        s if s.is_empty() => String::new(),
        s => format!(" as {s}"),
    };
    let tracked = format!(
        "SQL Server Change Tracking is on for {} captured table(s){by}",
        outcome.ready.len()
    );
    let (level, code, message) = if outcome.pending && outcome.applied() == 0 {
        (
            ERROR,
            codes::CAPTURE_CT_FAILED,
            format!(
                "SQL Server Change Tracking could not be enabled ({}); capture stays on signature polling and the agent tries again every 15 minutes.",
                outcome.summary()
            ),
        )
    } else if !outcome.skipped.is_empty() {
        (
            WARN,
            codes::CAPTURE_CT_PARTIAL,
            format!(
                "{tracked}; left out: {}{}",
                outcome.skipped_text(),
                if outcome.pending {
                    " (tried again every 15 minutes)"
                } else {
                    ""
                }
            ),
        )
    } else {
        (INFO, codes::CAPTURE_CT_ENABLED, tracked)
    };
    CtReport {
        level,
        code,
        message,
        detail,
    }
}

/// The steps of one table still to run.
type Group = (String, VecDeque<CtStep>);

fn groups(plan: &CtPlan) -> Vec<Group> {
    let mut out: Vec<Group> = Vec::new();
    for step in &plan.steps {
        let Some(table) = step.table() else { continue };
        match out.iter_mut().find(|(t, _)| t == table) {
            Some((_, steps)) => steps.push_back(step.clone()),
            None => out.push((table.to_owned(), VecDeque::from([step.clone()]))),
        }
    }
    out
}

/// Where one table's steps ended for one identity.
enum TableEnd {
    /// Every step ran.
    Ready,
    /// Left for the next identity (refused or not visible), with why.
    Refused(String),
    /// Left out for this round.
    Skipped(String, bool),
}

fn run_table(
    session: &mut dyn CtSession,
    steps: &mut VecDeque<CtStep>,
    applied: &mut usize,
    ctx: &Ctx,
) -> TableEnd {
    while let Some(step) = steps.front() {
        match session.run(step, ctx) {
            Ok(StepState::Done) => {
                *applied += 1;
                steps.pop_front();
            }
            Ok(StepState::NoPk) => return TableEnd::Skipped(reasons::NO_PK.to_owned(), false),
            Ok(StepState::NotFound | StepState::Kept) => {
                return TableEnd::Refused(format!("{} (not visible)", reasons::REFUSED));
            }
            Err(e) => {
                let (text, kind) = classify(&e);
                return match kind {
                    FailureKind::Refused => TableEnd::Refused(text),
                    FailureKind::LockTimeout | FailureKind::Failed => TableEnd::Skipped(text, true),
                };
            }
        }
    }
    TableEnd::Ready
}

fn attempt_text(applied: usize, failure: Option<String>) -> String {
    match (applied, failure) {
        (0, Some(f)) => f,
        (0, None) => "nothing to do".to_owned(),
        (n, None) => format!("applied {n} step(s)"),
        (n, Some(f)) => format!("applied {n} step(s), then {f}"),
    }
}

/// Runs `plan` as each of `attempts` in turn on `database` of `instance`. An empty plan
/// (the flag off, or nothing left to do) opens no connection.
pub fn run_round(
    plan: &CtPlan,
    attempts: &[Attempt],
    exec: &dyn CtExecutor,
    instance: &str,
    database: &str,
    ctx: &Ctx,
    log: &Logger,
) -> CtOutcome {
    let log = log.for_category(CATEGORY);
    let mut db_pending = plan.steps.contains(&CtStep::EnableDatabase);
    let mut out = CtOutcome {
        database_enabled: !db_pending,
        ready: plan.ready.clone(),
        skipped: plan.skipped.clone(),
        ..CtOutcome::default()
    };
    let mut left = groups(plan);
    let mut refusals: Vec<(String, String)> = Vec::new();
    for attempt in attempts {
        if !db_pending && left.is_empty() {
            break;
        }
        if ctx.is_done() {
            out.pending = true;
            break;
        }
        let (label, credential) = match attempt {
            Attempt::Skipped { label, reason } => {
                log.info(
                    "Change Tracking skips {Identity}: {Reason}",
                    &[Value::from(label.as_str()), Value::from(reason.as_str())],
                );
                out.attempts.push(CtAttempt {
                    identity: label.clone(),
                    applied: 0,
                    outcome: format!("skipped: {reason}"),
                });
                continue;
            }
            Attempt::Try { label, credential } => (label, credential),
        };
        let mut session = match exec.open(credential, instance, database, ctx) {
            Ok(s) => s,
            Err(e) => {
                let (text, kind) = classify(&e);
                log.warn_ex(
                    e.to_exception(),
                    "Change Tracking cannot connect as {Identity} ({Outcome})",
                    &[Value::from(label.as_str()), Value::from(text.as_str())],
                );
                out.attempts.push(CtAttempt {
                    identity: label.clone(),
                    applied: 0,
                    outcome: text.clone(),
                });
                if kind == FailureKind::Refused {
                    continue;
                }
                return end_round(out, left, &text);
            }
        };
        let mut applied = 0;
        let mut failure: Option<String> = None;
        if db_pending {
            match session.run(&CtStep::EnableDatabase, ctx) {
                Ok(StepState::Done) => {
                    db_pending = false;
                    out.database_enabled = true;
                    applied += 1;
                }
                Ok(_) => failure = Some(format!("{} (database not enabled)", reasons::REFUSED)),
                Err(e) => {
                    let (text, kind) = classify(&e);
                    log.warn_ex(
                        e.to_exception(),
                        "Change Tracking cannot enable the database as {Identity} ({Outcome})",
                        &[Value::from(label.as_str()), Value::from(text.as_str())],
                    );
                    if kind != FailureKind::Refused {
                        out.attempts.push(CtAttempt {
                            identity: label.clone(),
                            applied,
                            outcome: text.clone(),
                        });
                        return end_round(out, left, &text);
                    }
                    failure = Some(text);
                }
            }
            if db_pending {
                out.attempts.push(CtAttempt {
                    identity: label.clone(),
                    applied,
                    outcome: attempt_text(applied, failure),
                });
                continue;
            }
        }
        let mut next = Vec::new();
        for (table, mut steps) in std::mem::take(&mut left) {
            match run_table(&mut *session, &mut steps, &mut applied, ctx) {
                TableEnd::Ready => out.ready.push(table),
                TableEnd::Refused(why) => {
                    failure.get_or_insert_with(|| format!("{table}: {why}"));
                    refusals.retain(|(t, _)| t != &table);
                    refusals.push((table.clone(), why));
                    next.push((table, steps));
                }
                TableEnd::Skipped(reason, retry) => {
                    if retry {
                        out.pending = true;
                        log.warn(
                            "Change Tracking left {Table} for the next round as {Identity} ({Reason})",
                            &[
                                Value::from(table.as_str()),
                                Value::from(label.as_str()),
                                Value::from(reason.as_str()),
                            ],
                        );
                        failure.get_or_insert_with(|| format!("{table}: {reason}"));
                    }
                    out.skipped.push(CtSkip { table, reason });
                }
            }
        }
        left = next;
        log.info(
            "Change Tracking ran {Applied} step(s) as {Identity}",
            &[Value::from(applied), Value::from(label.as_str())],
        );
        out.attempts.push(CtAttempt {
            identity: label.clone(),
            applied,
            outcome: attempt_text(applied, failure),
        });
    }
    if db_pending || !left.is_empty() {
        out.pending = true;
    }
    for (table, _) in left {
        let reason = if db_pending {
            format!("{} (database not enabled)", reasons::REFUSED)
        } else {
            refusals
                .iter()
                .find(|(t, _)| t == &table)
                .map_or_else(|| reasons::REFUSED.to_owned(), |(_, why)| why.clone())
        };
        out.skipped.push(CtSkip { table, reason });
    }
    out
}

/// Ends a round on a failure no identity may cure: every table left is skipped with it.
fn end_round(mut out: CtOutcome, left: Vec<Group>, why: &str) -> CtOutcome {
    out.pending = true;
    for (table, _) in left {
        out.skipped.push(CtSkip {
            table,
            reason: why.to_owned(),
        });
    }
    out
}

/// Turns Change Tracking off for the tracked captured tables of `probe`, then for the
/// database when no table is tracked any more, as `credential` (tests and operators only;
/// nothing runs it on its own). The number of steps run.
///
/// # Errors
/// The connection or a step failed.
pub fn disable(
    probe: &CtProbe,
    credential: &SqlAdminCredential,
    exec: &dyn CtExecutor,
    instance: &str,
    database: &str,
    ctx: &Ctx,
) -> Result<usize, Error> {
    let steps = disable_plan(probe);
    if steps.is_empty() {
        return Ok(0);
    }
    let mut session = exec.open(credential, instance, database, ctx)?;
    for step in &steps {
        session.run(step, ctx)?;
    }
    Ok(steps.len())
}

#[cfg(test)]
#[path = "change_tracking_round_tests.rs"]
mod tests;
