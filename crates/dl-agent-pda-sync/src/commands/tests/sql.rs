//! The store SQL of the command verbs: every constant is the C# text byte for byte (read
//! from the C# sources), the SHA-256 pins of the C# tests hold, and each statement is
//! bound exactly its free parameters (`TableCommandSqlTests`, `SiteCommandOrderTests`,
//! `SiteCommandComboTests`, `BootstrapTests` SQL-text cases).

use std::collections::{BTreeSet, HashMap};

use dl_decimal::Decimal;
use dl_hash::{Digest, Sha256};
use dl_tds::{Param, ParamValue};

use crate::commands::batches as b;
use crate::commands::params as p;
use crate::commands::sql_text as q;
use crate::commands::{StoreColumn, schema_columns};
use crate::contracts::wire::{
    OrderCheckCancelEmptyPayload, OrderCheckExpectation, OrderCheckMergePayload,
    OrderCheckOpenPayload, OrderComboPostPayload, OrderCoversPayload, OrderItemPostPayload,
    OrderItemVoidPayload, OrderItemsMovePayload, OrderSendPayload, OrderTableChangePayload,
    OrderTableTransferPayload, table_command_types,
};

const RVC: i32 = 3;

const CS_BATCHES: &str = include_str!("../../../testdata/cs/Commands/OrderCommandSql.Batches.cs");
const CS_OW1C: &str = include_str!("../../../testdata/cs/Commands/OrderCommandSql.BatchesOw1c.cs");
const CS_OW1D: &str = include_str!("../../../testdata/cs/Commands/OrderCommandSql.BatchesOw1d.cs");
const CS_OW1E: &str = include_str!("../../../testdata/cs/Commands/OrderCommandSql.BatchesOw1e.cs");
const CS_ORDER_UNIT: &str = include_str!("../../../testdata/cs/Commands/SqlOrderCommandUnit.cs");
const CS_TABLE_UNIT: &str = include_str!("../../../testdata/cs/Commands/SqlTableCommandUnit.cs");
const CS_STORE: &str = include_str!("../../../testdata/cs/Commands/SqlSiteCommandStore.cs");

// ---- a reader of C# string constants: verbatim and regular literals joined by `+`,
// ---- references to constants declared earlier in the same file

fn skip_ws(s: &[u8], mut i: usize) -> usize {
    while i < s.len() && s[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn ident(s: &[u8], mut i: usize) -> (String, usize) {
    let start = i;
    while i < s.len() && (s[i].is_ascii_alphanumeric() || s[i] == b'_' || s[i] == b'.') {
        i += 1;
    }
    (String::from_utf8_lossy(&s[start..i]).into_owned(), i)
}

/// Parses `lit (+ lit)*` starting at `i`; returns the text and the index after it.
fn expr(src: &str, mut i: usize, known: &HashMap<String, String>) -> (String, usize) {
    let s = src.as_bytes();
    let mut out = String::new();
    loop {
        i = skip_ws(s, i);
        if s[i..].starts_with(b"@\"") {
            i += 2;
            let mut buf = Vec::new();
            loop {
                if s[i] == b'"' {
                    if s.get(i + 1) == Some(&b'"') {
                        buf.push(b'"');
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                buf.push(s[i]);
                i += 1;
            }
            out.push_str(std::str::from_utf8(&buf).unwrap());
        } else if s[i] == b'"' {
            i += 1;
            let mut buf = Vec::new();
            while s[i] != b'"' {
                assert_ne!(s[i], b'\\', "escapes are not expected in the C# SQL text");
                buf.push(s[i]);
                i += 1;
            }
            i += 1;
            out.push_str(std::str::from_utf8(&buf).unwrap());
        } else {
            let (name, next) = ident(s, i);
            let short = name.rsplit('.').next().unwrap();
            out.push_str(
                known
                    .get(short)
                    .unwrap_or_else(|| panic!("unknown constant {name}")),
            );
            i = next;
        }
        i = skip_ws(s, i);
        if s[i] == b'+' {
            i += 1;
        } else {
            return (out, i);
        }
    }
}

/// Every `const string Name = ...;` of a C# file (CRLF normalised).
pub(super) fn cs_consts(file: &str) -> HashMap<String, String> {
    let src = file.replace("\r\n", "\n");
    let mut known = HashMap::new();
    let mut from = 0;
    while let Some(at) = src[from..].find("const string ") {
        let i = from + at + "const string ".len();
        let (name, j) = ident(src.as_bytes(), i);
        let eq = src[j..].find('=').unwrap() + j + 1;
        let (value, end) = expr(&src, eq, &known);
        known.insert(name, value);
        from = end;
    }
    known
}

/// The `[KdsLineActions.X] = ...,` entries of `SqlSiteCommandUnit.KdsLineSql`.
fn cs_kds(file: &str) -> HashMap<String, String> {
    let src = file.replace("\r\n", "\n");
    let mut out = HashMap::new();
    let mut from = 0;
    while let Some(at) = src[from..].find("[KdsLineActions.") {
        let i = from + at + "[KdsLineActions.".len();
        let (name, j) = ident(src.as_bytes(), i);
        let eq = src[j..].find('=').unwrap() + j + 1;
        let (value, end) = expr(&src, eq, &HashMap::new());
        out.insert(name, value);
        from = end;
    }
    out
}

#[test]
fn every_batch_is_the_csharp_text_byte_for_byte() {
    let mut cs = cs_consts(CS_BATCHES);
    cs.extend(cs_consts(CS_OW1C));
    cs.extend(cs_consts(CS_OW1D));
    cs.extend(cs_consts(CS_OW1E));
    let rust: &[(&str, &str)] = &[
        ("OpenCheck", b::OPEN_CHECK),
        ("PostItem", b::POST_ITEM),
        ("PendingSendSeqs", b::PENDING_SEND_SEQS),
        ("SendOrder", b::SEND_ORDER),
        ("VoidUnsent", b::VOID_UNSENT),
        ("VoidSent", b::VOID_SENT),
        ("SetCovers", b::SET_COVERS),
        ("PostCombo", b::POST_COMBO),
        ("TableInfo", b::TABLE_INFO),
        ("TransferHeader", b::TRANSFER_HEADER),
        ("TransferLineInfo", b::TRANSFER_LINE_INFO),
        ("LockCheck", b::LOCK_CHECK),
        ("MergeLines", b::MERGE_LINES),
        ("MergeLineInfo", b::MERGE_LINE_INFO),
        ("CancelOrder", b::CANCEL_ORDER),
        ("ReCalcBill", b::RE_CALC_BILL),
        ("MoveSourceLines", b::MOVE_SOURCE_LINES),
        ("MoveLine", b::MOVE_LINE),
        ("MoveLineInfo", b::MOVE_LINE_INFO),
        ("LogTrnMove", b::LOG_TRN_MOVE),
    ];
    assert_eq!(cs.len(), rust.len(), "every C# batch has its Rust constant");
    for (name, text) in rust {
        assert_eq!(cs.get(*name).map(String::as_str), Some(*text), "{name}");
    }
}

#[test]
fn the_agent_sql_is_the_csharp_text_byte_for_byte() {
    let order = cs_consts(CS_ORDER_UNIT);
    let table = cs_consts(CS_TABLE_UNIT);
    let store = cs_consts(CS_STORE);
    let cases: &[(&HashMap<String, String>, &str, &str)] = &[
        (&order, "CheckStatus", q::CHECK_STATUS),
        (&order, "ItemExists", q::ITEM_EXISTS),
        (&order, "FindLine", q::FIND_LINE),
        (&order, "CgTrnExists", q::CG_TRN_EXISTS),
        (&order, "ReadHeader", q::READ_HEADER),
        (&order, "ReadLines", q::READ_LINES),
        (&order, "ReadLinesWithNotes", q::READ_LINES_WITH_NOTES),
        (&table, "MoneyFacts", q::MONEY_FACTS),
        (&table, "MissingColumnsHead", q::MISSING_COLUMNS_HEAD),
        (&table, "MissingColumnsTail", q::MISSING_COLUMNS_TAIL),
        (&table, "AuditSavepoint", q::AUDIT_SAVEPOINT),
        (&store, "ReadLogSql", q::READ_LOG),
        (&store, "LockLogSql", q::LOCK_LOG),
        (&store, "InsertLogSql", q::INSERT_LOG),
        (&store, "OpenCheckSql", q::OPEN_CHECK_BELONGS),
        (&store, "CheckStateSql", q::CHECK_STATE),
        (&store, "KdsAcceptColumnsSql", q::KDS_ACCEPT_COLUMNS),
        (&store, "InsertReservationSql", q::INSERT_RESERVATION),
        (&store, "DeleteReservationSql", q::DELETE_RESERVATION),
    ];
    for (map, name, text) in cases {
        assert_eq!(map.get(*name).map(String::as_str), Some(*text), "{name}");
    }
    let kds = cs_kds(CS_STORE);
    assert_eq!(kds.len(), 4);
    for (name, action) in [
        ("Accept", "accept"),
        ("Unaccept", "unaccept"),
        ("Done", "done"),
        ("Undone", "undone"),
    ] {
        assert_eq!(
            kds.get(name).map(String::as_str),
            q::kds_line_sql(action),
            "{name}"
        );
    }
    assert_eq!(q::kds_line_sql("other"), None);
}

fn sha256(text: &str) -> String {
    let normalised = text.replace("\r\n", "\n");
    dl_encoding::hex::encode(Sha256::digest(normalised.as_bytes()).as_ref()).to_ascii_uppercase()
}

#[test]
fn multi_line_batches_are_the_legacy_text_byte_for_byte() {
    // TableCommandSqlTests: SHA-256 of the legacy constant, line endings normalised.
    for (text, pin) in [
        (
            b::TABLE_INFO,
            "9C011A3C577192D8B8949B4A7A920B48521BD07E3D5808DC5467D4516110AEFF",
        ),
        (
            b::CANCEL_ORDER,
            "7A6AFA2AA8692C545F81742F2AE3F24514D443CD06DC22962BA1A0409E27B51B",
        ),
        (
            b::RE_CALC_BILL,
            "736AB5355BCF4BB451EEC589BF32E709C502EA2E7FB8FAA48035F011BC751388",
        ),
        (
            b::LOG_TRN_MOVE,
            "CA4CA9D915054C631E22B85453A52AC3D4E44A3A9C4120B045B12E3B878539DA",
        ),
    ] {
        assert_eq!(sha256(text), pin);
    }
}

#[test]
fn covers_batch_is_the_legacy_text_byte_for_byte() {
    assert_eq!(
        sha256(b::SET_COVERS),
        "EB4B222AE41DC0F32E4731310D6832870B3F9E847F7779CC2045E27CDB2C1387"
    );
}

#[test]
fn combo_batch_is_pinned() {
    assert_eq!(
        sha256(b::POST_COMBO),
        "45B5FD454EC1C43BB1579FAD4B263F48EC011CB5813D800EC18ED860BB93C908"
    );
}

#[test]
fn one_statement_batches_are_the_legacy_service_statements() {
    assert_eq!(
        b::TRANSFER_HEADER,
        "UPDATE dbo.TmpInvoice SET TableNo = @ToTableNo WHERE CheckNo = @CheckNo AND RVC = @RVCNo"
    );
    assert_eq!(
        b::TRANSFER_LINE_INFO,
        "UPDATE dbo.tmpTrnInfo SET TableNo = @ToTableNo WHERE CheckNo = @CheckNo"
    );
    assert_eq!(
        b::LOCK_CHECK,
        "SELECT CheckNo, Status FROM dbo.TmpInvoice WITH (UPDLOCK, HOLDLOCK) WHERE CheckNo = @CheckNo AND RVC = @RVCNo"
    );
    assert_eq!(
        b::MERGE_LINES,
        "UPDATE dbo.TmpTrn SET CheckNo = @Target WHERE CheckNo = @Source AND RVC = @RVCNo"
    );
    assert_eq!(
        b::MERGE_LINE_INFO,
        "UPDATE dbo.tmpTrnInfo SET CheckNo = @Target WHERE CheckNo = @Source"
    );
    assert_eq!(
        b::MOVE_SOURCE_LINES,
        "SELECT TrnSeq FROM dbo.TmpTrn WITH (UPDLOCK, HOLDLOCK) WHERE CheckNo = @CheckNo AND RVC = @RVCNo AND TrnCode = 100"
    );
    assert_eq!(
        b::MOVE_LINE,
        "UPDATE dbo.TmpTrn SET CheckNo = @ToCheckNo WHERE TrnSeq = @TrnSeq AND CheckNo = @FromCheckNo AND RVC = @RVCNo"
    );
    assert_eq!(
        b::MOVE_LINE_INFO,
        "UPDATE dbo.tmpTrnInfo SET CheckNo = @ToCheckNo WHERE TrnNo = @TrnNo AND CheckNo = @FromCheckNo"
    );
}

// ---- invocation shape: what the SQL unit binds is exactly what each statement expects

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `Regex.Replace(Regex.Replace(sql, "--[^\n]*", ""), "'[^']*'", "''")`.
fn strip(sql: &str) -> String {
    let mut no_comments = String::with_capacity(sql.len());
    let mut rest = sql;
    while let Some(at) = rest.find("--") {
        no_comments.push_str(&rest[..at]);
        rest = &rest[at..];
        rest = rest.find('\n').map_or("", |nl| &rest[nl..]);
    }
    no_comments.push_str(rest);
    let mut out = String::with_capacity(no_comments.len());
    let mut rest = no_comments.as_str();
    while let Some(open) = rest.find('\'') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('\'') {
            Some(close) => {
                out.push_str("''");
                rest = &after[close + 1..];
            }
            None => {
                out.push_str(&rest[open..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

const TYPES: &[&str] = &[
    "TABLE",
    "INT",
    "BIGINT",
    "TINYINT",
    "SMALLINT",
    "BIT",
    "DECIMAL",
    "NUMERIC",
    "MONEY",
    "VARCHAR",
    "NVARCHAR",
    "CHAR",
    "NCHAR",
    "DATETIME",
    "DATE",
    "VARBINARY",
    "CURSOR",
];

/// Whether the `@name` at `at` is declared there (the C# `Declared` regex).
fn declared_at(s: &[u8], at: usize, end: usize) -> bool {
    // Prefix: `,\s*` or `\bDECLARE\s+`.
    let mut i = at;
    while i > 0 && s[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    let ws = at - i;
    let prefix = (i > 0 && s[i - 1] == b',')
        || (ws > 0
            && i >= 7
            && s[i - 7..i].eq_ignore_ascii_case(b"DECLARE")
            && (i == 7 || !is_word(s[i - 8])));
    if !prefix {
        return false;
    }
    // Suffix: `\s+(?:AS\s+)?TYPE\b`.
    let mut j = end;
    let start = j;
    while j < s.len() && s[j].is_ascii_whitespace() {
        j += 1;
    }
    if j == start {
        return false;
    }
    let word = |k: usize| {
        let mut e = k;
        while e < s.len() && is_word(s[e]) {
            e += 1;
        }
        (
            std::str::from_utf8(&s[k..e]).unwrap().to_ascii_uppercase(),
            e,
        )
    };
    let (w, e) = word(j);
    if w == "AS" {
        let mut k = e;
        while k < s.len() && s[k].is_ascii_whitespace() {
            k += 1;
        }
        if k > e && TYPES.contains(&word(k).0.as_str()) {
            return true;
        }
    }
    TYPES.contains(&w.as_str())
}

/// The free `@variables` of a batch (case-insensitive); `exclude_declared` drops the
/// ones the batch declares itself.
pub(super) fn free(sql: &str, exclude_declared: bool) -> BTreeSet<String> {
    let text = strip(sql);
    let s = text.as_bytes();
    let mut referenced = Vec::new();
    let mut declared = BTreeSet::new();
    for i in 0..s.len() {
        if s[i] != b'@' || (i > 0 && s[i - 1] == b'@') {
            continue;
        }
        if i + 1 >= s.len() || !(s[i + 1].is_ascii_alphabetic() || s[i + 1] == b'_') {
            continue;
        }
        let mut e = i + 1;
        while e < s.len() && is_word(s[e]) {
            e += 1;
        }
        let name = text[i + 1..e].to_ascii_lowercase();
        if declared_at(s, i, e) {
            declared.insert(name.clone());
        }
        referenced.push(name);
    }
    referenced
        .into_iter()
        .filter(|n| !exclude_declared || !declared.contains(n))
        .collect()
}

pub(super) fn bound(params: &[Param]) -> BTreeSet<String> {
    params
        .iter()
        .map(|p| p.name().to_ascii_lowercase())
        .collect()
}

pub(super) fn param<'a>(params: &'a [Param], name: &str) -> &'a ParamValue {
    params
        .iter()
        .find(|p| p.name() == name)
        .unwrap_or_else(|| panic!("no @{name}"))
        .value()
}

fn expect(check_no: i64, expected: &str) -> OrderCheckExpectation {
    OrderCheckExpectation {
        check_no,
        expected: expected.into(),
    }
}

fn merge(target: i64, source: i64, cashier: &str) -> OrderCheckMergePayload {
    OrderCheckMergePayload {
        rvc_no: RVC,
        target_check_no: target,
        source_check_no: source,
        cashier: cashier.into(),
        checks: vec![expect(target, "a"), expect(source, "b")],
    }
}

fn mv() -> OrderItemsMovePayload {
    OrderItemsMovePayload {
        rvc_no: RVC,
        from_check_no: 1,
        to_check_no: 2,
        trn_seqs: vec![5, 6],
        cashier: "c".into(),
        checks: vec![expect(1, "a"), expect(2, "b")],
    }
}

#[test]
fn each_table_statement_is_bound_exactly_its_free_parameters() {
    let change = OrderTableChangePayload {
        rvc_no: RVC,
        check_no: 1,
        table_no: "T1".into(),
        num_guest: 2,
        cashier: "c".into(),
    };
    let transfer = OrderTableTransferPayload {
        rvc_no: RVC,
        check_no: 1,
        to_table_no: " T2 ".into(),
        cashier: "c".into(),
    };
    let merge = merge(1, 2, "c");
    let mv = mv();
    let cases: Vec<(&str, &str, Vec<Param>)> = vec![
        (
            "table-info",
            b::TABLE_INFO,
            p::table_info_parameters(&change),
        ),
        (
            "transfer-header",
            b::TRANSFER_HEADER,
            p::transfer_header_parameters(&transfer),
        ),
        (
            "transfer-line-info",
            b::TRANSFER_LINE_INFO,
            p::transfer_line_info_parameters(&transfer),
        ),
        ("lock", b::LOCK_CHECK, p::lock_parameters(RVC, 1)),
        ("money", q::MONEY_FACTS, p::lock_parameters(RVC, 1)),
        (
            "merge-lines",
            b::MERGE_LINES,
            p::merge_lines_parameters(&merge),
        ),
        (
            "merge-line-info",
            b::MERGE_LINE_INFO,
            p::merge_line_info_parameters(&merge),
        ),
        ("cancel", b::CANCEL_ORDER, p::cancel_parameters(&merge)),
        (
            "empty-facts",
            q::EMPTY_BILL_FACTS,
            p::empty_bill_facts_parameters(RVC, 1, "2026-11-23T04:00:00"),
        ),
        (
            "cancel-empty",
            b::CANCEL_ORDER,
            p::cancel_empty_parameters(&OrderCheckCancelEmptyPayload {
                rvc_no: RVC,
                check_no: 1,
                table_no: "A1234".into(),
                cashier: "c".into(),
                opened_before: "2026-11-23T04:00:00".into(),
            }),
        ),
        (
            "move-source",
            b::MOVE_SOURCE_LINES,
            p::lock_parameters(RVC, 1),
        ),
        ("move-line", b::MOVE_LINE, p::move_line_parameters(&mv, 5)),
        (
            "move-line-info",
            b::MOVE_LINE_INFO,
            p::move_line_info_parameters(&mv, 5),
        ),
        (
            "log-move",
            b::LOG_TRN_MOVE,
            p::log_trn_move_parameters(&mv, &[5, 6]),
        ),
    ];
    for (name, sql, params) in cases {
        assert_eq!(bound(&params), free(sql, false), "{name}");
    }
}

#[test]
fn recalculation_and_column_probe_take_only_their_own_parameters() {
    assert_eq!(
        free(b::RE_CALC_BILL, false),
        BTreeSet::from(["checkno".to_owned()])
    );
    let probe = q::missing_columns_sql(3);
    let want: BTreeSet<String> = ["t0", "c0", "t1", "c1", "t2", "c2"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    assert_eq!(free(&probe, false), want);
    let cols = [
        StoreColumn::new("a", "b"),
        StoreColumn::new("c", "d"),
        StoreColumn::new("e", "f"),
    ];
    assert_eq!(bound(&p::missing_columns_parameters(&cols)), want);
    let two = q::missing_columns_sql(2);
    assert!(two.contains("(@t0, @c0), (@t1, @c1)"));
    assert!(two.contains("INFORMATION_SCHEMA.COLUMNS"));
    assert!(two.contains("TABLE_SCHEMA = N'dbo'"));
}

#[test]
fn merge_cancels_the_source_and_binds_the_cashier_as_the_canceller() {
    let params = p::cancel_parameters(&merge(7001, 7002, "clerk-7"));
    assert_eq!(param(&params, "CheckNo"), &ParamValue::I64(Some(7002)));
    assert_eq!(
        param(&params, "CancelBy"),
        &ParamValue::String(Some("clerk-7".into()))
    );
}

#[test]
fn empty_bill_cancel_binds_the_cashier_and_the_facts_read_the_cutoff_as_iso_text() {
    let params = p::cancel_empty_parameters(&OrderCheckCancelEmptyPayload {
        rvc_no: RVC,
        check_no: 7001,
        table_no: "A1234".into(),
        cashier: "SML".into(),
        opened_before: "2026-11-23T04:00:00".into(),
    });
    assert_eq!(param(&params, "CheckNo"), &ParamValue::I64(Some(7001)));
    assert_eq!(
        param(&params, "CancelBy"),
        &ParamValue::String(Some("SML".into()))
    );
    let facts = p::empty_bill_facts_parameters(RVC, 7001, "2026-11-23T04:00:00");
    assert_eq!(
        param(&facts, "OpenedBefore"),
        &ParamValue::String(Some("2026-11-23T04:00:00".into()))
    );
    assert!(q::EMPTY_BILL_FACTS.contains("CONVERT(datetime, @OpenedBefore, 126)"));
    assert!(
        q::EMPTY_BILL_FACTS
            .contains("FROM dbo.TmpTrn t WHERE t.CheckNo = @CheckNo AND t.RVC = @RVCNo")
    );
}

#[test]
fn transfer_binds_the_trimmed_table_and_the_move_audit_lists_the_moved_lines() {
    let transfer = OrderTableTransferPayload {
        rvc_no: RVC,
        check_no: 1,
        to_table_no: "  T7 ".into(),
        cashier: "c".into(),
    };
    let params = p::transfer_header_parameters(&transfer);
    assert_eq!(
        param(&params, "ToTableNo"),
        &ParamValue::String(Some("T7".into()))
    );
    let mut mv = mv();
    mv.checks.clear();
    let audit = p::log_trn_move_parameters(&mv, &[5, 6]);
    assert_eq!(
        param(&audit, "TrnSeqCsv"),
        &ParamValue::String(Some("5,6".into()))
    );
}

fn post_payload() -> OrderItemPostPayload {
    OrderItemPostPayload {
        rvc_no: RVC,
        check_no: 1,
        cashier: "c".into(),
        item_code: "CF01".into(),
        qty: Decimal::ONE,
        seat_no: 0,
        price: Decimal::ONE,
        note: None,
        open_name: None,
        allow_zero_open_price: false,
    }
}

#[test]
fn each_order_batch_is_bound_exactly_its_free_parameters() {
    let void = OrderItemVoidPayload {
        rvc_no: RVC,
        check_no: 1,
        trn_seq: 1,
        cashier: "c".into(),
        sent: true,
        reason: Some("r".into()),
        author_cashier: None,
    };
    let open = OrderCheckOpenPayload {
        rvc_no: RVC,
        table_no: "T".into(),
        num_guest: 1,
        cashier: "c".into(),
    };
    let send = OrderSendPayload {
        rvc_no: RVC,
        check_no: 1,
        table_no: None,
        cashier: "c".into(),
    };
    let covers = OrderCoversPayload {
        rvc_no: RVC,
        check_no: 1,
        num_guest: 2,
        num_child: 0,
        cashier: "c".into(),
    };
    let cases: Vec<(&str, &str, Vec<Param>)> = vec![
        ("open", b::OPEN_CHECK, p::open_parameters(&open)),
        ("post", b::POST_ITEM, p::post_parameters(&post_payload())),
        ("send", b::SEND_ORDER, p::send_parameters(&send)),
        (
            "void-unsent",
            b::VOID_UNSENT,
            p::void_unsent_parameters(&void),
        ),
        ("covers", b::SET_COVERS, p::covers_parameters(&covers)),
        ("void-sent", b::VOID_SENT, p::void_parameters(&void)),
    ];
    for (name, sql, params) in cases {
        assert_eq!(bound(&params), free(sql, true), "{name}");
    }
}

#[test]
fn the_combo_batch_is_bound_exactly_its_free_parameters() {
    for with_selection in [false, true] {
        let combo = OrderComboPostPayload {
            rvc_no: RVC,
            check_no: 1,
            cashier: "c".into(),
            combo_code: "10".into(),
            qty: Decimal::ONE,
            seat_no: 0,
            price: Decimal::ZERO,
            note: None,
            selected_components: with_selection
                .then(|| vec!["CF01".into(), "CF01".into(), "TEA".into()]),
        };
        let params = p::combo_parameters(&combo);
        assert_eq!(bound(&params), free(b::POST_COMBO, true));
        let selected = if with_selection { "|CF01|TEA|" } else { "" };
        assert_eq!(
            param(&params, "Selected"),
            &ParamValue::String(Some(selected.into()))
        );
        assert_eq!(
            param(&params, "SelectedCount"),
            &ParamValue::I32(Some(if with_selection { 2 } else { 0 }))
        );
    }
}

#[test]
fn batches_call_the_allow_listed_procs_with_the_legacy_argument_order() {
    assert!(
        b::OPEN_CHECK.contains("EXEC dbo.GetRVCSeqNumber 'LastCheck', @RVCNo, @Cashier, 'NTPDA'")
    );
    assert!(b::POST_ITEM.contains("EXEC dbo.GetRVCSeqNumber 'LastTrn', @RVCNo, @Cashier, ''"));
    assert!(b::POST_ITEM.contains(
        "EXEC dbo.PostITrn @CheckNo, @PI_TrnSeq, @Cashier, @ItemCode, @TrnDesc, @Price, @Qty, @PI_OrderNo"
    ));
    assert!(b::POST_ITEM.contains("EXEC dbo.UpdateTmpInvoice @CheckNo"));
    assert!(b::VOID_UNSENT.contains("EXEC dbo.UpdateTmpInvoice @CheckNo"));
    assert!(b::VOID_SENT.contains("EXEC dbo.UpdateTmpInvoice @CheckNo"));
    assert!(b::POST_ITEM.contains("DECLARE @TrnDesc NVARCHAR(30)"));
}

#[test]
fn the_combo_batch_drops_the_header_recreate_and_keeps_the_legacy_proc_calls() {
    let code = strip_comments(b::POST_COMBO);
    assert!(!code.contains("IOrgInvoice"));
    assert!(!code.contains("BEGIN TRY\n    INSERT INTO #ComboComponents"));
    for needle in [
        "EXEC dbo.GetRVCSeqNumber 'LastTrn', @RVCNo, @Cashier, ''",
        "EXEC dbo.PostITrn @CheckNo, @PC2_TrnSeq, @Cashier, @CompCode, @PC2_TrnDesc, @CompPrice, @PC2_Qty, @CB_OrderNo",
        "EXEC dbo.PostITrn @CheckNo, @PC1_TrnSeq, @Cashier, @ComboCode, @PC1_TrnDesc, @Price, @PC1_Qty, @CB_OrderNo",
        "EXEC dbo.UpdateTmpInvoice @CheckNo",
        "DECLARE @PC2_LastTrn BIGINT = NULL;",
    ] {
        assert!(b::POST_COMBO.contains(needle), "{needle}");
    }
}

fn strip_comments(sql: &str) -> String {
    sql.lines()
        .map(|l| l.find("--").map_or(l, |i| &l[..i]))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `NotMatchRegex(@"(?i)\b(WORD|...)\b")`.
fn has_word(sql: &str, words: &[&str]) -> bool {
    let upper = sql.to_ascii_uppercase();
    let s = upper.as_bytes();
    words.iter().any(|w| {
        let w = w.to_ascii_uppercase();
        upper.match_indices(&w).any(|(i, _)| {
            (i == 0 || !is_word(s[i - 1])) && s.get(i + w.len()).is_none_or(|b| !is_word(*b))
        })
    })
}

const WRITES: &[&str] = &["INSERT", "UPDATE", "DELETE", "MERGE", "EXEC", "EXECUTE"];

#[test]
fn order_batches_keep_their_adr_guards_and_the_read_side_only_reads() {
    assert!(b::SEND_ORDER.contains("UPDATE i"));
    assert!(b::SEND_ORDER.contains("FROM dbo.tmpTrnInfo i"));
    let post = strip_comments(b::POST_ITEM);
    assert!(!post.contains("IOrgInvoice"));
    assert!(!post.contains("spReCreateCheckNo"));
    assert!(post.contains("CHECK_NOT_FOUND"));
    assert!(b::SEND_ORDER.contains("IF OBJECT_ID(N'dbo.PDASendOrder', N'U') IS NOT NULL"));
    for sql in [
        q::CHECK_STATUS,
        q::ITEM_EXISTS,
        q::FIND_LINE,
        q::READ_HEADER,
        q::READ_LINES,
        q::READ_LINES_WITH_NOTES,
        b::PENDING_SEND_SEQS,
        b::LOCK_CHECK,
        q::MONEY_FACTS,
        b::MOVE_SOURCE_LINES,
        &q::missing_columns_sql(2),
    ] {
        assert!(!has_word(sql, WRITES), "{sql}");
    }
}

#[test]
fn executor_statements_write_only_what_the_command_grants_allow() {
    let granted = ["IsDone", "DoneTime", "KdsAcceptedTime", "KdsAcceptedBy"];
    for action in ["accept", "unaccept", "done", "undone"] {
        let sql = q::kds_line_sql(action).unwrap();
        let head = "UPDATE dbo.tmpTrnInfo SET ";
        assert!(sql.starts_with(head));
        let set = &sql[head.len()..sql.find(" WHERE ").unwrap()];
        for assignment in set.split(',') {
            let column = assignment.split('=').next().unwrap().trim();
            assert!(granted.contains(&column), "{sql}");
        }
        assert!(sql.contains("@CheckNo") && sql.contains("@TrnNo"));
    }
    assert!(q::INSERT_RESERVATION.starts_with("INSERT INTO dbo.TableReservation "));
    assert!(
        q::DELETE_RESERVATION.starts_with("DELETE FROM dbo.TableReservation WHERE nID = @Id AND ")
    );
    assert!(!has_word(q::CHECK_STATE, WRITES));
    assert!(q::CHECK_STATE.contains("@CheckNo"));
}

#[test]
fn covers_batch_writes_only_the_open_check_header() {
    let sql = strip_comments(b::SET_COVERS);
    assert!(sql.contains("WHERE RVC = @RVCNo AND CheckNo = @CheckNo"));
    assert!(!has_word(
        &sql,
        &["EXEC", "EXECUTE", "MERGE", "INSERT", "DELETE"]
    ));
    assert_eq!(sql.matches("UPDATE").count(), 1);
    assert!(sql.contains("UPDATE dbo.TmpInvoice\n   SET NGuest         = @NumGuest,"));
}

#[test]
fn every_table_verb_declares_its_store_columns() {
    for verb in table_command_types::ALL {
        assert!(!schema_columns(verb).unwrap().is_empty(), "{verb}");
    }
    assert!(schema_columns("order.item_post").is_none());
}
