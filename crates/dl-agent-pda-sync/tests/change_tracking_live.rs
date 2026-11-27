//! RT-1318 live: SQL Server Change Tracking turned on for the captured SMILE_POS tables by
//! the production round, on the faithful fixture of `live_support` bootstrapped by the real
//! `010`–`090` scripts. The expectations are SQL Server's documented behaviour
//! (<https://learn.microsoft.com/en-us/sql/relational-databases/track-changes/enable-and-disable-change-tracking-sql-server>,
//! <https://learn.microsoft.com/en-us/sql/relational-databases/system-functions/changetable-transact-sql>):
//! only a table with a primary key can be tracked, `CHANGE_TRACKING_CURRENT_VERSION()`
//! moves with each committed change, `CHANGETABLE(CHANGES ..)` needs `SELECT` and
//! `VIEW CHANGE TRACKING` and reports an insert updated later as one `I`, and an identity
//! without `ALTER` on the database gets error 5011.
//!
//! Gated on `PDA_SYNC_TEST_SQL`; run with `--test-threads=1` (the `pda_sync` login is
//! server-wide).

mod live_support;

use dl_agent_pda_sync::bootstrap::admin::SqlAdminCredential;
use dl_agent_pda_sync::bootstrap::change_tracking::{CtProbe, CtStep, StepState, plan, reasons};
use dl_agent_pda_sync::bootstrap::change_tracking_sql::{CtExecutor, SqlCtExecutor, read_probe};
use dl_agent_pda_sync::capture::plan::captured_tables;
use dl_agent_pda_sync::commands::change_tracking_round::{CtOutcome, disable, report, run_round};
use dl_agent_pda_sync::config::codes;
use dl_agent_pda_sync::contracts::catalog::SyncCatalog;
use dl_agent_pda_sync::hosting::enroll_attempts::Attempt;
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_tds::Param;
use live_support::orders::Live;

/// A keyed table of the test, captured beside the vendor ones (DML moves its version).
const KEYED: &str = "CtLiveKeyed";
/// A heap of the test (no primary key).
const HEAP: &str = "CtLiveHeap";
/// A login without rights.
const LOW: &str = "pda_ct_low";

fn ctx() -> Ctx {
    Ctx::background()
}

/// The fixture with the two test tables, readable by `pda_sync` like the captured ones.
fn start() -> Option<Live> {
    let live = Live::start()?;
    live.pos.exec(&format!(
        "CREATE TABLE dbo.{KEYED} (Id int NOT NULL PRIMARY KEY, Note nvarchar(20) NULL);
CREATE TABLE dbo.{HEAP} (Id int NOT NULL, Note nvarchar(20) NULL);
GRANT SELECT ON dbo.{KEYED} TO pda_sync;
GRANT SELECT ON dbo.{HEAP} TO pda_sync;"
    ));
    Some(live)
}

/// The captured tables and the two test tables.
fn tables() -> Vec<String> {
    let mut t = captured_tables(SyncCatalog::load().unwrap());
    t.push(KEYED.into());
    t.push(HEAP.into());
    t
}

/// The probe as `pda_sync`.
fn probe(live: &Live, tables: &[String]) -> CtProbe {
    let mut c = dl_tds::Client::connect(&live.agent).unwrap();
    let p = read_probe(&mut c, tables, &ctx()).unwrap_or_else(|e| panic!("probe: {e}"));
    c.close();
    p
}

fn admin(live: &Live) -> Vec<Attempt> {
    vec![Attempt::Try {
        label: "sql:admin".into(),
        credential: live.pos.admin_credential(),
    }]
}

fn round(live: &Live, p: &CtProbe, attempts: &[Attempt]) -> CtOutcome {
    run_round(
        &plan(p, true),
        attempts,
        &SqlCtExecutor,
        &live.pos.instance,
        &live.pos.database,
        &ctx(),
        &Logger::none(),
    )
}

/// A login with a database user and no other right; dropped (with its sessions) at the end.
struct LowLogin<'a> {
    live: &'a Live,
    password: String,
}

impl<'a> LowLogin<'a> {
    fn create(live: &'a Live) -> LowLogin<'a> {
        let tag = dl_uuid::Uuid::new_v4().simple().to_string();
        let password = format!("Ct-{}-x9!", &tag[..12]);
        live.pos.exec(&format!(
            "IF SUSER_ID(N'{LOW}') IS NOT NULL DROP LOGIN [{LOW}];
CREATE LOGIN [{LOW}] WITH PASSWORD = N'{password}', CHECK_POLICY = OFF;
CREATE USER [{LOW}] FOR LOGIN [{LOW}];"
        ));
        LowLogin { live, password }
    }

    fn attempts(&self) -> Vec<Attempt> {
        vec![Attempt::Try {
            label: format!("sql:{LOW}"),
            credential: SqlAdminCredential::sql(LOW, &self.password),
        }]
    }
}

impl Drop for LowLogin<'_> {
    fn drop(&mut self) {
        let mut c = match dl_tds::Client::connect(&self.live.pos.admin) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("cleanup of {LOW} failed: {e}");
                return;
            }
        };
        let sql = format!(
            "DECLARE @k nvarchar(max) = N'';
SELECT @k = @k + N'KILL ' + CAST(session_id AS nvarchar(10)) + N';' FROM sys.dm_exec_sessions WHERE login_name = N'{LOW}';
EXEC (@k);
IF SUSER_ID(N'{LOW}') IS NOT NULL DROP LOGIN [{LOW}];"
        );
        if let Err(e) = c.batch(&sql) {
            eprintln!("cleanup of {LOW} failed: {e}");
        }
        c.close();
    }
}

#[test]
fn enabling_twice_tracks_each_keyed_table_once_and_skips_a_heap() {
    let Some(live) = start() else { return };
    let tables = tables();
    let before = probe(&live, &tables);
    assert!(
        !before.database_enabled,
        "a new database has Change Tracking off"
    );
    assert_eq!(before.current_version, None);

    let first = round(&live, &before, &admin(&live));

    assert!(!first.pending, "{}", first.summary());
    assert!(first.database_enabled);
    assert!(first.ready.iter().any(|t| t == KEYED), "{:?}", first.ready);
    assert!(
        first
            .skipped
            .iter()
            .any(|s| s.table == HEAP && s.reason == reasons::NO_PK),
        "{:?}",
        first.skipped
    );
    assert_eq!(
        report(&first, &live.pos.database).code,
        codes::CAPTURE_CT_PARTIAL
    );
    let after = probe(&live, &tables);
    assert!(after.database_enabled);
    assert!(after.current_version.is_some());
    for t in &after.tables {
        if t.present && t.has_pk {
            assert!(t.tracked && t.can_view, "{t:?}");
        } else {
            assert!(!t.tracked, "{t:?}");
        }
    }
    // The server's own catalog, read as the administrator.
    assert_eq!(
        live.pos
            .scalar_i64("SELECT CAST(COUNT(*) AS bigint) FROM sys.change_tracking_tables"),
        after.tables.iter().filter(|t| t.tracked).count() as i64
    );
    let retention = "FROM sys.change_tracking_databases WHERE database_id = DB_ID()";
    assert_eq!(
        live.pos.scalar_i64(&format!(
            "SELECT CAST(retention_period AS bigint) {retention}"
        )),
        2
    );
    assert_eq!(
        live.pos
            .scalar_text(&format!("SELECT retention_period_units_desc {retention}"))
            .as_deref(),
        Some("DAYS")
    );
    assert_eq!(
        live.pos.scalar_i64(&format!(
            "SELECT CAST(is_auto_cleanup_on AS bigint) {retention}"
        )),
        1
    );

    // Enabling again: nothing is planned, and each step run again answers the same and
    // changes nothing.
    assert!(plan(&after, true).is_done());
    let again = round(&live, &after, &admin(&live));
    assert!(again.attempts.is_empty(), "{}", again.summary());
    let mut s = SqlCtExecutor
        .open(
            &live.pos.admin_credential(),
            &live.pos.instance,
            &live.pos.database,
            &ctx(),
        )
        .unwrap();
    let mut run = |step: CtStep| s.run(&step, &ctx()).unwrap();
    assert_eq!(run(CtStep::EnableDatabase), StepState::Done);
    assert_eq!(run(CtStep::EnableTable(KEYED.into())), StepState::Done);
    assert_eq!(run(CtStep::Grant(KEYED.into())), StepState::Done);
    assert_eq!(run(CtStep::EnableTable(HEAP.into())), StepState::NoPk);
    drop(s);
    assert_eq!(probe(&live, &tables), after);
}

#[test]
fn a_change_moves_the_version_and_reads_as_pda_sync() {
    let Some(live) = start() else { return };
    let tables = tables();
    let enabled = round(&live, &probe(&live, &tables), &admin(&live));
    assert!(!enabled.pending, "{}", enabled.summary());
    let after = probe(&live, &tables);
    let v0 = after.current_version.unwrap();

    live.pos.exec(&format!(
        "INSERT INTO dbo.{KEYED} (Id, Note) VALUES (1, N'a');
UPDATE dbo.{KEYED} SET Note = N'b' WHERE Id = 1;"
    ));

    let v1 = probe(&live, &tables).current_version.unwrap();
    assert!(v1 > v0, "{v0} -> {v1}");
    let mut c = dl_tds::Client::connect(&live.agent).unwrap();
    let r = c
        .query(
            &format!(
                "SELECT CAST(c.Id AS int), c.SYS_CHANGE_OPERATION FROM CHANGETABLE(CHANGES dbo.{KEYED}, @v) AS c"
            ),
            &[Param::new("v", v0)],
        )
        .unwrap_or_else(|e| panic!("CHANGETABLE as pda_sync: {e}"));
    let rows: Vec<(i32, String)> = r
        .rows()
        .iter()
        .map(|row| {
            (
                row.get(0).unwrap(),
                row.get::<String>(1).unwrap().trim().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [(1, "I".to_owned())],
        "an insert updated later is one I"
    );
    // A tracked vendor table reads the same way.
    let vendor = after
        .tables
        .iter()
        .find(|t| t.tracked && t.name != KEYED)
        .expect("a captured SMILE_POS table is tracked");
    let r = c
        .query(
            &format!(
                "SELECT COUNT(*) FROM CHANGETABLE(CHANGES dbo.[{}], @v) AS c",
                vendor.name
            ),
            &[Param::new("v", v0)],
        )
        .unwrap_or_else(|e| panic!("CHANGETABLE on {} as pda_sync: {e}", vendor.name));
    assert_eq!(r.rows()[0].get::<i32>(0).unwrap(), 0);
    c.close();
}

#[test]
fn a_login_without_rights_is_refused_and_the_next_identity_takes_over() {
    let Some(live) = start() else { return };
    let low = LowLogin::create(&live);
    let tables = tables();
    let before = probe(&live, &tables);

    let refused = round(&live, &before, &low.attempts());

    assert!(
        refused.pending && !refused.database_enabled,
        "{}",
        refused.summary()
    );
    assert_eq!(
        refused.summary(),
        format!("sql:{LOW}: refused (SQL error 5011)")
    );
    let r = report(&refused, &live.pos.database);
    assert_eq!(r.code, codes::CAPTURE_CT_FAILED);
    assert!(!r.message.contains(&low.password));
    assert!(!probe(&live, &tables).database_enabled, "nothing changed");

    // ADR 0010: the next identity takes over what the first was refused.
    let mut both = low.attempts();
    both.extend(admin(&live));
    let out = round(&live, &before, &both);
    assert!(!out.pending, "{}", out.summary());
    assert!(
        out.summary().starts_with(&format!(
            "sql:{LOW}: refused (SQL error 5011); sql:admin: applied"
        )),
        "{}",
        out.summary()
    );

    // The reversal: every tracked table, then the database.
    let after = probe(&live, &tables);
    let n = disable(
        &after,
        &live.pos.admin_credential(),
        &SqlCtExecutor,
        &live.pos.instance,
        &live.pos.database,
        &ctx(),
    )
    .unwrap();
    assert_eq!(n, after.tables.iter().filter(|t| t.tracked).count() + 1);
    let off = probe(&live, &tables);
    assert!(
        !off.database_enabled && off.tables.iter().all(|t| !t.tracked),
        "{off:?}"
    );
    assert_eq!(
        live.pos
            .scalar_i64("SELECT CAST(COUNT(*) AS bigint) FROM sys.change_tracking_tables"),
        0
    );
}
