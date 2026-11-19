//! `CloudProbeTests`: `enroll --dry-run --url` checks DNS, TLS, HTTP reachability of
//! `/health/ready` and the clock skew.

use dl_agent_pda_sync::bootstrap::cloud_probe::{
    CloudProbe, HttpCloudProbe, ProbeFailure, ProbeResponse, READY_PATH, Resolver, Sender,
    default_resolver, default_sender,
};
use dl_agent_pda_sync::config::codes;
use dl_agent_pda_sync::config::time::{ManualClock, SharedClock, SystemClock, Timestamp};
use dl_agent_pda_sync::util::ctx::Ctx;
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, TcpListener};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const CLOUD: &str = "https://pda-cloud.example/";
const CLOUD_NOW: &str = "Fri, 13 Nov 2026 08:00:00 GMT";
const TICK: i64 = 10_000_000;

fn time() -> Arc<ManualClock> {
    Arc::new(ManualClock::new(
        Timestamp::parse_iso("2026-11-13T08:00:00Z").unwrap(),
    ))
}

fn resolves() -> Resolver {
    Arc::new(|_| Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]))
}

fn does_not_resolve() -> Resolver {
    Arc::new(|_| Err("No such host is known.".to_owned()))
}

fn answer(status: u16, date: Option<&str>) -> Sender {
    let date = date.map(str::to_owned);
    Arc::new(move |_, _| {
        Ok(ProbeResponse {
            status,
            date: date.clone(),
        })
    })
}

fn failing(f: ProbeFailure) -> Sender {
    Arc::new(move |_, _| Err(f.clone()))
}

fn probe(send: Sender, resolve: Resolver, clock: Arc<ManualClock>) -> HttpCloudProbe {
    HttpCloudProbe::new(send, resolve, clock as SharedClock)
}

fn codes_of(r: &dl_agent_pda_sync::bootstrap::cloud_probe::CloudProbeResult) -> Vec<String> {
    r.findings.iter().map(|f| f.code.clone()).collect()
}

#[test]
fn a_ready_cloud_with_a_matching_clock_has_no_findings() {
    let path = Arc::new(Mutex::new(String::new()));
    let p = path.clone();
    let send: Sender = Arc::new(move |url, _| {
        *p.lock().unwrap() = url.to_owned();
        Ok(ProbeResponse {
            status: 200,
            date: Some(CLOUD_NOW.into()),
        })
    });
    let r = probe(send, resolves(), time()).probe(CLOUD, &Ctx::background());
    assert_eq!(
        *path.lock().unwrap(),
        format!("https://pda-cloud.example{READY_PATH}")
    );
    assert!(r.findings.is_empty());
    assert!(r.ok());
    assert_eq!(r.http_status, Some(200));
    assert_eq!(r.clock_skew, Some(0));
}

#[test]
fn the_clock_skew_warns_over_two_minutes_and_fails_over_five() {
    for (ahead, code) in [
        (90i64, None),
        (-180, Some(codes::CLOCK_SKEW)),
        (180, Some(codes::CLOCK_SKEW)),
        (301, Some(codes::CLOCK_SKEW_TOO_LARGE)),
        (-360, Some(codes::CLOCK_SKEW_TOO_LARGE)),
    ] {
        let t = time();
        let start = Timestamp::parse_iso("2026-11-13T08:00:00Z").unwrap();
        t.set(Timestamp::from_unix_nanos(
            start.unix_nanos() + i128::from(ahead) * 1_000_000_000,
        ));
        let r = probe(answer(200, Some(CLOUD_NOW)), resolves(), t).probe(CLOUD, &Ctx::background());
        assert_eq!(r.clock_skew, Some(ahead * TICK), "{ahead}");
        match code {
            None => assert!(r.findings.is_empty(), "{ahead}"),
            Some(c) => assert_eq!(codes_of(&r), [c], "{ahead}"),
        }
        assert_eq!(r.ok(), code != Some(codes::CLOCK_SKEW_TOO_LARGE), "{ahead}");
    }
}

#[test]
fn no_date_header_is_a_warning_only() {
    let r = probe(answer(200, None), resolves(), time()).probe(CLOUD, &Ctx::background());
    assert!(r.ok());
    assert_eq!(codes_of(&r), [codes::CLOCK_SKEW_UNKNOWN]);
}

#[test]
fn a_cloud_that_is_not_ready_fails() {
    let r =
        probe(answer(503, Some(CLOUD_NOW)), resolves(), time()).probe(CLOUD, &Ctx::background());
    assert_eq!(r.http_status, Some(503));
    assert_eq!(codes_of(&r), [codes::CLOUD_NOT_READY]);
    assert_eq!(
        r.findings[0].message,
        "https://pda-cloud.example/health/ready answered HTTP 503; the cloud is not ready or the URL is wrong."
    );
}

#[test]
fn transport_failures_are_named() {
    for (f, code) in [
        (
            ProbeFailure::NameResolution("simulated".into()),
            codes::CLOUD_DNS_FAILED,
        ),
        (
            ProbeFailure::SecureConnection("simulated".into()),
            codes::CLOUD_TLS_FAILED,
        ),
        (
            ProbeFailure::Connection("simulated".into()),
            codes::CLOUD_UNREACHABLE,
        ),
    ] {
        let r = probe(failing(f), resolves(), time()).probe(CLOUD, &Ctx::background());
        assert_eq!(r.http_status, None);
        assert_eq!(codes_of(&r), [code]);
    }
}

#[test]
fn a_timeout_is_unreachable() {
    let r = probe(
        failing(ProbeFailure::Timeout("simulated timeout".into())),
        resolves(),
        time(),
    )
    .probe(CLOUD, &Ctx::background());
    assert_eq!(codes_of(&r), [codes::CLOUD_UNREACHABLE]);
    assert!(
        r.findings[0]
            .message
            .contains("did not answer /health/ready within 15 s")
    );
}

#[test]
fn a_name_that_does_not_resolve_is_a_dns_failure() {
    let r = probe(
        failing(ProbeFailure::Connection("simulated".into())),
        does_not_resolve(),
        time(),
    )
    .probe(CLOUD, &Ctx::background());
    assert_eq!(codes_of(&r), [codes::CLOUD_DNS_FAILED]);
}

#[test]
fn a_name_that_does_not_resolve_locally_but_works_through_a_proxy_is_a_warning() {
    let r = probe(answer(200, Some(CLOUD_NOW)), does_not_resolve(), time())
        .probe(CLOUD, &Ctx::background());
    assert!(r.ok());
    assert_eq!(r.findings.len(), 1);
    assert_eq!(r.findings[0].code, codes::CLOUD_DNS_FAILED);
    assert!(!r.findings[0].is_error());
    assert!(
        r.findings[0]
            .message
            .ends_with(" The request still succeeded through a proxy.")
    );
}

#[test]
fn an_ip_address_skips_the_name_lookup() {
    let resolved = Arc::new(AtomicBool::new(false));
    let r2 = resolved.clone();
    let resolve: Resolver = Arc::new(move |_| {
        r2.store(true, Ordering::SeqCst);
        Ok(Vec::new())
    });
    let r = probe(answer(200, Some(CLOUD_NOW)), resolve, time())
        .probe("http://127.0.0.1:5000/", &Ctx::background());
    assert!(!resolved.load(Ordering::SeqCst));
    assert!(r.ok());
}

/// A stand-in for the cloud's `/health/ready` with this machine's clock in `Date`.
fn ready_server(status: Arc<AtomicU16>) -> (String, std::thread::JoinHandle<()>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}/", l.local_addr().unwrap().port());
    let h = std::thread::spawn(move || {
        for _ in 0..2 {
            let Ok((mut s, _)) = l.accept() else { return };
            let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut first = String::new();
            let _ = r.read_line(&mut first);
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
            }
            let code = if first.starts_with(&format!("GET {READY_PATH} ")) {
                status.load(Ordering::SeqCst)
            } else {
                404
            };
            let date = dl_time::OffsetDateTime::now_utc().to_http_date();
            let _ = write!(
                s,
                "HTTP/1.1 {code} X\r\nDate: {date}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
        }
    });
    (base, h)
}

#[test]
fn the_real_http_path_reaches_the_fake_cloud() {
    let status = Arc::new(AtomicU16::new(200));
    let (base, h) = ready_server(status.clone());
    let p = HttpCloudProbe::new(default_sender(), default_resolver(), SystemClock::shared());
    let ready = p.probe(&base, &Ctx::background());
    status.store(503, Ordering::SeqCst);
    let starting = p.probe(&base, &Ctx::background());
    h.join().unwrap();

    assert!(ready.ok(), "{:?}", ready.findings);
    assert_eq!(ready.http_status, Some(200));
    assert!(ready.clock_skew.unwrap().abs() < 5 * TICK);
    assert_eq!(codes_of(&starting), [codes::CLOUD_NOT_READY]);
}

#[test]
fn an_unreachable_port_is_named_by_the_real_sender() {
    // Bind and drop: nothing listens on the port any more.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let p = HttpCloudProbe::new(default_sender(), default_resolver(), SystemClock::shared());
    let r = p.probe(&format!("http://127.0.0.1:{port}/"), &Ctx::background());
    assert_eq!(r.http_status, None);
    assert_eq!(codes_of(&r), [codes::CLOUD_UNREACHABLE]);
}
