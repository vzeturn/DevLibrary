//! Hardware loopback test: connect TX to RX (and RTS to CTS) on a real port, then run
//! `DL_SERIAL_LOOPBACK=COM5 cargo test -p dl-serial --test loopback -- --ignored`.

use std::io::{ErrorKind, Read, Write};
use std::time::Duration;

use dl_serial::{Buffers, SerialPort, Settings};

#[test]
#[ignore = "needs a port with TX wired to RX; set DL_SERIAL_LOOPBACK"]
fn loopback() {
    let Ok(name) = std::env::var("DL_SERIAL_LOOPBACK") else {
        eprintln!("DL_SERIAL_LOOPBACK is not set; skipping");
        return;
    };
    let settings = Settings {
        read_timeout: Some(Duration::from_millis(500)),
        ..Settings::new(115_200)
    };
    let mut port = SerialPort::open(&name, &settings).unwrap();
    port.clear(Buffers::Both).unwrap();
    let data: Vec<u8> = (0..=255u8).collect();
    port.write_all(&data).unwrap();
    port.flush().unwrap();
    let mut got = vec![0u8; data.len()];
    port.read_exact(&mut got).unwrap();
    assert_eq!(got, data);
    assert_eq!(port.bytes_available().unwrap(), 0);
    let e = port.read(&mut [0u8; 4]).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::TimedOut);
    port.set_dtr(false).unwrap();
    port.set_dtr(true).unwrap();
    port.set_rts(true).unwrap();
}
