//! End to end against a real SQL Server (gate `HL_AGENT_TEST_SQL`, the same string as the
//! `pms::sql_tests`, with `database=master`): a fake HL hub serves jobs over the module's
//! long poll, the real module (`run_configured`) with the real `SqlRepo` and reconciler
//! writes `SMILE_FO_E2E`, and the hub records every ack and mapping push.
//!
//! The module reaches SQL Server through [`Proxy`], a TCP relay the tests switch off to
//! play "SQL down" (connections refused and the live ones cut) and count the connection
//! attempts. Its connection string is the site spelling
//! `…;Encrypt=false;TrustServerCertificate=true;`. Without the gate the tests return early.

mod tests;

use std::io;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dl_agent::supervisor::ModuleContext;
use dl_json::Value;
use dl_tds::{Client, Config, Param};

use crate::module::{NAME, Wiring, run_configured, usable};
use crate::run::Timings;
use crate::testhub::{FakeHub, Req};

const ENV: &str = "HL_AGENT_TEST_SQL";
/// The test database (not `SMILE_FO`, so `pms::sql_tests` can run at the same time).
pub(crate) const DB: &str = "SMILE_FO_E2E";
const SCHEMA: &str = include_str!("../../tests/fixtures/smile-schema.sql");

/// One e2e test at a time: they share the database.
static SERIAL: Mutex<()> = Mutex::new(());

/// The gate's server and an admin session on [`DB`] (direct, not through the proxy).
pub(crate) struct Sql {
    pub upstream: SocketAddr,
    pub user: String,
    pub password: String,
    pub admin: Client,
    _serial: MutexGuard<'static, ()>,
}

/// `None` (skip) without the gate; otherwise [`DB`] created, the schema applied and the Go
/// e2e seed (`seedSmile`) loaded.
pub(crate) fn sql() -> Option<Sql> {
    let raw = std::env::var(ENV).ok().filter(|v| !v.is_empty())?;
    let serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let cfg: Config = raw.parse().expect("HL_AGENT_TEST_SQL parses");
    let mut master = Client::connect(&cfg).expect("connect master");
    master
        .execute(
            &format!("IF DB_ID('{DB}') IS NULL CREATE DATABASE {DB}"),
            &[],
        )
        .expect("create db");
    let mut admin = Client::connect(&cfg.clone().with_database(DB)).expect("connect db");
    apply_schema(&mut admin);
    seed(&mut admin);
    let upstream = (cfg.host(), cfg.port().unwrap_or(1433))
        .to_socket_addrs()
        .expect("resolve")
        .next()
        .expect("an address");
    Some(Sql {
        upstream,
        user: cfg.user().to_owned(),
        password: cfg.password().to_owned(),
        admin,
        _serial: serial,
    })
}

fn apply_schema(c: &mut Client) {
    let mut batch = String::new();
    let mut batches = Vec::new();
    for line in SCHEMA.lines() {
        if line.trim().eq_ignore_ascii_case("GO") {
            batches.push(std::mem::take(&mut batch));
        } else {
            batch.push_str(line);
            batch.push('\n');
        }
    }
    batches.push(batch);
    for b in batches {
        let b = b.trim();
        if b.is_empty()
            || b.contains("CREATE DATABASE")
            || b.to_ascii_uppercase().starts_with("USE ")
        {
            continue;
        }
        c.execute(b, &[])
            .unwrap_or_else(|e| panic!("schema batch failed: {e}\n{b}"));
    }
}

/// Go `e2e_test.go` `seedSmile`.
fn seed(c: &mut Client) {
    for q in [
        "DELETE FROM Folio",
        "DELETE FROM FolioRate",
        "DELETE FROM AdditionalName",
        "DELETE FROM CLIENT",
        "DELETE FROM ClientContacts",
        "DELETE FROM ClientSts",
        "DELETE FROM RoomTypeMapped",
        "DELETE FROM GlobalParameter",
        "DELETE FROM AvailabilitySeed",
        "INSERT INTO CLIENT VALUES (77, 'Booking.com')",
        "INSERT INTO ClientSts (ClientCode, Origin, MSegment) VALUES (77, 'OTA', 'ONLINE')",
        "INSERT INTO RoomTypeMapped VALUES ('r-100', 'DLX01', NULL, NULL, 1), ('r-100', 'DLX01', NULL, NULL, 0)",
        "INSERT INTO GlobalParameter VALUES ('HLInventorySync', '1')",
        "INSERT INTO AvailabilitySeed (RoomTypeCode, sDate, Definite, NumRoom) VALUES \
         ('DLX01', CAST(GETDATE() AS DATE), 3, 10), ('DLX01', DATEADD(DAY,1,CAST(GETDATE() AS DATE)), 0, 10)",
    ] {
        c.execute(q, &[])
            .unwrap_or_else(|e| panic!("seed {q:?}: {e}"));
    }
}

impl Sql {
    /// `SELECT COUNT(*)` (or any one-int query) with `@p1`.
    pub fn count(&mut self, sql: &str, p1: &str) -> i64 {
        let r = self
            .admin
            .query(sql, &[Param::new("p1", p1)])
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
        let set = r.first().expect("a result set");
        let row = set.rows().first().expect("one row");
        row.get::<i32>(0).map(i64::from).expect("an int")
    }

    /// The live folio numbers of a booker.
    pub fn folio_nums(&mut self, booker: &str) -> Vec<f64> {
        let r = self
            .admin
            .query(
                "SELECT FolioNum FROM Folio WHERE Booker = @p1 AND ISNULL(CancelNum, 0) = 0 ORDER BY FolioNum",
                &[Param::new("p1", booker)],
            )
            .expect("folio query");
        r.first()
            .map(|s| {
                s.rows()
                    .iter()
                    .map(|r| r.get::<f64>(0).expect("FolioNum"))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The site connection string through `proxy`.
    pub fn site_string(&self, proxy: &Proxy) -> String {
        format!(
            "Server=127.0.0.1,{};Database={DB};User Id={};Password=\"{}\";Encrypt=false;TrustServerCertificate=true;",
            proxy.port,
            self.user,
            self.password.replace('"', "\"\"")
        )
    }
}

/// A TCP relay to SQL Server that can be switched off.
pub(crate) struct Proxy {
    pub port: u16,
    up: Arc<AtomicBool>,
    attempts: Arc<AtomicUsize>,
    live: Arc<Mutex<Vec<TcpStream>>>,
}

fn pump(mut from: TcpStream, mut to: TcpStream) {
    if let Err(e) = io::copy(&mut from, &mut to) {
        // A cut link (down()) ends the copy with one of these.
        if !matches!(
            e.kind(),
            io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::NotConnected
        ) {
            eprintln!("proxy copy: {e}");
        }
    }
    for s in [&from, &to] {
        if let Err(e) = s.shutdown(Shutdown::Both)
            && e.kind() != io::ErrorKind::NotConnected
        {
            eprintln!("proxy shutdown: {e}");
        }
    }
}

impl Proxy {
    pub fn start(upstream: SocketAddr) -> Proxy {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind proxy");
        let port = l.local_addr().expect("addr").port();
        let up = Arc::new(AtomicBool::new(true));
        let attempts = Arc::new(AtomicUsize::new(0));
        let live: Arc<Mutex<Vec<TcpStream>>> = Arc::new(Mutex::new(Vec::new()));
        let (u, a, lv) = (up.clone(), attempts.clone(), live.clone());
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(client) = s else { continue };
                a.fetch_add(1, Ordering::SeqCst);
                if !u.load(Ordering::SeqCst) {
                    drop(client);
                    continue;
                }
                let Ok(server) = TcpStream::connect(upstream) else {
                    continue;
                };
                let clones = (client.try_clone(), server.try_clone());
                let (Ok(c2), Ok(s2)) = clones else { continue };
                if let (Ok(c3), Ok(s3)) = (client.try_clone(), server.try_clone()) {
                    let mut g = lv.lock().unwrap_or_else(|p| p.into_inner());
                    g.push(c3);
                    g.push(s3);
                }
                std::thread::spawn(move || pump(client, s2));
                std::thread::spawn(move || pump(server, c2));
            }
        });
        Proxy {
            port,
            up,
            attempts,
            live,
        }
    }

    /// SQL down: new connections are closed at once and the live ones cut.
    pub fn down(&self) {
        self.up.store(false, Ordering::SeqCst);
        for s in self
            .live
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain(..)
        {
            if let Err(e) = s.shutdown(Shutdown::Both)
                && e.kind() != io::ErrorKind::NotConnected
            {
                eprintln!("proxy cut: {e}");
            }
        }
    }

    /// SQL back.
    pub fn up(&self) {
        self.up.store(true, Ordering::SeqCst);
    }

    /// Connections the module opened (or tried to) so far.
    pub fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }
}

/// The fake hub's state.
#[derive(Default)]
pub(crate) struct HubSt {
    /// Served on every poll until an ack for it is delivered.
    pub job: Option<String>,
    pub job_id: String,
    /// The first delivered ack does not retire the job: it is served again (a redelivery).
    pub redeliver_once: bool,
    /// Mapping commands served until acked.
    pub mapping_cmds: Vec<(String, String)>,
    /// Ack requests answer only when this is false (a held ack, then a dropped client).
    pub hold_acks: bool,
    /// Bodies of the acks the hub answered (delivered).
    pub acks: Vec<String>,
    /// Bodies of acks received while held (the client may be gone).
    pub held_acks: Vec<String>,
    pub map_cmd_acks: Vec<String>,
    /// (`rows` count, `hash`) per mapping push.
    pub mappings: Vec<(usize, String)>,
    pub availability_pushes: usize,
    pub polls: usize,
    /// `enabled` in `/api/agent/config` (the availability push).
    pub availability: bool,
}

pub(crate) struct Hub {
    pub fake: FakeHub,
    st: Arc<Mutex<HubSt>>,
}

impl Hub {
    pub fn start() -> Hub {
        let st: Arc<Mutex<HubSt>> = Arc::default();
        let h = st.clone();
        Hub {
            fake: FakeHub::start(move |r| handle(&h, r)),
            st,
        }
    }

    pub fn st(&self) -> MutexGuard<'_, HubSt> {
        self.st.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.st().hold_acks = false;
    }
}

fn lock(st: &Mutex<HubSt>) -> MutexGuard<'_, HubSt> {
    st.lock().unwrap_or_else(|p| p.into_inner())
}

fn handle(st: &Mutex<HubSt>, r: &Req) -> (u16, String) {
    let path = r.path.split('?').next().unwrap_or("");
    if r.method == "GET" && path == "/api/agent/jobs" {
        let (jobs, cmds) = {
            let mut s = lock(st);
            s.polls += 1;
            let jobs = s.job.clone().into_iter().collect::<Vec<_>>();
            let cmds: Vec<String> = s.mapping_cmds.iter().map(|(_, j)| j.clone()).collect();
            (jobs, cmds)
        };
        if jobs.is_empty() && cmds.is_empty() {
            // The long poll's wait, shortened.
            std::thread::sleep(Duration::from_millis(100));
        }
        return (
            200,
            format!(
                r#"{{"jobs":[{}],"wantMapping":false,"mappingCmds":[{}]}}"#,
                jobs.join(","),
                cmds.join(",")
            ),
        );
    }
    if let Some(rest) = path.strip_prefix("/api/agent/jobs/") {
        let id = rest.trim_end_matches("/ack").to_owned();
        if lock(st).hold_acks {
            lock(st).held_acks.push(r.body.clone());
            let end = Instant::now() + Duration::from_secs(30);
            while lock(st).hold_acks && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(20));
            }
            // Released: the client that sent it is gone; this answer is lost.
            return (200, r#"{"ok":true}"#.into());
        }
        let mut s = lock(st);
        s.acks.push(r.body.clone());
        if s.job_id == id {
            if s.redeliver_once {
                s.redeliver_once = false;
            } else {
                s.job = None;
            }
        }
        return (200, r#"{"ok":true}"#.into());
    }
    if let Some(rest) = path.strip_prefix("/api/agent/mapping-cmds/") {
        let id = rest.trim_end_matches("/ack").to_owned();
        let mut s = lock(st);
        s.map_cmd_acks.push(r.body.clone());
        s.mapping_cmds.retain(|(c, _)| *c != id);
        return (200, r#"{"ok":true}"#.into());
    }
    let body = dl_json::parse(&r.body).unwrap_or(Value::Null);
    match path {
        "/api/agent/mapping" => {
            let n = body
                .get("rows")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            let hash = body
                .get("hash")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            lock(st).mappings.push((n, hash));
        }
        "/api/agent/availability" => lock(st).availability_pushes += 1,
        "/api/agent/config" => {
            let on = lock(st).availability;
            return (
                200,
                format!(r#"{{"enabled":{on},"availabilityIntervalMin":1,"availabilityDays":3}}"#),
            );
        }
        _ => {}
    }
    (200, r#"{"ok":true}"#.into())
}

/// A booking job for the seeded hotel: one DLX01 room, two nights from `check_in`.
pub(crate) fn booking_job(job_id: &str, booker: &str, check_in: dl_time::Date) -> String {
    let d1 = check_in;
    let d2 = check_in.add_days(1);
    let out = check_in.add_days(2);
    format!(
        r#"{{"jobId":"{job_id}","hotelId":"h1","action":"save","bookingId":"B-{booker}","booker":"{booker}","sourceName":"Booking.com","hlStatus":"Confirmed","bookingDate":"2026-06-01 10:00:00","checkIn":"{d1}","checkOut":"{out}","guestName":"NGUYEN VAN DUC","guestPhone":"0901","guestEmail":"a@b.c","bookStatus":1,"paymentCode":"","rooms":[{{"itemId":"it-{booker}","roomTypeCode":"DLX01","quantity":1,"adults":2,"children":0,"includeBf":true,"breakfastCode":"ABU","rates":[{{"date":"{d1}","rate":1500000}},{{"date":"{d2}","rate":1500000}}]}}],"attempts":0,"enqueuedAt":"2026-06-10T08:30:00Z"}}"#
    )
}

/// A check-in date a month from today (local), so the past-booking skip never applies.
pub(crate) fn future_day() -> dl_time::Date {
    dl_time::OffsetDateTime::now_utc()
        .to_offset(dl_time::Offset::VIETNAM)
        .date()
        .add_days(30)
}

/// The e2e waits: short backoffs, a 3 s breaker cooldown.
pub(crate) fn timings() -> Timings {
    Timings {
        poll_wait_sec: 0,
        backoff_start: Duration::from_millis(50),
        backoff_max: Duration::from_millis(200),
        heartbeat: Duration::from_secs(1),
        cfg_refresh: Duration::from_secs(300),
        supervise: Duration::from_millis(50),
        avail_interval: |_| Duration::from_secs(1),
        breaker_threshold: 5,
        breaker_cooldown: Duration::from_secs(3),
        breaker_max: Duration::from_secs(10),
        breaker_poll_wait: Duration::from_millis(200),
    }
}

/// A running module (stopped on drop).
pub(crate) struct Running {
    pub ctx: ModuleContext,
    h: Option<JoinHandle<Result<(), dl_agent::Error>>>,
}

impl Running {
    /// Stops the module and returns how long `run_configured` took to return.
    pub fn stop(&mut self) -> Duration {
        let t = Instant::now();
        self.ctx.stop().stop();
        if let Some(h) = self.h.take() {
            h.join().expect("module thread").expect("module result");
        }
        t.elapsed()
    }

    pub fn metric(&self, k: &str) -> i64 {
        self.ctx
            .status()
            .metrics
            .get(k)
            .and_then(Value::as_i64)
            .unwrap_or(-1)
    }

    pub fn metric_str(&self, k: &str) -> String {
        self.ctx
            .status()
            .metrics
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if self.h.is_some() && !std::thread::panicking() {
            self.stop();
        }
    }
}

/// Writes the Go agent's `config.json` (site connection string through the proxy) in `dir`
/// and runs the real module on it; the module's data dir is `dir/data`.
pub(crate) fn start_module(dir: &Path, hub_url: &str, conn: &str) -> Running {
    let path: PathBuf = dir.join("config.json");
    let mut sql = dl_json::Map::new();
    sql.insert("connectionStringOverride", conn);
    let mut root = dl_json::Map::new();
    root.insert("hubUrl", hub_url);
    root.insert("hotelId", "h1");
    root.insert("token", "tok-e2e");
    root.insert("instanceId", "i-e2e");
    root.insert("sql", Value::Object(sql));
    std::fs::write(&path, dl_json::to_string(&Value::Object(root))).expect("write config");
    let (cfg, repo) = usable(&path).expect("usable config");
    let ctx = ModuleContext::standalone(NAME, Value::Null, dir.join("data"));
    let c = ctx.clone();
    let h = std::thread::spawn(move || {
        let w = Wiring {
            repo,
            timings: timings(),
            grace: Duration::from_secs(5),
        };
        run_configured(&c, &cfg, &path, w)
    });
    Running { ctx, h: Some(h) }
}

/// Polls `cond` every 50 ms for up to `secs`.
pub(crate) fn wait(secs: u64, what: &str, mut cond: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out after {secs} s waiting for {what}");
}
