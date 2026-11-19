//! A fake backup hub on a local socket, and a [`Core`] wired to it with fake SQL Server
//! and destinations, for the sync, executor and module tests.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dl_json::{Value, json};

use crate::core::{Core, Settings, lock};
use crate::crypto::encrypt_credential;
use crate::fakes::{FakeDest, FakeSql, SqlState};
use crate::hub::HubClient;
use crate::report::tests::Recorder;
use crate::state::now_ms;
use crate::state::queue::Job;
use crate::sync::SyncLoop;
use crate::testutil::{Reply, Server, temp_dir};

/// The agent key the hub seals credentials under.
pub const KEY: &str = "agent-master-key-1";
/// The agent token the hub issues on self-register.
pub const TOKEN: &str = "agent-token-1";
/// The SQL password of server `s1`.
pub const PASSWORD: &str = "pw-s1-secret";

/// What the fake hub answers.
#[derive(Debug)]
pub struct HubState {
    /// `GET /config`.
    pub config: Value,
    /// `GET /master-key`.
    pub key: String,
    /// Not 200: every agent call gets this status (503 = down, 401 = rejected).
    pub status: u16,
    /// Reports the hub accepted.
    pub reports: Vec<Value>,
    /// Discover bodies.
    pub discovered: Vec<Value>,
    /// The tenant the agent belongs to.
    pub tenant: String,
    /// Backup data keys as (tenant, key id, base64url key), oldest first; a tenant's last
    /// key is its current one. None at all: an older hub, `/backup-key` is 404.
    pub backup_keys: Vec<(String, String, String)>,
}

/// The fake hub.
pub struct FakeHub {
    server: Server,
    state: Arc<Mutex<HubState>>,
}

impl FakeHub {
    /// A hub serving `config`, sealed under [`KEY`].
    pub fn start(config: Value) -> FakeHub {
        let state = Arc::new(Mutex::new(HubState {
            config,
            key: KEY.into(),
            status: 200,
            reports: Vec::new(),
            discovered: Vec::new(),
            tenant: "tenant-a".into(),
            backup_keys: Vec::new(),
        }));
        let st = state.clone();
        let server = Server::start(move |r| {
            let mut s = st.lock().unwrap();
            let path = r.path().strip_prefix("/api/agent").unwrap_or_default();
            if path == "/self-register" {
                return Reply::json(200, &json!({"agentId": "agent-1", "token": TOKEN}));
            }
            if s.status != 200 {
                return Reply::new(s.status, "hub unavailable");
            }
            if r.header("authorization") != Some(&format!("Bearer {TOKEN}")) {
                return Reply::new(401, "unknown agent");
            }
            match path {
                "/config" => Reply::json(200, &s.config),
                "/master-key" => Reply::json(200, &json!({"masterKey": s.key.as_str()})),
                "/report" => {
                    s.reports.push(r.json());
                    Reply::json(200, &json!({"ok": true}))
                }
                // Like the real hub must: only the agent's own tenant's keys, 404 otherwise.
                "/backup-key" => {
                    let want = r.query().strip_prefix("id=").map(str::to_owned);
                    let mut own = s.backup_keys.iter().filter(|k| k.0 == s.tenant);
                    let found = match &want {
                        Some(id) => own.rfind(|k| &k.1 == id),
                        None => own.next_back(),
                    };
                    match found {
                        Some((_, id, key)) => {
                            Reply::json(200, &json!({"keyId": id.as_str(), "key": key.as_str()}))
                        }
                        None => Reply::new(404, "no such key"),
                    }
                }
                "/discover" => {
                    s.discovered.push(r.json());
                    Reply::json(200, &json!({"ok": true}))
                }
                _ => Reply::new(404, "not found"),
            }
        });
        FakeHub { server, state }
    }

    /// The hub origin.
    pub fn url(&self) -> String {
        self.server.url()
    }

    /// A client holding the issued token.
    pub fn client(&self) -> HubClient {
        HubClient::new(&self.url(), TOKEN)
    }

    /// Changes what the hub answers.
    pub fn set(&self, f: impl FnOnce(&mut HubState)) {
        f(&mut self.state.lock().unwrap());
    }

    /// Reports the hub accepted, oldest first.
    pub fn reports(&self) -> Vec<Value> {
        self.state.lock().unwrap().reports.clone()
    }

    /// Discover bodies the hub accepted.
    pub fn discovered(&self) -> Vec<Value> {
        self.state.lock().unwrap().discovered.clone()
    }

    /// How many requests reached `path` (under `/api/agent`).
    pub fn count(&self, path: &str) -> usize {
        let full = format!("/api/agent{path}");
        self.server
            .requests()
            .iter()
            .filter(|r| r.path() == full)
            .count()
    }

    /// The bearer tokens sent to `path`.
    pub fn bearers(&self, path: &str) -> Vec<String> {
        let full = format!("/api/agent{path}");
        self.server
            .requests()
            .iter()
            .filter(|r| r.path() == full)
            .map(|r| r.header("authorization").unwrap_or_default().to_owned())
            .collect()
    }
}

/// `plain` sealed under [`KEY`].
pub fn seal(plain: &str) -> String {
    seal_with(plain, KEY)
}

/// `plain` sealed under `key`.
pub fn seal_with(plain: &str, key: &str) -> String {
    encrypt_credential(plain, key).unwrap()
}

/// A text field of `v`, empty when missing.
pub fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or_default()
}

/// Replaces the value at JSON `pointer` (which must exist).
pub fn put(v: &mut Value, pointer: &str, new: Value) {
    *v.pointer_mut(pointer)
        .unwrap_or_else(|| panic!("no {pointer}")) = new;
}

/// The steps of a backup report as (step, status, detail).
pub fn steps(report: &Value) -> Vec<(String, String, String)> {
    let Some(Value::Array(list)) = report.get("steps") else {
        return Vec::new();
    };
    list.iter()
        .map(|s| {
            (
                text(s, "step").into(),
                text(s, "status").into(),
                text(s, "detail").into(),
            )
        })
        .collect()
}

/// Server `s1` (host `sql1`) owning database `d1` = `Sales`.
pub fn server(id: &str, host: &str, db_id: &str, db: &str, sealed: &str) -> Value {
    json!({
        "id": id, "host": host, "port": 1433, "encryptedPassword": sealed,
        "credentialError": false, "authType": "sql",
        "databases": [{"id": db_id, "databaseName": db}],
    })
}

/// A daily full backup of `db_id` to the `s3` destination.
pub fn schedule(id: &str, db_id: &str, sealed_tokens: &str) -> Value {
    json!({
        "id": id, "databaseId": db_id, "cronExpression": "0 2 * * *",
        "timeZoneId": "Asia/Ho_Chi_Minh", "backupType": "Full",
        "retainDays": 0, "retainCount": 0,
        "cloudDestination": {"provider": "s3", "encryptedTokens": sealed_tokens,
            "endpoint": null, "credentialError": false},
    })
}

/// Destination tokens sealed under [`KEY`].
pub fn tokens() -> String {
    seal(r#"{"bucket":"backups","accessKeyId":"AKIDEXAMPLE","secretAccessKey":"s3-secret"}"#)
}

/// One server, one database, one schedule (`sc1`), nothing else.
pub fn default_config() -> Value {
    json!({
        "servers": [server("s1", "sql1", "d1", "Sales", &seal(PASSWORD))],
        "schedules": [schedule("sc1", "d1", &tokens())],
        "restoreRequests": [],
        "commands": [],
    })
}

/// A core over the fake hub, SQL Server and destinations.
pub struct Rig {
    /// The hub.
    pub hub: FakeHub,
    /// The core.
    pub core: Arc<Core>,
    /// SQL Server.
    pub sql: FakeSql,
    /// Destinations.
    pub dest: FakeDest,
    /// Problem reports and metrics.
    pub rec: Arc<Recorder>,
    /// The test's directory.
    pub dir: PathBuf,
}

/// Settings for tests: backups under `dir/bak`, fast polls.
pub fn settings(dir: &Path) -> Settings {
    let bak = dir.join("bak");
    std::fs::create_dir_all(&bak).unwrap();
    Settings {
        backup_dir: bak.display().to_string(),
        concurrency: 1,
        stop_grace: Duration::from_millis(200),
        poll: Duration::from_millis(50),
        worker_poll: Duration::from_millis(20),
    }
}

impl Rig {
    /// A rig with a hub serving `config`.
    pub fn new(tag: &str, config: Value) -> Rig {
        let hub = FakeHub::start(config);
        let client = hub.client();
        Rig::open(hub, client, temp_dir(tag), SqlState::default())
    }

    /// A rig over `client` (which may point elsewhere than `hub`).
    pub fn open(hub: FakeHub, client: HubClient, dir: PathBuf, sql: SqlState) -> Rig {
        let sql = FakeSql::new(sql);
        let dest = FakeDest::default();
        let rec = Arc::new(Recorder::default());
        let core = Core::open(
            &dir.join("module"),
            client,
            Arc::new(sql.clone()),
            Arc::new(dest.clone()),
            rec.clone(),
            settings(&dir),
        )
        .unwrap();
        Rig {
            hub,
            core: Arc::new(core),
            sql,
            dest,
            rec,
            dir,
        }
    }

    /// A sync loop over the core.
    pub fn sync(&self) -> SyncLoop {
        SyncLoop::new(self.core.clone())
    }

    /// The next queued job, leased.
    pub fn next_job(&self) -> Job {
        lock(&self.core.queue)
            .dequeue(now_ms())
            .unwrap()
            .expect("a queued job")
    }

    /// Problem codes reported so far.
    pub fn codes(&self) -> Vec<String> {
        self.rec.codes()
    }
}

/// Waits up to five seconds for `f`.
pub fn eventually(what: &str, f: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs(5);
    while !f() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}
