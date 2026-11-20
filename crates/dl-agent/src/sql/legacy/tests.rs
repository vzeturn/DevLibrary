use std::path::{Path, PathBuf};

use super::*;
use crate::store::testdir::TestDir;

/// A DPAPI blob as Windows lays it out (`dwVersion` 1, `dwFlags` at 40), wrapping
/// `entropy || payload` so the fake can check the entropy.
fn blob(machine: bool, entropy: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut b = 1u32.to_le_bytes().to_vec();
    b.resize(40, 0xAB);
    let flags: u32 = if machine { 0x4 | 0x1 } else { 0x1 };
    b.extend_from_slice(&flags.to_le_bytes());
    b.extend_from_slice(entropy);
    b.extend_from_slice(payload);
    b
}

struct Fake;

impl Unseal for Fake {
    fn unseal(&self, blob: &[u8], entropy: &[u8]) -> Result<Vec<u8>, String> {
        let rest = blob.get(44..).ok_or("not a blob")?;
        rest.strip_prefix(entropy)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| "wrong entropy".into())
    }
}

struct Layout {
    _t: TestDir,
    roots: Roots,
}

fn layout() -> Layout {
    let t = TestDir::new("legacy-sql");
    let roots = Roots {
        program_data: t.0.join("ProgramData"),
        program_files: vec![t.0.join("Program Files"), t.0.join("Program Files (x86)")],
        data_dir: t.0.join("ProgramData").join("dl-agent"),
    };
    std::fs::create_dir_all(&roots.data_dir).unwrap();
    Layout { _t: t, roots }
}

fn write(path: &Path, data: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, data).unwrap();
}

fn home(roots: &Roots, key: &str) -> PathBuf {
    let p = PRODUCTS.iter().find(|p| p.key == key).unwrap();
    let (base, rel) = p.places[0];
    roots.bases(base)[0].join(rel.replace('\\', std::path::MAIN_SEPARATOR_STR))
}

/// Writes `key`'s config, in its real shape, into `dir`, pointing at server `srv`.
fn sample(key: &str, dir: &Path, srv: &str) {
    match key {
        "checkin" => write(
            &dir.join("appsettings.Production.json"),
            format!(
                "{{\n  // hotel\n  \"Agent\": {{ \"PmsConnectionString\": \"Server={srv};Database=SMILE_FO;User ID=smile;Password=p1\", }}\n}}"
            )
            .as_bytes(),
        ),
        "membership" => write(
            &dir.join("config.dpapi"),
            &blob(
                true,
                b"MM.Membership.Agent.v1",
                format!(
                    "{{\"HubBaseUrl\":\"https://hub\",\"AgentToken\":\"t\",\"SmileConnectionString\":\"Data Source={srv};Initial Catalog=SMILE_FO;User Id=mem;Password=p2\"}}"
                )
                .as_bytes(),
            ),
        ),
        "pda_sync" => {
            write(
                &dir.join("agent.json"),
                format!("{{\"sqlInstance\":\"{srv}\",\"sqlDatabase\":\"SMILE_POS\"}}").as_bytes(),
            );
            write(
                &dir.join("secrets").join("pda-sync-sql-password.bin"),
                &blob(true, b"MM.PdaOrder.SyncAgent.v1", b"p\"3"),
            );
        }
        "pms" => write(
            &dir.join("state").join("databases.json"),
            format!(
                "{{\"SMILE_FO\":\"Server={srv};Database=SMILE_FO;User ID=pms;Password=p4\",\"MM_PMS\":\"Server=cloud\"}}"
            )
            .as_bytes(),
        ),
        "hotellink" => {
            let pw = dl_encoding::base64::STANDARD.encode(blob(true, b"", b"p5"));
            write(
                &dir.join("config.json"),
                format!(
                    "{{\"hubUrl\":\"https://hl\",\"sql\":{{\"server\":\"{srv}\",\"database\":\"SMILE_FO\",\"trustedConnection\":false,\"user\":\"hl\",\"password\":\"enc:{pw}\"}}}}"
                )
                .as_bytes(),
            );
        }
        "einvoice" => {
            let json = format!(
                "{{\"api_key\":\"k\",\"sql_dsn\":\"sqlserver://ei:p6@{srv}?database=SMILE_FO\",\"pos_sql_dsn\":\"\"}}"
            );
            let mut framed = (json.len() as u32).to_be_bytes().to_vec();
            framed.extend_from_slice(json.as_bytes());
            write(&dir.join("config.enc"), &blob(true, b"", &framed));
        }
        "srbackup" => write(
            &dir.join("agent.json"),
            format!(
                "{{\"server_url\":\"https://b\",\"api_key\":\"enc:v1:x\",\"sql\":{{\"connection_string\":\"Server={srv};User ID=bk;Password=p7\"}}}}"
            )
            .as_bytes(),
        ),
        _ => unreachable!(),
    }
}

fn parsed(f: &Found) -> crate::cli_install_sql::Saved {
    crate::cli_install_sql::legacy_saved(&f.connection, f.label).expect(f.product)
}

#[test]
fn reads_all_seven_agents_in_place() {
    let l = layout();
    for p in &PRODUCTS {
        sample(p.key, &home(&l.roots, p.key), &format!("srv-{}", p.key));
    }
    let s = scan(&l.roots, &Fake);
    let keys: Vec<&str> = s.found.iter().map(|f| f.product).collect();
    assert_eq!(
        keys,
        [
            "checkin",
            "membership",
            "pda_sync",
            "pms",
            "hotellink",
            "einvoice",
            "srbackup"
        ]
    );
    for f in &s.found {
        assert_eq!(f.location, Location::InPlace);
        let saved = parsed(f);
        assert_eq!(saved.server, format!("srv-{}", f.product), "{}", f.product);
        assert_eq!(saved.auth, "sql");
        assert!(saved.password.is_some(), "{}", f.product);
    }
    let pda = s.found.iter().find(|f| f.product == "pda_sync").unwrap();
    assert_eq!(parsed(pda).user, "pda_sync");
    assert_eq!(parsed(pda).password.as_deref(), Some("p\"3"));
    assert_eq!(parsed(&s.found[5]).password.as_deref(), Some("p6"));
    assert!(s.warnings.is_empty(), "{:?}", s.warnings);
    assert!(!format!("{:?}", s.found).contains("p1"));
}

#[test]
fn reads_all_seven_agents_renamed_by_the_bench() {
    let l = layout();
    for p in &PRODUCTS {
        let h = home(&l.roots, p.key);
        let old = PathBuf::from(format!("{}.bench-old-20261119101500", h.display()));
        sample(p.key, &old, &format!("old-{}", p.key));
    }
    let s = scan(&l.roots, &Fake);
    assert_eq!(s.found.len(), 7, "{:?}", s.warnings);
    for f in &s.found {
        assert_eq!(f.location, Location::BenchOld);
        assert_eq!(parsed(f).server, format!("old-{}", f.product));
    }
}

#[test]
fn reads_all_seven_agents_listed_in_a_bench_record() {
    let l = layout();
    let mut dirs = Vec::new();
    for p in &PRODUCTS {
        let h = home(&l.roots, p.key);
        let moved = l.roots.data_dir.join("moved").join(p.key);
        sample(p.key, &moved, &format!("rec-{}", p.key));
        dirs.push(format!(
            "{{\"from\":{},\"to\":{}}}",
            dl_json::to_string(&Value::from(h.display().to_string())),
            dl_json::to_string(&Value::from(moved.display().to_string()))
        ));
    }
    write(
        &l.roots.data_dir.join("bench-removed-20261119101500.json"),
        format!(
            "{{\"removed_at\":\"x\",\"services\":[],\"tasks\":[],\"dirs\":[{}]}}",
            dirs.join(",")
        )
        .as_bytes(),
    );
    let s = scan(&l.roots, &Fake);
    assert_eq!(s.found.len(), 7, "{:?}", s.warnings);
    for f in &s.found {
        assert_eq!(f.location, Location::BenchRecord);
        assert_eq!(parsed(f).server, format!("rec-{}", f.product));
    }
}

#[test]
fn in_place_comes_before_what_the_bench_moved_aside() {
    let l = layout();
    let h = home(&l.roots, "einvoice");
    sample(
        "einvoice",
        &PathBuf::from(format!("{}.bench-old-1", h.display())),
        "old",
    );
    sample("checkin", &home(&l.roots, "checkin"), "live");
    let s = scan(&l.roots, &Fake);
    let order: Vec<(&str, Location)> = s.found.iter().map(|f| (f.product, f.location)).collect();
    assert_eq!(
        order,
        [
            ("checkin", Location::InPlace),
            ("einvoice", Location::BenchOld)
        ]
    );
}

#[test]
fn a_current_user_blob_is_skipped_with_a_warning() {
    let l = layout();
    let dir = home(&l.roots, "membership");
    write(
        &dir.join("config.dpapi"),
        &blob(
            false,
            b"MM.Membership.Agent.v1",
            b"{\"SmileConnectionString\":\"Server=x;User ID=u;Password=secret\"}",
        ),
    );
    let s = scan(&l.roots, &Fake);
    assert!(s.found.is_empty());
    assert_eq!(s.warnings.len(), 1);
    assert!(s.warnings[0].contains("CurrentUser"), "{}", s.warnings[0]);
    assert!(!s.warnings[0].contains("secret"));
    assert_eq!(machine_scope(&blob(true, b"", b"")), Some(true));
    assert_eq!(machine_scope(&blob(false, b"", b"")), Some(false));
    assert_eq!(machine_scope(b"not dpapi"), None);
}

#[test]
fn an_undecryptable_secret_is_a_warning_not_a_failure() {
    let l = layout();
    let dir = home(&l.roots, "pda_sync");
    write(
        &dir.join("agent.json"),
        b"{\"sqlInstance\":\".\",\"sqlDatabase\":\"SMILE_POS\"}",
    );
    write(
        &dir.join("secrets").join("pda-sync-sql-password.bin"),
        &blob(true, b"another entropy", b"pw"),
    );
    sample("checkin", &home(&l.roots, "checkin"), "srv");
    let s = scan(&l.roots, &Fake);
    assert_eq!(s.found.len(), 1);
    assert!(
        s.warnings[0].contains("cannot be decrypted"),
        "{:?}",
        s.warnings
    );
}

#[test]
fn srbackups_real_config_has_no_sql_and_says_so() {
    let l = layout();
    write(
        &home(&l.roots, "srbackup").join("agent.json"),
        b"{\"server_url\":\"https://b\",\"agent_id\":\"a\",\"api_key\":\"enc:v1:x\",\"master_key\":\"enc:v1:y\"}",
    );
    let s = scan(&l.roots, &Fake);
    assert!(s.found.is_empty());
    assert!(s.warnings[0].contains("keeps no SQL connection"));
}

#[test]
fn hotellink_windows_login_and_override() {
    let l = layout();
    let dir = home(&l.roots, "hotellink");
    write(
        &dir.join("config.json"),
        b"{\"sql\":{\"server\":\".\\\\SMILE\",\"database\":\"SMILE_FO\",\"trustedConnection\":true}}",
    );
    let s = scan(&l.roots, &Fake);
    let saved = parsed(&s.found[0]);
    assert_eq!(
        (saved.auth.as_str(), saved.server.as_str()),
        ("windows", "localhost\\SMILE")
    );
    let over = dl_encoding::base64::STANDARD.encode(blob(
        true,
        b"",
        b"Server=db,1444;User ID=o;Password=q",
    ));
    write(
        &dir.join("config.json"),
        format!("{{\"sql\":{{\"server\":\"x\",\"connectionStringOverride\":\"enc:{over}\"}}}}")
            .as_bytes(),
    );
    let s = scan(&l.roots, &Fake);
    assert_eq!(parsed(&s.found[0]).server, "db,1444");
}

#[test]
fn jsonc_comments_and_trailing_commas_parse() {
    let v = formats::parse_jsonc("{ /* a */ \"a\": \"x//y\", // b\n \"b\": [1,2,], }").unwrap();
    assert_eq!(v["a"].as_str(), Some("x//y"));
    assert_eq!(v["b"].as_array().map(Vec::len), Some(2));
}
