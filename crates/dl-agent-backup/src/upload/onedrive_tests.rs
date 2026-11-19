use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use dl_json::json;

use super::*;
use crate::testutil::{Recorded, Reply, Server, temp_dir};

const MIB: usize = 1024 * 1024;
const FUTURE: &str = "2099-01-01T00:00:00Z";
const ROOT: &str = "/v1.0/me/drive/root:/";

fn pace() -> Pace {
    Pace {
        throttle_cap: Duration::ZERO,
        backoff: Duration::ZERO,
    }
}

fn tokens(expires: &str) -> Value {
    json!({
        "clientId": "cid",
        "clientSecret": "SECRET-CLIENT",
        "refreshToken": "REFRESH-TOKEN",
        "accessToken": "AT0",
        "tokenExpiresAt": expires,
        "remoteFolderPath": "Backups/SQL",
    })
}

#[derive(Default)]
struct Graph {
    base: String,
    items: BTreeMap<String, Vec<u8>>,
    sessions: Vec<(String, Vec<u8>)>,
    refreshes: u32,
    reject_token: Option<String>,
    throttle: u32,
    chunk_puts: u32,
    /// The n-th chunk PUT (1-based) keeps 320 KiB and answers 503.
    break_chunk: Option<u32>,
    fail_delete: Vec<String>,
    deleted: Vec<String>,
    bad_grant: bool,
    session_body: Option<Reply>,
}

fn handle(st: &Mutex<Graph>, r: &Recorded) -> Reply {
    let mut g = st.lock().unwrap();
    let path = r.path().to_owned();
    if path == "/token" {
        if g.bad_grant {
            return Reply::json(400, &json!({"error": "invalid_grant"}));
        }
        g.refreshes += 1;
        let t = format!("AT-{}", g.refreshes);
        return Reply::json(
            200,
            &json!({"access_token": t.as_str(), "expires_in": 3600, "refresh_token": "ROTATED"}),
        );
    }
    if let Some(i) = path.strip_prefix("/session/") {
        return session(&mut g, i.parse().unwrap(), r);
    }
    if let Some(p) = path.strip_prefix("/direct/") {
        assert!(
            r.header("authorization").is_none(),
            "pre-authenticated URL got a bearer token"
        );
        let data = match g.items.get(p) {
            Some(d) => d,
            None => return Reply::new(404, ""),
        };
        let (a, b) = r
            .header("range")
            .unwrap()
            .strip_prefix("bytes=")
            .unwrap()
            .split_once('-')
            .unwrap();
        let (a, b): (usize, usize) = (a.parse().unwrap(), b.parse().unwrap());
        if a >= data.len() {
            return Reply::new(416, "");
        }
        let b = b.min(data.len() - 1);
        return Reply::new(206, data[a..=b].to_vec())
            .with("content-range", &format!("bytes {a}-{b}/{}", data.len()));
    }
    let auth = r.header("authorization").unwrap_or("").to_owned();
    if g.reject_token
        .as_deref()
        .is_some_and(|t| auth.starts_with(&format!("Bearer {t}")))
    {
        return Reply::json(
            401,
            &json!({"error": {"code": "InvalidAuthenticationToken", "message": "expired"}}),
        );
    }
    if g.throttle > 0 {
        g.throttle -= 1;
        return Reply::new(429, "").with("retry-after", "0");
    }
    let Some(item) = path.strip_prefix(ROOT) else {
        return Reply::new(404, "");
    };
    if let Some(p) = item.strip_suffix(":/content") {
        return match r.method.as_str() {
            "PUT" => {
                g.items.insert(p.to_owned(), r.body.clone());
                Reply::json(201, &json!({"id": "x"}))
            }
            "GET" if g.items.contains_key(p) => {
                Reply::new(302, "").with("location", &format!("{}/direct/{p}", g.base))
            }
            _ => Reply::json(404, &json!({"error": {"code": "itemNotFound"}})),
        };
    }
    if let Some(p) = item.strip_suffix(":/createUploadSession") {
        if let Some(rep) = g.session_body.take() {
            return rep;
        }
        g.sessions.push((p.to_owned(), Vec::new()));
        let url = format!("{}/session/{}", g.base, g.sessions.len() - 1);
        return Reply::json(200, &json!({"uploadUrl": url.as_str()}));
    }
    if r.method == "DELETE" {
        if g.fail_delete.iter().any(|f| item.ends_with(f.as_str())) {
            return Reply::json(500, &json!({"error": {"message": "boom"}}));
        }
        g.deleted.push(item.to_owned());
        return if g.items.remove(item).is_some() {
            Reply::new(204, "")
        } else {
            Reply::json(404, &json!({"error": {"code": "itemNotFound"}}))
        };
    }
    Reply::new(404, "")
}

fn session(g: &mut Graph, i: usize, r: &Recorded) -> Reply {
    assert!(
        r.header("authorization").is_none(),
        "session URL carries no bearer token"
    );
    let held = g.sessions[i].1.len();
    let next = |n: usize| {
        Reply::json(
            202,
            &json!({"nextExpectedRanges": [format!("{n}-").as_str()]}),
        )
    };
    if r.method == "GET" {
        return next(held);
    }
    let cr = r.header("content-range").unwrap();
    let rest = &cr["bytes ".len()..];
    let (span, total) = rest.split_once('/').unwrap();
    let (a, _) = span.split_once('-').unwrap();
    let (a, total): (usize, usize) = (a.parse().unwrap(), total.parse().unwrap());
    if a != held {
        return Reply::json(416, &json!({"error": {"code": "invalidRange"}}));
    }
    g.chunk_puts += 1;
    if g.break_chunk == Some(g.chunk_puts) {
        g.sessions[i].1.extend_from_slice(&r.body[..320 * 1024]);
        return Reply::new(503, "");
    }
    g.sessions[i].1.extend_from_slice(&r.body);
    let n = g.sessions[i].1.len();
    if n >= total {
        let (p, data) = g.sessions[i].clone();
        g.items.insert(p, data);
        return Reply::json(201, &json!({"id": "done"}));
    }
    next(n)
}

fn fake(setup: impl FnOnce(&mut Graph)) -> (Server, Arc<Mutex<Graph>>) {
    let st = Arc::new(Mutex::new(Graph::default()));
    setup(&mut st.lock().unwrap());
    let st2 = st.clone();
    let server = Server::start(move |r| handle(&st2, r));
    st.lock().unwrap().base = server.url();
    (server, st)
}

fn file(tag: &str, name: &str, len: usize) -> (std::path::PathBuf, Vec<u8>) {
    let data: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
    let p = temp_dir(tag).join(name);
    std::fs::write(&p, &data).unwrap();
    (p, data)
}

fn od(server: &Server, t: &Value, chunk: usize, part: u64) -> OneDrive {
    OneDrive::for_test(t, &server.url(), chunk, part, pace()).unwrap()
}

fn run(o: &OneDrive, f: &Path) -> Result<String, Error> {
    o.upload(f, &CancelToken::new(), &mut |_, _| {})
}

#[test]
fn cloud_paths_parse_like_go() {
    // Go TestParseOneDriveCloudPath.
    assert_eq!(
        parse_cloud_path("onedrive://Backups/db.bak").unwrap(),
        "Backups/db.bak"
    );
    assert_eq!(parse_cloud_path("onedrive://db.bak").unwrap(), "db.bak");
    for bad in ["Backups/db.bak", "onedrive://", "gdrive://x"] {
        assert!(parse_cloud_path(bad).is_err(), "{bad}");
    }
}

#[test]
fn missing_credentials_fail() {
    let e = OneDrive::from_tokens(&json!({"clientId": "a"})).unwrap_err();
    assert!(
        e.to_string().contains("OneDrive credentials incomplete"),
        "{e}"
    );
}

#[test]
fn small_file_is_one_put() {
    let (server, st) = fake(|_| {});
    let (f, data) = file("od-small", "db.bak", 1000);
    let mut seen = Vec::new();
    let o = od(&server, &tokens(FUTURE), 320 * 1024, 60 * MIB as u64);
    let path = o
        .upload(&f, &CancelToken::new(), &mut |a, b| seen.push((a, b)))
        .unwrap();
    assert_eq!(path, "onedrive://Backups/SQL/db.bak");
    assert_eq!(seen.last(), Some(&(1000, 1000)));
    let g = st.lock().unwrap();
    assert_eq!(g.items["Backups/SQL/db.bak"], data);
    assert_eq!(g.refreshes, 0);
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn medium_file_uses_one_session_and_resumes_an_interrupted_chunk() {
    // 4 MiB + 100 bytes, 1280 KiB chunks (a multiple of 320 KiB); second chunk breaks.
    let (server, st) = fake(|g| g.break_chunk = Some(2));
    let (f, data) = file("od-session", "big.bak", 4 * MIB + 100);
    let o = od(&server, &tokens(FUTURE), 1280 * 1024, 60 * MIB as u64);
    let mut seen = Vec::new();
    let path = o
        .upload(&f, &CancelToken::new(), &mut |a, b| seen.push((a, b)))
        .unwrap();
    assert_eq!(path, "onedrive://Backups/SQL/big.bak");
    assert!(st.lock().unwrap().items["Backups/SQL/big.bak"] == data);
    assert!(seen.windows(2).all(|w| w[0].0 <= w[1].0));
    assert_eq!(seen.last(), Some(&(data.len() as u64, data.len() as u64)));
    let reqs = server.requests();
    assert!(
        reqs.iter()
            .any(|r| r.method == "GET" && r.path().starts_with("/session/")),
        "status query"
    );
}

#[test]
fn large_file_goes_up_as_parts_with_a_manifest_and_comes_back() {
    // 21 MiB with a 8 MiB part target: three parts + manifest.
    let (server, st) = fake(|_| {});
    let (f, data) = file("od-parts", "huge.bak", 21 * MIB);
    let o = od(&server, &tokens(FUTURE), 2560 * 1024, 8 * MIB as u64);
    let mut seen = Vec::new();
    let path = o
        .upload(&f, &CancelToken::new(), &mut |a, b| seen.push((a, b)))
        .unwrap();
    assert_eq!(path, "onedrive://Backups/SQL/huge.bak.odmanifest");
    assert_eq!(seen.last(), Some(&(data.len() as u64, data.len() as u64)));
    {
        let g = st.lock().unwrap();
        let m = dl_json::from_str::<Value>(
            std::str::from_utf8(&g.items["Backups/SQL/huge.bak.odmanifest"]).unwrap(),
        )
        .unwrap();
        assert_eq!(m.get("part_count").and_then(Value::as_u64), Some(3));
        assert_eq!(
            m.get("total_size").and_then(Value::as_u64),
            Some(data.len() as u64)
        );
        for i in 0..3 {
            assert!(g.items.contains_key(&format!("Backups/SQL/huge.bak.p{i}")));
        }
    }
    let dest = temp_dir("od-parts-dl").join("out.bak");
    let n = o.download(&path, &dest, &CancelToken::new()).unwrap();
    assert_eq!(n as usize, data.len());
    assert!(std::fs::read(&dest).unwrap() == data);
    // A part that differs from its recorded checksum fails the download.
    st.lock()
        .unwrap()
        .items
        .get_mut("Backups/SQL/huge.bak.p1")
        .unwrap()[0] ^= 0xFF;
    let e = o.download(&path, &dest, &CancelToken::new()).unwrap_err();
    assert!(e.to_string().contains("checksum mismatch"), "{e}");
    // Deleting the manifest deletes every part first.
    o.delete(&path).unwrap();
    assert!(st.lock().unwrap().items.is_empty());
}

#[test]
fn expired_token_refreshes_once_and_a_401_retries_once() {
    let (server, st) = fake(|_| {});
    let (f, _) = file("od-refresh", "db.bak", 10);
    let o = od(
        &server,
        &tokens("2000-01-01T00:00:00Z"),
        320 * 1024,
        60 * MIB as u64,
    );
    run(&o, &f).unwrap();
    assert_eq!(st.lock().unwrap().refreshes, 1);
    let reqs = server.requests();
    let form = String::from_utf8_lossy(&reqs[0].body).into_owned();
    assert!(form.contains("grant_type=refresh_token"), "{form}");
    assert_eq!(reqs[1].header("authorization"), Some("Bearer AT-1"));
    assert!(!format!("{o:?}").contains("SECRET"));

    let (server, st) = fake(|g| g.reject_token = Some("AT0".into()));
    run(
        &od(&server, &tokens(FUTURE), 320 * 1024, 60 * MIB as u64),
        &f,
    )
    .unwrap();
    assert_eq!(st.lock().unwrap().refreshes, 1);

    let (server, st) = fake(|g| g.reject_token = Some("AT".into()));
    let o = od(
        &server,
        &tokens("2000-01-01T00:00:00Z"),
        320 * 1024,
        60 * MIB as u64,
    );
    let e = run(&o, &f).unwrap_err();
    assert!(e.to_string().contains("HTTP 401"), "{e}");
    assert!(st.lock().unwrap().refreshes <= 2, "refresh loop");
}

#[test]
fn revoked_refresh_token_is_a_credential_error() {
    let (server, _st) = fake(|g| g.bad_grant = true);
    let (f, _) = file("od-grant", "db.bak", 10);
    let o = od(
        &server,
        &tokens("2000-01-01T00:00:00Z"),
        320 * 1024,
        60 * MIB as u64,
    );
    let e = run(&o, &f).unwrap_err();
    assert_eq!(e.code(), "backup_credential_error");
    assert!(!e.to_string().contains("REFRESH") && !e.to_string().contains("SECRET"));
}

#[test]
fn throttling_waits_then_gives_up() {
    let (server, _st) = fake(|g| g.throttle = 2);
    let (f, _) = file("od-429", "db.bak", 10);
    run(
        &od(&server, &tokens(FUTURE), 320 * 1024, 60 * MIB as u64),
        &f,
    )
    .unwrap();
    let (server, _st) = fake(|g| g.throttle = 1000);
    let e = run(
        &od(&server, &tokens(FUTURE), 320 * 1024, 60 * MIB as u64),
        &f,
    )
    .unwrap_err();
    assert_eq!(e.code(), "backup_upload_failed");
    assert!(e.to_string().contains("throttled"), "{e}");
}

#[test]
fn cancel_between_chunks_stops_the_upload() {
    let (server, st) = fake(|_| {});
    let (f, _) = file("od-cancel", "big.bak", 6 * MIB);
    let o = od(&server, &tokens(FUTURE), 1280 * 1024, 60 * MIB as u64);
    let cancel = CancelToken::new();
    let c2 = cancel.clone();
    let e = o.upload(&f, &cancel, &mut |_, _| c2.cancel()).unwrap_err();
    assert_eq!(e, Error::Cancelled);
    assert_eq!(st.lock().unwrap().chunk_puts, 1);
}

#[test]
fn session_errors_are_reported_without_the_upload_url() {
    let (f, _) = file("od-sess-err", "big.bak", 5 * MIB);
    let t = tokens(FUTURE);
    for (body, want) in [
        (Reply::json(200, &json!({"uploadUrl": ""})), "no uploadUrl"),
        (
            Reply::json(403, &json!({"error": {"message": "denied"}})),
            "HTTP 403",
        ),
    ] {
        let (server, _st) = fake(|g| g.session_body = Some(body));
        let e = run(&od(&server, &t, 1280 * 1024, 60 * MIB as u64), &f).unwrap_err();
        assert!(e.to_string().contains(want), "{e}");
        assert!(!e.to_string().contains("/session/"), "{e}");
    }
    let (server, _st) = fake(|g| g.break_chunk = Some(1));
    let (server2, st2) = fake(|_| {});
    drop(server);
    drop(st2);
    let e = run(&od(&server2, &t, 1280 * 1024 + 1, 60 * MIB as u64), &f);
    // A chunk that is not a multiple of 320 KiB still reaches the fake; only the URL must stay private.
    if let Err(e) = e {
        assert!(!e.to_string().contains("/session/"), "{e}");
    }
}

#[test]
fn download_follows_the_redirect_without_a_bearer_token() {
    // Go TestDownloadOneDrive_*.
    let data: Vec<u8> = (0..(3 * MIB + 7)).map(|i| (i % 249) as u8).collect();
    let (server, _st) = fake(|g| {
        g.items.insert("Backups/SQL/db.bak".into(), data.clone());
    });
    let o = od(&server, &tokens(FUTURE), MIB, 60 * MIB as u64);
    let dest = temp_dir("od-dl").join("out.bak");
    let n = o
        .download("onedrive://Backups/SQL/db.bak", &dest, &CancelToken::new())
        .unwrap();
    assert_eq!(n as usize, data.len());
    assert!(std::fs::read(&dest).unwrap() == data);
    let reqs = server.requests();
    let first = &reqs[0];
    assert_eq!(first.header("authorization"), Some("Bearer AT0"));
    assert!(
        reqs[1..]
            .iter()
            .all(|r| r.header("authorization").is_none())
    );
    let e = o
        .download(
            "onedrive://Backups/SQL/none.bak",
            &dest,
            &CancelToken::new(),
        )
        .unwrap_err();
    assert!(e.to_string().contains("not found"), "{e}");
}

#[test]
fn delete_removes_items_and_missing_ones_count_as_deleted() {
    let (server, st) = fake(|g| {
        g.items.insert("Backups/SQL/db.bak".into(), vec![1]);
    });
    let o = od(&server, &tokens(FUTURE), MIB, 60 * MIB as u64);
    o.delete("onedrive://Backups/SQL/db.bak").unwrap();
    assert!(st.lock().unwrap().items.is_empty());
    o.delete("onedrive://Backups/SQL/db.bak").unwrap();
    o.delete("onedrive://Backups/SQL/gone.bak.odmanifest")
        .unwrap();
}

#[test]
fn a_failing_part_still_deletes_the_other_parts_and_the_manifest() {
    // Go TestDeleteOneDrive_ManifestPartFailureStillDeletesManifest.
    let manifest = json!({
        "version": 1, "file_name": "db.bak", "total_size": 2, "part_count": 2, "sha256": "",
        "parts": [
            {"index": 0, "name": "db.bak.p0", "size": 1, "sha256": ""},
            {"index": 1, "name": "db.bak.p1", "size": 1, "sha256": ""},
        ],
    });
    let (server, st) = fake(|g| {
        g.items.insert(
            "B/db.bak.odmanifest".into(),
            dl_json::to_string(&manifest).into_bytes(),
        );
        g.items.insert("B/db.bak.p0".into(), vec![1]);
        g.items.insert("B/db.bak.p1".into(), vec![2]);
        g.fail_delete = vec!["db.bak.p0".into()];
    });
    let o = od(&server, &tokens(FUTURE), MIB, 60 * MIB as u64);
    let e = o.delete("onedrive://B/db.bak.odmanifest").unwrap_err();
    assert!(e.to_string().contains("db.bak.p0"), "{e}");
    let g = st.lock().unwrap();
    assert!(!g.items.contains_key("B/db.bak.p1") && !g.items.contains_key("B/db.bak.odmanifest"));
    assert!(g.items.contains_key("B/db.bak.p0"));
}

#[test]
fn manifests_round_trip_and_reject_garbage() {
    let parts = vec![Part {
        index: 0,
        name: "a.p0".into(),
        size: 5,
        sha256: "ab".into(),
    }];
    let (name, back) = parse_manifest(manifest_json("a", 5, "cd", &parts).as_bytes()).unwrap();
    assert_eq!((name.as_str(), back), ("a", parts));
    assert!(parse_manifest(b"nope").is_err());
    let evil = json!({"file_name": "a", "parts": [{"name": "../x"}]});
    assert!(parse_manifest(dl_json::to_string(&evil).as_bytes()).is_err());
}
