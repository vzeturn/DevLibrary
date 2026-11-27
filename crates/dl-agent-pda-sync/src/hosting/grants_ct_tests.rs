//! Tests of the `grants` Change Tracking step over fakes. The identity order is ADR 0010;
//! the event codes and the flag rule are the RT-1318 brief; SQL Server error 4060 is a
//! database the login cannot open.

use std::sync::{Mutex, PoisonError};

use super::*;
use crate::bootstrap::change_tracking::StepState;
use crate::commands::change_tracking_fakes::*;
use crate::contracts::wire::site_event_levels::{INFO, WARN};
use crate::util::console::MemoryLines;

/// The stages and failures of a run, as `<level> <code>` and `fail <code>`.
#[derive(Default)]
struct Stages(Mutex<Vec<String>>);

impl Stages {
    fn lines(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl RunStages for Stages {
    fn line(&self, _text: &str) {}

    fn stage(&self, level: &str, code: &str, _message: &str, _detail: Option<dl_json::Map>) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(format!("{level} {code}"));
    }

    fn progress(&self, _level: &str, _code: &str, _message: &str, _detail: Option<dl_json::Map>) {}

    fn fail(&self, code: &str, _message: &str, _level: &str) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(format!("fail {code}"));
    }
}

struct Run {
    exit: i32,
    stdout: String,
    stderr: String,
    stages: Vec<String>,
}

fn go(probe: &FakeProbe, exec: &FakeExec, password: Option<&'static str>) -> Run {
    let step = ChangeTrackingStep {
        probe,
        exec,
        attempts: attempts(),
        tables: vec!["TmpInvoice".into(), "Trn".into(), "CashierRVC".into()],
        log: Logger::none(),
    };
    let (stdout, stderr, stages) = (
        MemoryLines::default(),
        MemoryLines::default(),
        Stages::default(),
    );
    let exit = step
        .run(
            &FakeSecrets(password),
            "POS\\SQLEXPRESS",
            "SMILE_POS",
            &stages,
            &stdout,
            &stderr,
            &Ctx::background(),
        )
        .unwrap();
    Run {
        exit,
        stdout: stdout.text(),
        stderr: stderr.text(),
        stages: stages.lines(),
    }
}

fn labels(attempts: &[Attempt]) -> Vec<String> {
    attempts
        .iter()
        .map(|a| match a {
            Attempt::Try { label, .. } | Attempt::Skipped { label, .. } => label.clone(),
        })
        .collect()
}

#[test]
fn an_explicit_admin_login_is_the_only_identity() {
    let login = DefaultLogin::from_env(&|_| None, PASSWORD);

    let a = verb_attempts(&SqlAdminCredential::sql("posadmin", "x"), &login);

    assert_eq!(labels(&a), ["sql:posadmin"]);
}

#[test]
fn the_operator_windows_login_comes_before_the_smile_login() {
    let login = DefaultLogin::from_env(&|_| None, PASSWORD);

    let a = verb_attempts(&SqlAdminCredential::windows(), &login);

    assert_eq!(labels(&a), ["windows", "sql:smile"]);
}

#[test]
fn the_flag_off_builds_no_step() {
    let step = real_step(false, &SqlAdminCredential::windows(), &Logger::none()).unwrap();

    assert!(step.is_none(), "flag off: no Change Tracking statement");
}

#[test]
fn tracked_tables_are_printed_and_nothing_runs() {
    let (probe, exec) = (FakeProbe::new(|| Ok(tracked())), done());

    let r = go(&probe, &exec, Some("pw"));

    assert_eq!(r.exit, EXIT_OK);
    assert!(
        r.stdout.contains("Change Tracking: database on"),
        "{}",
        r.stdout
    );
    assert!(ran(&exec).is_empty());
    assert!(r.stages.is_empty());
}

#[test]
fn an_enabled_round_is_a_stage_of_the_run() {
    let probe = FakeProbe::new(|| {
        let mut p = fresh();
        p.tables.pop();
        Ok(p)
    });
    let exec = done();

    let r = go(&probe, &exec, Some("pw"));

    assert_eq!(r.exit, EXIT_OK);
    assert_eq!(r.stages, [format!("{INFO} {}", codes::CAPTURE_CT_ENABLED)]);
    assert!(
        r.stdout
            .contains("Change Tracking is on for 2 captured table(s)"),
        "{}",
        r.stdout
    );
}

#[test]
fn a_table_without_a_key_makes_the_stage_partial() {
    let (probe, exec) = (FakeProbe::new(|| Ok(fresh())), done());

    let r = go(&probe, &exec, Some("pw"));

    assert_eq!(r.exit, EXIT_OK);
    assert_eq!(r.stages, [format!("{WARN} {}", codes::CAPTURE_CT_PARTIAL)]);
}

#[test]
fn every_identity_refused_fails_the_run_without_a_password() {
    let probe = FakeProbe::new(|| Ok(fresh()));
    let exec = exec(
        |_| Err(refused(codes::SQL_LOGIN_FAILED)),
        |_, _| Ok(StepState::Done),
    );

    let r = go(&probe, &exec, Some("pw"));

    assert_eq!(r.exit, EXIT_FAILED);
    assert_eq!(r.stages, [format!("fail {}", codes::CAPTURE_CT_FAILED)]);
    assert!(
        r.stderr.contains("windows: SQL_LOGIN_FAILED"),
        "{}",
        r.stderr
    );
    assert!(!r.stderr.contains(PASSWORD));
}

#[test]
fn a_probe_refused_by_sql_server_fails_the_run() {
    let (probe, exec) = (FakeProbe::new(|| Err(sql(4060))), done());

    let r = go(&probe, &exec, Some("pw"));

    assert_eq!(r.exit, EXIT_FAILED);
    assert!(
        r.stderr
            .contains("could not be probed as pda_sync (SQL error 4060)"),
        "{}",
        r.stderr
    );
    assert!(ran(&exec).is_empty());
}

#[test]
fn without_the_password_nothing_is_probed() {
    let (probe, exec) = (FakeProbe::new(|| Ok(fresh())), done());

    let r = go(&probe, &exec, None);

    assert_eq!(r.exit, EXIT_OK);
    assert_eq!(probe.calls(), 0);
    assert!(
        r.stdout.contains("Change Tracking not checked"),
        "{}",
        r.stdout
    );
}
