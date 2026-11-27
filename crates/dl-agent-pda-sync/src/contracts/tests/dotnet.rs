//! Byte-for-byte cross-checks against .NET 10: the expected strings were printed by a
//! dotnet file-based app referencing the real `MM.PdaOrder.Sync.Contracts` and
//! `MM.PdaOrder.Sync.Mapping` projects (`%TEMP%\kx\sa-contracts\probe3\probe3.cs`) for the
//! same sample values built here.

use dl_decimal::Decimal;
use dl_uuid::Uuid;

use super::*;
use crate::contracts::json::{WireJson, write_str_map};
use crate::contracts::legacy::{
    LegacyBundleFile, LegacyBundleMaintenance, LegacyBundleManifest, LegacyBundleSource,
    LegacyDisposition, LegacyLeftOut, bundle,
};
use crate::contracts::mapping::MappedRow;
use crate::contracts::time::{
    DateTimeOffset, TICKS_PER_HOUR, TICKS_PER_MINUTE, TICKS_PER_SECOND, ticks_from_civil,
};
use crate::contracts::wire::*;
use crate::util::stj::{self, NEWLINE, Style, Writer};

const G1: &str = "6f1f8a3e-2c55-4c55-9a43-1f2e3d4c5b6a";
const G2: &str = "11111111-2222-3333-4444-555555555555";

fn g(s: &str) -> Uuid {
    Uuid::parse(s).unwrap()
}

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

/// `new DateTimeOffset(y, M, d, h, m, s, offset).AddTicks(extra)`.
fn local(civil: (i32, u32, u32, u32, u32, u32), offset_minutes: i16, extra: i64) -> DateTimeOffset {
    let (y, mo, day, h, mi, s) = civil;
    DateTimeOffset::from_local(
        ticks_from_civil(y, mo, day, h, mi, s).unwrap() + extra,
        offset_minutes,
    )
    .unwrap()
}

fn at() -> DateTimeOffset {
    DateTimeOffset::utc(2026, 11, 15, 8, 0, 0).unwrap()
}

#[test]
fn sync_batch_with_unicode_nulls_and_repair_mode() {
    let batch = SyncBatch {
        agent_id: "agent-1".into(),
        stream: "Invoice".into(),
        seq: 42,
        mapper_ver: wire_version::MAPPER,
        table: "Invoice".into(),
        mode: SyncMode::Repair,
        rows: vec![SyncRow {
            key: smap([("check_no", Some("500001234"))]),
            columns: smap([
                ("check_no", Some("500001234")),
                ("note", None),
                ("desc", Some("Phở bò <&> \"q\" 'x' + `y` 😀\u{1}\u{7f}\n")),
                ("z", Some("")),
            ]),
            hash: "ab12".into(),
            src_ver: 7,
        }],
        tombstones: vec![
            SyncTombstone {
                key: smap([("check_no", Some("9"))]),
                src_ver: 8,
                reason: Some("voided".into()),
            },
            SyncTombstone {
                key: smap([("k", None)]),
                src_ver: 9,
                reason: None,
            },
        ],
        watermark: None,
        hops: Default::default(),
    };
    let expected = r#"{"agent_id":"agent-1","stream":"Invoice","seq":42,"mapper_ver":1,"table":"Invoice","mode":"Repair","rows":[{"key":{"check_no":"500001234"},"columns":{"check_no":"500001234","note":null,"desc":"Ph\u1EDF b\u00F2 \u003C\u0026\u003E \u0022q\u0022 \u0027x\u0027 \u002B \u0060y\u0060 \uD83D\uDE00\u0001\u007F\n","z":""},"hash":"ab12","src_ver":7}],"tombstones":[{"key":{"check_no":"9"},"src_ver":8,"reason":"voided"},{"key":{"k":null},"src_ver":9}]}"#;
    assert_eq!(batch.to_json(), expected);
    assert_eq!(SyncBatch::from_json(expected).unwrap(), batch);

    let b2 = SyncBatch {
        mode: SyncMode::Delta,
        rows: vec![],
        tombstones: vec![],
        watermark: Some("500001234".into()),
        ..batch
    };
    assert_eq!(
        b2.to_json(),
        r#"{"agent_id":"agent-1","stream":"Invoice","seq":42,"mapper_ver":1,"table":"Invoice","mode":"Delta","rows":[],"tombstones":[],"watermark":"500001234"}"#
    );
}

#[test]
fn sync_command_with_a_nested_payload() {
    let payload = stj::parse(
        r#"{"rvc_no":3,"lines":[{"item_code":"PHO","qty":2.50,"price":1E5,"note":"ít cay <b>"}],"nested":{"a":null,"b":[true,false,-0.0],"c":{}},"e":[]}"#,
    )
    .unwrap();
    let cmd = SyncCommand {
        command_id: g(G1),
        type_: "order.item_post".into(),
        aggregate_key: "check:3:5001".into(),
        expected_hash: Some("cs1:ab".into()),
        payload,
        created_by: "u1".into(),
        expires_at_utc: Some(local((2026, 10, 24, 1, 2, 3), 420, 1_234_567)),
        contract_version: 2,
        aggregate_seq: Some(7),
        idempotency_key: Some("pda-42".into()),
        tenant_id: Some(g(G2)),
    };
    let expected = r#"{"command_id":"6f1f8a3e-2c55-4c55-9a43-1f2e3d4c5b6a","type":"order.item_post","aggregate_key":"check:3:5001","expected_hash":"cs1:ab","payload":{"rvc_no":3,"lines":[{"item_code":"PHO","qty":2.50,"price":1E5,"note":"\u00EDt cay \u003Cb\u003E"}],"nested":{"a":null,"b":[true,false,-0.0],"c":{}},"e":[]},"created_by":"u1","expires_at_utc":"2026-10-24T01:02:03.1234567+07:00","contract_version":2,"aggregate_seq":7,"idempotency_key":"pda-42","tenant_id":"11111111-2222-3333-4444-555555555555"}"#;
    assert_eq!(cmd.to_json(), expected);
    let back = SyncCommand::from_json(expected).unwrap();
    assert_eq!(back.to_json(), expected);
    assert_eq!(back.expires_at_utc.unwrap().offset_minutes(), 420);

    let minimal = SyncCommand {
        type_: "kds.line_state".into(),
        aggregate_key: "check:1:9".into(),
        expected_hash: None,
        payload: stj::parse("{}").unwrap(),
        expires_at_utc: None,
        contract_version: 1,
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
        ..cmd
    };
    assert_eq!(
        minimal.to_json(),
        r#"{"command_id":"6f1f8a3e-2c55-4c55-9a43-1f2e3d4c5b6a","type":"kds.line_state","aggregate_key":"check:1:9","payload":{},"created_by":"u1","contract_version":1}"#
    );
}

#[test]
fn sync_command_result() {
    let r = SyncCommandResult {
        command_id: g(G1),
        status: CommandStatus::Applied,
        reason: None,
        result: Some(stj::parse(r#"{"trn_seq":9,"x":"é"}"#).unwrap()),
        at_utc: DateTimeOffset::UNIX_EPOCH,
        exec_ms: Some(840),
        hops: Default::default(),
    };
    assert_eq!(
        r.to_json(),
        r#"{"command_id":"6f1f8a3e-2c55-4c55-9a43-1f2e3d4c5b6a","status":"Applied","result":{"trn_seq":9,"x":"\u00E9"},"at_utc":"1970-01-01T00:00:00+00:00","exec_ms":840}"#
    );
    let r2 = SyncCommandResult {
        command_id: g(G2),
        status: CommandStatus::Rejected,
        reason: Some("check_changed".into()),
        result: None,
        at_utc: local((2026, 11, 4, 0, 30, 0), -150, 1_230_000),
        exec_ms: None,
        hops: Default::default(),
    };
    let expected = r#"{"command_id":"11111111-2222-3333-4444-555555555555","status":"Rejected","reason":"check_changed","at_utc":"2026-11-04T00:30:00.123-02:30"}"#;
    assert_eq!(r2.to_json(), expected);
    assert_eq!(SyncCommandResult::from_json(expected).unwrap(), r2);
}

#[test]
fn sync_heartbeat_with_double_lags() {
    let lag = |s: &str, rows: i64, secs: f64| StreamLag {
        stream: s.into(),
        lag_rows: rows,
        lag_seconds: secs,
    };
    let hb = SyncHeartbeat {
        agent_id: "agent-1".into(),
        at_utc: at(),
        agent_version: "3.30.0".into(),
        envelope_version: 1,
        mapper_version: 1,
        catalog_version: 1,
        sql_reachable: true,
        queue_depth: 3,
        streams: vec![
            lag("open", 2, 1.5),
            lag("Invoice", 0, 0.1 + 0.2),
            lag("x", 5, 1e-7),
            lag("y", 1, 123_456_789.123),
            lag("z", 0, 0.0),
        ],
        blob_queue_depth: Some(3),
        capture_error: None,
        resnapshots: Some(vec![StreamResnapshot {
            stream: "Invoice".into(),
            through_seq: 42,
            completed_at_utc: at(),
        }]),
        commands: None,
    };
    let expected = r#"{"agent_id":"agent-1","at_utc":"2026-11-15T08:00:00+00:00","agent_version":"3.30.0","envelope_version":1,"mapper_version":1,"catalog_version":1,"sql_reachable":true,"queue_depth":3,"streams":[{"stream":"open","lag_rows":2,"lag_seconds":1.5},{"stream":"Invoice","lag_rows":0,"lag_seconds":0.30000000000000004},{"stream":"x","lag_rows":5,"lag_seconds":1E-07},{"stream":"y","lag_rows":1,"lag_seconds":123456789.123},{"stream":"z","lag_rows":0,"lag_seconds":0}],"blob_queue_depth":3,"resnapshots":[{"stream":"Invoice","through_seq":42,"completed_at_utc":"2026-11-15T08:00:00+00:00"}]}"#;
    assert_eq!(hb.to_json(), expected);
    assert_eq!(SyncHeartbeat::from_json(expected).unwrap(), hb);

    let hb2 = SyncHeartbeat {
        sql_reachable: false,
        queue_depth: 0,
        streams: vec![],
        blob_queue_depth: None,
        capture_error: Some("SQL_LOGIN_FAILED".into()),
        resnapshots: None,
        commands: None,
        ..hb
    };
    assert_eq!(
        hb2.to_json(),
        r#"{"agent_id":"agent-1","at_utc":"2026-11-15T08:00:00+00:00","agent_version":"3.30.0","envelope_version":1,"mapper_version":1,"catalog_version":1,"sql_reachable":false,"queue_depth":0,"streams":[],"capture_error":"SQL_LOGIN_FAILED"}"#
    );
}

#[test]
fn site_event_batch() {
    let batch = SiteEventBatch {
        agent_id: "agent-1".into(),
        agent_version: "3.30.0".into(),
        events: vec![
            SiteEvent {
                event_id: g(G1),
                at_utc: at(),
                source: site_event_sources::AGENT.into(),
                level: site_event_levels::WARN.into(),
                code: "SQL_LOGIN_FAILED".into(),
                message: "Login failed for 'sa' <x> & ở".into(),
                run_id: Some("run-1".into()),
                detail: Some(stj::parse(r#"{"count":3,"tables":["a"],"r":1.0}"#).unwrap()),
            },
            SiteEvent {
                event_id: g(G2),
                at_utc: at().to_offset(420).unwrap(),
                source: "legacy-export".into(),
                level: "info".into(),
                code: "LEGACY_EXPORT_STARTED".into(),
                message: "started".into(),
                run_id: None,
                detail: None,
            },
        ],
    };
    let expected = r#"{"agent_id":"agent-1","agent_version":"3.30.0","events":[{"event_id":"6f1f8a3e-2c55-4c55-9a43-1f2e3d4c5b6a","at_utc":"2026-11-15T08:00:00+00:00","source":"agent","level":"warn","code":"SQL_LOGIN_FAILED","message":"Login failed for \u0027sa\u0027 \u003Cx\u003E \u0026 \u1EDF","run_id":"run-1","detail":{"count":3,"tables":["a"],"r":1.0}},{"event_id":"11111111-2222-3333-4444-555555555555","at_utc":"2026-11-15T15:00:00+07:00","source":"legacy-export","level":"info","code":"LEGACY_EXPORT_STARTED","message":"started"}]}"#;
    assert_eq!(batch.to_json(), expected);
    let back = SiteEventBatch::from_json(expected).unwrap();
    assert_eq!(back.to_json(), expected);
}

#[test]
fn blob_ref_batch() {
    let b = SyncBlobRefBatch {
        agent_id: "agent-1".into(),
        contract_version: blob_contract::VERSION,
        refs: vec![SyncBlobRef {
            table: "DefRVCList".into(),
            key: smap([("RVCNo", Some("1"))]),
            column: "Picture".into(),
            sha256: "a".repeat(64),
        }],
    };
    assert_eq!(
        b.to_json(),
        format!(
            r#"{{"agent_id":"agent-1","contract_version":1,"refs":[{{"table":"DefRVCList","key":{{"RVCNo":"1"}},"column":"Picture","sha256":"{}"}}]}}"#,
            "a".repeat(64)
        )
    );
}

#[test]
fn print_queue_report() {
    let r = PrintQueueReport {
        agent_id: "agent-1".into(),
        queues: vec![
            PrintQueueInfo {
                name: "Kitchen 1".into(),
                kind: print_queue_kinds::TCP.into(),
                port_name: "IP_10.0.0.5".into(),
                host: Some("10.0.0.5".into()),
                port: Some(9100),
            },
            PrintQueueInfo {
                name: "USB Bar \"2\"".into(),
                kind: print_queue_kinds::USB.into(),
                port_name: "USB001".into(),
                host: None,
                port: None,
            },
        ],
        spooler_printing: Some(true),
    };
    let expected = r#"{"agent_id":"agent-1","queues":[{"name":"Kitchen 1","kind":"tcp","port_name":"IP_10.0.0.5","host":"10.0.0.5","port":9100},{"name":"USB Bar \u00222\u0022","kind":"usb","port_name":"USB001"}],"spooler_printing":true}"#;
    assert_eq!(r.to_json(), expected);
    assert_eq!(PrintQueueReport::from_json(expected).unwrap(), r);
    let empty = PrintQueueReport {
        agent_id: "agent-1".into(),
        queues: vec![],
        spooler_printing: None,
    };
    assert_eq!(empty.to_json(), r#"{"agent_id":"agent-1","queues":[]}"#);
}

#[test]
fn staff_snapshot() {
    let s = StaffSnapshot {
        agent_id: "agent-1".into(),
        agent_version: "3.30.0".into(),
        key_id: "e346975cd4c277eb".into(),
        clerks: vec![
            StaffClerk {
                key: "NV01".into(),
                name: "Nguyễn Văn A".into(),
                active: true,
                pin_lookup: Some("FnBxjjdDkD4NU/2PPVfVQ+rDq2ELINJH2Z63OYZEh64=".into()),
                pin_status: staff_pin_status::SET.into(),
                security: Some(3),
                rvcs: vec![1, 2],
                functions: vec!["PT_Payment".into(), "Cashier".into()],
            },
            StaffClerk {
                key: "NV02".into(),
                name: "B".into(),
                active: false,
                pin_lookup: None,
                pin_status: staff_pin_status::NONE.into(),
                security: None,
                rvcs: vec![],
                functions: vec![],
            },
        ],
    };
    let expected = r#"{"agent_id":"agent-1","agent_version":"3.30.0","key_id":"e346975cd4c277eb","clerks":[{"key":"NV01","name":"Nguy\u1EC5n V\u0103n A","active":true,"pin_lookup":"FnBxjjdDkD4NU/2PPVfVQ\u002BrDq2ELINJH2Z63OYZEh64=","pin_status":"set","security":3,"rvcs":[1,2],"functions":["PT_Payment","Cashier"]},{"key":"NV02","name":"B","active":false,"pin_status":"none","rvcs":[],"functions":[]}]}"#;
    assert_eq!(s.to_json(), expected);
    assert_eq!(StaffSnapshot::from_json(expected).unwrap(), s);
}

const MANIFEST: &str = r#"{
  "format": "pda-legacy-bundle",
  "formatVersion": 1,
  "createdAt": "2026-11-04T10:00:00+07:00",
  "source": {
    "appVersion": "3.30.32",
    "sqliteSchemaVersion": null,
    "sqlDatabase": "SMILE_POS",
    "exporter": "PdaSyncAgent 3.30.0",
    "store": null
  },
  "maintenance": {
    "mode": "maintenance",
    "evidence": "no writes for 10 min \u003Cok\u003E"
  },
  "files": [
    {
      "table": "pda_config",
      "path": "tables/pda_config.jsonl.gz",
      "rows": 12,
      "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      "disposition": "import"
    },
    {
      "table": "pda_audit_log",
      "path": "tables/pda_audit_log.jsonl.gz",
      "rows": 0,
      "sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
      "disposition": "archive"
    }
  ],
  "leftOut": [
    {
      "table": "pda_refresh_token",
      "rows": 3,
      "reason": "secret: login sessions; users sign in again"
    }
  ]
}"#;

const MANIFEST2: &str = r#"{
  "format": "pda-legacy-bundle",
  "formatVersion": 1,
  "createdAt": "2026-11-04T10:00:00+07:00",
  "source": {
    "appVersion": null,
    "sqliteSchemaVersion": 14,
    "sqlDatabase": "SMILE_POS",
    "exporter": "x",
    "store": "sqlserver:dbo"
  },
  "maintenance": {
    "mode": "maintenance",
    "evidence": "no writes for 10 min \u003Cok\u003E"
  },
  "files": [],
  "leftOut": []
}"#;

#[test]
fn legacy_bundle_manifest() {
    let m = LegacyBundleManifest {
        format: bundle::FORMAT.into(),
        format_version: bundle::FORMAT_VERSION,
        created_at: local((2026, 11, 4, 10, 0, 0), 420, 0),
        source: LegacyBundleSource {
            app_version: Some("3.30.32".into()),
            sqlite_schema_version: None,
            sql_database: "SMILE_POS".into(),
            exporter: "PdaSyncAgent 3.30.0".into(),
            store: None,
        },
        maintenance: LegacyBundleMaintenance {
            mode: "maintenance".into(),
            evidence: "no writes for 10 min <ok>".into(),
        },
        files: vec![
            LegacyBundleFile {
                table: "pda_config".into(),
                path: "tables/pda_config.jsonl.gz".into(),
                rows: 12,
                sha256: "b".repeat(64),
                disposition: LegacyDisposition::Import,
            },
            LegacyBundleFile {
                table: "pda_audit_log".into(),
                path: "tables/pda_audit_log.jsonl.gz".into(),
                rows: 0,
                sha256: "c".repeat(64),
                disposition: LegacyDisposition::Archive,
            },
        ],
        left_out: vec![LegacyLeftOut {
            table: "pda_refresh_token".into(),
            rows: 3,
            reason: "secret: login sessions; users sign in again".into(),
        }],
    };
    let expected = MANIFEST.replace('\n', NEWLINE);
    assert_eq!(m.to_json(), expected);
    assert_eq!(
        LegacyBundleManifest::from_json(&expected).unwrap(),
        Some(m.clone())
    );

    let m2 = LegacyBundleManifest {
        source: LegacyBundleSource {
            app_version: None,
            sqlite_schema_version: Some(14),
            sql_database: "SMILE_POS".into(),
            exporter: "x".into(),
            store: Some("sqlserver:dbo".into()),
        },
        files: vec![],
        left_out: vec![],
        ..m
    };
    assert_eq!(m2.to_json(), MANIFEST2.replace('\n', NEWLINE));

    // JsonSerializerDefaults.Web reading: case-insensitive names, numbers from strings,
    // enum names in any case or as numbers (probe3 "MANIFESTREAD").
    let back = LegacyBundleManifest::from_json(
        r#"{"FORMAT":"f","formatVersion":"2","createdAt":"2026-11-04T10:00:00+07:00","files":[{"table":"t","rows":"5","disposition":"ARCHIVE"},{"disposition":2}]}"#,
    )
    .unwrap()
    .unwrap();
    assert_eq!(back.format, "f");
    assert_eq!(back.format_version, 2);
    assert_eq!(
        back.created_at.to_round_trip_string(),
        "2026-11-04T10:00:00.0000000+07:00"
    );
    assert_eq!(back.files.len(), 2);
    assert_eq!(back.files[0].rows, 5);
    assert_eq!(back.files[0].disposition, LegacyDisposition::Archive);
    assert_eq!(back.files[1].disposition, LegacyDisposition::Unsupported);
}

#[test]
fn secret_config_keys_like_the_dotnet_regex() {
    for (k, expected) in [
        ("smtp.password", true),
        ("PassWd", true),
        ("paſsword", false),
        ("api_key", true),
        ("API-KEY", true),
        ("apikey", true),
        ("api..key", false),
        ("private.key", true),
        ("toKen", true),
        ("\u{212a}ey", false),
        ("api\u{212a}ey", true),
        ("credentials", true),
        ("user", false),
    ] {
        assert_eq!(bundle::is_secret_config_key(k), expected, "{k}");
    }
}

#[test]
fn site_command_payloads() {
    let kds = KdsLineStatePayload {
        rvc_no: 3,
        check_no: 500_001_234,
        line_seqs: vec![7, 8],
        action: "done".into(),
        cashier: "c01".into(),
    };
    assert_eq!(
        kds.to_json(),
        r#"{"rvc_no":3,"check_no":500001234,"line_seqs":[7,8],"action":"done","cashier":"c01"}"#
    );
    let resv = ReservationSavePayload {
        rvc_no: 1,
        guest_info: "Anh Minh".into(),
        num_adult: 2,
        num_child: 0,
        from_date_time: DateTime::new(2026, 10, 24, 19, 0, 0).unwrap(),
        to_date_time: DateTime::new(2026, 10, 24, 21, 0, 0)
            .unwrap()
            .checked_add_ticks(5)
            .unwrap(),
        table_num: "T5".into(),
        tel_num: "0901".into(),
        booker: Some("host".into()),
        company_name: None,
        vat_code: None,
        address: None,
    };
    let expected = r#"{"rvc_no":1,"guest_info":"Anh Minh","num_adult":2,"num_child":0,"from_date_time":"2026-10-24T19:00:00","to_date_time":"2026-10-24T21:00:00.0000005","table_num":"T5","tel_num":"0901","booker":"host"}"#;
    assert_eq!(resv.to_json(), expected);
    assert_eq!(ReservationSavePayload::from_json(expected).unwrap(), resv);
    let p = SyncResnapshotPayload {
        streams: vec!["Invoice".into(), "Trn".into()],
        reason: "parent pending".into(),
    };
    assert_eq!(
        p.to_json(),
        r#"{"streams":["Invoice","Trn"],"reason":"parent pending"}"#
    );
    let r = SyncResnapshotResult {
        streams: vec![],
        full: true,
        cooldown_bypassed: false,
        already_pending: true,
    };
    assert_eq!(
        r.to_json(),
        r#"{"streams":[],"full":true,"cooldown_bypassed":false,"already_pending":true}"#
    );
}

#[test]
fn order_payload_and_check_result() {
    let post = OrderItemPostPayload {
        rvc_no: 3,
        check_no: 5001,
        cashier: "c01".into(),
        item_code: "PHO".into(),
        qty: d("2.50"),
        seat_no: 0,
        price: d("50000.00"),
        note: None,
        open_name: Some("Phở đặc biệt".into()),
        allow_zero_open_price: false,
    };
    let expected = r#"{"rvc_no":3,"check_no":5001,"cashier":"c01","item_code":"PHO","qty":2.50,"seat_no":0,"price":50000.00,"open_name":"Ph\u1EDF \u0111\u1EB7c bi\u1EC7t","allow_zero_open_price":false}"#;
    assert_eq!(post.to_json(), expected);
    assert_eq!(
        OrderItemPostPayload::from_json(expected).unwrap().to_json(),
        expected
    );

    let lines = vec![
        OrderCheckLine {
            line_seq: 1,
            transaction_code: Some("100".into()),
            item_code: Some("PHO".into()),
            item_name: Some("Phở".into()),
            qty: d("2.0"),
            price: d("50000.00"),
            amount: d("100000.0000"),
            seat_no: 0,
            cashier: Some("c01".into()),
            sent: true,
            note: None,
        },
        OrderCheckLine {
            line_seq: 2,
            transaction_code: Some("9".into()),
            item_code: None,
            item_name: None,
            qty: d("1"),
            price: d("-0.5"),
            amount: d("0"),
            seat_no: 1,
            cashier: None,
            sent: false,
            note: Some("n".into()),
        },
    ];
    let header = CheckStateHeader {
        check_no: 5001,
        table_no: Some("T4".into()),
        guests: 2,
        sub_total: d("100000"),
        total_paid: d("0"),
    };
    let chk = OrderCheckResult {
        rvc_no: 3,
        check_no: 5001,
        table_no: Some("T4".into()),
        guests: 2,
        sub_total: d("100000"),
        total_paid: d("0"),
        fingerprint_after: OrderCheckResult::fingerprint(&header, &lines),
        lines,
        trn_seq: Some(2),
        sent_seqs: Some(vec![1, 2]),
    };
    let expected = r#"{"rvc_no":3,"check_no":5001,"table_no":"T4","guests":2,"sub_total":100000,"total_paid":0,"lines":[{"line_seq":1,"transaction_code":"100","item_code":"PHO","item_name":"Ph\u1EDF","qty":2.0,"price":50000.00,"amount":100000.0000,"seat_no":0,"cashier":"c01","sent":true,"is_item":true},{"line_seq":2,"transaction_code":"9","qty":1,"price":-0.5,"amount":0,"seat_no":1,"sent":false,"note":"n","is_item":false}],"fingerprint_after":"cs1:8723343b76b1b76d0bb02a3f8bbaa2e1","trn_seq":2,"sent_seqs":[1,2]}"#;
    assert_eq!(chk.to_json(), expected);
    assert_eq!(OrderCheckResult::from_json(expected).unwrap(), chk);
    assert_eq!(
        order_aggregate::open(3, g(G1)),
        "check-open:3:6f1f8a3e2c554c559a431f2e3d4c5b6a"
    );
}

#[test]
fn check_state_fingerprints() {
    let line = |seq, code: &str, item: &str, qty: &str, price: &str| CheckStateLine {
        line_seq: seq,
        transaction_code: Some(code.into()),
        item_code: Some(item.into()),
        qty: d(qty),
        price: d(price),
    };
    let h = CheckStateHeader {
        check_no: 5001,
        table_no: Some("T4".into()),
        guests: 2,
        sub_total: d("100000"),
        total_paid: d("0"),
    };
    assert_eq!(
        CheckStateFingerprint::compute(
            &h,
            &[
                line(1, "1", "CF01", "1", "50000"),
                line(2, "1", "TEA", "2", "25000")
            ]
        ),
        "cs1:200d59b76d10c9f4ce3859d3aa286264"
    );
    // Rounding of "0.############" (away from zero), trimming, '|' replaced, big values.
    let h2 = CheckStateHeader {
        check_no: 7,
        table_no: Some(" T|4 ".into()),
        guests: 0,
        sub_total: d("-0.0000000000005"),
        total_paid: d("1.23456789012345"),
    };
    let l2 = CheckStateLine {
        line_seq: 1,
        transaction_code: None,
        item_code: Some("Ở".into()),
        qty: d("0.0000000000004"),
        price: d("79228162514264337593543950335"),
    };
    assert_eq!(
        CheckStateFingerprint::compute(&h2, &[l2]),
        "cs1:6f8b11903103bb10573d7d0d97cb1359"
    );
}

// ---- Mapper golden (probe3 "MAP*" lines) -----------------------------------------------

fn json_str_map(m: &StrMap) -> String {
    let mut w = Writer::new(Style::Compact);
    write_str_map(&mut w, m);
    w.finish()
}

fn json_string_map(m: &OrderedMap<String>) -> String {
    let mut w = Writer::new(Style::Compact);
    w.begin_object();
    for (k, v) in m.iter() {
        w.key(k).string(v);
    }
    w.end_object();
    w.finish()
}

/// The probe's line: target|hash|key|columns|excluded|dropped|blobs|aligned|defaulted.
fn line_of(m: &MappedRow) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}",
        m.pg_target,
        m.hash,
        json_str_map(&m.key),
        json_str_map(&m.columns),
        m.excluded.join(","),
        m.dropped.join(","),
        json_string_map(&m.blob_hashes),
        if m.key_aligned { "True" } else { "False" },
        m.defaulted_keys.join(",")
    )
}

fn invoice() -> VendorRow {
    row([
        ("CheckNo", RawValue::I64(500_001_234)),
        ("RVC", RawValue::I32(1)),
        ("OpenTime", dt(2026, 10, 24, 12, 0, 0)),
        ("CloseTime", dt(2026, 10, 24, 13, 30, 0)),
        ("TotalPaid", dec("125000.50")),
        ("SubTotal", dec("100000.00")),
        ("PaymentInfo", RawValue::Bytes(PHO_BO_TCVN3.to_vec())),
        ("PONumber", RawValue::Null),
        ("CardNumber", "4111111111111111".into()),
    ])
}

#[test]
fn mapper_matches_dotnet() {
    let m = mapper();
    let pho = || RawValue::Bytes(PHO_BO_TCVN3.to_vec());
    let cases: Vec<(&str, VendorRow, &str)> = vec![
        (
            "Invoice",
            invoice(),
            r#"app.closed_checks|c7b962e4557c598e580669509e4cfa9d8a0ed63c706fdeabc345fc2cdc621e8e|{"check_no":"500001234"}|{"check_no":"500001234","closed_at":"2026-10-24T06:30:00.0000000Z","opened_at":"2026-10-24T05:00:00.0000000Z","rvc_no":"1","src_extra":"{\u0022PONumber\u0022:null,\u0022PaymentInfo\u0022:\u0022Ph\\u1EDF b\\u00F2\u0022}","sub_total":"100000","total_paid":"125000.5"}|CardNumber||{}|True|"#,
        ),
        (
            "CashierDef",
            row([
                ("Initial", "NV01".into()),
                ("FullName", pho()),
                ("PassWord", "1234".into()),
                ("TouchID", "T".into()),
                ("CardID", "C".into()),
            ]),
            r#"app.users|5e933d7b303cb815ebbafb919ab92ad83a0294f04b3467be1978369e3d0133da|{"Initial":"NV01"}|{"FullName":"Ph\u1EDF b\u00F2","Initial":"NV01"}|PassWord,TouchID,CardID||{}|True|"#,
        ),
        (
            "MemberList",
            row([
                ("MemberID", "M1".into()),
                ("MemberSubID", RawValue::I32(0)),
                ("MemberName", pho()),
                ("Picture", RawValue::Bytes(vec![1, 2, 3])),
            ]),
            r#"app.members|791a18a27abeb33a56ad1061bc500846d6a512909cee41d873bf94a01f29811f|{"MemberID":"M1","MemberSubID":"0"}|{"MemberID":"M1","MemberSubID":"0"}|||{"MemberName":"f5b01214a1b2306171df1914d1a266936a78031193da29b43ffd782966aa18fd","Picture":"039058c6f2c0cb492c533b0a4d14ef77cc0f78abccced5287d84a1a2011cfb81"}|False|"#,
        ),
        (
            "GlobPara",
            row([
                ("nSeq", RawValue::I32(7)),
                ("RVC", RawValue::Null),
                ("ParaName", "TaxByIClass".into()),
                ("ParaStr", "1".into()),
                ("ParaType", "S".into()),
                ("ParaDesc", "desc".into()),
                ("Status", RawValue::I32(1)),
            ]),
            r#"app.config|cc4d247954e69d76c076a6e28b27dcf27c7ff4e9f2a04d02ae0fc38a4f4ebbfd|{"key":"TaxByIClass","rvc_no":"0"}|{"key":"TaxByIClass","rvc_no":"0","src_extra":"{\u0022ParaDesc\u0022:\u0022desc\u0022,\u0022ParaType\u0022:\u0022S\u0022,\u0022Status\u0022:\u00221\u0022,\u0022nSeq\u0022:\u00227\u0022}","value":"1"}|||{}|True|rvc_no"#,
        ),
        (
            "ReOpenLog",
            row([
                ("CheckNo", RawValue::I32(7)),
                ("OpenTime", dt(2026, 10, 24, 12, 0, 0)),
                ("CloseTime", RawValue::Null),
                ("ReOpenTime", dt(2026, 10, 24, 14, 0, 0)),
            ]),
            r#"app.reopen_log|027e1fa9cd100288454e94339014dde249f2846631618c13d431a9e4caa10c10|{"CheckNo":"7","CloseTime":null,"OpenTime":"2026-10-24T05:00:00.0000000Z"}|{"CheckNo":"7","CloseTime":null,"OpenTime":"2026-10-24T05:00:00.0000000Z","ReOpenTime":"2026-10-24T07:00:00.0000000Z"}|||{}|False|"#,
        ),
        (
            "TmpTrn",
            row([
                ("CheckNo", RawValue::I64(7)),
                ("TrnSeq", RawValue::I32(3)),
                ("TrnCode", RawValue::I32(100)),
                ("ItemCode", "PHO".into()),
                ("TrnQty", dec("2")),
                ("BaseTTL", dec("90000.00")),
                ("Ratio", RawValue::F64(0.1)),
                ("F", RawValue::F32(1.5)),
                ("G", RawValue::Guid(g(G1))),
                ("S", RawValue::I16(-2)),
                (
                    "T",
                    RawValue::TimeSpan(
                        TICKS_PER_HOUR + 2 * TICKS_PER_MINUTE + 3 * TICKS_PER_SECOND,
                    ),
                ),
                ("B", RawValue::Bool(true)),
            ]),
            r#"app.open_check_lines|ed6883617cafab1f25bb1ab67060c9f88d075fad541b870cc6815b66ba3fdc72|{"check_no":"7","line_seq":"3"}|{"base_ttl":"90000","check_no":"7","item_code":"PHO","line_seq":"3","src_extra":"{\u0022B\u0022:\u0022true\u0022,\u0022F\u0022:\u00221.5\u0022,\u0022G\u0022:\u00226f1f8a3e-2c55-4c55-9a43-1f2e3d4c5b6a\u0022,\u0022Ratio\u0022:\u00220.1\u0022,\u0022S\u0022:\u0022-2\u0022,\u0022T\u0022:\u002201:02:03\u0022}","transaction_code":"100","trn_qty":"2"}|||{}|True|"#,
        ),
    ];
    for (table, r, expected) in cases {
        assert_eq!(line_of(&m.map(table, &r).unwrap()), expected, "{table}");
    }
    let canon = m.canonicalize("Invoice", &invoice()).unwrap();
    assert_eq!(
        format!("{}|{}", canon.hash, json_str_map(&canon.columns)),
        r#"c7b962e4557c598e580669509e4cfa9d8a0ed63c706fdeabc345fc2cdc621e8e|{"CheckNo":"500001234","CloseTime":"2026-10-24T06:30:00.0000000Z","OpenTime":"2026-10-24T05:00:00.0000000Z","PONumber":null,"PaymentInfo":"Ph\u1EDF b\u00F2","RVC":"1","SubTotal":"100000","TotalPaid":"125000.5"}"#
    );
}

/// Pinned to the same literal `SiteCommandCancelEmptyTests.The_wire_payload_is_snake_case_and_the_verb_is_durable`
/// asserts on the .NET side (ADR 0144): both agents and the cloud speak one payload.
#[test]
fn check_cancel_empty_payload_is_byte_identical_to_dotnet() {
    let p = OrderCheckCancelEmptyPayload {
        rvc_no: 4,
        check_no: 7001,
        table_no: "A1234".into(),
        cashier: "SML".into(),
        opened_before: "2026-11-23T04:00:00".into(),
    };
    assert_eq!(
        p.to_element().to_string(),
        "{\"rvc_no\":4,\"check_no\":7001,\"table_no\":\"A1234\",\"cashier\":\"SML\",\"opened_before\":\"2026-11-23T04:00:00\"}"
    );
    assert_eq!(p.problem(), None);
    let back = OrderCheckCancelEmptyPayload::from_element(&p.to_element()).unwrap();
    assert_eq!(back, p);
    let spec = site_command_contract::find(Some(table_command_types::CHECK_CANCEL_EMPTY)).unwrap();
    assert_eq!(spec.delivery, SiteCommandDelivery::Durable);
    assert_eq!(spec.max_attempts, 8);
}

#[test]
fn local_timestamp_accepts_only_real_calendar_moments() {
    for ok in [
        "2026-11-23T04:00:00",
        "2028-02-29T23:59:59",
        "2026-12-31T00:00:00",
    ] {
        assert!(is_local_timestamp(ok), "{ok}");
    }
    for bad in [
        "",
        "2026-11-23",
        "2026-11-23 04:00:00",
        "2026-02-29T04:00:00",
        "2026-13-01T00:00:00",
        "2026-11-23T24:00:00",
        "2026-11-23T04:60:00",
        "2026-11-23T04:00:60",
        "0000-01-01T00:00:00",
        "2026-11-23T04:00:0x",
        "２026-11-23T04:00:00",
    ] {
        assert!(!is_local_timestamp(bad), "{bad}");
    }
}
