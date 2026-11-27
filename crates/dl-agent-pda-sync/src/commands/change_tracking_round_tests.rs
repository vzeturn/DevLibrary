//! Tests of a Change Tracking round over fake identities. The identity order and the
//! refusal rule are ADR 0010; the event codes and skip reasons are the RT-1318 brief; the
//! SQL error numbers are SQL Server's (1222 lock timeout, 5011 cannot alter database, 15151
//! cannot find the object or no permission).

use super::*;
use crate::bootstrap::change_tracking::plan;
use crate::commands::change_tracking_fakes::*;

fn round(p: &CtPlan, attempts: &[Attempt], e: &FakeExec) -> CtOutcome {
    run_round(
        p,
        attempts,
        e,
        "POS\\SQLEXPRESS",
        "SMILE_POS",
        &Ctx::background(),
        &Logger::none(),
    )
}

fn skip(table: &str, reason: &str) -> CtSkip {
    CtSkip {
        table: table.into(),
        reason: reason.into(),
    }
}

#[test]
fn a_refused_windows_identity_falls_back_to_the_smile_login() {
    let e = exec(
        |who| match who {
            "windows" => Err(refused(codes::SQL_NO_ACCESS)),
            _ => Ok(()),
        },
        |_, _| Ok(StepState::Done),
    );

    let out = round(&plan(&fresh(), true), &attempts(), &e);

    assert_eq!(
        out.summary(),
        "windows: SQL_NO_ACCESS; sql:smile: applied 5 step(s)"
    );
    assert_eq!(out.ready, ["TmpInvoice", "Trn"]);
    assert_eq!(out.skipped, [skip("CashierRVC", "no_pk")]);
    assert!(out.database_enabled && !out.pending);
    let r = report(&out, "SMILE_POS");
    assert_eq!((r.level, r.code), (WARN, codes::CAPTURE_CT_PARTIAL));
    assert!(r.message.contains("CashierRVC (no_pk)"), "{}", r.message);
    assert!(r.message.contains("as sql:smile"), "{}", r.message);
    let json = dl_json::to_string(&dl_json::Value::Object(r.detail));
    assert!(!json.contains(PASSWORD), "never a password: {json}");
}

#[test]
fn a_permission_error_on_the_database_moves_to_the_next_identity() {
    let e = exec(
        |_| Ok(()),
        |who, step| match (who, step) {
            ("windows", CtStep::EnableDatabase) => Err(sql(5011)),
            _ => Ok(StepState::Done),
        },
    );

    let out = round(&plan(&fresh(), true), &attempts(), &e);

    assert_eq!(
        out.summary(),
        "windows: refused (SQL error 5011); sql:smile: applied 5 step(s)"
    );
    // Without the database on, Windows never touches a table.
    assert!(
        !ran(&e)
            .iter()
            .any(|r| r.starts_with("windows: enable table"))
    );
    assert!(!out.pending);
}

#[test]
fn a_table_refused_by_windows_only_gets_its_missing_step_from_the_smile_login() {
    let probe = CtProbe {
        database_enabled: true,
        current_version: Some(3),
        tables: vec![t("Trn", true, false, false)],
    };
    let e = exec(
        |_| Ok(()),
        |who, step| match (who, step) {
            ("windows", CtStep::Grant(_)) => Err(sql(15151)),
            _ => Ok(StepState::Done),
        },
    );

    let out = round(&plan(&probe, true), &attempts(), &e);

    assert_eq!(
        ran(&e),
        [
            "windows: open",
            "windows: enable table Trn",
            "windows: grant on Trn",
            "smile: open",
            "smile: grant on Trn",
        ]
    );
    assert_eq!(out.ready, ["Trn"]);
    assert_eq!(
        out.summary(),
        "windows: applied 1 step(s), then Trn: refused (SQL error 15151); sql:smile: applied 1 step(s)"
    );
    let r = report(&out, "SMILE_POS");
    assert_eq!((r.level, r.code), (INFO, codes::CAPTURE_CT_ENABLED));
    assert_eq!(
        r.message,
        "SQL Server Change Tracking is on for 1 captured table(s) as windows, sql:smile"
    );
}

#[test]
fn a_lock_timeout_leaves_the_table_for_the_next_round() {
    let e = exec(
        |_| Ok(()),
        |_, step| match step {
            CtStep::EnableTable(t) if t == "TmpInvoice" => Err(sql(1222)),
            _ => Ok(StepState::Done),
        },
    );

    let out = round(&plan(&fresh(), true), &attempts()[..1], &e);

    assert!(out.pending, "retried by a later round");
    assert_eq!(out.ready, ["Trn"]);
    assert!(
        out.skipped
            .contains(&skip("TmpInvoice", "lock_timeout (SQL error 1222)")),
        "{:?}",
        out.skipped
    );
    let r = report(&out, "SMILE_POS");
    assert_eq!((r.level, r.code), (WARN, codes::CAPTURE_CT_PARTIAL));
    assert!(r.message.contains("tried again every 15 minutes"));
}

#[test]
fn when_every_identity_is_refused_the_round_failed() {
    let e = exec(
        |who| match who {
            "windows" => Err(refused(codes::SQL_NO_ACCESS)),
            _ => Err(refused(codes::SQL_LOGIN_FAILED)),
        },
        |_, _| Ok(StepState::Done),
    );

    let out = round(&plan(&fresh(), true), &attempts(), &e);

    assert!(out.pending && !out.database_enabled);
    assert_eq!(out.applied(), 0);
    let r = report(&out, "SMILE_POS");
    assert_eq!((r.level, r.code), (ERROR, codes::CAPTURE_CT_FAILED));
    assert!(
        r.message
            .contains("windows: SQL_NO_ACCESS; sql:smile: SQL_LOGIN_FAILED"),
        "{}",
        r.message
    );
    assert!(!r.message.contains(PASSWORD));
    assert!(
        out.skipped
            .contains(&skip("Trn", "refused (database not enabled)"))
    );
}

#[test]
fn a_failure_that_is_not_a_refusal_ends_the_round() {
    let e = exec(
        |_| {
            Err(Error::unexpected(
                "System.InvalidOperationException",
                "boom",
            ))
        },
        |_, _| Ok(StepState::Done),
    );

    let out = round(&plan(&fresh(), true), &attempts(), &e);

    assert_eq!(ran(&e), ["windows: open"]);
    assert!(out.pending);
    assert_eq!(out.summary(), "windows: System.InvalidOperationException");
}

#[test]
fn an_identity_without_a_password_is_recorded_as_skipped() {
    let attempts = vec![Attempt::Skipped {
        label: "sql:smile".into(),
        reason: "no default password in this build".into(),
    }];
    let e = exec(|_| Ok(()), |_, _| Ok(StepState::Done));

    let out = round(&plan(&fresh(), true), &attempts, &e);

    assert!(ran(&e).is_empty());
    assert_eq!(
        out.summary(),
        "sql:smile: skipped: no default password in this build"
    );
    assert_eq!(report(&out, "SMILE_POS").code, codes::CAPTURE_CT_FAILED);
}

#[test]
fn an_empty_plan_opens_no_connection() {
    let e = exec(|_| Ok(()), |_, _| Ok(StepState::Done));

    let out = round(&plan(&fresh(), false), &attempts(), &e);

    assert!(ran(&e).is_empty(), "flag off: no statement");
    assert!(out.attempts.is_empty() && !out.pending);
}

#[test]
fn the_failures_are_classified_by_sql_server_error_number() {
    assert_eq!(
        classify(&refused(codes::SQL_NO_ACCESS)),
        ("SQL_NO_ACCESS".to_owned(), FailureKind::Refused)
    );
    assert_eq!(
        classify(&sql(5011)),
        ("refused (SQL error 5011)".to_owned(), FailureKind::Refused)
    );
    assert_eq!(
        classify(&sql(1222)),
        (
            "lock_timeout (SQL error 1222)".to_owned(),
            FailureKind::LockTimeout
        )
    );
    assert_eq!(
        classify(&sql(5061)).1,
        FailureKind::LockTimeout,
        "ALTER DATABASE could not place its lock"
    );
    assert_eq!(
        classify(&sql(4997)),
        ("SQL error 4997".to_owned(), FailureKind::Failed)
    );
}

#[test]
fn the_disable_path_runs_each_table_then_the_database() {
    let probe = CtProbe {
        database_enabled: true,
        current_version: Some(9),
        tables: vec![
            t("TmpInvoice", true, true, true),
            t("CashierRVC", false, false, false),
        ],
    };
    let e = exec(|_| Ok(()), |_, _| Ok(StepState::Done));

    let n = disable(
        &probe,
        &SqlAdminCredential::windows(),
        &e,
        "POS",
        "SMILE_POS",
        &Ctx::background(),
    )
    .unwrap();

    assert_eq!(n, 2);
    assert_eq!(
        ran(&e),
        [
            "windows: open",
            "windows: disable table TmpInvoice",
            "windows: disable database"
        ]
    );
}
