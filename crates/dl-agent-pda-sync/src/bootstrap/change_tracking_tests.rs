//! Tests of the Change Tracking plan and SQL text. The statement shapes are the documented
//! syntax (learn.microsoft.com: "Enable and disable change tracking", `ALTER DATABASE SET
//! options`, `GRANT Object Permissions`, `QUOTENAME`, `SET LOCK_TIMEOUT`); the decisions
//! are ADR 0011 D2 and the RT-1318 slice 8 brief.

use super::*;

fn table(name: &str, present: bool, has_pk: bool, tracked: bool, can_view: bool) -> CtTable {
    CtTable {
        name: name.to_owned(),
        present,
        has_pk,
        tracked,
        can_view,
    }
}

/// A SMILE_POS database before anything was enabled: two keyed tables, one without a
/// primary key and one this build lacks.
fn fresh() -> CtProbe {
    CtProbe {
        database_enabled: false,
        current_version: None,
        tables: vec![
            table("TmpInvoice", true, true, false, false),
            table("Trn", true, true, false, false),
            table("CashierRVC", true, false, false, false),
            table("Gone", false, false, false, false),
        ],
    }
}

fn all_sql() -> [&'static str; 5] {
    [
        ENABLE_DATABASE_SQL,
        ENABLE_TABLE_SQL,
        GRANT_SQL,
        DISABLE_TABLE_SQL,
        DISABLE_DATABASE_SQL,
    ]
}

#[test]
fn the_statements_follow_the_documented_change_tracking_syntax() {
    // learn.microsoft.com/sql/relational-databases/track-changes/enable-and-disable-change-tracking-sql-server
    assert!(
        ENABLE_DATABASE_SQL
            .contains("SET CHANGE_TRACKING = ON (CHANGE_RETENTION = 2 DAYS, AUTO_CLEANUP = ON)")
    );
    assert!(ENABLE_TABLE_SQL.contains("ENABLE CHANGE_TRACKING WITH (TRACK_COLUMNS_UPDATED = OFF)"));
    assert!(DISABLE_TABLE_SQL.contains("DISABLE CHANGE_TRACKING;"));
    assert!(DISABLE_DATABASE_SQL.contains("SET CHANGE_TRACKING = OFF;"));
    // GRANT VIEW CHANGE TRACKING ON OBJECT::schema.table TO principal.
    assert!(GRANT_SQL.contains("GRANT VIEW CHANGE TRACKING ON OBJECT::dbo."));
    assert_eq!(RETENTION_DAYS, 2, "ADR 0011 D2: two days of history");
}

#[test]
fn every_step_waits_at_most_five_seconds_for_a_lock() {
    // SET LOCK_TIMEOUT takes milliseconds; the brief asks for 5000.
    assert_eq!(LOCK_TIMEOUT_MS, 5000);
    assert_eq!(LOCK_TIMEOUT_SQL, "SET LOCK_TIMEOUT 5000;");
    for sql in all_sql() {
        assert!(sql.starts_with(LOCK_TIMEOUT_SQL), "{sql}");
    }
    // Error 1222 is "Lock request time out period exceeded".
    assert!(LOCK_TIMEOUT_ERRORS.contains(&1222));
}

#[test]
fn no_statement_needs_more_than_sql_server_2008_r2() {
    // Newer than 2008 R2: ALTER DATABASE CURRENT, IIF, CONCAT, TRY_CONVERT (2012); DROP ..
    // IF EXISTS, CREATE OR ALTER (2016); STRING_AGG (2017). Out of scope by the brief: CDC,
    // Service Broker and triggers.
    let newer = [
        "DATABASE CURRENT",
        "IIF(",
        "CONCAT(",
        "TRY_CONVERT",
        "DROP ",
        "OR ALTER",
        "STRING_AGG",
        "THROW",
        "sp_cdc",
        "BROKER",
        "TRIGGER",
    ];
    let probe = probe_tables_sql(3);
    for sql in all_sql()
        .into_iter()
        .chain([PROBE_DATABASE_SQL, probe.as_str()])
    {
        for word in newer {
            assert!(
                !sql.to_uppercase().contains(&word.to_uppercase()),
                "{word} in {sql}"
            );
        }
    }
}

#[test]
fn names_reach_the_server_only_as_parameters_quoted_by_quotename() {
    let hostile = "x]; DROP TABLE dbo.Trn; --";
    for step in [
        CtStep::EnableTable(hostile.into()),
        CtStep::Grant(hostile.into()),
        CtStep::DisableTable(hostile.into()),
    ] {
        assert!(!step.sql().contains(hostile));
        assert!(step.sql().contains("QUOTENAME(@table)"), "{step:?}");
        assert_eq!(step.params()[0], ("table", hostile.to_owned()));
        assert_eq!(step.table(), Some(hostile));
    }
    // The database is the connection's own, quoted on the server.
    for sql in [ENABLE_DATABASE_SQL, DISABLE_DATABASE_SQL] {
        assert!(sql.contains("QUOTENAME(DB_NAME())"));
    }
    assert!(GRANT_SQL.contains("QUOTENAME(@grantee)"));
    let probe = probe_tables_sql(3);
    assert!(
        probe.contains("FROM (VALUES (@t0), (@t1), (@t2)) AS v(name)"),
        "{probe}"
    );
    assert!(probe.contains("N'VIEW CHANGE TRACKING'"));
}

#[test]
fn the_grant_names_the_pda_sync_user() {
    let grant = CtStep::Grant("TmpInvoice".into());
    assert_eq!(
        grant.params(),
        [
            ("table", "TmpInvoice".to_owned()),
            ("grantee", "pda_sync".to_owned())
        ]
    );
    assert!(CtStep::EnableDatabase.params().is_empty());
    assert!(CtStep::DisableDatabase.params().is_empty());
}

#[test]
fn with_the_flag_off_no_statement_is_planned() {
    let off = plan(&fresh(), false);
    assert_eq!(off, CtPlan::default());
    assert!(off.is_done());
}

#[test]
fn a_fresh_database_is_enabled_first_then_each_keyed_table_tracked_and_granted() {
    let p = plan(&fresh(), true);
    assert_eq!(
        p.steps,
        [
            CtStep::EnableDatabase,
            CtStep::EnableTable("TmpInvoice".into()),
            CtStep::Grant("TmpInvoice".into()),
            CtStep::EnableTable("Trn".into()),
            CtStep::Grant("Trn".into()),
        ]
    );
    assert_eq!(
        p.skipped,
        [
            CtSkip {
                table: "CashierRVC".into(),
                reason: "no_pk".into()
            },
            CtSkip {
                table: "Gone".into(),
                reason: "missing".into()
            },
        ]
    );
    assert!(p.ready.is_empty());
}

#[test]
fn a_tracked_table_without_the_grant_only_gets_the_grant() {
    let probe = CtProbe {
        database_enabled: true,
        current_version: Some(0),
        tables: vec![table("Trn", true, true, true, false)],
    };
    assert_eq!(plan(&probe, true).steps, [CtStep::Grant("Trn".into())]);
}

#[test]
fn enabling_twice_plans_nothing_the_second_time() {
    // What the probe reads after a round that enabled everything it could.
    let after = CtProbe {
        database_enabled: true,
        current_version: Some(0),
        tables: vec![
            table("TmpInvoice", true, true, true, true),
            table("Trn", true, true, true, true),
            table("CashierRVC", true, false, false, false),
        ],
    };
    let p = plan(&after, true);
    assert!(p.is_done(), "{p:?}");
    assert_eq!(p.ready, ["TmpInvoice", "Trn"]);
    assert_eq!(p.skipped.len(), 1, "the table without a key stays reported");
    assert_eq!(after.ready().count(), 2);
}

#[test]
fn the_disable_path_untracks_each_table_before_the_database() {
    let on = CtProbe {
        database_enabled: true,
        current_version: Some(7),
        tables: vec![
            table("TmpInvoice", true, true, true, true),
            table("CashierRVC", true, false, false, false),
        ],
    };
    assert_eq!(
        disable_plan(&on),
        [
            CtStep::DisableTable("TmpInvoice".into()),
            CtStep::DisableDatabase
        ]
    );
    assert!(disable_plan(&fresh()).is_empty());
}

#[test]
fn each_answer_maps_to_one_step_state() {
    assert_eq!(step_state("tracked"), Some(StepState::Done));
    assert_eq!(step_state("no_pk"), Some(StepState::NoPk));
    assert_eq!(step_state("not_found"), Some(StepState::NotFound));
    assert_eq!(step_state("kept"), Some(StepState::Kept));
    assert_eq!(step_state("whatever"), None);
    // Every answer the batches can give is known.
    for sql in all_sql() {
        for answer in sql.split("N'").skip(1).filter_map(|s| s.split('\'').next()) {
            if answer.chars().all(|c| c.is_ascii_lowercase() || c == '_') && !answer.is_empty() {
                assert!(step_state(answer).is_some(), "{answer} in {sql}");
            }
        }
    }
}

#[test]
fn the_probe_describes_the_state_for_the_self_test() {
    let mut probe = fresh();
    assert_eq!(
        probe.describe(),
        "database off, 0/4 captured tables tracked, version none"
    );
    probe.database_enabled = true;
    probe.current_version = Some(42);
    probe.tables[0].tracked = true;
    assert_eq!(
        probe.describe(),
        "database on, 1/4 captured tables tracked, version 42"
    );
}
