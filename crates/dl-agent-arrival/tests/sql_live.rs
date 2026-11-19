//! The PICTURE phase's grants and `op_sweep` against a real SQL Server (testdata/pms-fake.sql
//! standing in for SMILE_FO), as the hotel's service login sees them:
//!
//! 1. a login that is only `db_owner` of SMILE_FO: `picture_probe` says `blocked` (SQL error
//!    916) and the install fails naming the missing `CREATE USER`;
//! 2. a user in SMILE_PICTURE with `db_ddladmin` + `db_datareader` + `db_datawriter` (not
//!    `db_owner`): the probe says `pending`, the install runs, the probe says `installed`;
//! 3. `op_sweep` of every catalog operation: no permission, parameter or timeout error, every
//!    call rolled back (every table's row count unchanged), and a second sweep answers the same.
//!    The fake PMS is a minimal stand-in: the operations in [`FAKE_LACKS`] stop at SQL error 208
//!    on a table it does not have (the real SMILE_FO has them; `op_sweep` on a hotel's PMS is
//!    the proof there). Any other failure, or a change to that set, fails the test.
//!
//! Env-gated like `sql_integration.rs`: `ARRIVAL_AGENT_TEST_SQL` is a sysadmin connection string
//! without a database. Unset, the test returns at once.

use std::collections::BTreeMap;

use dl_agent_arrival::agent::installer;
use dl_agent_arrival::agent::tds;
use dl_agent_arrival::contracts::catalog::catalog;
use dl_agent_arrival::module::{picture, sweep};
use dl_json::{Value, json};

const ENV: &str = "ARRIVAL_AGENT_TEST_SQL";
const FAKE_PMS: &str = include_str!("../testdata/pms-fake.sql");
const LOGIN: &str = "arrival_svc";

/// Operations whose procedure reads a table testdata/pms-fake.sql does not create, and that table.
const FAKE_LACKS: [(&str, &str); 17] = [
    ("folio.assign-room", "dbo.Room"),
    ("folio.breakfast", "dbo.BreakfastVoucher"),
    ("folio.print-breakfast-voucher", "dbo.BreakfastVoucher"),
    ("folio.special-services", "dbo.SpecialServicePosted"),
    ("guest.id-pictures", "dbo.GuestPicture"),
    ("guest.remove-picture", "dbo.GuestPicture"),
    ("housekeeping.create-maintenance", "dbo.RoomMaintainant"),
    ("housekeeping.maintenance", "dbo.RoomMaintainant"),
    ("housekeeping.rooms", "dbo.Room"),
    ("housekeeping.update-room-status", "dbo.Room"),
    ("reference.countries", "dbo.Country"),
    ("reference.dropdowns", "dbo.SalesAccount"),
    ("reference.vip-levels", "dbo.VIP"),
    ("report.market-segment", "dbo.MarketSegment"),
    ("settings.ci-online", "dbo.GlobalParameter"),
    ("settings.ci-online-update", "dbo.GlobalParameter"),
    ("walkin.create-reservation", "dbo.AutoFolioNumber"),
];

fn server() -> Option<String> {
    std::env::var(ENV)
        .ok()
        .map(|s| s.trim().trim_end_matches(';').to_string())
        .filter(|s| !s.is_empty())
}

fn apply_fake_pms(server: &str) {
    let mut conn = tds::connect(server, Some("master")).expect("connect to master");
    let mut batch = String::new();
    for line in FAKE_PMS.lines().chain(std::iter::once("GO")) {
        if line.trim().eq_ignore_ascii_case("GO") {
            if !batch.trim().is_empty() {
                conn.batch(&batch).expect("fake PMS batch");
            }
            batch.clear();
        } else {
            batch.push_str(line);
            batch.push('\n');
        }
    }
}

/// `server` with the restricted login instead of its credentials, on SMILE_FO.
fn as_service(server: &str, password: &str) -> String {
    let rest: Vec<&str> = server
        .split(';')
        .filter(|p| {
            let k = p
                .split('=')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            !matches!(
                k.as_str(),
                "user id" | "uid" | "user" | "password" | "pwd" | "database" | "initial catalog"
            )
        })
        .collect();
    format!(
        "{};User ID={LOGIN};Password={password};Database=SMILE_FO",
        rest.join(";")
    )
}

/// Every user table's row count in `db`.
fn row_counts(server: &str, db: &str) -> BTreeMap<String, i64> {
    let mut conn = tds::connect(server, Some(db)).expect("connect for counts");
    let tables = conn
        .query(
            "SELECT QUOTENAME(s.name) + '.' + QUOTENAME(t.name) FROM sys.tables t JOIN sys.schemas s ON s.schema_id = t.schema_id ORDER BY 1",
            &[],
        )
        .expect("list tables");
    let names: Vec<String> = tables
        .rows()
        .iter()
        .filter_map(|r| match r.values().first() {
            Some(dl_tds::Value::String(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    let mut out = BTreeMap::new();
    for n in names {
        let r = conn
            .query(&format!("SELECT COUNT_BIG(*) FROM {n}"), &[])
            .expect("count rows");
        let count = match r.rows().first().and_then(|r| r.values().first()) {
            Some(dl_tds::Value::I64(c)) => *c,
            other => panic!("count of {n}: {other:?}"),
        };
        out.insert(format!("{db}.{n}"), count);
    }
    out
}

fn counts_of(sweep: &Value) -> Value {
    sweep.get("counts").cloned().unwrap_or(Value::Null)
}

#[test]
fn picture_grants_and_the_op_sweep_hold_on_sql_server() {
    let Some(server) = server() else {
        eprintln!("{ENV} not set: live SQL checks skipped");
        return;
    };
    apply_fake_pms(&server);
    let password = format!("Ci-{:016x}-aA1!", dl_rand::u64());
    let mut sa = tds::connect(&server, Some("master")).expect("connect as sysadmin");
    sa.batch(&format!(
        "IF SUSER_ID(N'{LOGIN}') IS NOT NULL DROP LOGIN [{LOGIN}];
         CREATE LOGIN [{LOGIN}] WITH PASSWORD = N'{password}', CHECK_POLICY = OFF;"
    ))
    .expect("create the service login");
    sa.batch(&format!(
        "USE [SMILE_FO]; CREATE USER [{LOGIN}] FOR LOGIN [{LOGIN}]; EXEC sp_addrolemember N'db_owner', N'{LOGIN}';"
    ))
    .expect("db_owner of the PMS");
    let svc = as_service(&server, &password);

    // 1. db_owner of the PMS only.
    let (f, a) = picture::probe(&svc).expect("probe");
    assert_eq!(f.login, LOGIN);
    assert!(f.picture_exists && !f.picture_access, "{f:?}");
    assert!(
        !f.sysadmin && f.pms_create_procedure && f.pms_alter,
        "{f:?}"
    );
    assert_eq!(a.state, "blocked", "{a:?}");
    assert!(a.needs.iter().any(|n| n.contains("916")), "{a:?}");
    let failed = installer::ensure_installed(&svc, false).expect_err("no user in SMILE_PICTURE");
    assert!(
        failed.0.contains("[916]") && failed.0.contains("CREATE USER"),
        "{failed}"
    );
    assert!(
        !failed.0.contains(&password),
        "no secret in the message: {failed}"
    );

    // 2. The least the phase needs: ddladmin + datareader + datawriter in SMILE_PICTURE.
    sa.batch(&format!(
        "USE [SMILE_PICTURE]; CREATE USER [{LOGIN}] FOR LOGIN [{LOGIN}];
         EXEC sp_addrolemember N'db_ddladmin', N'{LOGIN}';
         EXEC sp_addrolemember N'db_datareader', N'{LOGIN}';
         EXEC sp_addrolemember N'db_datawriter', N'{LOGIN}';"
    ))
    .expect("grant in SMILE_PICTURE");
    let (f, a) = picture::probe(&svc).expect("probe after the grant");
    assert!(
        f.picture_access && f.picture_ddl && f.picture_read && f.picture_write,
        "{f:?}"
    );
    assert_eq!(a.state, "pending", "{a:?}");
    assert!(
        installer::ensure_installed(&svc, false).expect("install as the service login"),
        "the bundle runs"
    );
    let (f, a) = picture::probe(&svc).expect("probe after the install");
    assert_eq!(a.state, "installed", "{a:?}");
    assert_eq!(f.picture_tables, vec!["FontDeskSignture", "ProfilePicture"]);
    assert_eq!(
        f.bundle_installed.as_deref(),
        Some(installer::bundle().hash.as_str())
    );
    let probe_json = dl_json::to_string(&picture::to_json(&f, &a));
    assert!(!probe_json.contains(&password), "{probe_json}");

    // 3. Every operation, rolled back.
    let before = [
        row_counts(&server, "SMILE_FO"),
        row_counts(&server, "SMILE_PICTURE"),
    ];
    let opts = sweep::SweepOptions::from_args(&json!({}), "2026-11-19").expect("options");
    let first = sweep::sweep(&svc, &opts).expect("sweep");
    let total = catalog().expect("catalog").operations().len() as u64;
    assert_eq!(first.get("total").and_then(Value::as_u64), Some(total));
    let ops = first.get("ops").and_then(Value::as_array).expect("ops");
    let mut wrong = Vec::new();
    let mut lacking = Vec::new();
    for op in ops {
        let name = op.get("op").and_then(Value::as_str).unwrap_or("");
        let class = op.get("class").and_then(Value::as_str);
        let result = op.get("result").and_then(Value::as_str);
        let rolled_back = op.get("transaction").and_then(Value::as_str) == Some("rolled_back");
        let message = op.get("message").and_then(Value::as_str).unwrap_or("");
        let fake_gap = class == Some("schema")
            && op.get("number").and_then(Value::as_i64) == Some(208)
            && FAKE_LACKS
                .iter()
                .any(|(o, t)| *o == name && message == format!("Invalid object name '{t}'."));
        if fake_gap {
            lacking.push(name.to_string());
        }
        let bad_class = matches!(class, Some("schema" | "permission" | "other" | "timeout"));
        if (bad_class && !fake_gap) || result == Some("skipped") || !rolled_back {
            wrong.push(dl_json::to_string(op));
        }
    }
    eprintln!(
        "op_sweep: {} operations, counts {}",
        total,
        dl_json::to_string(&counts_of(&first))
    );
    assert!(
        wrong.is_empty(),
        "operations that point at the install:\n{}",
        wrong.join("\n")
    );
    let expected: Vec<String> = FAKE_LACKS.iter().map(|(o, _)| (*o).to_string()).collect();
    assert_eq!(
        lacking, expected,
        "the fake PMS gaps changed: update FAKE_LACKS"
    );
    let after = [
        row_counts(&server, "SMILE_FO"),
        row_counts(&server, "SMILE_PICTURE"),
    ];
    assert_eq!(before, after, "the sweep left every table as it was");
    let second = sweep::sweep(&svc, &opts).expect("second sweep");
    assert_eq!(
        counts_of(&first),
        counts_of(&second),
        "a replay answers the same"
    );
    let text = dl_json::to_string(&second);
    assert!(
        !text.contains(&password) && !text.contains("Password"),
        "no secret in the sweep"
    );
    assert!(
        sweep::SweepOptions::from_args(&json!({"commit": true}), "2026-11-19").is_err(),
        "commit needs an explicit only list"
    );

    sa.batch(&format!(
        "USE [master]; IF SUSER_ID(N'{LOGIN}') IS NOT NULL DROP LOGIN [{LOGIN}];"
    ))
    .expect("drop the service login");
}
