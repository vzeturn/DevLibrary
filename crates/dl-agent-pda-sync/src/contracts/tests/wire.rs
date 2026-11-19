//! `BlobContractTests.cs`, `CheckStateFingerprintTests.cs`, `EnvelopeTests.cs`,
//! `SiteCommandContractTests.cs`.

use std::time::Duration;

use dl_decimal::Decimal;
use dl_uuid::Uuid;

use super::*;
use crate::contracts::json::WireJson;
use crate::contracts::time::DateTimeOffset;
use crate::contracts::wire::*;
use crate::util::stj;

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

fn sha() -> String {
    "a".repeat(64)
}

fn blob_ref(table: &str, column: &str, sha256: Option<&str>, key: Option<StrMap>) -> SyncBlobRef {
    SyncBlobRef {
        table: table.into(),
        key: key.unwrap_or_else(|| smap([("RVCNo", Some("1"))])),
        column: column.into(),
        sha256: sha256.map_or_else(sha, str::to_owned),
    }
}

fn default_ref() -> SyncBlobRef {
    blob_ref("DefRVCList", "Picture", None, None)
}

// ---- BlobContractTests -----------------------------------------------------------------

#[test]
fn catalog_lists_blob_columns_from_hash_exclude() {
    for t in ["DefRVCList", "MemberList"] {
        assert_eq!(
            catalog().find(t).unwrap().hash_exclude.as_deref(),
            Some(&["Picture".to_owned()][..])
        );
    }
}

#[test]
fn ref_to_a_blob_column_of_a_synced_table_is_accepted_case_insensitively() {
    assert_eq!(blob_contract::check(Some(&default_ref()), catalog()), None);
    let r = blob_ref("defrvclist", "picture", None, None);
    assert_eq!(blob_contract::check(Some(&r), catalog()), None);
}

#[test]
fn ref_outside_the_catalog_blob_columns_is_refused() {
    for (table, column, reason) in [
        ("app.outlet_details", "Picture", "not a synced table"),
        ("NoSuchTable", "Picture", "not a synced table"),
        ("DefRVCList", "RVCName", "not a blob column"),
        ("DefRVCList", "", "column is required"),
    ] {
        let r = blob_ref(table, column, None, None);
        let got = blob_contract::check(Some(&r), catalog()).unwrap();
        assert!(got.contains(reason), "{got}");
    }
}

#[test]
fn ref_with_a_malformed_hash_is_refused() {
    let (upper, non_hex) = ("A".repeat(64), "g".repeat(64));
    for s in ["ABC", upper.as_str(), non_hex.as_str()] {
        assert!(!blob_contract::is_sha256(Some(s)));
        let r = blob_ref("DefRVCList", "Picture", Some(s), None);
        assert!(
            blob_contract::check(Some(&r), catalog())
                .unwrap()
                .contains("sha256")
        );
    }
}

#[test]
fn ref_without_a_key_is_refused() {
    let r = blob_ref("DefRVCList", "Picture", None, Some(StrMap::new()));
    assert!(
        blob_contract::check(Some(&r), catalog())
            .unwrap()
            .contains("key")
    );
    let r = blob_ref(
        "DefRVCList",
        "Picture",
        None,
        Some(smap([(" ", Some("1"))])),
    );
    assert!(
        blob_contract::check(Some(&r), catalog())
            .unwrap()
            .contains("key")
    );
}

#[test]
fn never_sync_and_excluded_columns_are_refused_even_when_listed_as_blobs() {
    let mut tampered = catalog().clone();
    for t in &mut tampered.tables {
        if t.table == "DefRVCList" {
            t.hash_exclude = Some(vec!["Picture".into(), "Secret".into()]);
            t.exclude_columns = Some(vec!["Secret".into()]);
        }
    }
    tampered
        .conventions
        .never_sync
        .push("DefRVCList.Picture".into());
    assert!(
        blob_contract::check(Some(&default_ref()), &tampered)
            .unwrap()
            .contains("must not leave the site")
    );
    let r = blob_ref("DefRVCList", "Secret", None, None);
    assert!(
        blob_contract::check(Some(&r), &tampered)
            .unwrap()
            .contains("must not leave the site")
    );
}

#[test]
fn ref_batch_round_trips_in_snake_case() {
    let batch = SyncBlobRefBatch {
        agent_id: "agent-1".into(),
        contract_version: blob_contract::VERSION,
        refs: vec![default_ref()],
    };
    let json = batch.to_json();
    assert!(json.contains("\"contract_version\":1"));
    assert!(json.contains(&format!("\"sha256\":\"{}\"", sha())));
    assert!(json.contains("\"table\":\"DefRVCList\""));
    let back = SyncBlobRefBatch::from_json(&json).unwrap();
    assert_eq!(back.refs.len(), 1);
    assert_eq!(back.refs[0].key.get("RVCNo"), Some(&Some("1".to_owned())));
    let ack = SyncBlobRefsAck::from_json(&format!("{{\"applied\":1,\"missing\":[\"{}\"]}}", sha()))
        .unwrap();
    assert_eq!(ack.missing, [sha()]);
}

#[test]
fn heartbeat_without_blob_queue_depth_is_accepted_from_older_agents() {
    let json = r#"
        {"agent_id":"a","at_utc":"2026-10-28T00:00:00+00:00","agent_version":"1","envelope_version":1,"mapper_version":1,
         "catalog_version":1,"sql_reachable":true,"queue_depth":0,"streams":[]}
        "#;
    assert_eq!(
        SyncHeartbeat::from_json(json).unwrap().blob_queue_depth,
        None
    );
}

#[test]
fn content_path_is_versioned_under_the_sync_transport() {
    assert_eq!(
        blob_contract::content_path(&sha()),
        format!("/sync/v1/blobs/{}", sha())
    );
    assert_eq!(
        blob_contract::MAX_BYTES,
        i64::from(wire_version::MAX_BODY_BYTES)
    );
}

// ---- CheckStateFingerprintTests --------------------------------------------------------

fn header() -> CheckStateHeader {
    CheckStateHeader {
        check_no: 5001,
        table_no: Some("T4".into()),
        guests: 2,
        sub_total: d("100000"),
        total_paid: d("0"),
    }
}

fn line(seq: i64, code: &str, item: &str, qty: &str, price: &str) -> CheckStateLine {
    CheckStateLine {
        line_seq: seq,
        transaction_code: Some(code.into()),
        item_code: Some(item.into()),
        qty: d(qty),
        price: d(price),
    }
}

fn coffee() -> CheckStateLine {
    line(1, "1", "CF01", "1", "50000")
}

fn tea() -> CheckStateLine {
    line(2, "1", "TEA", "2", "25000")
}

fn fp(h: &CheckStateHeader, lines: &[CheckStateLine]) -> String {
    CheckStateFingerprint::compute(h, lines)
}

#[test]
fn fingerprint_is_versioned_and_short() {
    let f = fp(&header(), &[coffee(), tea()]);
    assert!(f.starts_with("cs1:"));
    assert_eq!(f.len(), 4 + 32);
}

#[test]
fn line_order_and_decimal_scale_do_not_change_the_fingerprint() {
    let a = fp(&header(), &[coffee(), tea()]);
    let mut h = header();
    h.sub_total = d("100000.0000");
    let mut t = tea();
    t.qty = d("2.000");
    t.price = d("25000.0000");
    let mut c = coffee();
    c.price = d("50000.00");
    assert_eq!(fp(&h, &[t, c]), a);
}

#[test]
fn surrounding_spaces_of_vendor_char_columns_do_not_change_the_fingerprint() {
    let a = fp(&header(), &[coffee()]);
    let mut h = header();
    h.table_no = Some("T4   ".into());
    let mut c = coffee();
    c.item_code = Some("CF01  ".into());
    assert_eq!(fp(&h, &[c]), a);
}

#[test]
fn any_change_pos_win_can_make_changes_the_fingerprint() {
    let seen = fp(&header(), &[coffee(), tea()]);
    let with = |f: fn(&mut CheckStateHeader)| {
        let mut h = header();
        f(&mut h);
        h
    };
    let tea_with = |f: fn(&mut CheckStateLine)| {
        let mut t = tea();
        f(&mut t);
        t
    };
    let changes: Vec<(CheckStateHeader, Vec<CheckStateLine>)> = vec![
        (
            with(|h| h.table_no = Some("T5".into())),
            vec![coffee(), tea()],
        ),
        (with(|h| h.guests = 3), vec![coffee(), tea()]),
        (
            with(|h| h.sub_total = "125000".parse().unwrap()),
            vec![coffee(), tea()],
        ),
        (
            with(|h| h.total_paid = "10000".parse().unwrap()),
            vec![coffee(), tea()],
        ),
        (header(), vec![coffee()]),
        (
            header(),
            vec![coffee(), tea_with(|t| t.qty = "3".parse().unwrap())],
        ),
        (
            header(),
            vec![coffee(), tea_with(|t| t.price = "30000".parse().unwrap())],
        ),
        (
            header(),
            vec![
                coffee(),
                tea_with(|t| t.transaction_code = Some("9".into())),
            ],
        ),
        (
            header(),
            vec![coffee(), tea(), line(3, "1", "CAKE", "1", "40000")],
        ),
    ];
    for (h, lines) in changes {
        assert!(!CheckStateFingerprint::matches(
            Some(&seen),
            Some(&fp(&h, &lines))
        ));
    }
}

#[test]
fn separator_inside_a_value_cannot_forge_another_state() {
    let mut h = header();
    h.table_no = Some("T4|2".into());
    assert_ne!(fp(&h, &[]), fp(&header(), &[]));
}

#[test]
fn matches_is_exact() {
    let f = fp(&header(), &[coffee()]);
    assert!(CheckStateFingerprint::matches(Some(&f), Some(&f)));
    assert!(!CheckStateFingerprint::matches(
        Some(&f.to_uppercase()),
        Some(&f)
    ));
    assert!(!CheckStateFingerprint::matches(None, None));
}

#[test]
fn check_aggregate_key_round_trips() {
    for (key, ok, rvc, check) in [
        ("check:3:5001", true, 3, 5001),
        ("check:12:900000001", true, 12, 900_000_001),
        ("reservation:3", false, 0, 0),
        ("check:x:1", false, 0, 0),
        ("check:3:", false, 0, 0),
        ("check:3:1:2", false, 0, 0),
    ] {
        let parsed = check_aggregate::try_parse(Some(key));
        assert_eq!(parsed.is_some(), ok, "{key}");
        if let Some((r, c)) = parsed {
            assert_eq!((r, c), (rvc, check));
            assert_eq!(check_aggregate::key(r, c), key);
        }
    }
}

fn uuid(s: &str) -> Uuid {
    Uuid::parse(s).unwrap()
}

#[test]
fn ordering_fields_round_trip_in_snake_case_and_old_agents_ignore_them() {
    let tenant = uuid("11111111-2222-3333-4444-555555555555");
    let cmd = SyncCommand {
        command_id: uuid("6f1f8a3e-2c55-4c55-9a43-1f2e3d4c5b6a"),
        type_: site_command_types::KDS_LINE_STATE.into(),
        aggregate_key: check_aggregate::key(3, 5001),
        expected_hash: Some("cs1:ab".into()),
        payload: stj::parse("{}").unwrap(),
        created_by: "u1".into(),
        expires_at_utc: None,
        contract_version: 1,
        aggregate_seq: Some(7),
        idempotency_key: Some("pda-42".into()),
        tenant_id: Some(tenant),
    };
    let json = cmd.to_json();
    let back = SyncCommand::from_json(&json).unwrap();
    assert!(json.contains("\"aggregate_seq\":7"));
    assert!(json.contains("\"idempotency_key\":\"pda-42\""));
    assert!(json.contains("\"tenant_id\""));
    assert_eq!(back.aggregate_seq, Some(7));
    assert_eq!(back.idempotency_key.as_deref(), Some("pda-42"));
    assert_eq!(back.tenant_id, Some(tenant));
    let v1 = SyncCommand {
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
        ..cmd
    }
    .to_json();
    // A v1 command stays byte-compatible with older agents.
    assert!(!v1.contains("aggregate_seq") && !v1.contains("tenant_id"));
}

#[test]
fn result_carries_store_execution_time() {
    let r = SyncCommandResult {
        command_id: uuid("6f1f8a3e-2c55-4c55-9a43-1f2e3d4c5b6a"),
        status: CommandStatus::Applied,
        reason: None,
        result: None,
        at_utc: DateTimeOffset::UNIX_EPOCH,
        exec_ms: Some(840),
    };
    assert!(r.to_json().contains("\"exec_ms\":840"));
}

// ---- EnvelopeTests ---------------------------------------------------------------------

fn sample_batch() -> SyncBatch {
    SyncBatch {
        agent_id: "agent-1".into(),
        stream: "closed-ledger".into(),
        seq: 42,
        mapper_ver: wire_version::MAPPER,
        table: "app.closed_checks".into(),
        mode: SyncMode::Delta,
        rows: vec![SyncRow {
            key: smap([("check_no", Some("500001234"))]),
            columns: smap([
                ("check_no", Some("500001234")),
                ("note", None),
                ("desc", Some("Phở bò")),
            ]),
            hash: "ab12".into(),
            src_ver: 7,
        }],
        tombstones: vec![SyncTombstone {
            key: smap([("check_no", Some("9"))]),
            src_ver: 8,
            reason: Some("voided".into()),
        }],
        watermark: Some("500001234".into()),
    }
}

#[test]
fn batch_round_trips_and_uses_snake_case() {
    let batch = sample_batch();
    let json = batch.to_json();
    for part in [
        "\"agent_id\":\"agent-1\"",
        "\"mapper_ver\":1",
        "\"mode\":\"Delta\"",
        "\"src_ver\":7",
        "\"tombstones\"",
    ] {
        assert!(json.contains(part), "{part}");
    }
    let back = SyncBatch::from_json(&json).unwrap();
    assert_eq!(back, batch);
    assert_eq!(
        back.rows[0].columns.get("desc"),
        Some(&Some("Phở bò".to_owned()))
    );
}

#[test]
fn command_and_result_round_trip() {
    let cmd = SyncCommand {
        command_id: uuid("0c8d5f8e-1a2b-4c3d-8e9f-a0b1c2d3e4f5"),
        type_: "order.post".into(),
        aggregate_key: "check:123".into(),
        expected_hash: Some("hash".into()),
        payload: stj::parse(r#"{"check_no":123,"items":[1,2]}"#).unwrap(),
        created_by: "u1".into(),
        expires_at_utc: DateTimeOffset::utc(2026, 10, 24, 1, 2, 3),
        contract_version: 1,
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
    };
    let back = SyncCommand::from_json(&cmd.to_json()).unwrap();
    assert_eq!(back.command_id, cmd.command_id);
    assert_eq!(
        back.payload
            .get("items")
            .and_then(|i| i.get_index(1))
            .and_then(|v| v.as_i64()),
        Some(2)
    );
    assert_eq!(back.expires_at_utc, cmd.expires_at_utc);

    let result = SyncCommandResult {
        command_id: cmd.command_id,
        status: CommandStatus::Rejected,
        reason: Some("stale_hash".into()),
        result: Some(stj::parse(r#"{"trn_seq":9}"#).unwrap()),
        at_utc: DateTimeOffset::utc(2026, 10, 24, 1, 2, 4).unwrap(),
        exec_ms: None,
    };
    let json = result.to_json();
    assert!(json.contains("\"status\":\"Rejected\""));
    let rb = SyncCommandResult::from_json(&json).unwrap();
    assert_eq!(rb.status, CommandStatus::Rejected);
    assert_eq!(
        rb.result.unwrap().get("trn_seq").and_then(|v| v.as_i64()),
        Some(9)
    );
}

fn heartbeat() -> SyncHeartbeat {
    SyncHeartbeat {
        agent_id: "agent-1".into(),
        at_utc: DateTimeOffset::utc(2026, 10, 24, 0, 0, 0).unwrap(),
        agent_version: "1.0.0".into(),
        envelope_version: wire_version::ENVELOPE,
        mapper_version: wire_version::MAPPER,
        catalog_version: 1,
        sql_reachable: true,
        queue_depth: 3,
        streams: vec![StreamLag {
            stream: "open-checks".into(),
            lag_rows: 2,
            lag_seconds: 1.5,
        }],
        blob_queue_depth: None,
        capture_error: None,
        resnapshots: None,
    }
}

#[test]
fn heartbeat_round_trips() {
    let hb = heartbeat();
    assert_eq!(SyncHeartbeat::from_json(&hb.to_json()).unwrap(), hb);
}

#[test]
fn heartbeat_capture_error_is_optional_on_the_wire() {
    let hb = SyncHeartbeat {
        sql_reachable: false,
        queue_depth: 0,
        streams: vec![],
        capture_error: Some("SQL_LOGIN_FAILED".into()),
        ..heartbeat()
    };
    let json = hb.to_json();
    assert!(json.contains("\"capture_error\":\"SQL_LOGIN_FAILED\""));
    assert_eq!(SyncHeartbeat::from_json(&json).unwrap(), hb);
    // A healthy agent omits the field.
    assert!(
        !SyncHeartbeat {
            capture_error: None,
            ..hb
        }
        .to_json()
        .contains("capture_error")
    );
}

#[test]
fn heartbeat_resnapshots_are_optional_on_the_wire() {
    let at = DateTimeOffset::utc(2026, 11, 15, 8, 0, 0).unwrap();
    let hb = SyncHeartbeat {
        at_utc: at,
        queue_depth: 0,
        streams: vec![],
        resnapshots: Some(vec![StreamResnapshot {
            stream: "Invoice".into(),
            through_seq: 42,
            completed_at_utc: at,
        }]),
        ..heartbeat()
    };
    let json = hb.to_json();
    assert!(json.contains("\"resnapshots\":[{\"stream\":\"Invoice\",\"through_seq\":42"));
    assert_eq!(SyncHeartbeat::from_json(&json).unwrap(), hb);
    // An older cloud never sees the field.
    assert!(
        !SyncHeartbeat {
            resnapshots: None,
            ..hb
        }
        .to_json()
        .contains("resnapshots")
    );
}

#[test]
fn wire_constants_are_pinned() {
    assert_eq!(wire_version::ENVELOPE, 1);
    assert_eq!(wire_version::MAPPER, 1);
    assert_eq!(wire_version::MAX_BODY_BYTES, 8_388_608);
    assert_eq!(wire_version::HASH_ALGORITHM, "sha256-v1");
}

// ---- SiteCommandContractTests ----------------------------------------------------------

#[test]
fn command_without_contract_version_on_the_wire_is_version_1() {
    let json = r#"
        {"command_id":"6f1f8a3e-2c55-4c55-9a43-1f2e3d4c5b6a","type":"kds.line_state","aggregate_key":"check:1:9",
         "payload":{},"created_by":"u1"}
        "#;
    assert_eq!(SyncCommand::from_json(json).unwrap().contract_version, 1);
}

#[test]
fn contract_version_round_trips_in_snake_case() {
    let cmd = SyncCommand {
        command_id: uuid("6f1f8a3e-2c55-4c55-9a43-1f2e3d4c5b6a"),
        type_: site_command_types::RESERVATION_SAVE.into(),
        aggregate_key: "rvc:1".into(),
        expected_hash: None,
        payload: stj::parse("{}").unwrap(),
        created_by: "u1".into(),
        expires_at_utc: None,
        contract_version: 2,
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
    };
    let json = cmd.to_json();
    assert!(json.contains("\"contract_version\":2"));
    assert_eq!(SyncCommand::from_json(&json).unwrap().contract_version, 2);
}

#[test]
fn kds_payload_round_trips_with_snake_case_names() {
    let p = KdsLineStatePayload {
        rvc_no: 3,
        check_no: 500_001_234,
        line_seqs: vec![7, 8],
        action: kds_line_actions::DONE.into(),
        cashier: "c01".into(),
    };
    let e = p.to_element();
    assert_eq!(
        e.get("check_no").and_then(|v| v.as_i64()),
        Some(500_001_234)
    );
    assert_eq!(
        e.get("line_seqs").and_then(|v| v.as_array()).map(Vec::len),
        Some(2)
    );
    assert_eq!(KdsLineStatePayload::from_element(&e), Some(p));
}

fn reservation(
    from: DateTime,
    to: DateTime,
    tel: &str,
    booker: Option<&str>,
) -> ReservationSavePayload {
    ReservationSavePayload {
        rvc_no: 1,
        guest_info: "Anh Minh".into(),
        num_adult: 2,
        num_child: 0,
        from_date_time: from,
        to_date_time: to,
        table_num: "T5".into(),
        tel_num: tel.into(),
        booker: booker.map(str::to_owned),
        company_name: None,
        vat_code: None,
        address: None,
    }
}

#[test]
fn reservation_payload_keeps_the_wall_clock_time() {
    let p = reservation(
        DateTime::new(2026, 10, 24, 19, 0, 0).unwrap(),
        DateTime::new(2026, 10, 24, 21, 0, 0).unwrap(),
        "0901",
        Some("host"),
    );
    let e = p.to_element();
    assert_eq!(
        e.get("from_date_time").and_then(|v| v.as_str()),
        Some("2026-10-24T19:00:00")
    );
    // Nulls are omitted on the wire.
    assert!(e.get("company_name").is_none());
    assert_eq!(ReservationSavePayload::from_element(&e), Some(p));
}

#[test]
fn malformed_payload_reads_as_null() {
    for json in ["[]", "\"x\"", r#"{"rvc_no":"not-a-number"}"#] {
        let v = stj::parse(json).unwrap();
        assert_eq!(ReservationCancelPayload::from_element(&v), None, "{json}");
    }
}

#[test]
fn payload_problems_are_reported() {
    let kds = |seqs: Vec<i64>, action: &str| KdsLineStatePayload {
        rvc_no: 1,
        check_no: 9,
        line_seqs: seqs,
        action: action.into(),
        cashier: "c".into(),
    };
    assert!(
        kds(vec![], kds_line_actions::DONE)
            .problem()
            .unwrap()
            .contains("line_seqs")
    );
    assert!(kds(vec![1], "bump").problem().unwrap().contains("action"));
    assert_eq!(kds(vec![1], kds_line_actions::ACCEPT).problem(), None);
    let mut r = reservation(
        DateTime::new(2026, 1, 1, 10, 0, 0).unwrap(),
        DateTime::new(2026, 1, 1, 9, 0, 0).unwrap(),
        "",
        None,
    );
    r.guest_info = "g".into();
    r.num_adult = 1;
    r.table_num = "T1".into();
    assert!(r.problem().unwrap().contains("before"));
    let c = ReservationCancelPayload {
        rvc_no: 1,
        id: 0,
        cancelled_by: "u".into(),
    };
    assert!(c.problem().unwrap().contains("id"));
}

#[test]
fn delivery_rules_per_type() {
    let find = |t: &str| site_command_contract::find(Some(t)).map(|s| s.delivery);
    assert_eq!(
        find(site_command_types::KDS_LINE_STATE),
        Some(SiteCommandDelivery::Interactive)
    );
    assert_eq!(
        find(site_command_types::RESERVATION_SAVE),
        Some(SiteCommandDelivery::Durable)
    );
    assert_eq!(
        find(site_command_types::RESERVATION_CANCEL),
        Some(SiteCommandDelivery::Durable)
    );
    assert_eq!(
        find(site_command_types::SYNC_RESNAPSHOT),
        Some(SiteCommandDelivery::Durable)
    );
    assert_eq!(find("order.post"), None);
    assert!(
        site_command_contract::SPECS
            .iter()
            .all(|s| s.max_attempts >= 1)
    );
}

#[test]
fn sync_resnapshot_payload_names_streams_and_a_reason_on_the_wire() {
    let p = SyncResnapshotPayload {
        streams: vec!["Invoice".into(), "Trn".into()],
        reason: "parent pending".into(),
    };
    let e = p.to_element();
    let mut w = stj::Writer::new(stj::Style::Compact);
    w.value(&e);
    let raw = w.finish();
    assert!(raw.contains("\"streams\":[\"Invoice\",\"Trn\"]"));
    assert!(raw.contains("\"reason\":\"parent pending\""));
    assert_eq!(SyncResnapshotPayload::from_element(&e), Some(p.clone()));
    assert_eq!(p.problem(), None);
    let many = SyncResnapshotPayload {
        streams: (0..17).map(|i| format!("s{i}")).collect(),
        ..p.clone()
    };
    assert!(many.problem().is_some());
    let long = SyncResnapshotPayload {
        streams: vec!["s".repeat(65)],
        ..p.clone()
    };
    assert!(long.problem().is_some());
    let reason = SyncResnapshotPayload {
        reason: "r".repeat(301),
        ..p
    };
    assert!(reason.problem().is_some());
}

#[test]
fn backoff_doubles_and_caps_at_five_minutes() {
    for (attempts, seconds) in [(1, 5), (2, 10), (3, 20), (7, 300), (40, 300)] {
        assert_eq!(
            site_command_contract::backoff(attempts),
            Duration::from_secs(seconds)
        );
    }
}
