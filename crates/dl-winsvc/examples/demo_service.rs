//! A minimal service: appends a heartbeat line to the file named by the first argument
//! every 500 ms until stopped. Runs as a console program when not started by the SCM.
//!
//! Used by `tests/service.rs`.

use std::io::Write;
use std::time::Duration;

use dl_winsvc::{StopSignal, is_service_process, run_service};

fn work(path: String, stop: StopSignal) {
    let mut n = 0u64;
    loop {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(f, "beat {n}");
        }
        n += 1;
        if stop.wait_timeout(Duration::from_millis(500)) {
            break;
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&path) {
        let _ = writeln!(f, "stopped");
    }
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "heartbeat.txt".to_owned());
    match is_service_process() {
        Ok(true) => {
            if let Err(e) = run_service("dl-winsvc-test", move |stop| work(path, stop)) {
                eprintln!("service failed: {e}");
                std::process::exit(1);
            }
        }
        _ => {
            let stop = StopSignal::new();
            let s = stop.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(2));
                s.stop();
            });
            work(path, stop);
        }
    }
}
