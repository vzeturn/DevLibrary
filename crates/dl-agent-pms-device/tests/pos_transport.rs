//! Port of Pos/PosTransportTests.cs: TCP 9100 against a loopback listener, serial and
//! spooler through fakes.

#[path = "pos_fakes/mod.rs"]
mod fakes;

use std::io::{ErrorKind, Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dl_agent_pms_device::adapters::Cx;
use dl_agent_pms_device::adapters::pos::{
    EscPos, EscPosSerialAdapter, EscPosTcpAdapter, EscPosTransport, Win32Error,
    WindowsSpoolerAdapter,
};
use dl_agent_pms_device::agent::StopToken;
use dl_agent_pms_device::contracts::{PosTarget, codes};
use fakes::{PosSerialPortFake, PosSpoolerFake, target};

fn cx() -> Cx {
    Cx::unbounded()
}

#[test]
fn prt03_tcp_sends_the_bytes_to_the_printer_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let received = std::thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        let mut all = Vec::new();
        peer.read_to_end(&mut all).unwrap();
        all
    });
    EscPosTcpAdapter
        .send(
            &target("tcp", &format!("127.0.0.1:{port}")),
            &[0x1B, 0x40, 0x41],
            &cx(),
        )
        .unwrap();
    assert_eq!(received.join().unwrap(), [0x1B, 0x40, 0x41]);
}

#[test]
fn prt03_tcp_query_reads_the_status_reply() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let printer = std::thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        let mut request = [0u8; 3];
        peer.read_exact(&mut request).unwrap();
        peer.write_all(&[0x16]).unwrap();
        request
    });
    let reply = EscPosTcpAdapter
        .query(
            &target("tcp", &format!("127.0.0.1:{port}")),
            &EscPos::PRINTER_STATUS_QUERY,
            1,
            &cx(),
        )
        .unwrap();
    assert_eq!(reply, [0x16]);
    assert_eq!(printer.join().unwrap(), EscPos::PRINTER_STATUS_QUERY);
}

#[test]
fn prt03_tcp_printer_that_refuses_the_connection_is_device_unavailable() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let endpoint = format!("127.0.0.1:{port}");
    let e = EscPosTcpAdapter
        .send(&target("tcp", &endpoint), &[0x41], &cx())
        .unwrap_err();
    assert_eq!(e.code, codes::UNAVAILABLE);
    assert!(
        e.message
            .starts_with(&format!("tcp device {endpoint} unavailable: ")),
        "{}",
        e.message
    );
    if cfg!(windows) {
        assert!(e.message.ends_with(
            "No connection could be made because the target machine actively refused it."
        ));
    }
}

#[test]
fn prt03_tcp_silent_printer_times_out_the_status_read() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let printer = std::thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        let mut request = [0u8; 3];
        peer.read_exact(&mut request).unwrap();
        // Says nothing; keeps the socket open until the agent gives up.
        let mut rest = Vec::new();
        let _ = peer.read_to_end(&mut rest);
    });
    // The command deadline (0.4 s) comes before the 3 s reply timeout: a cancellation.
    let short = Cx::new(Duration::from_millis(400), StopToken::new());
    let started = Instant::now();
    let e = EscPosTcpAdapter
        .query(
            &target("tcp", &format!("127.0.0.1:{port}")),
            &EscPos::PRINTER_STATUS_QUERY,
            1,
            &short,
        )
        .unwrap_err();
    // The cancellation (not the 3 s reply timeout) proves which deadline fired; the bound
    // only rules out a hang and leaves headroom for a loaded machine.
    assert!(Cx::is_cancellation(&e), "{e}");
    assert!(started.elapsed() < Duration::from_secs(10));
    printer.join().unwrap();
}

#[test]
fn prt03_tcp_endpoint_defaults_to_port_9100() {
    for (endpoint, host, port) in [
        ("192.168.1.16", "192.168.1.16", 9100),
        (" 192.168.1.16:9101 ", "192.168.1.16", 9101),
        ("::1", "::1", 9100),
    ] {
        assert_eq!(
            EscPosTcpAdapter::parse_endpoint(endpoint).unwrap(),
            (host.to_owned(), port)
        );
    }
}

#[test]
fn prt03_bad_tcp_endpoint_is_a_bad_payload() {
    for endpoint in ["", "printer:abc", "printer:70000", "printer:-1", "printer:"] {
        let e = EscPosTcpAdapter::parse_endpoint(endpoint).unwrap_err();
        assert_eq!(e.code, codes::BAD_PAYLOAD, "{endpoint:?}");
    }
    assert_eq!(
        EscPosTcpAdapter::parse_endpoint("").unwrap_err().message,
        "tcp endpoint is empty"
    );
    assert_eq!(
        EscPosTcpAdapter::parse_endpoint(" printer:abc")
            .unwrap_err()
            .message,
        "tcp endpoint ' printer:abc' is not host:port"
    );
}

#[test]
fn prt03_serial_opens_the_port_at_the_baud_rate_writes_and_closes() {
    let port = PosSerialPortFake::new();
    let target = PosTarget {
        baud_rate: Some(19200),
        ..target("serial", "COM3")
    };
    EscPosSerialAdapter::new(Arc::new(port.clone()))
        .send(&target, &[0x41, 0x42], &cx())
        .unwrap();
    assert_eq!(port.opened(), [("COM3".to_owned(), 19200)]);
    assert_eq!(port.written(), [0x41, 0x42]);
    assert!(port.closed());
}

#[test]
fn prt03_serial_defaults_to_9600_and_reads_the_status_reply() {
    let port = PosSerialPortFake::with(|s| s.reply = vec![0x12]);
    let reply = EscPosSerialAdapter::new(Arc::new(port.clone()))
        .query(
            &target("serial", "COM3"),
            &EscPos::PAPER_STATUS_QUERY,
            1,
            &cx(),
        )
        .unwrap();
    assert_eq!(reply, [0x12]);
    assert_eq!(port.opened()[0].1, 9600);
    assert_eq!(port.written(), EscPos::PAPER_STATUS_QUERY);
}

#[test]
fn prt03_serial_port_busy_is_device_unavailable_and_a_silent_device_too() {
    let busy = PosSerialPortFake::with(|s| {
        s.open_fails = Some((
            ErrorKind::PermissionDenied,
            "Access to the port 'COM3' is denied.".into(),
        ));
    });
    let e = EscPosSerialAdapter::new(Arc::new(busy))
        .send(&target("serial", "COM3"), &[0x41], &cx())
        .unwrap_err();
    assert_eq!(e.code, codes::UNAVAILABLE);
    assert_eq!(
        e.message,
        "serial device COM3 unavailable: Access to the port 'COM3' is denied."
    );

    let silent = PosSerialPortFake::new();
    let e = EscPosSerialAdapter::new(Arc::new(silent))
        .query(
            &target("serial", "COM3"),
            &EscPos::PRINTER_STATUS_QUERY,
            1,
            &cx(),
        )
        .unwrap_err();
    assert_eq!(e.code, codes::UNAVAILABLE);
    assert_eq!(
        e.message,
        "serial device COM3 unavailable: the device closed the link before answering"
    );
}

#[test]
fn prt03_serial_endpoint_and_baud_rate_are_checked_before_opening() {
    let port = PosSerialPortFake::new();
    let serial = EscPosSerialAdapter::new(Arc::new(port.clone()));
    let e = serial
        .send(&target("serial", "  "), &[0x41], &cx())
        .unwrap_err();
    assert_eq!(e.message, "serial endpoint (port name) is empty");
    let fast = PosTarget {
        baud_rate: Some(1_000_000),
        ..target("serial", "COM3")
    };
    let e = serial.send(&fast, &[0x41], &cx()).unwrap_err();
    assert_eq!(e.message, "baud rate 1000000 is out of range");
    assert!(port.opened().is_empty());
}

#[test]
fn prt03_spooler_writes_one_raw_job_to_the_named_queue() {
    let spooler = Arc::new(PosSpoolerFake::default());
    WindowsSpoolerAdapter::new(Some(spooler.clone()))
        .send(&target("spooler", "EPSON TM-T82"), &[0x41], &cx())
        .unwrap();
    let jobs = spooler.jobs.lock().unwrap().clone();
    assert_eq!(
        jobs,
        [(
            "EPSON TM-T82".to_owned(),
            WindowsSpoolerAdapter::DOCUMENT_NAME.to_owned(),
            vec![0x41]
        )]
    );
}

#[test]
fn prt03_spooler_refusals_are_typed() {
    let target = target("spooler", "EPSON TM-T82");
    let no_host = WindowsSpoolerAdapter::new(None)
        .send(&target, &[0x41], &cx())
        .unwrap_err();
    assert_eq!(no_host.code, codes::PLATFORM_UNSUPPORTED);

    let refused = Arc::new(PosSpoolerFake::default());
    *refused.fail.lock().unwrap() = Some(Win32Error::from_code(1801));
    let unknown = WindowsSpoolerAdapter::new(Some(refused))
        .send(&target, &[0x41], &cx())
        .unwrap_err();
    assert_eq!(unknown.code, codes::UNAVAILABLE);
    assert!(unknown.message.contains("1801"));
    if cfg!(windows) {
        assert_eq!(
            unknown.message,
            "spooler device EPSON TM-T82 unavailable: spooler error 1801: The printer name is invalid."
        );
    }

    let read = WindowsSpoolerAdapter::new(Some(Arc::new(PosSpoolerFake::default())))
        .query(&target, &EscPos::PRINTER_STATUS_QUERY, 1, &cx())
        .unwrap_err();
    assert_eq!(read.code, codes::TRANSPORT_UNSUPPORTED);
}

#[cfg(not(windows))]
#[test]
fn prt03_off_windows_the_spooler_refuses_with_device_platform_unsupported() {
    let e = WindowsSpoolerAdapter::create()
        .send(&target("spooler", "x"), &[0x41], &cx())
        .unwrap_err();
    assert_eq!(e.code, codes::PLATFORM_UNSUPPORTED);
}
