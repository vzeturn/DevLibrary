//! The folio-observation scanner (`FolioObservationScanner` and its parts): reads folios
//! changed since a cursor, posts what the hub has not acknowledged, and moves the cursor
//! only after the hub answered 2xx.

pub mod cursor;
pub mod earn;
pub mod poster;
pub mod read;
pub mod scanner;

use std::sync::Mutex;

use dl_time::{DateTime, OffsetDateTime};

pub use scanner::{FolioError, FolioScanner, ScanOutcome};

/// One reading of the scan state, for the heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanSnapshot {
    /// The watermark (PMS local time).
    pub watermark: DateTime,
    /// How far it trails the PMS clock, in seconds (0 once drained).
    pub lag_seconds: i64,
    /// When this was recorded (UTC).
    pub scan_at: OffsetDateTime,
}

/// `FolioScanState`: shared between the scanner and the heartbeat.
#[derive(Debug, Default)]
pub struct FolioScanState {
    inner: Mutex<Option<ScanSnapshot>>,
}

impl FolioScanState {
    /// `Record`: scanning from `watermark` while the PMS clock reads `now_pms`.
    pub fn record(&self, watermark: DateTime, now_pms: DateTime) {
        let lag = now_pms.duration_since(watermark).whole_seconds().max(0);
        self.set(watermark, lag);
    }

    /// `RecordDrained`: nothing left behind `watermark`.
    pub fn record_drained(&self, watermark: DateTime) {
        self.set(watermark, 0);
    }

    /// The latest reading.
    pub fn snapshot(&self) -> Option<ScanSnapshot> {
        *self.lock()
    }

    fn set(&self, watermark: DateTime, lag_seconds: i64) {
        *self.lock() = Some(ScanSnapshot {
            watermark,
            lag_seconds,
            scan_at: OffsetDateTime::now_utc(),
        });
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<ScanSnapshot>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
