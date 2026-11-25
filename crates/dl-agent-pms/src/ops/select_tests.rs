//! Select calls: every refusal rule of ADR 0007 and the golden SQL text.

use super::*;

const TABLES: &str = r#"[{"name":"Room","columns":["RoomNo","RoomType","Status"]}]"#;

const PARAM: &str =
    r#"{"name":"roomType","type":"varchar","arg":"roomType","size":10,"nullable":false}"#;

fn select_json(table: &str, columns: &str, filters: &str, order: &str, max: &str) -> String {
    format!(
        r#"{{"table":"{table}","columns":{columns},"where":{filters},"order_by":{order},"max_rows":{max}}}"#
    )
}

fn good_select() -> String {
    select_json(
        "Room",
        r#"["RoomNo","RoomType","Status"]"#,
        &format!(r#"[{{"column":"RoomType","op":"=","param":{PARAM}}}]"#),
        r#"["RoomNo"]"#,
        "2000",
    )
}

fn manifest(tables: &str, kind: &str, call: &str) -> Result<Manifest, String> {
    Manifest::parse(
        format!(
            r#"{{"manifest_version":2,"allowlist_hash":"x","procs":["P"],"tables":{tables},"excluded_proc_patterns":[],"ops":[{{"name":"fo.room.list","contract_ver":1,"kind":"{kind}","target":"FO","target_db":"SMILE_FO","timeout_ms":1000,"idempotent":true,"calls":[{call}]}}]}}"#
        )
        .as_bytes(),
    )
}

fn with_select(sel: &str) -> Result<Manifest, String> {
    manifest(TABLES, "read", &format!(r#"{{"select":{sel}}}"#))
}

fn refused(r: Result<Manifest, String>, what: &str) -> String {
    match r {
        Ok(_) => panic!("{what}: accepted"),
        Err(e) => e,
    }
}

#[test]
fn a_valid_select_manifest_parses() {
    let m = with_select(&good_select()).unwrap();
    assert_eq!(m.tables.len(), 1);
    assert_eq!(m.tables[0].name, "Room");
    let call = &m.lookup("fo.room.list").unwrap().calls[0];
    assert!(call.proc_name.is_empty() && call.params.is_empty());
    let sel = call.select.as_ref().unwrap();
    assert_eq!(sel.max_rows, 2000);
    assert_eq!(sel.filters[0].param.arg, "roomType");
    assert_eq!(m.implemented_ops(), ["fo.room.list@1"]);
    assert_eq!(m.contract_max(), 1);
    // Table and column names are matched case-insensitively, as SQL Server does.
    assert!(with_select(&select_json("room", r#"["roomno"]"#, "[]", "[]", "1")).is_ok());
    assert!(with_select(&select_json("Room", r#"["RoomNo"]"#, "[]", "[]", "10000")).is_ok());
}

#[test]
fn golden_sql() {
    let m = with_select(&good_select()).unwrap();
    let sel = m.ops[0].calls[0].select.as_ref().unwrap();
    assert_eq!(
        sel.sql(),
        "SELECT TOP (2000) [RoomNo], [RoomType], [Status] FROM [dbo].[Room] WHERE [RoomType] = @roomType ORDER BY [RoomNo]"
    );
    let two = select_json(
        "Room",
        r#"["RoomNo"]"#,
        &format!(
            r#"[{{"column":"RoomType","op":"<>","param":{PARAM}}},{{"column":"Status","op":">=","param":{{"name":"st","type":"int","value":1}}}}]"#
        ),
        r#"["RoomType","RoomNo"]"#,
        "5",
    );
    let m = with_select(&two).unwrap();
    assert_eq!(
        m.ops[0].calls[0].select.as_ref().unwrap().sql(),
        "SELECT TOP (5) [RoomNo] FROM [dbo].[Room] WHERE [RoomType] <> @roomType AND [Status] >= @st ORDER BY [RoomType], [RoomNo]"
    );
    let bare = with_select(&select_json("Room", r#"["Status"]"#, "[]", "[]", "1")).unwrap();
    assert_eq!(
        bare.ops[0].calls[0].select.as_ref().unwrap().sql(),
        "SELECT TOP (1) [Status] FROM [dbo].[Room]"
    );
}

#[test]
fn refuses_unknown_tables_and_columns() {
    let cols = r#"["RoomNo"]"#;
    let e = refused(
        with_select(&select_json("Guest", cols, "[]", "[]", "1")),
        "unknown table",
    );
    assert!(e.contains("not declared in tables"), "{e}");
    for (c, w, o) in [
        (r#"["Rate"]"#, "[]".to_owned(), "[]"),
        (
            cols,
            r#"[{"column":"Rate","op":"=","param":{"name":"r","type":"int","arg":"r"}}]"#.into(),
            "[]",
        ),
        (cols, "[]".into(), r#"["Rate"]"#),
    ] {
        let e = refused(
            with_select(&select_json("Room", c, &w, o, "1")),
            "unknown column",
        );
        assert!(e.contains("is not declared"), "{e}");
    }
    refused(
        with_select(&select_json("Room", "[]", "[]", "[]", "1")),
        "no columns",
    );
}

#[test]
fn refuses_non_bare_identifiers() {
    // In the table list, in the call's table and in each column position.
    for bad in ["Room;DROP", "[x]", "a.b", "Room Name", "", "1Room"] {
        let tables = format!(r#"[{{"name":{},"columns":["RoomNo"]}}]"#, json::quote(bad));
        refused(
            manifest(
                &tables,
                "read",
                &format!(
                    r#"{{"select":{}}}"#,
                    select_json(bad, r#"["RoomNo"]"#, "[]", "[]", "1")
                ),
            ),
            bad,
        );
        let q = json::quote(bad);
        refused(
            with_select(&select_json(bad, r#"["RoomNo"]"#, "[]", "[]", "1")),
            bad,
        );
        refused(
            with_select(&select_json("Room", &format!("[{q}]"), "[]", "[]", "1")),
            bad,
        );
        refused(
            with_select(&select_json(
                "Room",
                r#"["RoomNo"]"#,
                &format!(
                    r#"[{{"column":{q},"op":"=","param":{{"name":"p","type":"int","arg":"p"}}}}]"#
                ),
                "[]",
                "1",
            )),
            bad,
        );
        refused(
            with_select(&select_json(
                "Room",
                r#"["RoomNo"]"#,
                "[]",
                &format!("[{q}]"),
                "1",
            )),
            bad,
        );
        let tcols = format!(r#"[{{"name":"Room","columns":[{q}]}}]"#);
        refused(manifest(&tcols, "read", r#"{"proc":"P"}"#), bad);
    }
}

#[test]
fn refuses_bad_ops_params_and_limits() {
    let filter = |op: &str, param: &str| {
        select_json(
            "Room",
            r#"["RoomNo"]"#,
            &format!(r#"[{{"column":"RoomType","op":"{op}","param":{param}}}]"#),
            "[]",
            "1",
        )
    };
    for op in ["LIKE", "==", "!=", "IN", "= 1 OR 1", ""] {
        let e = refused(with_select(&filter(op, PARAM)), op);
        assert!(e.contains("is not one of"), "{e}");
    }
    for sel in ["=", "<>", "<", "<=", ">", ">="] {
        assert!(with_select(&filter(sel, PARAM)).is_ok(), "{sel}");
    }
    for param in [
        r#"{"name":"o","type":"int","output":true}"#,
        r#"{"name":"o","type":"int","arg":"o","output":true}"#,
        r#"{"name":"v","type":"int","arg":"v","value":1}"#,
        r#"{"name":"v","type":"int"}"#,
        r#"{"name":"m","type":"money","arg":"m"}"#,
        r#"{"name":"a.b","type":"int","arg":"m"}"#,
        r#"{"name":"m","type":"int","arg":"m","extra":1}"#,
    ] {
        refused(with_select(&filter("=", param)), param);
    }
    let dup = select_json(
        "Room",
        r#"["RoomNo"]"#,
        r#"[{"column":"RoomType","op":"=","param":{"name":"p","type":"int","arg":"a"}},{"column":"Status","op":"=","param":{"name":"P","type":"int","arg":"b"}}]"#,
        "[]",
        "1",
    );
    assert!(
        refused(with_select(&dup), "duplicate param").contains("unique"),
        "param names are unique within the call"
    );
    for max in ["0", "10001", "-1", "1.5", "\"10\"", "null"] {
        refused(
            with_select(&select_json("Room", r#"["RoomNo"]"#, "[]", "[]", max)),
            max,
        );
    }
    let no_max = r#"{"table":"Room","columns":["RoomNo"]}"#;
    refused(with_select(no_max), "max_rows absent");
    refused(
        with_select(r#"{"table":"Room","columns":["RoomNo"],"max_rows":1,"limit":5}"#),
        "unknown select key",
    );
}

#[test]
fn refuses_select_outside_a_plain_read_call() {
    let sel = good_select();
    let e = refused(
        manifest(TABLES, "write", &format!(r#"{{"select":{sel}}}"#)),
        "select in a write op",
    );
    assert!(e.contains("only in a read op"), "{e}");
    for call in [
        format!(r#"{{"proc":"P","select":{sel}}}"#),
        format!(r#"{{"params":[{PARAM}],"select":{sel}}}"#),
    ] {
        let e = refused(manifest(TABLES, "read", &call), "proc and select");
        assert!(e.contains("exactly one of proc"), "{e}");
    }
    let e = refused(manifest(TABLES, "read", "{}"), "neither");
    assert!(e.contains("exactly one of proc or select"), "{e}");
    // A plain proc call next to the tables list still works.
    assert!(manifest(TABLES, "read", r#"{"proc":"P"}"#).is_ok());
}

#[test]
fn refuses_bad_table_lists() {
    let dup_col = select_json("Room", r#"["RoomNo","roomno"]"#, "[]", "[]", "1");
    let e = refused(with_select(&dup_col), "duplicate column");
    assert!(e.contains("unique bare name"), "{e}");
    let call = r#"{"proc":"P"}"#;
    for tables in [
        r#"[{"name":"Room","columns":["A","a"]}]"#,
        r#"[{"name":"Room","columns":[]}]"#,
        r#"[{"name":"Room","columns":["A"]},{"name":"ROOM","columns":["B"]}]"#,
        r#"[{"name":"Room","columns":["A"],"schema":"sys"}]"#,
        r#"[{"name":"Room","columns":[1]}]"#,
    ] {
        refused(manifest(tables, "read", call), tables);
    }
    for floor in [
        "sysobjects",
        "SysColumns",
        "sys_x",
        "#tmp",
        "INFORMATION_SCHEMA",
        "information_schema",
    ] {
        let tables = format!(r#"[{{"name":{},"columns":["A"]}}]"#, json::quote(floor));
        let e = refused(manifest(&tables, "read", call), floor);
        assert!(e.contains("not an allowed bare name"), "{floor}: {e}");
    }
    assert!(
        manifest(
            r#"[{"name":"INFORMATION_SCHEMA_X","columns":["A"]}]"#,
            "read",
            call
        )
        .is_ok(),
        "the INFORMATION_SCHEMA floor is the whole name"
    );
}

const AGG_TABLES: &str = r#"[{"name":"Trn","columns":["TrnTime","RVC","BaseTTL","ClassCode"]}]"#;

fn with_agg(sel: &str) -> Result<Manifest, String> {
    manifest(AGG_TABLES, "read", &format!(r#"{{"select":{sel}}}"#))
}

fn agg_select(extra: &str) -> String {
    format!(
        r#"{{"table":"Trn","columns":["TrnTime","RVC"],"where":[],"order_by":["TrnTime"],"max_rows":100,
          "aggregates":[{{"fn":"sum","column":"BaseTTL","as":"amount"}},{{"fn":"count","as":"lines"}}],
          "group_by":["TrnTime","RVC"],"date_bucket":["TrnTime"]{extra}}}"#
    )
}

#[test]
fn golden_grouped_sql() {
    let m = with_agg(&agg_select("")).unwrap();
    let sel = m.ops[0].calls[0].select.as_ref().unwrap();
    assert_eq!(
        sel.sql(),
        "SELECT TOP (100) CAST([TrnTime] AS date) AS [TrnTime], [RVC], SUM([BaseTTL]) AS [amount], COUNT(*) AS [lines] FROM [dbo].[Trn] GROUP BY CAST([TrnTime] AS date), [RVC] ORDER BY CAST([TrnTime] AS date)"
    );
    // A single total row: aggregates only, no columns, no group_by.
    let total = r#"{"table":"Trn","columns":[],"where":[],"order_by":[],"max_rows":1,"aggregates":[{"fn":"count","column":"RVC","as":"n"}]}"#;
    let m = with_agg(total).unwrap();
    assert_eq!(
        m.ops[0].calls[0].select.as_ref().unwrap().sql(),
        "SELECT TOP (1) COUNT([RVC]) AS [n] FROM [dbo].[Trn]"
    );
}

#[test]
fn refuses_bad_grouping() {
    let base = |agg: &str, gb: &str, cols: &str, order: &str, bucket: &str| {
        format!(
            r#"{{"table":"Trn","columns":{cols},"where":[],"order_by":{order},"max_rows":5,"aggregates":{agg},"group_by":{gb},"date_bucket":{bucket}}}"#
        )
    };
    let sum = r#"[{"fn":"sum","column":"BaseTTL","as":"a"}]"#;
    for (what, sel, needle) in [
        (
            "avg",
            base(
                r#"[{"fn":"avg","column":"BaseTTL","as":"a"}]"#,
                "[]",
                "[]",
                "[]",
                "[]",
            ),
            "aggregate fn",
        ),
        (
            "sum star",
            base(r#"[{"fn":"sum","as":"a"}]"#, "[]", "[]", "[]", "[]"),
            "not declared",
        ),
        (
            "sum unknown",
            base(
                r#"[{"fn":"sum","column":"Nope","as":"a"}]"#,
                "[]",
                "[]",
                "[]",
                "[]",
            ),
            "not declared",
        ),
        (
            "alias injection",
            base(
                r#"[{"fn":"sum","column":"BaseTTL","as":"a] FROM x--"}]"#,
                "[]",
                "[]",
                "[]",
                "[]",
            ),
            "alias",
        ),
        (
            "alias clashes with column",
            base(sum, r#"["RVC"]"#, r#"["RVC"]"#, "[]", "[]")
                .replace(r#""as":"a""#, r#""as":"rvc""#),
            "alias",
        ),
        (
            "column not grouped",
            base(sum, r#"["RVC"]"#, r#"["RVC","ClassCode"]"#, "[]", "[]"),
            "must be in group_by",
        ),
        (
            "order not grouped",
            base(sum, r#"["RVC"]"#, r#"["RVC"]"#, r#"["ClassCode"]"#, "[]"),
            "must be in group_by",
        ),
        (
            "unknown group column",
            base(sum, r#"["Nope"]"#, "[]", "[]", "[]"),
            "not declared",
        ),
        (
            "duplicate group column",
            base(sum, r#"["RVC","rvc"]"#, "[]", "[]", "[]"),
            "group_by",
        ),
        (
            "bucket not grouped",
            base(sum, r#"["RVC"]"#, "[]", "[]", r#"["TrnTime"]"#),
            "not in group_by",
        ),
        (
            "bucket without group",
            base("[]", "[]", r#"["RVC"]"#, "[]", r#"["RVC"]"#),
            "needs group_by",
        ),
        (
            "no output",
            base("[]", "[]", "[]", "[]", "[]"),
            "names no columns",
        ),
    ] {
        let e = refused(with_agg(&sel), what);
        assert!(e.contains(needle), "{what}: {e}");
    }
    assert!(
        with_agg(&agg_select(r#","limit":1"#)).is_err(),
        "unknown key refused"
    );
}
