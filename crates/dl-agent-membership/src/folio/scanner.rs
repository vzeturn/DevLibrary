//! `FolioObservationScanner.ScanOnceAsync`: one pass from the cursor.

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use dl_time::{DateTime, OffsetDateTime};

use super::FolioScanState;
use super::cursor::{Cursor, CursorIdentity, CursorStore, cold_start, next_watermark};
use super::earn::{self, EarnPairSet};
use super::poster::{PostOutcome, Poster};
use super::read;
use crate::hub::{HubApi, HubError};
use crate::identity::{self, Tracker};
use crate::log::{self, V};
use crate::settings::{FolioSettings, SmileSettings};
use crate::smile::{FolioSource, SmileError, hotel_now};
use crate::wire::parse::SyncConfig;

/// Why a pass stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolioError {
    /// The scanner turned itself off (the SMILE schema does not fit); it stays off until
    /// the agent restarts.
    Disabled(String),
    /// SMILE does not say which charge code is the room charge.
    Ambiguous(String),
    /// SMILE failed; the cursor did not move.
    Smile(SmileError),
    /// The hub failed; the cursor did not move.
    Hub(HubError),
}

impl fmt::Display for FolioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FolioError::Disabled(why) => write!(f, "folio scanner disabled: {why}"),
            FolioError::Ambiguous(why) => write!(f, "earn charge codes ambiguous: {why}"),
            FolioError::Smile(e) => write!(f, "folio scan: {e}"),
            FolioError::Hub(e) => write!(f, "folio post: {e}"),
        }
    }
}

impl std::error::Error for FolioError {}

/// What a pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanOutcome {
    /// Observations read.
    pub rows: usize,
    /// What the poster did.
    pub posted: PostOutcome,
    /// A full batch moved the cursor: scan again at once.
    pub more: bool,
}

/// The scanner. One per scan thread.
#[derive(Debug)]
pub struct FolioScanner {
    hotel_id: String,
    agent_version: String,
    folio: FolioSettings,
    smile: SmileSettings,
    store: CursorStore,
    poster: Poster,
    state: Arc<FolioScanState>,
    last_pairs: Option<String>,
    override_logged: bool,
    disabled: Option<String>,
}

impl FolioScanner {
    /// A scanner for `hotel_id` with its cursor in `dir`.
    pub fn new(
        hotel_id: &str,
        agent_version: &str,
        folio: FolioSettings,
        smile: SmileSettings,
        dir: &Path,
        state: Arc<FolioScanState>,
    ) -> FolioScanner {
        FolioScanner {
            hotel_id: hotel_id.to_owned(),
            agent_version: agent_version.to_owned(),
            poster: Poster::new(folio.max_per_scan),
            folio,
            smile,
            store: CursorStore::new(dir),
            state,
            last_pairs: None,
            override_logged: false,
            disabled: None,
        }
    }

    /// Why the scanner turned itself off, if it did.
    pub fn disabled(&self) -> Option<&str> {
        self.disabled.as_deref()
    }

    /// One pass: read from the cursor, post, then move the cursor (only after the hub
    /// answered 2xx).
    pub fn scan_once(
        &mut self,
        src: &dyn FolioSource,
        hub: &dyn HubApi,
        tracker: &Tracker,
        sync: Option<&SyncConfig>,
    ) -> Result<ScanOutcome, FolioError> {
        if let Some(why) = &self.disabled {
            return Err(FolioError::Disabled(why.clone()));
        }
        if tracker.current_fingerprint().is_none()
            && let identity::Refresh::Failed(e) = tracker.refresh_if_due(true)
        {
            log::warn(
                "SMILE identity read failed; the folio cursor is keyed without it this pass",
                &[("error", V::S(&e))],
            );
        }
        let identity = CursorIdentity {
            current: tracker.current_fingerprint(),
            baseline: sync
                .and_then(|s| s.baseline_identity.as_ref())
                .map(identity::fingerprint),
        };
        let now_pms = tracker.pms_now().unwrap_or_else(hotel_now);
        let cursor = self.store.load(&self.hotel_id, &identity);
        let watermark = match &cursor {
            Some(c) => c.watermark,
            None => {
                let wm = cold_start(now_pms, self.folio.cold_start_lookback_days);
                log::warn(
                    "folio cursor missing; cold start from the look-back window",
                    &[("lookback_days", V::I(self.folio.cold_start_lookback_days))],
                );
                wm
            }
        };
        self.state.record(watermark, now_pms);
        let extra_bed_code = sync
            .and_then(|s| s.extra_bed_special_code.as_deref())
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .unwrap_or(&self.smile.extra_bed_special_code)
            .to_owned();
        let pairs = self.earn_pairs(src, watermark, &extra_bed_code)?;
        let max_row = self.folio.max_per_scan;
        let read = read::read(
            src,
            &self.smile.guest_phone_column,
            watermark,
            i64::try_from(max_row).unwrap_or(i64::MAX),
            &pairs,
            &extra_bed_code,
        )
        .map_err(|e| self.smile_failed(e))?;
        let count = read.observations.len();
        if count == 0 {
            self.state.record_drained(watermark);
            return Ok(ScanOutcome::default());
        }
        let posted = self
            .poster
            .post(
                hub,
                &read.observations,
                &self.agent_version,
                tracker.current_fingerprint().as_deref(),
            )
            .map_err(FolioError::Hub)?;
        let next = next_watermark(
            watermark,
            read.max_last_change_time,
            count,
            max_row,
            self.folio.overlap,
        );
        let saved = Cursor {
            hotel_id: self.hotel_id.clone(),
            watermark: next,
            last_scan_at: OffsetDateTime::now_utc(),
            last_batch_size: i64::try_from(count).unwrap_or(i64::MAX),
        };
        if let Err(e) = self.store.save(&saved, &identity) {
            log::warn(
                "folio cursor not saved; it is kept in memory and the next restart rescans the overlap",
                &[("error", V::S(&e.kind().to_string()))],
            );
        }
        let full = count >= max_row;
        if full {
            self.state.record(next, now_pms);
        } else {
            self.state.record_drained(next);
        }
        log::info(
            "folio observations posted",
            &[
                ("read", V::I(count as i64)),
                ("sent", V::I(posted.sent as i64)),
                ("skipped", V::I(posted.skipped as i64)),
                ("accepted", V::I(posted.accepted)),
                ("earned", V::I(posted.earned)),
                ("reversed", V::I(posted.reversed)),
                ("unchanged", V::I(posted.unchanged)),
            ],
        );
        let stalled = full && next == watermark;
        if stalled {
            log::warn(
                "a full folio batch shares one LastChangeTime; waiting for the interval instead of rescanning",
                &[("max_per_scan", V::I(max_row as i64))],
            );
        }
        Ok(ScanOutcome {
            rows: count,
            posted,
            more: full && !stalled,
        })
    }

    fn earn_pairs(
        &mut self,
        src: &dyn FolioSource,
        watermark: DateTime,
        extra_bed_code: &str,
    ) -> Result<Vec<earn::EarnPair>, FolioError> {
        let set: EarnPairSet = match earn::from_override(&self.smile.earn_charge_codes) {
            Some(set) => {
                if !self.override_logged {
                    self.override_logged = true;
                    log::warn(
                        "PMS_EARN_CHARGE_CODE_OVERRIDE: earn charge codes come from configuration, not SMILE",
                        &[("pairs", V::S(&set.describe()))],
                    );
                }
                set
            }
            None => {
                let catalog = src
                    .earn_catalog(extra_bed_code)
                    .map_err(|e| self.smile_failed(e))?;
                let rm = src
                    .room_charge_trn_since(watermark)
                    .map_err(|e| self.smile_failed(e))?;
                earn::resolve(&catalog, &rm)
            }
        };
        if set.is_ambiguous() {
            let why = format!(
                "{}: {} TransactionCode rows have AutoPost = 1 (expected exactly one)",
                earn::ROOM_CODE_AMBIGUOUS,
                set.auto_post_count
            );
            log::error(&why, &[]);
            return Err(FolioError::Ambiguous(why));
        }
        let described = set.describe();
        if self.last_pairs.as_deref() != Some(described.as_str()) {
            log::info(
                "earn charge codes",
                &[
                    ("pairs", V::S(&described)),
                    ("dropped", V::S(&set.dropped.join(","))),
                ],
            );
            self.last_pairs = Some(described);
        }
        Ok(earn::with_member_discount(
            set.pairs,
            &self.smile.member_discount_code,
        ))
    }

    /// A SQL error 207/208 (unknown column or table) or an invalid phone column turns the
    /// scanner off, as in .NET: retrying cannot fix the schema.
    fn smile_failed(&mut self, e: SmileError) -> FolioError {
        match e {
            SmileError::Server(n @ (207 | 208)) => {
                let why = format!("SMILE schema does not fit the folio query (SQL error {n})");
                log::error(&why, &[]);
                self.disabled = Some(why.clone());
                FolioError::Disabled(why)
            }
            SmileError::Invalid(what) => {
                let why = format!("invalid configuration: {what}");
                log::error("folio scanner disabled", &[("why", V::S(&why))]);
                self.disabled = Some(why.clone());
                FolioError::Disabled(why)
            }
            other => FolioError::Smile(other),
        }
    }
}
