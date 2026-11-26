//! The wire and storage side of `sync.verify` (ADR 0126/0128), byte for byte with C#:
//! the payload and result JSON (expected strings printed by .NET 10 over the real
//! `MM.PdaOrder.Sync.Contracts` project), the shadow manifest golden vector shared with the
//! cloud (`ShadowManifestTests.cs` / `PgSyncIngestTests`), the manifest answers
//! (`HttpCloudManifestSource.Parse*`), `RowKey.TryParse` and the outbox `key_repair` queue
//! (`KeyRepairQueueTests.cs`).

mod support;
mod verify_support;

use std::collections::HashSet;
use std::sync::Arc;

use dl_agent_pda_sync::capture::codecs::key_codec;
use dl_agent_pda_sync::capture::shadow_manifest::{self, ManifestBucket, ROWS_PER_PASS};
use dl_agent_pda_sync::capture::{
    CaptureState, InMemoryCaptureState, KeyRepairList, SqliteCaptureState,
};
use dl_agent_pda_sync::commands::verify_manifest::{
    HttpCloudManifestSource, escape_data_string, parse, parse_keys,
};
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::hashing::row_key;
use dl_agent_pda_sync::contracts::map::StrMap;
use dl_agent_pda_sync::contracts::time::DateTimeOffset;
use dl_agent_pda_sync::contracts::wire::{
    SyncVerifyCommandResult, SyncVerifyPayload, SyncVerifySkippedTable, SyncVerifyTableResult,
    site_command_contract, site_command_types, sync_verify_repair as repair,
};
use dl_agent_pda_sync::outbox::{OutboxKeyRepairList, SqliteOutbox};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::transport::SyncResponse;
use dl_agent_pda_sync::util::ctx::Ctx;
use support::TestEnv;
use verify_support::seed;

fn ctx() -> Ctx {
    Ctx::background()
}

#[test]
fn the_command_is_durable_with_three_attempts() {
    let spec = site_command_contract::find(Some(site_command_types::SYNC_VERIFY)).unwrap();
    assert_eq!(spec.type_, "sync.verify");
    assert_eq!(spec.max_attempts, 3);
    assert_eq!(
        spec.delivery,
        dl_agent_pda_sync::contracts::wire::SiteCommandDelivery::Durable
    );
    assert_eq!(SyncVerifyPayload::AGGREGATE_KEY, "sync:verify");
}

#[test]
fn payload_json_matches_dotnet() {
    let p = SyncVerifyPayload {
        tables: vec!["Invoice".into(), "Trn".into()],
        buckets: 256,
        reason: "nightly \"check\" <é>".into(),
    };
    assert_eq!(
        p.to_json(),
        r##"{"tables":["Invoice","Trn"],"buckets":256,"reason":"nightly \u0022check\u0022 \u003C\u00E9\u003E"}"##
    );
    assert_eq!(SyncVerifyPayload::from_json(&p.to_json()).unwrap(), p);
    let empty = SyncVerifyPayload {
        tables: Vec::new(),
        buckets: 1,
        reason: "x".into(),
    };
    assert_eq!(empty.to_json(), r#"{"tables":[],"buckets":1,"reason":"x"}"#);
}

#[test]
fn payload_validation_follows_the_contract() {
    let p = SyncVerifyPayload {
        tables: vec!["Invoice".into(), "Trn".into()],
        buckets: 256,
        reason: "nightly".into(),
    };
    assert_eq!(p.problem(), None);
    let with = |f: &dyn Fn(&mut SyncVerifyPayload)| {
        let mut q = p.clone();
        f(&mut q);
        q.problem()
    };
    assert_eq!(
        with(&|q| q.tables.clear()),
        None,
        "an empty list means every mirrored table"
    );
    assert_eq!(with(&|q| q.buckets = 1), None);
    assert!(with(&|q| q.buckets = 0).is_some());
    assert!(with(&|q| q.buckets = 257).is_some());
    assert!(with(&|q| q.tables = (0..65).map(|i| format!("t{i}")).collect()).is_some());
    assert!(with(&|q| q.tables = vec!["t".repeat(65)]).is_some());
    assert!(with(&|q| q.tables = vec![" ".into()]).is_some());
    assert!(with(&|q| q.reason = String::new()).is_some());
    assert!(with(&|q| q.reason = "r".repeat(301)).is_some());

    let missing = SyncVerifyPayload::from_json(r#"{"tables":[],"reason":"x"}"#).unwrap();
    assert_eq!(
        missing.problem(),
        Some("buckets must be 1..256"),
        "no silent default"
    );
}

#[test]
fn result_json_matches_dotnet() {
    let r = SyncVerifyCommandResult {
        buckets: 256,
        tables: vec![
            SyncVerifyTableResult {
                table: "Invoice".into(),
                store_rows: 10,
                cloud_rows: 9,
                mismatched_buckets: vec![3, 200],
                repair_scheduled: true,
                repair_keys: 7,
            },
            SyncVerifyTableResult {
                table: "Trn".into(),
                store_rows: 5,
                cloud_rows: 5,
                ..SyncVerifyTableResult::default()
            },
        ],
        skipped: vec![SyncVerifySkippedTable::new("tmpInvoice", "open_check")],
        store_rows: 15,
        cloud_rows: 14,
        mismatched_tables: 1,
        mismatched_buckets: 2,
        repair: repair::KEYS_QUEUED.into(),
    };
    assert_eq!(
        r.to_json(),
        r#"{"buckets":256,"tables":[{"table":"Invoice","store_rows":10,"cloud_rows":9,"mismatched_buckets":[3,200],"repair_scheduled":true,"repair_keys":7},{"table":"Trn","store_rows":5,"cloud_rows":5,"mismatched_buckets":[],"repair_scheduled":false,"repair_keys":0}],"skipped":[{"table":"tmpInvoice","reason":"open_check"}],"store_rows":15,"cloud_rows":14,"mismatched_tables":1,"mismatched_buckets":2,"repair":"keys_queued"}"#
    );
    assert_eq!(SyncVerifyCommandResult::from_json(&r.to_json()).unwrap(), r);
    let none = SyncVerifyCommandResult {
        buckets: 1,
        repair: repair::NONE.into(),
        ..SyncVerifyCommandResult::default()
    };
    assert_eq!(
        none.to_json(),
        r#"{"buckets":1,"tables":[],"skipped":[],"store_rows":0,"cloud_rows":0,"mismatched_tables":0,"mismatched_buckets":0,"repair":"none"}"#
    );
    // An answer of an agent before ADR 0128 has no repair_keys.
    let old = SyncVerifyTableResult::from_json(
        r#"{"table":"T","store_rows":1,"cloud_rows":1,"mismatched_buckets":[],"repair_scheduled":false}"#,
    )
    .unwrap();
    assert_eq!(old.repair_keys, 0);
}

fn map(pairs: &[(&str, Option<&str>)]) -> StrMap {
    let mut m = StrMap::new();
    for (k, v) in pairs {
        m.insert(*k, v.map(str::to_owned));
    }
    m
}

#[test]
fn row_key_try_parse_inverts_of_like_dotnet() {
    let k = map(&[("No", Some("1")), ("A", None)]);
    assert_eq!(row_key::of(&k), "A\u{1f}No=1");
    assert_eq!(
        row_key::bucket("A\u{1f}No=1", 256),
        51,
        ".NET RowKey.Bucket"
    );
    let parsed = |s: &str| {
        row_key::try_parse(s).map(|m| {
            m.iter()
                .map(|(k, v)| format!("{k}:{}", v.as_deref().unwrap_or("<null>")))
                .collect::<Vec<_>>()
                .join(",")
        })
    };
    // Expected values printed by .NET RowKey.TryParse.
    assert_eq!(parsed(""), None);
    assert_eq!(parsed("A").as_deref(), Some("A:<null>"));
    assert_eq!(parsed("A=1").as_deref(), Some("A:1"));
    assert_eq!(parsed("B=1\u{1f}A=2"), None, "columns out of order");
    assert_eq!(parsed("A=1\u{1f}A=2"), None, "a column twice");
    assert_eq!(parsed("A=1\u{1f}B").as_deref(), Some("A:1,B:<null>"));
    assert_eq!(parsed("=1"), None);
    assert_eq!(parsed("A=1=2").as_deref(), Some("A:1=2"));
}

/// Golden vector shared with `PgSyncIngestTests.Manifest_digest_matches_the_shadow_golden_vector`:
/// one bucket, keys whose order differs between byte order (PostgreSQL `COLLATE "C"`),
/// UTF-16 ordinal order and a linguistic collation.
const GOLDEN_ROWS: [(&str, &str); 7] = [
    ("a", "h1"),
    ("B", "h2"),
    ("_x", "h3"),
    ("á", "h4"),
    ("ｚ", "h5"),
    ("𝔸", "h6"),
    ("Z", "h7"),
];
const GOLDEN_DIGEST: &str = "b849393bb8d4985c34172611f03476d707a0a77cc59c4ba92696169994af0f91";

fn shadow_key(value: &str) -> String {
    key_codec::encode(&map(&[("K", Some(value))]))
}

#[test]
fn one_bucket_digest_matches_the_golden_vector_in_byte_order() {
    let env = TestEnv::new();
    let states: [Arc<dyn CaptureState>; 2] = [
        Arc::new(InMemoryCaptureState::new()),
        Arc::new(SqliteCaptureState::open(env.dir.join("capture.db")).unwrap()),
    ];
    for state in states {
        let rows: Vec<(String, String)> = GOLDEN_ROWS
            .iter()
            .map(|(v, h)| (shadow_key(v), (*h).to_owned()))
            .collect();
        seed(&*state, "Gold", &rows);

        let m = shadow_manifest::build(&*state, "Gold", 1, ROWS_PER_PASS, &ctx()).unwrap();

        assert!(!m.duplicate_keys);
        assert_eq!(m.rows, 7);
        assert_eq!(m.buckets, [ManifestBucket::new(0, 7, GOLDEN_DIGEST)]);
    }
}

#[test]
fn buckets_follow_the_cloud_row_key_and_several_passes_give_the_same_manifest() {
    let rows: Vec<(String, String)> = (0..500)
        .map(|i| {
            let k = map(&[
                ("Id", Some(&i.to_string())),
                ("Rvc", if i % 7 == 0 { None } else { Some("1") }),
            ]);
            (key_codec::encode(&k), format!("hash{i}"))
        })
        .collect();
    let state = InMemoryCaptureState::new();
    seed(&state, "T", &rows);

    let single = shadow_manifest::build(&state, "T", 256, ROWS_PER_PASS, &ctx()).unwrap();
    let multi = shadow_manifest::build(&state, "T", 256, 60, &ctx()).unwrap();

    assert_eq!(multi, single);
    assert_eq!(single.rows, 500);
    let mut expected = std::collections::BTreeMap::new();
    for (k, _) in &rows {
        let b = row_key::bucket(&row_key::of(&key_codec::decode(k).unwrap()), 256) as i32;
        *expected.entry(b).or_insert(0) += 1;
    }
    assert_eq!(single.buckets.len(), expected.len());
    assert!(
        single
            .buckets
            .iter()
            .all(|b| expected[&b.bucket] == b.count)
    );
    assert!(single.buckets.windows(2).all(|w| w[0].bucket < w[1].bucket));
}

#[test]
fn a_duplicate_key_occurrence_makes_the_table_incomparable() {
    let key = shadow_key("dup");
    let state = InMemoryCaptureState::new();
    seed(
        &state,
        "T",
        &[
            (key.clone(), "h1".into()),
            (format!("{key}#1"), "h2".into()),
            (shadow_key("x"), "h3".into()),
        ],
    );

    let m = shadow_manifest::build(&state, "T", 256, ROWS_PER_PASS, &ctx()).unwrap();

    assert!(m.duplicate_keys);
    assert!(m.buckets.is_empty());
    let wanted: HashSet<i32> = (0..256).collect();
    assert_eq!(
        shadow_manifest::bucket_rows(&state, "T", 256, &wanted, &ctx()).unwrap(),
        None
    );
}

#[test]
fn an_empty_table_has_no_buckets_and_bad_bucket_counts_are_refused() {
    let state = InMemoryCaptureState::new();
    let m = shadow_manifest::build(&state, "None", 256, ROWS_PER_PASS, &ctx()).unwrap();
    assert_eq!((m.rows, m.buckets.len(), m.duplicate_keys), (0, 0, false));
    assert!(shadow_manifest::build(&state, "None", 0, ROWS_PER_PASS, &ctx()).is_err());
    assert!(shadow_manifest::build(&state, "None", 257, ROWS_PER_PASS, &ctx()).is_err());
}

#[test]
fn an_unreadable_shadow_key_is_an_error_not_a_mismatch() {
    let state = InMemoryCaptureState::new();
    seed(&state, "T", &[("not a key".into(), "h".into())]);
    let e = shadow_manifest::build(&state, "T", 256, ROWS_PER_PASS, &ctx()).unwrap_err();
    assert!(matches!(
        e.kind,
        dl_agent_pda_sync::capture::ErrorKind::Format
            | dl_agent_pda_sync::capture::ErrorKind::Argument
    ));
}

#[test]
fn mismatched_lists_buckets_with_another_count_or_digest_or_present_on_one_side() {
    let b = ManifestBucket::new;
    let store = [b(1, 2, "aa"), b(2, 1, "bb"), b(3, 1, "cc"), b(5, 4, "ee")];
    let cloud = [b(1, 2, "AA"), b(2, 1, "bx"), b(4, 1, "dd"), b(5, 3, "ee")];

    assert_eq!(shadow_manifest::mismatched(&store, &cloud), [2, 3, 4, 5]);
    assert!(shadow_manifest::mismatched(&store, &store).is_empty());
}

fn body(json: &str) -> SyncResponse {
    SyncResponse {
        status: 200,
        body: json.as_bytes().to_vec(),
        reused: false,
    }
}

#[test]
fn the_cloud_manifest_answer_is_read_from_the_api_envelope() {
    let ok = parse(&body(
        r#"{"result":"ok","data":{"table":"Invoice","buckets":[{"bucket":3,"count":2,"digest":"ab"},{"bucket":9,"count":1,"digest":"cd"}]}}"#,
    ));
    assert_eq!(ok.error, None);
    assert_eq!(
        ok.buckets,
        [
            ManifestBucket::new(3, 2, "ab"),
            ManifestBucket::new(9, 1, "cd")
        ]
    );
    for bad in [
        r#"{"result":"ok","data":{"table":"Invoice","buckets":[{"bucket":"x"}]}}"#,
        r#"{"result":"ok","data":{}}"#,
        r#"{"result":"ok","data":{"buckets":[{"bucket":1,"count":1,"digest":null}]}}"#,
        "not json",
    ] {
        assert_eq!(
            parse(&body(bad)).error.as_deref(),
            Some("malformed"),
            "{bad}"
        );
    }
}

#[test]
fn the_cloud_key_answer_is_read_from_the_api_envelope() {
    let ok = parse_keys(&body(
        r#"{"result":"ok","data":{"table":"Invoice","keys":[{"key":"No=1","hash":"ab"}],"truncated":true}}"#,
    ));
    assert_eq!(ok.rows.len(), 1);
    assert_eq!(ok.rows["No=1"], "ab");
    assert_eq!((ok.truncated, ok.error), (true, None));
    assert!(!parse_keys(&body(r#"{"data":{"keys":[]}}"#)).truncated);
    for bad in [
        r#"{"result":"ok","data":{"keys":[{"key":1}]}}"#,
        r#"{"result":"ok","data":{}}"#,
    ] {
        assert_eq!(
            parse_keys(&body(bad)).error.as_deref(),
            Some("malformed"),
            "{bad}"
        );
    }
}

#[test]
fn request_paths_escape_the_table_like_uri_escape_data_string() {
    assert_eq!(escape_data_string("Invoice"), "Invoice");
    assert_eq!(escape_data_string("a b/c&d~é"), "a%20b%2Fc%26d~%C3%A9");
    assert_eq!(
        HttpCloudManifestSource::manifest_path("Invoice", 256),
        "/sync/v1/manifest?table=Invoice&buckets=256"
    );
    assert_eq!(
        HttpCloudManifestSource::keys_path("Invoice", 256, &[200, 3, 17]),
        "/sync/v1/manifest/keys?table=Invoice&buckets=256&bucket=3,17,200"
    );
}

#[test]
fn keys_are_queued_once_kept_across_a_restart_and_removed_when_completed() {
    let env = TestEnv::new();
    let now = DateTimeOffset::utc(2026, 11, 19, 8, 0, 0).unwrap();
    let open =
        || Arc::new(SqliteOutbox::open(env.paths.outbox_db(), i64::MAX, &Logger::none()).unwrap());
    {
        let outbox = open();
        assert_eq!(
            outbox
                .queue_key_repairs("Invoice", &["No=1", "No=2", "No=1"], now)
                .unwrap(),
            2
        );
        assert_eq!(
            outbox
                .queue_key_repairs("Invoice", &["No=2", "No=3"], now)
                .unwrap(),
            1,
            "a key already waiting is not queued twice"
        );
    }

    let reopened = open();
    let queue = OutboxKeyRepairList::new(reopened.clone());
    let pending = queue.pending(&ctx()).unwrap();
    let seen: Vec<(&str, &str)> = pending
        .iter()
        .map(|t| (t.table.as_str(), t.row_key.as_str()))
        .collect();
    assert_eq!(
        seen,
        [
            ("Invoice", "No=1"),
            ("Invoice", "No=2"),
            ("Invoice", "No=3")
        ]
    );

    queue
        .complete(&[pending[0].id, pending[2].id], &ctx())
        .unwrap();

    assert_eq!(reopened.key_repairs_pending("Invoice").unwrap(), 1);
    assert_eq!(queue.pending(&ctx()).unwrap()[0].row_key, "No=2");
}
