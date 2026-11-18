//! A minimal service: appends a heartbeat line to the file named by the first argument
//! every 500 ms until stopped. With a second argument `exit=N` it stops by itself after
//! about a second with the service-specific exit code N. Runs as a console program when
//! not started by the SCM.
//!
//! Used by `tests/service.rs`.

use std::io::Write;
use std::time::Duration;

use dl_winsvc::{StopSignal, is_service_process, run_service_with_exit_code};

fn work(path: String, exit: Option<u32>, stop: StopSignal) -> u32 {
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
        if let Some(code) = exit
            && n >= 2
        {
            return code;
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&path) {
        let _ = writeln!(f, "stopped");
    }
    0
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "heartbeat.txt".to_owned());
    let exit = args
        .next()
        .and_then(|a| a.strip_prefix("exit=").and_then(|n| n.parse().ok()));
    match is_service_process() {
        Ok(true) => {
            if let Err(e) =
                run_service_with_exit_code("dl-winsvc-test", move |stop| work(path, exit, stop))
            {
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
            std::process::exit(i32::try_from(work(path, exit, stop)).unwrap_or(1));
        }
    }
}
