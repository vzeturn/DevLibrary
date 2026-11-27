//! `BootstrapTests` (the SQL bootstrap against the in-memory SQL admin, and the content
//! rules of the embedded scripts). The cases that pin the command and order batches
//! against the grants belong to the commands module.

mod bootstrap_support;
mod support;

use bootstrap_support::{FakeSqlAdmin, new_bootstrapper};
use dl_agent_pda_sync::bootstrap::{STEPS, StepKind, TableSet, read_script};
use dl_agent_pda_sync::config::codes;
use dl_agent_pda_sync::security::names;
use dl_agent_pda_sync::util::ctx::Ctx;
use support::TestEnv;

fn ctx() -> Ctx {
    Ctx::background()
}

#[test]
fn first_run_creates_login_user_tables_and_grants_in_order() {
    let env = TestEnv::new();
    let sql = FakeSqlAdmin::new();
    let report = new_bootstrapper(&sql, &env.secrets)
        .run(false, None, None, &ctx())
        .unwrap();
    assert_eq!(report.database, "SMILE_POS");
    let ids: Vec<&str> = report.steps.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "login",
            "user",
            "command_log",
            "sync_state",
            "grants",
            "command_grants",
            "order_grants",
            "order_grants_ow1c",
            "order_grants_ow1e",
            "sequence_grants",
            "money_grants",
            "fo_inhouse_grants",
            "post_to_smile_grants"
        ]
    );
    assert!(report.steps.iter().all(|s| s.executed));
    assert!(env.secrets.load(names::SQL_PASSWORD).unwrap().is_some());
    // The login script got the stored password, never the placeholder.
    let pw = String::from_utf8(env.secrets.load(names::SQL_PASSWORD).unwrap().unwrap()).unwrap();
    let login = &sql.executed()[0];
    assert!(login.contains(&format!("PASSWORD = N'{pw}'")) && !login.contains("{{PASSWORD}}"));
}

#[test]
fn second_run_is_a_no_op_and_keeps_the_same_password() {
    let env = TestEnv::new();
    let sql = FakeSqlAdmin::new();
    let b = new_bootstrapper(&sql, &env.secrets);
    b.run(false, None, None, &ctx()).unwrap();
    let password = env.secrets.load(names::SQL_PASSWORD).unwrap();
    let executed = sql.executed().len();

    let second = b.run(false, None, None, &ctx()).unwrap();

    assert!(second.steps.iter().all(|s| s.skipped && !s.executed));
    assert_eq!(sql.executed().len(), executed);
    assert_eq!(env.secrets.load(names::SQL_PASSWORD).unwrap(), password);
}

#[test]
fn existing_login_without_stored_password_gets_a_reset_instead_of_being_skipped() {
    let env = TestEnv::new();
    let sql = FakeSqlAdmin::new();
    let b = new_bootstrapper(&sql, &env.secrets);
    b.run(false, None, None, &ctx()).unwrap();
    env.secrets.delete(names::SQL_PASSWORD).unwrap();

    let report = b.run(false, None, None, &ctx()).unwrap();

    assert!(
        report
            .steps
            .iter()
            .find(|s| s.id == "login")
            .unwrap()
            .executed
    );
    assert!(env.secrets.load(names::SQL_PASSWORD).unwrap().is_some());
}

fn held_password(env: &TestEnv) -> Option<String> {
    env.secrets
        .load(names::SQL_PASSWORD)
        .unwrap()
        .map(|b| String::from_utf8(b).unwrap())
}

#[test]
fn a_failed_login_step_keeps_the_held_password_and_an_admin_rerun_resets_it() {
    // RT-0353: pda_sync exists with a password the agent never held (old SyncAgent).
    let env = TestEnv::new();
    let sql = FakeSqlAdmin::new();
    let b = new_bootstrapper(&sql, &env.secrets);
    let login = STEPS.iter().find(|s| s.kind == StepKind::Login).unwrap();
    {
        let mut st = sql.st();
        st.created.insert(FakeSqlAdmin::key("master", login));
        st.login_password = Some("old-password".to_owned());
        st.deny_login_step = true;
    }

    // Not an administrator: ALTER LOGIN fails and nothing new is left in the store.
    let err = b.run(false, None, None, &ctx()).unwrap_err();
    assert_eq!(err.code(), Some(codes::SQL_NO_ACCESS));
    assert_eq!(held_password(&env), None);

    // A stale held password (saved by a build before the fix) is detected and reset.
    env.secrets
        .save(names::SQL_PASSWORD, b"stale-password")
        .unwrap();
    let dry = b.run(true, None, None, &ctx()).unwrap();
    assert!(!dry.steps.iter().find(|s| s.id == "login").unwrap().skipped);
    sql.st().deny_login_step = false;

    let report = b.run(false, None, None, &ctx()).unwrap();

    assert!(
        report
            .steps
            .iter()
            .find(|s| s.id == "login")
            .unwrap()
            .executed
    );
    let held = held_password(&env).unwrap();
    assert_ne!(held, "stale-password");
    assert_eq!(sql.st().login_password.as_deref(), Some(held.as_str()));
}

#[test]
fn a_failed_reset_puts_the_previous_password_back() {
    let env = TestEnv::new();
    let sql = FakeSqlAdmin::new();
    let b = new_bootstrapper(&sql, &env.secrets);
    b.run(false, None, None, &ctx()).unwrap();
    let good = held_password(&env).unwrap();
    {
        let mut st = sql.st();
        st.login_password = Some("changed-by-someone".to_owned());
        st.deny_login_step = true;
    }

    b.run(false, None, None, &ctx()).unwrap_err();

    assert_eq!(held_password(&env).as_deref(), Some(good.as_str()));
}

#[test]
fn dry_run_changes_nothing() {
    let env = TestEnv::new();
    let sql = FakeSqlAdmin::new();
    let report = new_bootstrapper(&sql, &env.secrets)
        .run(true, None, None, &ctx())
        .unwrap();
    assert!(report.dry_run);
    assert!(sql.executed().is_empty());
    assert!(report.steps.iter().all(|s| !s.executed && !s.skipped));
    assert!(env.secrets.load(names::SQL_PASSWORD).unwrap().is_none());
}

#[test]
fn fingerprint_mismatch_has_its_own_error_code() {
    let env = TestEnv::new();
    let sql = FakeSqlAdmin::new();
    sql.st().databases.insert(
        "SMILE_POS".into(),
        ["Customers", "Orders"].into_iter().collect(),
    );
    let e = new_bootstrapper(&sql, &env.secrets)
        .run(false, None, None, &ctx())
        .unwrap_err();
    assert_eq!(e.code(), Some(codes::FINGERPRINT_MISMATCH));
    assert!(sql.executed().is_empty());
}

#[test]
fn missing_sql_access_is_reported_with_a_clear_code() {
    let env = TestEnv::new();
    let sql = FakeSqlAdmin::new();
    sql.st().deny_access = true;
    let e = new_bootstrapper(&sql, &env.secrets)
        .run(false, None, None, &ctx())
        .unwrap_err();
    assert_eq!(e.code(), Some(codes::SQL_NO_ACCESS));
}

#[test]
fn no_instance_and_no_pos_database_are_distinct_errors() {
    let env = TestEnv::new();
    let none = FakeSqlAdmin::new();
    none.st().instances.clear();
    let a = new_bootstrapper(&none, &env.secrets)
        .run(false, None, None, &ctx())
        .unwrap_err();
    assert_eq!(a.code(), Some(codes::SQL_INSTANCE_NOT_FOUND));

    let no_db = FakeSqlAdmin::new();
    no_db.st().databases.remove("SMILE_POS");
    let b = new_bootstrapper(&no_db, &env.secrets)
        .run(false, None, None, &ctx())
        .unwrap_err();
    assert_eq!(b.code(), Some(codes::SQL_DATABASE_NOT_FOUND));
    assert!(b.to_string().contains(r"(.\SQLEXPRESS)"), "{b}");
}

#[test]
fn pos_database_is_found_even_when_it_has_another_name() {
    let env = TestEnv::new();
    let sql = FakeSqlAdmin::new();
    {
        let mut st = sql.st();
        let tables: TableSet = st.databases.remove("SMILE_POS").unwrap();
        st.databases.insert("VICTORY_POS".into(), tables);
    }
    let report = new_bootstrapper(&sql, &env.secrets)
        .run(true, None, None, &ctx())
        .unwrap();
    assert_eq!(report.database, "VICTORY_POS");
}

// ---- Script content (the C# regular expressions, written out by hand) ----

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The upper-cased words of `sql` (`\b...\b` matching).
fn words(sql: &str) -> Vec<String> {
    sql.split(|c: char| !is_word(c))
        .filter(|w| !w.is_empty())
        .map(str::to_uppercase)
        .collect()
}

fn has_word(sql: &str, any: &[&str]) -> bool {
    words(sql).iter().any(|w| any.contains(&w.as_str()))
}

/// `(?im)^\s*(A|B)\b`.
fn a_line_starts_with(sql: &str, any: &[&str]) -> bool {
    sql.lines().any(|l| {
        let first: String = l.trim_start().chars().take_while(|c| is_word(*c)).collect();
        any.contains(&first.to_uppercase().as_str())
    })
}

/// Collapses white space and upper-cases (for `A\s+B` searches).
fn squash(sql: &str) -> String {
    sql.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_uppercase()
}

/// The identifiers after every `prefix` (case-insensitive) in `sql`.
fn idents_after(sql: &str, prefix: &str) -> Vec<String> {
    let s = squash(sql);
    let original: Vec<char> = sql
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .collect();
    let upper: Vec<char> = s.chars().collect();
    let p: Vec<char> = prefix.to_uppercase().chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + p.len() <= upper.len() {
        if upper[i..i + p.len()] == p[..] {
            let id: String = original[i + p.len()..]
                .iter()
                .take_while(|c| is_word(**c))
                .collect();
            out.push(id);
            i += p.len();
        } else {
            i += 1;
        }
    }
    out
}

/// `GRANT\s+(?<perm>[A-Z, ]+?)\s+ON\s+OBJECT::dbo\.(?<obj>\w+)(\s*\((?<cols>[^)]*)\))?\s+TO\s+\[pda_sync\]`.
fn grants(sql: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut rest = sql;
    while let Some(at) = rest.find("GRANT") {
        let after = &rest[at + 5..];
        rest = after;
        let Some(after) = after.strip_prefix(|c: char| c.is_whitespace()) else {
            continue;
        };
        let after = after.trim_start();
        let Some(on) = after.find(" ON OBJECT::dbo.") else {
            continue;
        };
        let perm = after[..on].trim_end();
        if perm.is_empty()
            || !perm
                .chars()
                .all(|c| c.is_ascii_uppercase() || c == ',' || c == ' ')
        {
            continue;
        }
        let tail = &after[on + " ON OBJECT::dbo.".len()..];
        let obj: String = tail.chars().take_while(|c| is_word(*c)).collect();
        let mut tail = tail[obj.len()..].trim_start();
        let mut cols = String::new();
        if let Some(t) = tail.strip_prefix('(') {
            let Some(close) = t.find(')') else { continue };
            cols = t[..close].to_owned();
            tail = t[close + 1..].trim_start();
        }
        if tail.starts_with("TO [pda_sync]") {
            out.push((perm.to_owned(), obj, cols));
        }
    }
    out
}

fn statements(sql: &str) -> Vec<&str> {
    sql.split('\n')
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("--"))
        .collect()
}

fn script(kind: StepKind) -> &'static str {
    let step = STEPS.iter().find(|s| s.kind == kind).unwrap();
    read_script(step.resource_name).unwrap()
}

fn sorted(mut v: Vec<(String, String, String)>) -> Vec<(String, String, String)> {
    v.sort();
    v
}

fn expect(rows: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    sorted(
        rows.iter()
            .map(|(a, b, c)| ((*a).to_owned(), (*b).to_owned(), (*c).to_owned()))
            .collect(),
    )
}

#[test]
fn embedded_scripts_only_create_agent_owned_objects_and_never_touch_vendor_tables() {
    for step in STEPS.iter().filter(|s| {
        !matches!(
            s.kind,
            StepKind::CommandGrants
                | StepKind::OrderGrants
                | StepKind::OrderGrantsOw1c
                | StepKind::OrderGrantsOw1e
                | StepKind::SequenceGrants
                | StepKind::MoneyGrants
                | StepKind::FoInhouseGrants
                | StepKind::PostToSmileGrants
        )
    }) {
        let sql = read_script(step.resource_name).unwrap();
        assert!(
            !a_line_starts_with(
                sql,
                &[
                    "DROP", "TRUNCATE", "DELETE", "UPDATE", "INSERT", "MERGE", "EXEC"
                ]
            ),
            "{}",
            step.id
        );
        assert!(!squash(sql).contains("ALTER TABLE"), "{}", step.id);
        for t in idents_after(sql, "CREATE TABLE dbo.") {
            assert!(t.starts_with("Pda_Sync"), "{}: {t}", step.id);
        }
        for t in idents_after(sql, "ON OBJECT::dbo.") {
            assert!(t.starts_with("Pda_Sync"), "{}: {t}", step.id);
        }
    }
}

#[test]
fn command_grants_script_only_grants_the_documented_vendor_writes() {
    let sql = script(StepKind::CommandGrants);
    for l in statements(sql) {
        assert!(
            l.starts_with("GRANT ")
                || l.starts_with("IF COL_LENGTH(")
                || l.starts_with("EXECUTE sys.sp_executesql N'GRANT "),
            "{l}"
        );
    }
    assert!(!has_word(
        sql,
        &[
            "CREATE", "ALTER", "DROP", "TRUNCATE", "DENY", "REVOKE", "MERGE"
        ]
    ));
    assert_eq!(
        sorted(grants(sql)),
        expect(&[
            ("UPDATE", "tmpTrnInfo", "IsDone, DoneTime"),
            ("UPDATE", "tmpTrnInfo", "KdsAcceptedTime, KdsAcceptedBy"),
            ("INSERT, DELETE", "TableReservation", ""),
        ])
    );
}

#[test]
fn order_grants_script_only_grants_the_documented_vendor_writes() {
    let sql = script(StepKind::OrderGrants);
    for l in statements(sql) {
        assert!(
            l.starts_with("GRANT ")
                || l.starts_with("IF OBJECT_ID(")
                || l.starts_with("EXECUTE sys.sp_executesql N'GRANT "),
            "{l}"
        );
    }
    assert!(!has_word(
        sql,
        &[
            "CREATE", "ALTER", "DROP", "TRUNCATE", "DENY", "REVOKE", "MERGE"
        ]
    ));
    assert_eq!(
        sorted(grants(sql)),
        expect(&[
            ("EXECUTE", "GetRVCSeqNumber", ""),
            ("EXECUTE", "PostITrn", ""),
            ("EXECUTE", "UpdateTmpInvoice", ""),
            ("EXECUTE", "Unicode2TCVN", ""),
            ("INSERT", "tmpInvoice", ""),
            ("INSERT", "tmpInvoiceInfo", ""),
            ("INSERT", "checkopenlog", ""),
            ("UPDATE", "tmpTrn", "Rsv2, DscRef"),
            ("DELETE", "tmpTrn", ""),
            ("INSERT", "tmpTrnInfo", ""),
            (
                "UPDATE",
                "tmpTrnInfo",
                "PToOrder, POrderTime, OrderQty, Void"
            ),
            ("INSERT", "VoidTrn", ""),
            ("INSERT, DELETE", "CGTrn", ""),
            ("INSERT", "PDASendOrder", ""),
        ])
    );
}

#[test]
fn order_grants_ow1c_script_only_grants_the_documented_vendor_writes() {
    let step = STEPS
        .iter()
        .find(|s| s.kind == StepKind::OrderGrantsOw1c)
        .unwrap();
    assert_eq!(step.resource_name, "Sql.080_order_grants_ow1c.sql");
    let sql = read_script(step.resource_name).unwrap();
    let st = statements(sql);
    assert!(st.iter().all(|l| l.starts_with("GRANT ")));
    assert!(!has_word(
        &st.join("\n"),
        &[
            "CREATE", "ALTER", "DROP", "TRUNCATE", "DENY", "REVOKE", "MERGE", "EXEC", "EXECUTE"
        ]
    ));
    assert_eq!(
        grants(sql),
        expect(&[(
            "UPDATE",
            "tmpInvoice",
            "NGuest, NChild, LastChgCashier, LastChgTime"
        )])
    );
}

/// The code of a script without its `--` comments.
fn code(sql: &str) -> String {
    sql.split('\n')
        .map(|l| l.find("--").map_or(l, |at| &l[..at]))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn sequence_grants_script_grants_only_the_per_rvc_counters_by_their_kind() {
    let step = STEPS
        .iter()
        .find(|s| s.kind == StepKind::SequenceGrants)
        .unwrap();
    assert_eq!(step.id, "sequence_grants");
    assert_eq!(step.resource_name, "Sql.100_sequence_grants.sql");
    // ADR 0174: the money grants (180) run after the sequence grants.
    let at = STEPS.iter().position(|s| s == step).unwrap();
    assert_eq!(STEPS[at + 1].kind, StepKind::MoneyGrants);
    let sql = code(read_script(step.resource_name).unwrap());
    // RT-0654: grants only; the counter name comes from sys.objects (prefix and digits)
    // and is quoted, and the grant follows the object's kind.
    assert!(!has_word(
        &sql,
        &[
            "CREATE", "ALTER", "DROP", "TRUNCATE", "DENY", "REVOKE", "MERGE", "EXEC"
        ]
    ));
    assert!(!a_line_starts_with(&sql, &["UPDATE", "INSERT", "DELETE"]));
    for needle in [
        "FROM sys.objects o",
        "o.type IN ('SO', 'U')",
        "o.name LIKE N'LastCheck[0-9]%' AND SUBSTRING(o.name, 10, 128) NOT LIKE N'%[^0-9]%'",
        "o.name LIKE N'LastTrn[0-9]%' AND SUBSTRING(o.name, 8, 128) NOT LIKE N'%[^0-9]%'",
        "QUOTENAME(@name)",
        "CASE WHEN @type = 'SO' THEN N'GRANT UPDATE ON OBJECT::dbo.' ELSE N'GRANT SELECT, INSERT, UPDATE, DELETE ON OBJECT::dbo.' END",
        "N' TO [pda_sync];'",
        "EXECUTE sys.sp_executesql @sql;",
    ] {
        assert!(sql.contains(needle), "{needle}");
    }
    assert_eq!(sql.matches("GRANT ").count(), 2);
}
