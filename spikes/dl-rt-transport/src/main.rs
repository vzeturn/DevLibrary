//! `rt-bench`: QUIC vs WebSocket+TLS latency and throughput on loopback and through the
//! delay/loss shim. `cargo run --release -- [--quick]`. Prints a Markdown table.

use dl_rt_transport::link::Link;
use dl_rt_transport::shim::{self, Impairment, Shim};
use dl_rt_transport::stats::{Summary, ms, summarize};
use dl_rt_transport::tlsconf::TestPki;
use dl_rt_transport::{Result, quic, workload, ws};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::time::Instant;

/// One small capture batch / command.
const PING_SIZE: usize = 1024;
/// One bulk frame.
const BULK_SIZE: usize = 16 * 1024;
/// Bulk frames in flight for the throughput run.
const TPUT_WINDOW: u64 = 64;
/// Bulk frames in flight while pings are measured (64 KiB, about 10 ms at 50 Mbit/s).
const MIX_WINDOW: u64 = 4;

#[derive(Clone, Copy)]
struct Plan {
    connects: usize,
    pings: u64,
    frames: u64,
    loaded: u64,
}

struct Row {
    transport: &'static str,
    scenario: &'static str,
    connect: Summary,
    rtt: Summary,
    loaded: Summary,
    mbps: f64,
    modelled: bool,
}

fn summary(s: &[Duration]) -> Result<Summary> {
    summarize(s).ok_or_else(|| "no samples".into())
}

async fn shimmed(
    imp: Option<Impairment>,
    server: SocketAddr,
    udp: bool,
) -> Result<(Option<Shim>, SocketAddr)> {
    let Some(imp) = imp else {
        return Ok((None, server));
    };
    let s = if udp {
        shim::udp(server, imp).await?
    } else {
        shim::tcp(server, imp).await?
    };
    let addr = s.addr;
    Ok((Some(s), addr))
}

async fn measure(link: Link, plan: Plan) -> Result<(Vec<Duration>, Duration, Vec<Duration>)> {
    let Link {
        mut control,
        mut bulk,
        tasks,
    } = link;
    let rtt = workload::ping(&mut control, plan.pings, PING_SIZE).await?;
    let tput = workload::throughput(&mut bulk, plan.frames, BULK_SIZE, TPUT_WINDOW).await?;
    let (loaded, _bytes, bulk) = workload::ping_under_bulk(
        &mut control,
        bulk,
        plan.loaded,
        PING_SIZE,
        BULK_SIZE,
        MIX_WINDOW,
    )
    .await?;
    Link {
        control,
        bulk,
        tasks,
    }
    .close()
    .await?;
    Ok((rtt, tput, loaded))
}

async fn run_quic(
    pki: &TestPki,
    scenario: &'static str,
    imp: Option<Impairment>,
    plan: Plan,
) -> Result<Row> {
    let server = quic::serve(pki).await?;
    let (_shim, target) = shimmed(imp, server.addr, true).await?;
    let client = quic::Client::new(pki)?;
    let mut connects = Vec::new();
    for _ in 0..plan.connects {
        let t = Instant::now();
        let c = client.connect(target).await?;
        connects.push(t.elapsed());
        c.close(0u32.into(), b"probe");
    }
    let conn = client.connect(target).await?;
    let link = client.open_link(conn).await?;
    let (rtt, tput, loaded) = measure(link, plan).await?;
    row(
        "quic", scenario, &connects, &rtt, &loaded, tput, plan, false,
    )
}

async fn run_ws(
    pki: &TestPki,
    scenario: &'static str,
    imp: Option<Impairment>,
    plan: Plan,
) -> Result<Row> {
    let server = ws::serve(pki).await?;
    let (_shim, target) = shimmed(imp, server.addr, false).await?;
    let client = ws::Client::new(pki)?;
    let mut connects = Vec::new();
    for _ in 0..plan.connects {
        let t = Instant::now();
        let link = client.connect(target).await?;
        connects.push(t.elapsed());
        link.close().await?;
    }
    let link = client.connect(target).await?;
    let (rtt, tput, loaded) = measure(link, plan).await?;
    let lossy = imp.is_some_and(|i| i.loss > 0.0);
    row(
        "ws+tls", scenario, &connects, &rtt, &loaded, tput, plan, lossy,
    )
}

#[allow(clippy::too_many_arguments)]
fn row(
    transport: &'static str,
    scenario: &'static str,
    connects: &[Duration],
    rtt: &[Duration],
    loaded: &[Duration],
    tput: Duration,
    plan: Plan,
    modelled: bool,
) -> Result<Row> {
    Ok(Row {
        transport,
        scenario,
        connect: summary(connects)?,
        rtt: summary(rtt)?,
        loaded: summary(loaded)?,
        mbps: (plan.frames as f64 * BULK_SIZE as f64) / tput.as_secs_f64() / 1e6,
        modelled,
    })
}

fn triple(s: &Summary) -> String {
    format!("{} / {} / {}", ms(s.p50), ms(s.p95), ms(s.p99))
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    let quick = std::env::args().any(|a| a == "--quick");
    let div = if quick { 5 } else { 1 };
    let lan = Plan {
        connects: 20,
        pings: 3000 / div,
        frames: 4000 / div,
        loaded: 1000 / div,
    };
    let wan = Plan {
        connects: 10,
        pings: 1000 / div,
        frames: 600 / div,
        loaded: 500 / div,
    };
    let scenarios: [(&'static str, Option<Impairment>, Plan); 3] = [
        ("loopback", None, lan),
        (
            "30ms RTT, 50 Mbit/s",
            Some(Impairment::wan(30, 0.0, 50)),
            wan,
        ),
        (
            "30ms RTT, 50 Mbit/s, 1% loss",
            Some(Impairment::wan(30, 0.01, 50)),
            wan,
        ),
    ];
    let pki = TestPki::generate()?;
    let mut rows = Vec::new();
    for (name, imp, plan) in scenarios {
        eprintln!("running {name} ...");
        rows.push(run_quic(&pki, name, imp, plan).await?);
        rows.push(run_ws(&pki, name, imp, plan).await?);
    }
    println!(
        "Ping = {PING_SIZE} B frame and its ack; bulk = {BULK_SIZE} B frames; times in ms (p50 / p95 / p99)."
    );
    println!();
    println!(
        "| transport | link | connect p50 | RTT idle | RTT under bulk ({MIX_WINDOW} x 16 KiB in flight) | bulk MB/s ({TPUT_WINDOW} in flight) |"
    );
    println!("|---|---|---|---|---|---|");
    for r in &rows {
        let mark = if r.modelled {
            " (TCP loss modelled, no congestion control)"
        } else {
            ""
        };
        println!(
            "| {} | {} | {} | {} | {} | {:.1}{} |",
            r.transport,
            r.scenario,
            ms(r.connect.p50),
            triple(&r.rtt),
            triple(&r.loaded),
            r.mbps,
            mark
        );
    }
    Ok(())
}
