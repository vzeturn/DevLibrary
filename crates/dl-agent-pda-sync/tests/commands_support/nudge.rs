//! A capture nudge that counts its calls (RT-1309).

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use dl_agent_pda_sync::capture::CaptureNudge;

/// Counts capture nudges and records the checks a nudge asked to re-read (RT-1309).
#[derive(Default)]
pub struct CountingNudge(pub AtomicUsize, pub Mutex<Vec<String>>);

impl CountingNudge {
    pub fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }

    #[allow(dead_code)]
    pub fn checks(&self) -> Vec<String> {
        self.1.lock().unwrap().clone()
    }
}

impl CaptureNudge for CountingNudge {
    fn nudge_open_checks(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn nudge_open_check(&self, check_key: &str) {
        self.1.lock().unwrap().push(check_key.to_owned());
        self.nudge_open_checks();
    }
}
