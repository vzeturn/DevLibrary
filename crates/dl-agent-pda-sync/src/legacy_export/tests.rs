//! Unit tests of `legacy.export`: payload, gate, old-build values, print split, bundle
//! writer and the signed upload against a fake hub. The command flow is in
//! `tests/legacy_export.rs`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dl_tds::Value as Tds;

use super::bundle::{self, ExportParts};
use super::payload::LegacyExportPayload;
use super::sources::{
    AppPoolProbe, BrandingSource, Cell, LegacySqlSource, LegacyStore, Result, Row, RowSink,
    VietQrRead, convert, explain_sql_failure, maintenance_gate,
};
use super::sql_source::{LegacySqlStoreSource, split_print_profiles};
use super::upload::{BundleUploader, BundleZip, HttpBundleUploader};
use crate::config::time::{ManualClock, SharedClock, Timestamp};
use crate::config::{AgentPaths, codes};
use crate::contracts::WireJson;
use crate::contracts::legacy::{LegacyBundleMaintenance, LegacyDisposition, LegacySqlKind};
use crate::contracts::time::{DateTimeOffset, TimeZone};
use crate::reporting::{Logger, LoggerConfig};
use crate::security::{AgentKey, FileSecretStore, SharedSecretStore, names};
use crate::transport::{RequestSigner, RetryPolicy, SyncHttpClient, TokenProvider};
use crate::util::ctx::Ctx;

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir()
            .join("pda-legacy-export-unit")
            .join(dl_uuid::Uuid::new_v4().simple().to_string());
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn text(c: &Cell) -> &str {
    match c {
        Cell::Text(s) => s,
        other => panic!("not text: {other:?}"),
    }
}

// ---- payload

#[test]
fn the_payload_reads_snake_case_and_validates_like_the_cloud() {
    let p = LegacyExportPayload::from_json(
        r#"{"source":"from-sql","sql_instance":".\\SQLEXPRESS","commit":true,"prefer_legacy":true,"reason":"why"}"#,
    )
    .unwrap();
    assert_eq!(p.source, "from-sql");
    assert_eq!(p.sql_instance.as_deref(), Some(r".\SQLEXPRESS"));
    assert!(p.commit && p.prefer_legacy && !p.allow_live && !p.vietqr_only);
    assert_eq!(p.problem(), None);
    assert!(p.to_json().contains(r#""prefer_legacy":true"#));

    let case = |f: fn(&mut LegacyExportPayload)| {
        let mut q = p.clone();
        f(&mut q);
        q.problem()
    };
    assert_eq!(
        case(|q| q.source = "somewhere".into()).unwrap(),
        "source must be auto, pda-db or from-sql"
    );
    assert_eq!(
        case(|q| q.database = Some(String::new())).unwrap(),
        "database must be 1..260 characters when given"
    );
    assert_eq!(
        case(|q| q.app_pool = Some("a\u{7}b".into())).unwrap(),
        "app_pool must not contain control characters"
    );
    assert_eq!(
        case(|q| q.site_root = Some("x".repeat(261))).unwrap(),
        "site_root must be 1..260 characters when given"
    );
    assert_eq!(
        case(|q| q.reason = "  ".into()).unwrap(),
        "reason is required"
    );
    assert_eq!(
        case(|q| q.reason = "x".repeat(301)).unwrap(),
        "reason must be at most 300 characters"
    );
}

// ---- the gates

struct Pool(Option<&'static str>);

impl AppPoolProbe for Pool {
    fn state(&self, _pool: &str) -> std::result::Result<Option<String>, String> {
        Ok(self.0.map(str::to_owned))
    }
}

#[test]
fn the_maintenance_gate_wants_evidence_or_allow_live() {
    let tmp = Tmp::new();
    let root = tmp.0.to_str().unwrap();
    let gate = |root: Option<&str>, pool: Option<&str>, live: bool, p: &Pool| {
        maintenance_gate(root, pool, live, p)
    };

    // An app_offline.htm in the site root is maintenance.
    std::fs::write(tmp.0.join("app_offline.htm"), b"").unwrap();
    let m = gate(Some(root), None, false, &Pool(None)).unwrap();
    assert_eq!(m.mode, "maintenance");
    assert!(m.evidence.contains("app_offline.htm"));
    std::fs::remove_file(tmp.0.join("app_offline.htm")).unwrap();

    // A stopped pool is maintenance; a started one is not.
    assert_eq!(
        gate(None, Some("App"), false, &Pool(Some("Stopped")))
            .unwrap()
            .mode,
        "maintenance"
    );
    let live = gate(Some(root), Some("App"), false, &Pool(Some("Started"))).unwrap_err();
    assert_eq!(
        (live.code.as_str(), live.refusal),
        (codes::LEGACY_SITE_LIVE, true)
    );

    // allow_live exports anyway and marks the bundle as a rehearsal.
    assert_eq!(
        gate(Some(root), None, true, &Pool(None)).unwrap().mode,
        "live_rehearsal"
    );

    // Where the gate cannot be evaluated it counts as closed, and says so.
    let e = gate(None, None, false, &Pool(None)).unwrap_err();
    assert_eq!(e.code, codes::LEGACY_SITE_LIVE);
    assert!(e.message.contains("counts as closed"), "{}", e.message);
    let e = gate(None, Some("App"), false, &Pool(None)).unwrap_err();
    assert!(e.message.contains("not available here"), "{}", e.message);

    // A site root that does not exist is a bad argument, not a closed gate.
    let missing = tmp.0.join("nope");
    let e = gate(missing.to_str(), None, true, &Pool(None)).unwrap_err();
    assert_eq!(e.code, codes::BAD_ARGUMENTS);
}

#[test]
fn sql_errors_are_explained_by_code_and_never_quote_other_server_text() {
    let e = explain_sql_failure(53, 20, "network path was not found", "box", "SMILE_POS");
    assert_eq!(
        (e.code.as_str(), e.sql_number),
        (codes::SQL_UNREACHABLE, Some(53))
    );
    assert!(e.message.contains("sql_instance") && e.message.contains("1433"));
    // An error that may quote a row value keeps only its number.
    let e = explain_sql_failure(
        245,
        16,
        "Conversion failed when converting 'Nguyen Van A'",
        "box",
        "D",
    );
    assert!(!e.message.contains("Nguyen"), "{}", e.message);
    assert_eq!(e.sql_number, Some(245));
}

// ---- the values of the oldest builds

fn stamp_of(y: i32, mo: u8, d: u8, h: u8, mi: u8, s: u8) -> Tds {
    Tds::DateTime(dl_time::DateTime::new(
        dl_time::Date::new(y, mo, d).unwrap(),
        dl_time::Time::new(h, mi, s, 0).unwrap(),
    ))
}

#[test]
fn sql_values_become_what_the_pda_db_move_stored() {
    let utc = TimeZone::utc();
    let hanoi = TimeZone::fixed("SE Asia Standard Time", 7 * 60);
    let c = |k, v: Tds| convert(k, &v, &utc);

    assert_eq!(
        c(LegacySqlKind::Trim, Tds::String("ab  ".into())),
        Cell::Text("ab".into())
    );
    assert_eq!(c(LegacySqlKind::Flag, Tds::Bool(true)), Cell::Int(1));
    assert_eq!(
        c(LegacySqlKind::Flag, Tds::String(" TRUE ".into())),
        Cell::Int(1)
    );
    assert_eq!(c(LegacySqlKind::Flag, Tds::I32(0)), Cell::Int(0));
    assert_eq!(
        text(&c(LegacySqlKind::Stamp, stamp_of(2026, 10, 24, 8, 5, 9))),
        "2026-10-24T08:05:09Z"
    );
    // A local-time column of a +07:00 site is stored as UTC.
    assert_eq!(
        text(&convert(
            LegacySqlKind::LocalStamp,
            &stamp_of(2026, 10, 24, 8, 5, 9),
            &hanoi
        )),
        "2026-10-24T01:05:09Z"
    );
    assert_eq!(
        text(&c(LegacySqlKind::Date, stamp_of(2026, 1, 2, 23, 0, 0))),
        "2026-01-02"
    );
    let money: dl_decimal::Decimal = "1234.565".parse().unwrap();
    assert_eq!(
        text(&c(LegacySqlKind::Fixed2, Tds::Decimal(money))),
        "1234.57"
    );
    assert_eq!(text(&c(LegacySqlKind::Fixed0, Tds::Decimal(money))), "1235");
    let scaled: dl_decimal::Decimal = "12.50".parse().unwrap();
    assert_eq!(
        text(&c(LegacySqlKind::Number, Tds::Decimal(scaled))),
        "12.50"
    );
    let whole: dl_decimal::Decimal = "12.90".parse().unwrap();
    assert_eq!(
        c(LegacySqlKind::Truncated, Tds::Decimal(whole)),
        Cell::Int(12)
    );
    assert_eq!(
        c(LegacySqlKind::Text, Tds::I64(42)),
        Cell::Text("42".into())
    );
    assert_eq!(
        c(LegacySqlKind::Blob, Tds::Bytes(vec![1, 2])),
        Cell::Blob(vec![1, 2])
    );
    assert_eq!(c(LegacySqlKind::Raw, Tds::Bool(false)), Cell::Int(0));
    assert_eq!(c(LegacySqlKind::Raw, Tds::I8(7)), Cell::Int(7));
    assert_eq!(c(LegacySqlKind::Raw, Tds::F64(1.5)), Cell::Real(1.5));
    assert_eq!(c(LegacySqlKind::Raw, Tds::Null), Cell::Null);
    // A type the map did not expect is kept as text, never dropped.
    assert_eq!(
        c(LegacySqlKind::Fixed2, Tds::String("n/a".into())),
        Cell::Text("n/a".into())
    );
}

fn profile(id: i64, rvc: i64, doc: &str, name: &str, header: &str, created: &str) -> Row {
    vec![
        ("profile_id".into(), Cell::Int(id)),
        ("rvc".into(), Cell::Int(rvc)),
        ("doc_type".into(), Cell::Text(doc.into())),
        ("name".into(), Cell::Text(name.into())),
        ("header_tpl".into(), Cell::Text(header.into())),
        ("footer_tpl".into(), Cell::Null),
        ("layout_json".into(), Cell::Null),
        ("created_at".into(), Cell::Text(created.into())),
    ]
}

#[test]
fn print_profiles_of_the_oldest_shape_split_into_templates_and_profiles() {
    let split = split_print_profiles(vec![
        profile(3, 1, "bill", "Late", "H1", "2026-02-01"),
        profile(1, 1, "bill", "Early", "H1", "2026-01-01"),
        profile(2, 1, "bill", "Other", "H2", "2026-03-01"),
    ]);
    assert_eq!(split.templates.len(), 2, "one template per distinct design");
    assert_eq!(split.profiles.len(), 3);
    let id = |row: &Row, c: &str| super::sources::cell(row, c).cloned();
    // Profiles in id order: 1 and 3 share design H1 (template 1), 2 has H2 (template 2).
    let ids: Vec<_> = split
        .profiles
        .iter()
        .map(|r| id(r, "template_id"))
        .collect();
    assert_eq!(
        ids,
        vec![Some(Cell::Int(1)), Some(Cell::Int(2)), Some(Cell::Int(1))]
    );
    assert!(split.profiles.iter().all(|r| id(r, "header_tpl").is_none()));
    // The template takes the latest name and the earliest creation time of its profiles.
    let t1 = &split.templates[0];
    assert_eq!(
        id(t1, "name"),
        Some(Cell::Text("bill \u{2013} Late".into()))
    );
    assert_eq!(id(t1, "created_at"), Some(Cell::Text("2026-01-01".into())));
    assert_eq!(id(t1, "is_active"), Some(Cell::Int(1)));
}

// ---- the SQL Server store

struct Fake {
    tables: Vec<(String, i64)>,
    columns: Vec<String>,
    rows: Vec<Vec<Tds>>,
}

impl LegacySqlSource for Fake {
    fn database(&self) -> String {
        "SMILE_POS".into()
    }

    fn rvc_list(&self) -> Result<Vec<(i32, String)>> {
        Ok(vec![(1, "Main".into())])
    }

    fn pda_tables(&self) -> Result<Vec<(String, i64)>> {
        Ok(self.tables.clone())
    }

    fn columns(&self, _q: &str) -> Result<Vec<String>> {
        Ok(self.columns.clone())
    }

    fn for_each_row(
        &self,
        _q: &str,
        columns: &[String],
        sink: &mut dyn FnMut(Vec<Tds>) -> Result<()>,
    ) -> Result<()> {
        assert_eq!(
            columns,
            self.columns.as_slice(),
            "only mapped columns are selected"
        );
        self.rows.iter().try_for_each(|r| sink(r.clone()))
    }
}

#[test]
fn a_build_from_before_pda_db_is_read_through_the_store_map() {
    let sql = Arc::new(Fake {
        tables: vec![
            ("dbo.Pda_Config".into(), 1),
            ("dbo.Pda_RefreshToken".into(), 9),
            ("dbo.Pda_Mystery".into(), 4),
            ("dbo.Pda_MigrationHistory".into(), 12),
        ],
        columns: ["RVCNo", "Key", "Value", "LastChangeBy", "LastChangeTime"]
            .map(String::from)
            .to_vec(),
        rows: vec![vec![
            Tds::I32(1),
            Tds::String("tax.rate  ".into()),
            Tds::String("8".into()),
            Tds::Null,
            stamp_of(2026, 10, 24, 8, 0, 0),
        ]],
    });
    let store = LegacySqlStoreSource::open(sql, Arc::new(TimeZone::utc())).unwrap();
    assert_eq!(store.kind().as_deref(), Some("sqlserver:dbo"));
    assert_eq!(store.tables().unwrap(), vec!["pda_config".to_owned()]);
    assert_eq!(store.count("pda_config").unwrap(), 1);
    let mut rows = Vec::new();
    store
        .for_each_row("pda_config", &mut |r| {
            rows.push(r);
            Ok(())
        })
        .unwrap();
    let key = super::sources::cell(&rows[0], "key").unwrap();
    assert_eq!(text(key), "tax.rate", "trimmed");
    assert_eq!(
        text(super::sources::cell(&rows[0], "last_change_time").unwrap()),
        "2026-10-24T08:00:00Z"
    );

    // What is not exported says why, and only unaccounted rows need a look.
    assert_eq!(store.sql_table_left_out("dbo.Pda_Config"), None);
    assert!(
        store
            .sql_table_left_out("dbo.Pda_RefreshToken")
            .unwrap()
            .contains("never exported")
    );
    assert_eq!(
        store
            .sql_table_left_out("dbo.Pda_MigrationHistory")
            .unwrap(),
        "legacy schema runner bookkeeping"
    );
    assert!(store.sql_table_needs_review("dbo.Pda_Mystery"));
    assert!(!store.sql_table_needs_review("dbo.Pda_RefreshToken"));
    assert!(!store.sql_table_needs_review("dbo.Pda_MigrationHistory"));
}

#[test]
fn rows_in_two_schemas_or_no_known_table_refuse_the_from_sql_export() {
    let two = Arc::new(Fake {
        tables: vec![("dbo.Pda_Config".into(), 2), ("pda.Pda_Config".into(), 3)],
        columns: Vec::new(),
        rows: Vec::new(),
    });
    let Err(e) = LegacySqlStoreSource::open(two, Arc::new(TimeZone::utc())) else {
        panic!("two schemas with rows must be refused");
    };
    assert_eq!(e.code, codes::LEGACY_EXPORT_FAILED);
    assert!(e.message.contains("more than one schema"));
    let none = Arc::new(Fake {
        tables: Vec::new(),
        columns: Vec::new(),
        rows: Vec::new(),
    });
    let Err(e) = LegacySqlStoreSource::open(none, Arc::new(TimeZone::utc())) else {
        panic!("no table must be refused");
    };
    assert_eq!(e.code, codes::LEGACY_STORE_MISSING);
}

// ---- the bundle writer

struct Mem(Vec<(String, Vec<Row>)>);

impl LegacyStore for Mem {
    fn kind(&self) -> Option<String> {
        None
    }

    fn tables(&self) -> Result<Vec<String>> {
        Ok(self.0.iter().map(|(t, _)| t.clone()).collect())
    }

    fn count(&self, table: &str) -> Result<i64> {
        Ok(self
            .0
            .iter()
            .find(|(t, _)| t == table)
            .map_or(0, |(_, r)| r.len() as i64))
    }

    fn for_each_row(&self, table: &str, sink: RowSink<'_>) -> Result<()> {
        let rows = self
            .0
            .iter()
            .find(|(t, _)| t == table)
            .map(|(_, r)| r.clone());
        rows.unwrap_or_default().into_iter().try_for_each(sink)
    }

    fn schema_version(&self) -> Result<Option<i32>> {
        Ok(Some(3))
    }

    fn sql_table_left_out(&self, _q: &str) -> Option<String> {
        None
    }

    fn sql_table_needs_review(&self, _q: &str) -> bool {
        false
    }
}

struct Fixed;

impl BrandingSource for Fixed {
    fn read(&self) -> Vec<(String, String)> {
        Vec::new()
    }
}

fn export_to(dir: &Path) -> crate::contracts::legacy::LegacyBundleManifest {
    let store = Mem(vec![
        (
            "pda_config".into(),
            vec![vec![
                ("key".into(), Cell::Text("a".into())),
                ("value".into(), Cell::Real(f64::NAN)),
            ]],
        ),
        ("pda_unknown".into(), vec![vec![("x".into(), Cell::Int(1))]]),
    ]);
    let sql = Fake {
        tables: Vec::new(),
        columns: Vec::new(),
        rows: Vec::new(),
    };
    let vq = VietQrRead::NotSet;
    let parts = ExportParts {
        store: &store,
        sql: &sql,
        branding: &Fixed,
        vietqr: &vq,
        app_version: Some("3.30.32".into()),
        maintenance: LegacyBundleMaintenance {
            mode: "maintenance".into(),
            evidence: "test".into(),
        },
        exporter: "PdaSyncAgent test".into(),
        created_at: DateTimeOffset::UNIX_EPOCH,
    };
    bundle::export(dir, &parts, &mut |_, _| {}).unwrap()
}

#[test]
fn the_bundle_follows_the_contract_and_its_hashes_are_stable() {
    let (a, b) = (Tmp::new(), Tmp::new());
    let m = export_to(&a.0.join("out"));
    let again = export_to(&b.0.join("out"));
    assert_eq!(
        m.to_json(),
        again.to_json(),
        "same store and time, same manifest"
    );
    let names: Vec<_> = m
        .files
        .iter()
        .map(|f| (f.table.as_str(), f.path.as_str(), f.disposition))
        .collect();
    assert_eq!(
        names,
        vec![
            (
                "pda_config",
                "tables/pda_config.jsonl.gz",
                LegacyDisposition::Import
            ),
            (
                "dbo.DefRVCList",
                "tables/dbo.DefRVCList.jsonl.gz",
                LegacyDisposition::Import
            ),
        ]
    );
    assert!(
        m.left_out
            .iter()
            .any(|l| l.table == "pda_unknown" && l.rows == 1)
    );
    assert!(m.left_out.iter().any(|l| l.table == "registry.dpapi"));
    // The verified reader of the contract accepts what the writer made, rows and hashes.
    let verified = crate::contracts::legacy::bundle_files::read_verified(&a.0.join("out")).unwrap();
    assert_eq!(verified.manifest.files, m.files);
    let rows: Vec<_> = crate::contracts::legacy::bundle_files::read_rows(&verified, &m.files[0])
        .unwrap()
        .collect();
    assert_eq!(rows.len(), 1);
    // A directory that already holds something is refused: nothing is overwritten.
    assert_eq!(
        bundle::prepare_out_dir(&a.0.join("out")).unwrap_err().code,
        codes::BAD_ARGUMENTS
    );
}

// ---- the signed upload against a fake hub

type Seen = Arc<Mutex<Vec<(String, String, Vec<(String, String)>, usize)>>>;

struct Hub {
    base: String,
    port: u16,
    stop: Arc<AtomicBool>,
    seen: Seen,
    fail_401: Arc<AtomicU32>,
    thread: Option<std::thread::JoinHandle<()>>,
}

type Request = (String, String, Vec<(String, String)>, Vec<u8>);

fn read_request(stream: &TcpStream) -> Option<Request> {
    let mut r = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next()?.to_owned(), parts.next()?.to_owned());
    let (mut headers, mut length) = (Vec::new(), 0usize);
    loop {
        let mut h = String::new();
        r.read_line(&mut h).ok()?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let (k, v) = h.split_once(':')?;
        if k.eq_ignore_ascii_case("content-length") {
            length = v.trim().parse().ok()?;
        }
        headers.push((k.to_owned(), v.trim().to_owned()));
    }
    let mut body = vec![0u8; length];
    r.read_exact(&mut body).ok()?;
    Some((method, target, headers, body))
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let head = format!(
        "HTTP/1.1 {status} X\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

fn ack_body(dry: bool) -> String {
    let sha = "ab".repeat(32);
    format!(
        r#"{{"result":200,"data":{{"slug":"demo","dryRun":{dry},"committed":{},"refusalCode":null,"manifestSha256":"{sha}","tenantStatus":"onboarding","dataMode":"cloud","branches":[],"tables":[{{"source":"pda_config","target":"pda_config","sourceRows":2,"inserted":2,"same":0,"differing":0,"updated":false,"differingKeys":[]}}],"archived":[],"leftOut":[],"hostPrinters":[]}}}}"#,
        !dry
    )
}

impl Hub {
    fn start(agent_token: String) -> Hub {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (stop, seen) = (Arc::new(AtomicBool::new(false)), Seen::default());
        let fail_401 = Arc::new(AtomicU32::new(0));
        let (sp, sn, f4) = (stop.clone(), seen.clone(), fail_401.clone());
        let thread = std::thread::spawn(move || {
            let tokens = AtomicU32::new(0);
            for conn in listener.incoming() {
                if sp.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut stream) = conn else { continue };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let Some((method, target, headers, body)) = read_request(&stream) else {
                    continue;
                };
                if method == "POST" && target == "/sync/v1/token" {
                    if !String::from_utf8_lossy(&body).contains(&agent_token) {
                        respond(&mut stream, 401, "");
                        continue;
                    }
                    let n = tokens.fetch_add(1, Ordering::SeqCst) + 1;
                    let id = dl_uuid::Uuid::new_v4();
                    respond(
                        &mut stream,
                        200,
                        &format!(
                            r#"{{"result":200,"data":{{"accessToken":"jwt-{n}","tokenType":"Bearer","expiresIn":900,"agentId":"{id}"}}}}"#
                        ),
                    );
                    continue;
                }
                sn.lock()
                    .unwrap()
                    .push((method, target.clone(), headers.clone(), body.len()));
                if f4.load(Ordering::SeqCst) > 0 {
                    f4.fetch_sub(1, Ordering::SeqCst);
                    respond(&mut stream, 401, "");
                    continue;
                }
                let bearer = headers.iter().any(|(k, v)| {
                    k.eq_ignore_ascii_case("authorization") && v.starts_with("Bearer jwt-")
                });
                if bearer && target.starts_with("/sync/v1/legacy/import/") {
                    respond(&mut stream, 200, &ack_body(target.contains("dryRun=true")));
                } else {
                    respond(
                        &mut stream,
                        404,
                        r#"{"code":"NOPE","message":"no such route"}"#,
                    );
                }
            }
        });
        Hub {
            base: format!("http://127.0.0.1:{port}/"),
            port,
            stop,
            seen,
            fail_401,
            thread: Some(thread),
        }
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[test]
fn the_upload_is_signed_names_its_hash_and_survives_one_expired_token() {
    let tmp = Tmp::new();
    let paths = AgentPaths::new(&tmp.0);
    paths.ensure_created().unwrap();
    let secrets: SharedSecretStore = Arc::new(FileSecretStore::new(paths.secrets_dir()));
    let clock: SharedClock = Arc::new(ManualClock::new(Timestamp::from_unix_seconds(
        1_800_000_000,
    )));
    let log = Logger::new(LoggerConfig::default());
    let (key, _) = AgentKey::load_or_create(secrets.as_ref()).unwrap();
    let token = format!("agent-token-{}", dl_uuid::Uuid::new_v4().simple());
    secrets.save(names::AGENT_TOKEN, token.as_bytes()).unwrap();
    let hub = Hub::start(token);
    let http = Arc::new(
        SyncHttpClient::new(
            &hub.base,
            Duration::from_secs(30),
            Some(RequestSigner::new(Arc::new(key), clock.clone())),
            RetryPolicy::default(),
            &log,
        )
        .unwrap(),
    );
    let tokens = Arc::new(TokenProvider::new(
        http.clone(),
        secrets,
        paths,
        clock,
        &log,
    ));
    let uploader = HttpBundleUploader::new(http, tokens);

    let out = tmp.0.join("bundle");
    export_to(&out);
    let zip = BundleZip::create(&out, &tmp.0, 95 * 1024 * 1024).unwrap();

    hub.fail_401.store(1, Ordering::SeqCst);
    let answer = uploader
        .send(&zip, true, false, &Ctx::background())
        .unwrap();
    assert!(
        answer.token_refreshed,
        "the first 401 got one fresh token and one more try"
    );
    let ack = answer.ack.unwrap();
    assert!(
        ack.dry_run && !ack.committed && ack.tables.len() == 1,
        "{ack:?}"
    );

    let committed = uploader
        .send(&zip, false, true, &Ctx::background())
        .unwrap();
    assert!(committed.ack.unwrap().committed);
    let seen = hub.seen.lock().unwrap();
    let put = seen.last().unwrap();
    assert_eq!(put.0, "PUT");
    assert_eq!(
        put.1,
        format!(
            "/sync/v1/legacy/import/{}?dryRun=false&preferLegacy=true",
            zip.sha256
        )
    );
    for h in [
        "X-Sync-Timestamp",
        "X-Sync-Nonce",
        "X-Sync-Signature",
        "Authorization",
    ] {
        assert!(
            put.2.iter().any(|(k, _)| k.eq_ignore_ascii_case(h)),
            "{h} missing"
        );
    }
    assert_eq!(put.3 as u64, zip.length);
    drop(seen);

    assert!(zip.delete().is_none() && !zip.path.exists());
}

#[test]
fn a_cloud_error_answer_carries_its_code_and_a_short_message() {
    use super::upload::{parse_ack, parse_problem};
    use crate::transport::SyncResponse;

    let (code, message) = parse_problem(br#"{"code":"TENANT_ACTIVE","message":"only VietQR"}"#);
    assert_eq!(
        (code.as_deref(), message.as_deref()),
        (Some("TENANT_ACTIVE"), Some("only VietQR"))
    );
    let long = format!(r#"{{"detail":"{}"}}"#, "x".repeat(900));
    assert_eq!(
        parse_problem(long.as_bytes()).1.unwrap().chars().count(),
        503
    );
    // A proxy page is not an API answer: the status alone describes it.
    assert_eq!(parse_problem(b"<html>502</html>"), (None, None));
    // An answer without a manifest is not an import result.
    let resp = SyncResponse {
        status: 200,
        body: br#"{"result":200,"data":{"slug":"x"}}"#.to_vec(),
    };
    let e = parse_ack(&resp).unwrap_err();
    assert_eq!(e.code, codes::LEGACY_UPLOAD_FAILED);
    // Both spellings of the import result are accepted.
    let snake = SyncResponse {
        status: 200,
        body: br#"{"data":{"slug":"x","manifest_sha256":"ab","dry_run":true}}"#.to_vec(),
    };
    assert!(parse_ack(&snake).unwrap().dry_run);
}
