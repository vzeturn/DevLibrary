//! `txn.snapshot` wire contract (ADR 0159, RT-0849): the snake_case shape the cloud's
//! `TxnSnapshotPayload` / `TxnSnapshotResult` (System.Text.Json, `SiteCommandJson`)
//! writes and reads, the scope allow-list and the `Problem` messages, same text.

use crate::contracts::json::WireJson;
use crate::contracts::wire::*;

const ROW: &str = r#"{"CheckNo":41,"Remark":"Bàn 4"}"#;

fn payload(tables: &str) -> String {
    format!(
        r#"{{"scope":"bill","scope_key":41,"version":3,"tables":[{tables}],"actor":"clerk-7"}}"#
    )
}

fn all_tables(rows_of_tmp_invoice: &str) -> String {
    txn_scopes::BILL_TABLES
        .iter()
        .map(|t| {
            let rows = if *t == "TmpInvoice" {
                rows_of_tmp_invoice
            } else {
                "[]"
            };
            format!(r#"{{"table":"{t}","rows":{rows}}}"#)
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn problem(json: &str) -> Option<String> {
    TxnSnapshotPayload::from_json(json).unwrap().problem()
}

#[test]
fn txn_names_match_the_dotnet_constants() {
    assert_eq!(txn_command_types::SNAPSHOT, "txn.snapshot");
    assert!(txn_command_types::is_txn(Some("txn.snapshot")));
    assert!(txn_command_types::is_txn(Some("txn.other")));
    assert!(!txn_command_types::is_txn(Some("order.post")));
    assert!(!txn_command_types::is_txn(None));
    assert_eq!(
        txn_scopes::BILL_TABLES,
        [
            "TmpInvoice",
            "TmpTrn",
            "TmpPayment",
            "Invoice",
            "Trn",
            "Payment"
        ]
    );
    assert_eq!(txn_scopes::aggregate("bill", 41), "bill:41");
    assert_eq!(txn_scopes::aggregate_key("bill", 41), "txn:bill:41");
    assert_eq!(txn_scopes::find("bill").map(|f| f.0), Some("CheckNo"));
    assert!(txn_scopes::find("Bill").is_none());
    assert_eq!(
        (txn_outcomes::APPLIED, txn_outcomes::STALE),
        ("applied", "stale")
    );
    assert_eq!(
        (
            txn_reasons::COLUMN_UNKNOWN,
            txn_reasons::VALUE_INVALID,
            txn_reasons::KEY_CONFLICT
        ),
        (
            "txn_column_unknown",
            "txn_value_invalid",
            "txn_key_conflict"
        )
    );
}

#[test]
fn payload_round_trips_in_snake_case_with_row_values_verbatim() {
    let json = payload(&all_tables(&format!("[{ROW}]")));
    let p = TxnSnapshotPayload::from_json(&json).unwrap();
    assert_eq!((p.scope.as_str(), p.scope_key, p.version), ("bill", 41, 3));
    assert_eq!(p.actor, "clerk-7");
    assert_eq!(p.aggregate(), "bill:41");
    assert_eq!(p.rows_of("TmpInvoice").len(), 1);
    assert!(p.rows_of("Trn").is_empty());
    assert!(p.rows_of("Nope").is_empty());
    assert_eq!(p.problem(), None);
    // System.Text.Json's default encoder escapes non-ASCII text.
    assert_eq!(p.to_json(), json.replace("Bàn", "B\\u00E0n"));
}

#[test]
fn result_writes_row_counts_as_an_object_in_scope_order() {
    let res = TxnSnapshotResult {
        aggregate: "bill:41".into(),
        version: 3,
        outcome: txn_outcomes::APPLIED.into(),
        applied_version: 3,
        hash: "ab".into(),
        row_counts: vec![("TmpInvoice".into(), 1), ("Invoice".into(), 0)],
    };
    let json = res.to_json();
    assert_eq!(
        json,
        r#"{"aggregate":"bill:41","version":3,"outcome":"applied","applied_version":3,"hash":"ab","row_counts":{"TmpInvoice":1,"Invoice":0}}"#
    );
    assert_eq!(TxnSnapshotResult::from_json(&json).ok(), Some(res));
    assert!(TxnSnapshotResult::from_json(
        r#"{"aggregate":"a","version":1,"outcome":"applied","applied_version":1,"hash":"h","row_counts":{"T":1.5}}"#
    )
    .is_err());
}

#[test]
fn problems_carry_the_dotnet_messages() {
    let ok = all_tables("[]");
    assert_eq!(problem(&payload(&ok)), None);
    let cases: [(String, &str); 9] = [
        (
            payload(&ok).replace(r#""scope":"bill""#, r#""scope":"room""#),
            "Scope is not allowed.",
        ),
        (
            payload(&ok).replace(r#""scope_key":41"#, r#""scope_key":0"#),
            "ScopeKey must be positive.",
        ),
        (
            payload(&ok).replace(r#""version":3"#, r#""version":0"#),
            "Version must be positive.",
        ),
        (
            payload(&ok).replace(r#""actor":"clerk-7""#, r#""actor":" ""#),
            "Actor is required.",
        ),
        (
            payload(r#"{"table":"TmpInvoice","rows":[]}"#),
            "Tables must list every table of the scope exactly once.",
        ),
        (
            payload(&ok.replace(r#""table":"Payment""#, r#""table":"Users""#)),
            "Table is not allowed.",
        ),
        (
            payload(&ok.replace(r#""table":"Payment""#, r#""table":"Trn""#)),
            "Table Trn is listed twice.",
        ),
        (
            payload(&ok.replace(
                r#"{"table":"Trn","rows":[]}"#,
                r#"{"table":"Trn","rows":null}"#,
            )),
            "Rows of Trn are required.",
        ),
        (
            payload(&all_tables(r#"[{"CheckNo":42}]"#)),
            "Every row of TmpInvoice must carry CheckNo = ScopeKey.",
        ),
    ];
    for (json, expected) in cases {
        assert_eq!(problem(&json).as_deref(), Some(expected), "{json}");
    }
    // The scope key must be an integer lexeme (`TryGetInt64`), not 41.0 or "41".
    for key in ["41.0", r#""41""#, "null"] {
        let json = payload(&all_tables(&format!(r#"[{{"CheckNo":{key}}}]"#)));
        assert!(problem(&json).is_some(), "{key}");
    }
    let many = format!("[{}]", vec![r#"{"CheckNo":41}"#; 501].join(","));
    assert_eq!(
        problem(&payload(&all_tables(&many))).as_deref(),
        Some("Too many rows for TmpInvoice.")
    );
}
