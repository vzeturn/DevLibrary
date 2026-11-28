//! The SQL text of `txn.counters` and the room-charge verbs (RT-1427) is the C# text
//! (`SqlTxnCounterUnit.cs`, `SqlTxnRoomChargeUnit.cs`, copies of the retired .NET agent's
//! sources in `testdata/cs`) so a change of the T-SQL fails here; line ends are compared
//! without CR.

use dl_agent_pda_sync::commands::{FoPostLine, params};
use dl_agent_pda_sync::contracts::DateTime;
use dl_decimal::Decimal;

const CS_COUNTERS: &str = include_str!("../testdata/cs/Commands/SqlTxnCounterUnit.cs");
const CS_ROOM_CHARGE: &str = include_str!("../testdata/cs/Commands/SqlTxnRoomChargeUnit.cs");

/// The value of the C# `const string {name}`: a raw string (dedented by its closing quotes)
/// or the concatenation of its `"..."` pieces.
fn cs_const(source: &str, name: &str) -> String {
    let source = source.replace("\r\n", "\n");
    let at = source
        .find(&format!("const string {name} ="))
        .unwrap_or_else(|| panic!("{name}"));
    let rest = source[at..].split_once('=').unwrap().1.trim_start();
    if let Some(raw) = rest.strip_prefix("\"\"\"") {
        let raw = &raw[..raw.find("\"\"\"").expect("closing quotes")];
        let lines: Vec<&str> = raw.split('\n').collect();
        let indent = lines.last().map_or(0, |l| l.len());
        return lines[1..lines.len() - 1]
            .iter()
            .map(|l| l.get(indent..).unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
    }
    let end = rest.find("\";\n").expect("end of const") + 1;
    rest[..end]
        .split('"')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, piece)| piece)
        .collect()
}

fn lf(s: &str) -> String {
    s.replace("\r\n", "\n")
}

#[test]
fn the_counter_statements_are_the_csharp_text() {
    for (rust, name) in [
        (params::COUNTER_OWN_OBJECT_SQL, "OwnObjectSql"),
        (params::COUNTER_LOCK_SQL, "LockSql"),
        (params::COUNTER_RAISE_SQL, "RaiseSql"),
    ] {
        assert_eq!(rust, cs_const(CS_COUNTERS, name), "{name}");
    }
}

#[test]
fn the_counter_backing_tables_are_the_csharp_list_and_compose_the_in_use_read() {
    for (name, tables) in [
        (
            "LastCheck",
            r#"[("Invoice", "CheckNo", true, false), ("TmpInvoice", "CheckNo", true, false), ("IOrgInvoice", "CheckNo", false, false)]"#,
        ),
        (
            "LastTrn",
            r#"[("Trn", "TrnSeq", true, false), ("TmpTrn", "TrnSeq", true, false)]"#,
        ),
        (
            "LastPayment",
            r#"[("Payment", "PaymentNo", true, true), ("TmpPayment", "PaymentNo", true, true)]"#,
        ),
        (
            "CloseNumber",
            r#"[("TaxTable", "Sub2", true, false), ("TmpTaxTable", "Sub2", true, false)]"#,
        ),
    ] {
        assert!(
            CS_COUNTERS.contains(&format!("[TxnCounterNames.{name}] = {tables}")),
            "{name}"
        );
        let ours: Vec<String> = params::counter_backing(name)
            .iter()
            .map(|(t, c, r, raw)| format!("(\"{t}\", \"{c}\", {r}, {raw})"))
            .collect();
        assert_eq!(format!("[{}]", ours.join(", ")), tables, "{name}");
    }
    assert_eq!(
        params::counter_in_use_sql("Payment", "PaymentNo", true, true).unwrap(),
        "IF OBJECT_ID(N'dbo.Payment', N'U') IS NOT NULL SELECT CAST(MAX(CASE WHEN [PaymentNo] >= @Prefix AND [PaymentNo] < @Prefix + 100000000 THEN [PaymentNo] - @Prefix WHEN [PaymentNo] >= 1 AND [PaymentNo] < 100000000 THEN [PaymentNo] END) AS bigint) FROM dbo.[Payment] WHERE RVC = @Rvc; ELSE SELECT CAST(NULL AS bigint);"
    );
    assert_eq!(
        params::counter_in_use_sql("IOrgInvoice", "CheckNo", false, false).unwrap(),
        "IF OBJECT_ID(N'dbo.IOrgInvoice', N'U') IS NOT NULL SELECT CAST(MAX(CASE WHEN [CheckNo] >= @Prefix AND [CheckNo] < @Prefix + 100000000 THEN [CheckNo] - @Prefix END) AS bigint) FROM dbo.[IOrgInvoice] WHERE [CheckNo] >= @Prefix AND [CheckNo] < @Prefix + 100000000; ELSE SELECT CAST(NULL AS bigint);"
    );
    assert!(params::counter_backing("Nope").is_empty());
}

#[test]
fn the_room_charge_statements_are_the_csharp_text() {
    for (rust, name) in [
        (params::DATABASE_ACCESS_SQL, "DatabaseAccessSql"),
        (params::LINK_SQL, "LinkSql"),
        (params::FOLIOS_BY_ROOM_SQL, "FoliosByRoomSql"),
        (params::FOLIO_BY_NUMBER_SQL, "FolioByNumberSql"),
        (params::ROOM_CHARGE_ACTIVE_SQL, "ActiveSql"),
        (params::ROOM_CHARGE_POST_SQL, "PostSql"),
    ] {
        assert_eq!(lf(rust), cs_const(CS_ROOM_CHARGE, name), "{name}");
    }
    let reverse = cs_const(CS_ROOM_CHARGE, "ReverseSql")
        .replace("{0}", "[SMILE_FO]")
        .replace("{1}", "@Id0, @Id1");
    assert_eq!(lf(&params::room_charge_reverse_sql("SMILE_FO", 2)), reverse);
}

#[test]
fn the_post_binds_every_value_as_a_parameter_with_the_csharp_names() {
    let line = FoPostLine {
        folio_num: 5001,
        transaction_code: 12,
        transaction_sub_code: 3,
        ref_number: "70001".into(),
        amount: Decimal::from(250_000),
        meal_period: 0,
        comment: "Cloud bill 70001".into(),
        clerk_id: "$AM".into(),
        transaction_time: DateTime::try_parse_invariant("2026-11-24T10:15:00").unwrap(),
        sts_code8: "x".into(),
    };
    let names: Vec<String> = params::room_charge_post_parameters(&line)
        .unwrap()
        .iter()
        .map(|p| p.name().trim_start_matches('@').to_owned())
        .collect();
    assert_eq!(
        names,
        [
            "FolioNum",
            "TrnCode",
            "TrnSub",
            "Ref",
            "Amount",
            "MealPeriod",
            "Comment",
            "Clerk",
            "At",
            "Sts8"
        ]
    );
    for n in &names {
        assert!(
            CS_ROOM_CHARGE.contains(&format!("cmd.Parameters.Add(\"@{n}\"")),
            "{n}"
        );
        assert!(
            params::ROOM_CHARGE_POST_SQL.contains(&format!("= @{n}")),
            "{n}"
        );
    }
}
