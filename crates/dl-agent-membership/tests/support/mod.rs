//! Hand-written fakes shared by the integration tests: a hub on a local socket (one
//! scripted reply per route, every request recorded) and a SMILE that serves fixed rows.
#![allow(dead_code)] // each test binary uses its own subset

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dl_agent_membership::folio::earn::EarnPair;
use dl_agent_membership::identity::{ForeignState, IdentityProbe, Snapshot};
use dl_agent_membership::smile::{
    AvailabilitySource, EarnCatalog, ExtraBedUsage, FolioRow, FolioSource, GroupBlock, GuestRow,
    OccupancyColumns, OccupancyFingerprint, OccupancyStay, RevenueRow, RoomBlock, RoomRow,
    RoomSource, SmileError, SmileHealth, hotel_now,
};
use dl_agent_membership::wire::{DbIdentity, RoomTypeLine};
use dl_decimal::Decimal;
use dl_time::{Date, DateTime};

/// The token the fakes hand out; no report or log line may carry it.
pub const TOKEN: &str = "agent-token-s3cr3t-0123456789";
/// The hotel the fakes enroll.
pub const HOTEL: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";

/// A fresh directory under the system temp dir, removed on drop.
pub struct TestDir(pub PathBuf);

impl TestDir {
    pub fn new(tag: &str) -> TestDir {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!(
            "dl-membership-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&p).unwrap();
        TestDir(p)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            eprintln!("could not remove {}: {e}", self.0.display());
        }
    }
}

/// Polls `cond` for up to 10 s.
pub fn eventually(what: &str, cond: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}

/// A request the fake hub received.
#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The path without the query.
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("")
    }

    /// `METHOD path`.
    pub fn line(&self) -> String {
        format!("{} {}", self.method, self.path())
    }
}

/// A scripted reply.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

type Routes = Arc<Mutex<HashMap<String, Reply>>>;

/// The hub on 127.0.0.1. Each route answers with its scripted reply (defaults: a hub that
/// takes everything); unknown routes are 404. Every response closes the connection.
pub struct FakeHub {
    addr: SocketAddr,
    routes: Routes,
    seen: Arc<Mutex<Vec<Seen>>>,
    done: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl FakeHub {
    pub fn start() -> FakeHub {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let routes: Routes = Arc::default();
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let done = Arc::new(AtomicBool::new(false));
        let (r, s, d) = (routes.clone(), seen.clone(), done.clone());
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if d.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(stream) = stream {
                    serve(stream, &r, &s);
                }
            }
        });
        let hub = FakeHub {
            addr,
            routes,
            seen,
            done,
            handle: Some(handle),
        };
        hub.reply("/api/agent/heartbeat", 200, "");
        hub.reply(
            "/api/agent/sync-config",
            200,
            r#"{"RoomInventorySyncEnabled":true,"AvailabilitySyncEnabled":true,"BaselineIdentity":null,"WriteGate":null,"ExtraBedSpecialCode":null}"#,
        );
        hub.reply("/api/agent/jobs/pull", 200, "[]");
        hub.reply("/api/agent/availability", 204, "");
        hub.reply("/api/agent/room-inventory", 204, "");
        hub.reply("/api/agent/room-types", 204, "");
        hub.reply(
            "/api/agent/folio-observations",
            200,
            r#"{"Accepted":1,"Earned":0,"Reversed":0,"Unchanged":1,"Items":[]}"#,
        );
        hub
    }

    /// `http://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Scripts `path` (no query) to answer `status` with `body`.
    pub fn reply(&self, path: &str, status: u16, body: &str) {
        self.reply_with(path, status, &[], body);
    }

    /// Scripts `path` with extra response headers.
    pub fn reply_with(&self, path: &str, status: u16, headers: &[(&str, &str)], body: &str) {
        self.routes.lock().unwrap().insert(
            path.to_owned(),
            Reply {
                status,
                headers: headers
                    .iter()
                    .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
                    .collect(),
                body: body.to_owned(),
            },
        );
    }

    pub fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    /// `METHOD path` of every request so far.
    pub fn lines(&self) -> Vec<String> {
        self.requests().iter().map(Seen::line).collect()
    }

    /// The requests to `path`.
    pub fn to(&self, path: &str) -> Vec<Seen> {
        self.requests()
            .into_iter()
            .filter(|s| s.path() == path)
            .collect()
    }
}

impl Drop for FakeHub {
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
        // Wakes the accept loop so it sees `done`; a failed connect means it already ended.
        if TcpStream::connect(self.addr).is_err() {
            eprintln!("fake hub listener already closed");
        }
        if let Some(h) = self.handle.take()
            && h.join().is_err()
        {
            eprintln!("fake hub thread panicked");
        }
    }
}

fn serve(mut stream: TcpStream, routes: &Routes, seen: &Mutex<Vec<Seen>>) {
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    let reply = routes
        .lock()
        .unwrap()
        .get(request.path())
        .cloned()
        .unwrap_or(Reply {
            status: 404,
            headers: Vec::new(),
            body: String::new(),
        });
    seen.lock().unwrap().push(request);
    let mut head = format!(
        "HTTP/1.1 {} Scripted\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        reply.body.len()
    );
    for (n, v) in &reply.headers {
        head.push_str(&format!("{n}: {v}\r\n"));
    }
    head.push_str("\r\n");
    head.push_str(&reply.body);
    if let Err(e) = stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.flush())
    {
        eprintln!("fake hub could not answer: {e}");
    }
}

/// Reads one HTTP/1.1 request (head and `Content-Length` body).
fn read_request(stream: &mut TcpStream) -> Option<Seen> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let method = first.next().unwrap_or("").to_owned();
    let target = first.next().unwrap_or("").to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_owned(), v.trim().to_owned()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end..].to_vec();
    while body.len() < length {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    Some(Seen {
        method,
        target,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

/// A SMILE database: two DLX rooms, one stay tonight, the folios the test adds. Only
/// reads exist; nothing in this fake can write.
pub struct FakeSmile {
    pub down: AtomicBool,
    pub database: Mutex<String>,
    pub folios: Mutex<Vec<FolioRow>>,
    pub calls: Mutex<Vec<&'static str>>,
}

impl Default for FakeSmile {
    fn default() -> FakeSmile {
        FakeSmile {
            down: AtomicBool::new(false),
            database: Mutex::new("SMILE_FO".into()),
            folios: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl FakeSmile {
    fn call(&self, name: &'static str) -> Result<(), SmileError> {
        self.calls.lock().unwrap().push(name);
        if self.down.load(Ordering::SeqCst) {
            Err(SmileError::Unavailable("fake SMILE is down".into()))
        } else {
            Ok(())
        }
    }

    pub fn set_down(&self, down: bool) {
        self.down.store(down, Ordering::SeqCst);
    }

    /// Another database shows up under the same connection string.
    pub fn swap_database(&self, name: &str) {
        *self.database.lock().unwrap() = name.to_owned();
    }

    pub fn identity(&self) -> DbIdentity {
        let today = hotel_now().date();
        DbIdentity {
            server: "SMILESRV\\SQL2008".into(),
            database: self.database.lock().unwrap().clone(),
            create_date: DateTime::new(today, dl_time::Time::MIDNIGHT),
            database_guid: None,
            family_guid: None,
            service_broker_guid: None,
        }
    }

    /// A checked-out folio changed an hour ago.
    pub fn add_folio(&self, folio_num: i64) {
        let changed = hotel_now()
            .checked_sub(dl_time::Duration::from_hours(1))
            .unwrap();
        let today = hotel_now().date();
        self.folios.lock().unwrap().push(FolioRow {
            folio_num: Decimal::from(folio_num),
            folio_sub_num: Decimal::ZERO,
            folio_id: folio_num * 10,
            folio_status: 3,
            check_in_time: Some(changed),
            check_out_time: Some(changed),
            last_change_time: changed,
            arrival_date: Some(DateTime::new(today, dl_time::Time::MIDNIGHT)),
            departure_date: Some(DateTime::new(today, dl_time::Time::MIDNIGHT)),
            confirm_num: None,
            external_confirm_number: None,
            cancel_num: None,
            cancel_time: None,
            no_show: false,
            room_code: Some("101".into()),
            user_info1: None,
            last_name: None,
            room_type_booked: Some("DLX".into()),
            room_type_code: Some("DLX".into()),
            num_adult: Some(2),
            num_child: Some(0),
            num_enf: None,
            extra_bed: None,
            origin_code: None,
            market_segment_code: None,
            room_rate_code: None,
        });
    }
}

impl SmileHealth for FakeSmile {
    fn ping(&self) -> Result<(), SmileError> {
        self.call("ping")
    }
}

impl IdentityProbe for FakeSmile {
    fn read_identity(&self) -> Result<Snapshot, SmileError> {
        self.call("read_identity")?;
        let now = hotel_now();
        Ok(Snapshot {
            identity: self.identity(),
            pms_now: now,
            last_foreign_change: Some(now),
            foreign_state: ForeignState::Known,
        })
    }
}

fn room(code: &str) -> RoomRow {
    RoomRow {
        room_code: code.into(),
        room_name: Some(format!("Room {code}")),
        room_type_code: "DLX".into(),
        floor: Some(1),
        sts_code: Some(1),
        ..RoomRow::default()
    }
}

impl RoomSource for FakeSmile {
    fn scan_rooms(&self) -> Result<Vec<RoomRow>, SmileError> {
        self.call("scan_rooms")?;
        Ok(vec![room("101"), room("102")])
    }

    fn room_types(&self) -> Result<Vec<RoomTypeLine>, SmileError> {
        self.call("room_types")?;
        Ok(vec![RoomTypeLine {
            pms_room_type_code: "DLX".into(),
            pms_name: Some("Deluxe".into()),
            num_room: Some(2),
            display_order: Some(1),
        }])
    }
}

impl AvailabilitySource for FakeSmile {
    fn probe_occupancy(&self, from: Date, _to: Date) -> Result<OccupancyFingerprint, SmileError> {
        self.call("probe_occupancy")?;
        Ok(OccupancyFingerprint {
            folio_count: 1,
            folio_hash: 7,
            folio_sum: 7,
            room_count: 2,
            room_hash: 3,
            pms_date: from,
            ooo_count: 0,
            ooo_hash: 0,
            oos_count: 0,
            oos_hash: 0,
            group_block_count: 0,
            group_block_hash: 0,
            group_block_sum: 0,
            hotel_date: None,
        })
    }

    fn scan_occupancy(
        &self,
        from: Date,
        _to: Date,
        _max: i64,
    ) -> Result<Vec<OccupancyStay>, SmileError> {
        self.call("scan_occupancy")?;
        Ok(vec![OccupancyStay {
            room_type_code: "DLX".into(),
            arrival: from,
            departure: from.succ(),
            group_code: None,
            room_code: Some("101".into()),
        }])
    }

    fn scan_room_blocks(&self, _: Date, _: Date) -> Result<Vec<RoomBlock>, SmileError> {
        self.call("scan_room_blocks")?;
        Ok(Vec::new())
    }

    fn scan_group_blocks(&self, _: Date, _: Date) -> Result<Vec<GroupBlock>, SmileError> {
        self.call("scan_group_blocks")?;
        Ok(Vec::new())
    }

    fn extra_bed_usage(
        &self,
        _: Date,
        _: Date,
        _: i64,
        _: &str,
    ) -> Result<ExtraBedUsage, SmileError> {
        self.call("extra_bed_usage")?;
        Ok(ExtraBedUsage::Beds(Default::default()))
    }
}

impl FolioSource for FakeSmile {
    fn earn_catalog(&self, _: &str) -> Result<EarnCatalog, SmileError> {
        self.call("earn_catalog")?;
        Ok(EarnCatalog::default())
    }

    fn room_charge_trn_since(&self, _: DateTime) -> Result<Vec<i64>, SmileError> {
        self.call("room_charge_trn_since")?;
        Ok(Vec::new())
    }

    fn occupancy_columns(&self) -> Result<OccupancyColumns, SmileError> {
        self.call("occupancy_columns")?;
        Ok(OccupancyColumns::default())
    }

    fn changed_folios(
        &self,
        watermark: DateTime,
        max: i64,
        _: OccupancyColumns,
    ) -> Result<Vec<FolioRow>, SmileError> {
        self.call("changed_folios")?;
        Ok(self
            .folios
            .lock()
            .unwrap()
            .iter()
            .filter(|f| f.last_change_time >= watermark)
            .take(usize::try_from(max).unwrap_or(usize::MAX))
            .cloned()
            .collect())
    }

    fn revenues(&self, _: &[String], _: &[EarnPair]) -> Result<Vec<RevenueRow>, SmileError> {
        self.call("revenues")?;
        Ok(Vec::new())
    }

    fn guests(&self, _: &[Decimal], _: &str) -> Result<Vec<GuestRow>, SmileError> {
        self.call("guests")?;
        Ok(Vec::new())
    }

    fn extra_bed_special_qty(
        &self,
        _: &[Decimal],
        _: &str,
        _: bool,
    ) -> Result<Vec<(Decimal, i64)>, SmileError> {
        self.call("extra_bed_special_qty")?;
        Ok(Vec::new())
    }
}
