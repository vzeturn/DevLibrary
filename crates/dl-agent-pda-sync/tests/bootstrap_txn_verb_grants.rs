//! The shift, cash and room-charge scripts on this agent (RT-1427; port of the C#
//! `BootstrapTests` for 145, 150 and 160): they run right after 140, 145 adds only the one
//! nullable column to the agent-owned table, 150 and 160 grant only what the verbs write, and
//! the order-grants probe checks the shift and cash-log grants as the .NET probe does.

use dl_agent_pda_sync::bootstrap::{CHECKED, STEPS, StepKind, read_script};

/// The code of a script without its `--` comments.
fn code(resource: &str) -> String {
    read_script(resource)
        .expect(resource)
        .split('\n')
        .map(|l| l.find("--").map_or(l, |at| &l[..at]))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether `sql` holds one of `any` as a whole word (case-insensitive).
fn has_word(sql: &str, any: &[&str]) -> bool {
    sql.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|w| any.contains(&w.to_uppercase().as_str()))
}

/// Every `GRANT <perm> ON OBJECT::dbo.<obj> [(<cols>)] TO [pda_sync]` as `"<perm> <obj>(<cols>)"`.
fn grants(sql: &str) -> Vec<String> {
    let flat = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.split("GRANT ")
        .skip(1)
        .map(|g| {
            let g = &g[..g.find(" TO [pda_sync]").expect(g)];
            let (perm, target) = g.split_once(" ON OBJECT::dbo.").expect(g);
            let (obj, cols) = match target.split_once('(') {
                Some((obj, cols)) => (obj.trim(), cols.trim_end_matches(')')),
                None => (target.trim(), ""),
            };
            format!("{perm} {obj}({cols})")
        })
        .collect()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[test]
fn the_txn_verb_scripts_run_right_after_the_txn_grants() {
    let at = STEPS.iter().position(|s| s.id == "txn_grants").unwrap();
    let next: Vec<_> = STEPS[at + 1..at + 4]
        .iter()
        .map(|s| (s.id, s.kind, s.object_name, s.resource_name, s.server_level))
        .collect();
    assert_eq!(
        next,
        [
            (
                "txn_store_key",
                StepKind::TxnStoreKey,
                "Pda_TxnApplied",
                "Sql.145_txn_store_key.sql",
                false
            ),
            (
                "txn_shift_grants",
                StepKind::TxnShiftGrants,
                "pda_sync",
                "Sql.150_txn_shift_grants.sql",
                false
            ),
            (
                "fo_room_charge_grants",
                StepKind::FoRoomChargeGrants,
                "pda_sync",
                "Sql.160_fo_room_charge_grants.sql",
                false
            ),
        ]
    );
    // The stock verbs' scripts (170..177) follow, then the money verbs' 180.
    assert_eq!(STEPS[at + 4].id, "txn_stock");
    assert_eq!(STEPS[at + 8].id, "money_grants");
}

#[test]
fn the_order_grants_probe_checks_the_shift_and_cash_log_grants() {
    for row in [
        ("dbo.CashierShift", "INSERT", None),
        ("dbo.CashierShift", "UPDATE", Some("CashEnd")),
        ("dbo.ShiftMoney", "DELETE", None),
        ("dbo.DailyCashLog", "INSERT", None),
    ] {
        assert!(CHECKED.contains(&row), "{row:?}");
    }
}

#[test]
fn the_store_key_script_only_adds_one_nullable_column_to_the_agent_owned_table() {
    let sql = code("Sql.145_txn_store_key.sql");
    let flat = sql.split_whitespace().collect::<Vec<_>>().join(" ");

    assert!(flat.contains("ALTER TABLE dbo.Pda_TxnApplied ADD StoreKey BIGINT NULL;"));
    let words: usize = [
        "ALTER", "CREATE", "DROP", "GRANT", "UPDATE", "INSERT", "DELETE",
    ]
    .iter()
    .map(|w| has_word_count(&sql, w))
    .sum();
    assert_eq!(words, 1, "only the one ALTER");
    assert!(
        sql.contains("COL_LENGTH(N'dbo.Pda_TxnApplied', N'StoreKey') IS NULL"),
        "idempotent"
    );
}

fn has_word_count(sql: &str, word: &str) -> usize {
    sql.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| w.eq_ignore_ascii_case(word))
        .count()
}

#[test]
fn the_shift_grants_script_grants_only_what_the_shift_and_cash_verbs_write() {
    let sql = code("Sql.150_txn_shift_grants.sql");

    assert!(!has_word(
        &sql,
        &[
            "CREATE", "ALTER", "DROP", "TRUNCATE", "DENY", "REVOKE", "MERGE", "SELECT"
        ]
    ));
    assert!(
        !sql.lines()
            .any(|l| l.trim_start().to_uppercase().starts_with("UPDATE"))
    );
    let found = grants(&sql);
    let expected = [
        "INSERT CashierShift()",
        "UPDATE CashierShift(ShiftStatus)",
        "UPDATE CashierShift(EndTime)",
        "UPDATE CashierShift(CloseDate)",
        "UPDATE CashierShift(CashOpen)",
        "UPDATE CashierShift(CashIn)",
        "UPDATE CashierShift(CashOut)",
        "UPDATE CashierShift(CashEnd)",
        "UPDATE CashierShift(ShiftNotes)",
        "INSERT, DELETE ShiftMoney()",
        "INSERT DailyCashLog()",
    ];
    assert_eq!(
        sorted(found.clone()),
        sorted(expected.map(String::from).to_vec())
    );
    assert_eq!(
        has_word_count(&sql, "GRANT"),
        found.len(),
        "every grant is one of the listed ones"
    );
}

#[test]
fn the_room_charge_script_grants_only_the_folio_reads_the_reversal_and_the_vendor_procedure() {
    let sql = code("Sql.160_fo_room_charge_grants.sql");

    assert!(!has_word(
        &sql,
        &[
            "ALTER", "DROP", "TRUNCATE", "DENY", "REVOKE", "MERGE", "DELETE"
        ]
    ));
    let flat = sql
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_uppercase();
    for banned in [
        "UPDATE DBO",
        "INSERT INTO",
        "CREATE TABLE",
        "CREATE PROC",
        "CREATE LOGIN",
    ] {
        assert!(!flat.contains(banned), "{banned}");
    }
    let found = grants(&sql);
    let expected = [
        "SELECT Folio()",
        "SELECT, INSERT FolioTransaction()",
        "UPDATE FolioTransaction(FolioBalanceCode)",
        "EXECUTE PostInterfaceTransactionToFolio()",
    ];
    assert_eq!(
        sorted(found.clone()),
        sorted(expected.map(String::from).to_vec())
    );
    assert_eq!(
        has_word_count(&sql, "GRANT"),
        found.len(),
        "every grant is one of the listed ones"
    );
    assert!(
        sql.contains("NOT LIKE N'%[^A-Za-z0-9_]%'"),
        "the database name is checked before it is quoted into the batch"
    );
}
