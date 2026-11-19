use std::sync::{Arc, Mutex};

use dl_json::json;

use super::*;
use crate::testutil::{Recorded, Reply, Server, temp_dir};

const MIB: usize = 1024 * 1024;

fn pace() -> Pace {
    Pace {
        throttle_cap: std::time::Duration::ZERO,
        backoff: std::time::Duration::ZERO,
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

const FUTURE: &str = "2099-01-01T00:00:00Z";

#[derive(Default)]
struct Drive {
    base: String,
    folders: Vec<String>,
    files: Vec<(String, Vec<u8>)>,
    session: Vec<u8>,
    refreshes: u32,
    reject_token: Option<String>,
    throttle_lists: u32,
    chunk_puts: u32,
    /// The n-th chunk PUT (1-based) stores only 256 KiB and answers 503.
    break_chunk: Option<u32>,
    bad_grant: bool,
    deleted: Vec<String>,
}

fn decode_q(query: &str) -> String {
    let q = query
        .split('&')
        .find_map(|p| p.strip_prefix("q="))
        .unwrap_or("");
    String::from_utf8(dl_http::percent_decode(q).unwrap()).unwrap()
}

fn quoted(q: &str) -> String {
    let start = q.find('\'').unwrap() + 1;
    q[start..start + q[start..].find('\'').unwrap()].to_owned()
}

fn handle(st: &Mutex<Drive>, r: &Recorded) -> Reply {
    let mut d = st.lock().unwrap();
    let path = r.path().to_owned();
    if path == "/token" {
        if d.bad_grant {
            return Reply::json(
                400,
                &json!({"error": "invalid_grant", "error_description": "Token has been revoked"}),
            );
        }
        d.refreshes += 1;
        let t = format!("AT-{}", d.refreshes);
        return Reply::json(
            200,
            &json!({"access_token": t.as_str(), "expires_in": 3600}),
        );
    }
    if path.starts_with("/session/") {
        return session(&mut d, r);
    }
    let auth = r.header("authorization").unwrap_or("").to_owned();
    if d.reject_token
        .as_deref()
        .is_some_and(|t| auth.starts_with(&format!("Bearer {t}")))
    {
        return Reply::json(
            401,
            &json!({"error": {"code": 401, "message": "Invalid Credentials"}}),
        );
    }
    match (r.method.as_str(), path.as_str()) {
        ("GET", "/drive/v3/files") => {
            if d.throttle_lists > 0 {
                d.throttle_lists -= 1;
                return Reply::new(429, "").with("retry-after", "0");
            }
            let q = decode_q(r.query());
            let name = quoted(&q);
            let hit = if q.contains("mimeType") {
                d.folders.contains(&name).then(|| format!("id-{name}"))
            } else {
                d.files
                    .iter()
                    .any(|(n, _)| *n == name)
                    .then(|| format!("file-{name}"))
            };
            let files: Vec<Value> = hit.iter().map(|id| json!({"id": id.as_str()})).collect();
            Reply::json(200, &json!({"files": files}))
        }
        ("POST", "/drive/v3/files") => {
            let name = r
                .json()
                .get("name")
                .and_then(Value::as_str)
                .unwrap()
                .to_owned();
            d.folders.push(name.clone());
            Reply::json(200, &json!({"id": format!("id-{name}")}))
        }
        ("POST", "/upload/drive/v3/files") if r.query().contains("multipart") => {
            let at = |pat: &[u8], from: usize| {
                from + r.body[from..]
                    .windows(pat.len())
                    .position(|w| w == pat)
                    .unwrap()
            };
            let n0 = at(b"\"name\":\"", 0) + 8;
            let name = String::from_utf8(r.body[n0..at(b"\"", n0)].to_vec()).unwrap();
            let start = at(b"octet-stream\r\n\r\n", 0) + 16;
            let end = r.body.windows(4).rposition(|w| w == b"\r\n--").unwrap();
            d.files.push((name, r.body[start..end].to_vec()));
            Reply::json(200, &json!({"id": "new"}))
        }
        ("POST", "/upload/drive/v3/files") => {
            d.session.clear();
            let loc = format!("{}/session/1", d.base);
            Reply::json(200, &json!({})).with("location", &loc)
        }
        ("GET", p) if p.starts_with("/drive/v3/files/") => {
            let name = p.rsplit('/').next().unwrap().trim_start_matches("file-");
            let data = &d.files.iter().find(|(n, _)| n == name).unwrap().1;
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
            Reply::new(206, data[a..=b].to_vec())
                .with("content-range", &format!("bytes {a}-{b}/{}", data.len()))
        }
        ("DELETE", p) => {
            d.deleted.push(p.to_owned());
            Reply::new(204, "")
        }
        _ => Reply::new(404, ""),
    }
}

fn session(d: &mut Drive, r: &Recorded) -> Reply {
    assert!(
        r.header("authorization").is_none(),
        "session URL carries no bearer token"
    );
    let cr = r.header("content-range").unwrap();
    let total: usize = cr.rsplit('/').next().unwrap().parse().unwrap();
    let held = d.session.len();
    let range = |n: usize| {
        if n == 0 {
            Reply::new(308, "")
        } else {
            Reply::new(308, "").with("range", &format!("bytes=0-{}", n - 1))
        }
    };
    if cr.starts_with("bytes */") {
        return range(held);
    }
    let a: usize = cr["bytes ".len()..]
        .split('-')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    if a != held {
        return Reply::new(416, "");
    }
    d.chunk_puts += 1;
    if d.break_chunk == Some(d.chunk_puts) {
        d.session.extend_from_slice(&r.body[..256 * 1024]);
        return Reply::new(503, "");
    }
    d.session.extend_from_slice(&r.body);
    if d.session.len() >= total {
        let data = std::mem::take(&mut d.session);
        d.files.push(("big.bak".into(), data));
        return Reply::json(200, &json!({"id": "big"}));
    }
    let n = d.session.len();
    range(n)
}

fn fake(setup: impl FnOnce(&mut Drive)) -> (Server, Arc<Mutex<Drive>>) {
    let st = Arc::new(Mutex::new(Drive::default()));
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

fn drive(server: &Server, t: &Value, chunk: usize) -> GDrive {
    GDrive::for_test(t, &server.url(), chunk, pace()).unwrap()
}

#[test]
fn cloud_paths_parse_like_go() {
    // Go TestParseGDriveCloudPath.
    let ok = |p: &str| parse_cloud_path(p).unwrap();
    assert_eq!(
        ok("gdrive://Backups/Daily/db.bak"),
        ("Backups/Daily".into(), "db.bak".into())
    );
    assert_eq!(ok("gdrive://db.bak"), ("".into(), "db.bak".into()));
    assert_eq!(ok("gdrive:///db.bak"), ("".into(), "db.bak".into()));
    for bad in ["Backups/db.bak", "gdrive://Backups/", "gdrive://"] {
        assert!(parse_cloud_path(bad).is_err(), "{bad}");
    }
}

#[test]
fn missing_credentials_fail_like_go() {
    // Go TestUploadGoogleDrive_MissingCreds.
    let e = GDrive::from_tokens(&json!({"clientId": "a"})).unwrap_err();
    assert_eq!(
        e,
        Error::Upload(
            "Google Drive credentials incomplete (need clientId, clientSecret, refreshToken)"
                .into()
        )
    );
}

#[test]
fn small_file_goes_up_in_one_request_and_creates_folders() {
    // Go TestGoogleDriveUpload_RoundTrip / WithProgress / ExistingFolder.
    let (server, st) = fake(|_| {});
    let (f, data) = file("gd-small", "db.bak", 1000);
    let mut seen = Vec::new();
    let path = drive(&server, &tokens(FUTURE), 256 * 1024)
        .upload(&f, &CancelToken::new(), &mut |a, b| seen.push((a, b)))
        .unwrap();
    assert_eq!(path, "gdrive://Backups/SQL/db.bak");
    assert_eq!(seen.last(), Some(&(1000, 1000)));
    let d = st.lock().unwrap();
    assert_eq!(d.folders, ["Backups", "SQL"]);
    assert_eq!(d.files, [("db.bak".to_owned(), data)]);
    assert_eq!(d.refreshes, 0, "the hub's access token was still valid");
    let reqs = server.requests();
    assert!(
        reqs.iter()
            .all(|r| r.header("authorization") == Some("Bearer AT0"))
    );
}

#[test]
fn empty_remote_path_uses_the_drive_root() {
    // Go TestGoogleDriveUpload_EmptyRemotePath: path format "gdrive:///name".
    let (server, st) = fake(|_| {});
    let mut t = tokens(FUTURE);
    if let Value::Object(m) = &mut t {
        m.insert("remoteFolderPath", "");
    }
    let (f, _) = file("gd-root", "r.bak", 10);
    let path = drive(&server, &t, 256 * 1024)
        .upload(&f, &CancelToken::new(), &mut |_, _| {})
        .unwrap();
    assert_eq!(path, "gdrive:///r.bak");
    assert!(st.lock().unwrap().folders.is_empty());
}

#[test]
fn expired_token_refreshes_once_and_never_leaks() {
    let (server, st) = fake(|_| {});
    let (f, _) = file("gd-refresh", "db.bak", 10);
    let g = drive(&server, &tokens("2000-01-01T00:00:00Z"), 256 * 1024);
    g.upload(&f, &CancelToken::new(), &mut |_, _| {}).unwrap();
    assert_eq!(st.lock().unwrap().refreshes, 1);
    let reqs = server.requests();
    let form = String::from_utf8_lossy(&reqs[0].body).into_owned();
    assert!(form.contains("grant_type=refresh_token") && form.contains("client_id=cid"));
    assert!(
        reqs[1..]
            .iter()
            .all(|r| r.header("authorization") == Some("Bearer AT-1"))
    );
    assert!(!format!("{g:?}").contains("SECRET"));
}

#[test]
fn a_401_refreshes_once_and_retries() {
    let (server, st) = fake(|d| d.reject_token = Some("AT0".into()));
    let (f, _) = file("gd-401", "db.bak", 10);
    drive(&server, &tokens(FUTURE), 256 * 1024)
        .upload(&f, &CancelToken::new(), &mut |_, _| {})
        .unwrap();
    assert_eq!(st.lock().unwrap().refreshes, 1);
    // Both tokens rejected: one refresh, then the error, no loop.
    let (server, st) = fake(|d| d.reject_token = Some("AT".into()));
    let g = drive(&server, &tokens("2000-01-01T00:00:00Z"), 256 * 1024);
    let e = g
        .upload(&f, &CancelToken::new(), &mut |_, _| {})
        .unwrap_err();
    assert!(e.to_string().contains("HTTP 401"), "{e}");
    assert!(st.lock().unwrap().refreshes <= 2, "refresh loop");
}

#[test]
fn revoked_refresh_token_is_a_credential_error() {
    let (server, _st) = fake(|d| d.bad_grant = true);
    let (f, _) = file("gd-grant", "db.bak", 10);
    let e = drive(&server, &tokens("2000-01-01T00:00:00Z"), 256 * 1024)
        .upload(&f, &CancelToken::new(), &mut |_, _| {})
        .unwrap_err();
    assert_eq!(e.code(), "backup_credential_error");
    let text = e.to_string();
    assert!(
        text.contains("invalid_grant") && !text.contains("SECRET") && !text.contains("REFRESH"),
        "{text}"
    );
}

#[test]
fn throttling_waits_then_gives_up() {
    let (server, _st) = fake(|d| d.throttle_lists = 2);
    let (f, _) = file("gd-429", "db.bak", 10);
    drive(&server, &tokens(FUTURE), 256 * 1024)
        .upload(&f, &CancelToken::new(), &mut |_, _| {})
        .unwrap();
    let (server, _st) = fake(|d| d.throttle_lists = 1000);
    let e = drive(&server, &tokens(FUTURE), 256 * 1024)
        .upload(&f, &CancelToken::new(), &mut |_, _| {})
        .unwrap_err();
    assert_eq!(e.code(), "backup_upload_failed");
    assert!(e.to_string().contains("throttled"), "{e}");
}

#[test]
fn resumable_upload_resumes_an_interrupted_chunk() {
    // 5 MiB + 100 bytes in 1 MiB chunks; the third chunk dies after 256 KiB arrived.
    let (server, st) = fake(|d| d.break_chunk = Some(3));
    let (f, data) = file("gd-resume", "big.bak", 5 * MIB + 100);
    let mut seen = Vec::new();
    let path = drive(&server, &tokens(FUTURE), MIB)
        .upload(&f, &CancelToken::new(), &mut |a, b| seen.push((a, b)))
        .unwrap();
    assert_eq!(path, "gdrive://Backups/SQL/big.bak");
    let d = st.lock().unwrap();
    assert!(d.files[0].1 == data, "stored bytes differ from the file");
    assert!(
        seen.windows(2).all(|w| w[0].0 <= w[1].0),
        "progress went backwards"
    );
    assert_eq!(seen.last(), Some(&(data.len() as u64, data.len() as u64)));
    let all = server.requests();
    let puts: Vec<&Recorded> = all.iter().filter(|r| r.method == "PUT").collect();
    assert!(puts.iter().any(|r| {
        r.header("content-range")
            .is_some_and(|c| c.starts_with("bytes */"))
    }));
    assert!(puts.len() >= 7, "{} chunk requests", puts.len());
}

#[test]
fn cancel_between_chunks_stops_the_upload() {
    let (server, st) = fake(|_| {});
    let (f, _) = file("gd-cancel", "big.bak", 6 * MIB);
    let cancel = CancelToken::new();
    let c2 = cancel.clone();
    let e = drive(&server, &tokens(FUTURE), MIB)
        .upload(&f, &cancel, &mut |_, _| c2.cancel())
        .unwrap_err();
    assert_eq!(e, Error::Cancelled);
    assert_eq!(st.lock().unwrap().chunk_puts, 1);
}

#[test]
fn an_unreachable_session_error_carries_no_session_url() {
    let (server, _st) = fake(|d| d.break_chunk = Some(1));
    let (f, _) = file("gd-err", "big.bak", 6 * MIB);
    drop(server);
    let dead = crate::testutil::dead_url();
    let g = GDrive::for_test(&tokens(FUTURE), &dead, MIB, pace()).unwrap();
    let e = g
        .upload(&f, &CancelToken::new(), &mut |_, _| {})
        .unwrap_err();
    assert!(!e.to_string().contains("/session/"), "{e}");
}

#[test]
fn download_reads_ranges_and_missing_is_an_error() {
    // Go TestDownloadGoogleDrive_WritesFileContent / FileNotFound.
    let data: Vec<u8> = (0..(2 * MIB + 5)).map(|i| (i % 253) as u8).collect();
    let (server, _st) = fake(|d| {
        d.folders = vec!["Backups".into(), "SQL".into()];
        d.files.push(("db.bak".into(), data.clone()));
    });
    let g = drive(&server, &tokens(FUTURE), MIB);
    let dest = temp_dir("gd-dl").join("out.bak");
    let n = g
        .download("gdrive://Backups/SQL/db.bak", &dest, &CancelToken::new())
        .unwrap();
    assert_eq!(n as usize, data.len());
    assert!(std::fs::read(&dest).unwrap() == data);
    let e = g
        .download("gdrive://Backups/SQL/none.bak", &dest, &CancelToken::new())
        .unwrap_err();
    assert!(e.to_string().contains("not found"), "{e}");
    let e = g
        .download("gdrive://Other/none.bak", &dest, &CancelToken::new())
        .unwrap_err();
    assert!(e.to_string().contains("not found"), "{e}");
}

#[test]
fn delete_removes_the_file_and_a_missing_one_counts_as_deleted() {
    // Go TestDeleteGoogleDrive_RoundTrip / FileNotFound (Go failed; the brief says gone = done).
    let (server, st) = fake(|d| {
        d.folders = vec!["Backups".into(), "SQL".into()];
        d.files.push(("db.bak".into(), vec![1]));
    });
    let g = drive(&server, &tokens(FUTURE), MIB);
    g.delete("gdrive://Backups/SQL/db.bak").unwrap();
    assert_eq!(st.lock().unwrap().deleted, ["/drive/v3/files/file-db.bak"]);
    g.delete("gdrive://Backups/SQL/gone.bak").unwrap();
    g.delete("gdrive://Nowhere/gone.bak").unwrap();
    assert_eq!(st.lock().unwrap().deleted.len(), 1);
    assert_eq!(
        st.lock().unwrap().folders,
        ["Backups", "SQL"],
        "delete never creates folders"
    );
}
