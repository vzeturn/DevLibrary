//! End-to-end: both transports carry pings and bulk through loopback and through the shim.

use dl_rt_transport::link::Link;
use dl_rt_transport::shim::{self, Impairment};
use dl_rt_transport::tlsconf::TestPki;
use dl_rt_transport::{quic, workload, ws};
use std::time::Duration;

fn slow() -> Impairment {
    Impairment::wan(20, 0.0, 100)
}

async fn exercise(link: Link, min_rtt: Duration) {
    let Link {
        mut control,
        mut bulk,
        tasks,
    } = link;
    let rtts = workload::ping(&mut control, 10, 512).await.unwrap();
    assert!(rtts.iter().all(|r| *r >= min_rtt), "{rtts:?}");
    workload::throughput(&mut bulk, 50, 4096, 8).await.unwrap();
    Link {
        control,
        bulk,
        tasks,
    }
    .close()
    .await
    .unwrap();
}

#[tokio::test]
async fn quic_loopback_and_shim() {
    let pki = TestPki::generate().unwrap();
    let server = quic::serve(&pki).await.unwrap();
    let client = quic::Client::new(&pki).unwrap();
    let conn = client.connect(server.addr).await.unwrap();
    exercise(client.open_link(conn).await.unwrap(), Duration::ZERO).await;
    let s = shim::udp(server.addr, slow()).await.unwrap();
    let conn = client.connect(s.addr).await.unwrap();
    exercise(
        client.open_link(conn).await.unwrap(),
        Duration::from_millis(20),
    )
    .await;
}

#[tokio::test]
async fn ws_loopback_and_shim() {
    let pki = TestPki::generate().unwrap();
    let server = ws::serve(&pki).await.unwrap();
    let client = ws::Client::new(&pki).unwrap();
    exercise(client.connect(server.addr).await.unwrap(), Duration::ZERO).await;
    let s = shim::tcp(server.addr, slow()).await.unwrap();
    exercise(
        client.connect(s.addr).await.unwrap(),
        Duration::from_millis(20),
    )
    .await;
}
