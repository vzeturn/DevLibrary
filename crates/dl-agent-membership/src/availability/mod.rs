//! `AvailabilityScanner`: a cheap SMILE probe every few seconds; when it changes (or the
//! heartbeat is due) a full read of occupancy, rooms, blocks, group holds and extra beds,
//! pushed to the hub as per-night lines (deltas, with a full push every N scans).

pub mod count;

use std::collections::BTreeMap;
use std::fmt;
use std::time::Instant;

use dl_time::Date;

use crate::hub::{HubApi, HubError};
use crate::identity::Tracker;
use crate::identity::gate::GatedPush;
use crate::log::{self, V};
use crate::settings::AvailabilitySettings;
use crate::smile::{AvailabilitySource, ExtraBedUsage, OccupancyFingerprint, SmileError};
use crate::wire::AvailabilityRequest;
use crate::wire::parse::SyncConfig;
use count::{LastPushed, RoomsByType};

/// `MaxLinesPerRequest`.
pub const MAX_LINES_PER_REQUEST: usize = 1_000;

/// Why a tick failed (the caller backs off 30 s, like .NET `ErrorBackoff`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AvailabilityError {
    /// Reading SMILE failed.
    Smile(SmileError),
    /// Posting failed.
    Hub(HubError),
}

impl fmt::Display for AvailabilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AvailabilityError::Smile(e) => write!(f, "availability scan: {e}"),
            AvailabilityError::Hub(e) => write!(f, "availability post: {e}"),
        }
    }
}

impl std::error::Error for AvailabilityError {}

/// What a full scan did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanOutcome {
    /// `availability_sync_enabled` is off (or no sync-config yet).
    SyncOff,
    /// The write gate is closed.
    GateClosed,
    /// The folio read hit `max_folios`; nothing pushed.
    OverCap,
    /// SMILE returned no room; nothing pushed.
    NoRooms,
    /// Pushed `lines` lines (0 = heartbeat only).
    Pushed {
        /// Occupancy lines pushed.
        lines: usize,
        /// Stays read.
        stays: usize,
        /// Whether it was a full push.
        full: bool,
    },
}

/// What a tick did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickOutcome {
    /// The probe did not change and the heartbeat is not due.
    Unchanged,
    /// A full scan ran.
    Scanned(ScanOutcome),
}

/// The scanner. One per scan thread.
#[derive(Debug)]
pub struct AvailabilityScanner {
    settings: AvailabilitySettings,
    default_extra_bed_code: String,
    gate: GatedPush,
    last_pushed: LastPushed,
    last_usage: BTreeMap<Date, i64>,
    scans_since_full: i64,
    last_probe: Option<(Date, OccupancyFingerprint)>,
    last_scan_at: Option<Instant>,
    /// Probes in a row that saw no change: feeds the adaptive pace (RT-0756).
    idle_probes: u32,
}

impl AvailabilityScanner {
    /// A scanner with `settings`; `extra_bed_code` is used when sync-config names none.
    pub fn new(settings: AvailabilitySettings, extra_bed_code: &str) -> AvailabilityScanner {
        AvailabilityScanner {
            settings,
            default_extra_bed_code: extra_bed_code.to_owned(),
            gate: GatedPush::new("availability"),
            last_pushed: LastPushed::new(),
            last_usage: BTreeMap::new(),
            scans_since_full: 0,
            last_probe: None,
            last_scan_at: None,
            idle_probes: 0,
        }
    }

    fn horizon_end(&self, today: Date) -> Date {
        today
            .checked_add_days(self.settings.horizon_days.max(0))
            .unwrap_or(today)
    }

    fn extra_bed_code(&self, sync: Option<&SyncConfig>) -> String {
        sync.and_then(|s| s.extra_bed_special_code.as_deref())
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .unwrap_or(&self.default_extra_bed_code)
            .to_owned()
    }

    /// `TickAsync`: probe, then scan when the probe changed or the heartbeat is due.
    pub fn tick(
        &mut self,
        src: &dyn AvailabilitySource,
        hub: &dyn HubApi,
        sync: Option<&SyncConfig>,
        tracker: &Tracker,
        today: Date,
        now: Instant,
    ) -> Result<TickOutcome, AvailabilityError> {
        let fingerprint = src
            .probe_occupancy(today, self.horizon_end(today))
            .map_err(AvailabilityError::Smile)?;
        let heartbeat_due = self
            .last_scan_at
            .is_none_or(|last| now.saturating_duration_since(last) >= self.settings.heartbeat);
        let probe = (today, fingerprint);
        let changed = self.last_probe.as_ref() != Some(&probe);
        // The pace follows real SMILE changes only; a heartbeat scan does not speed it up.
        self.idle_probes = if changed {
            0
        } else {
            self.idle_probes.saturating_add(1)
        };
        if !heartbeat_due && !changed {
            return Ok(TickOutcome::Unchanged);
        }
        let outcome = self.scan_once(src, hub, sync, tracker, today)?;
        self.last_probe = Some(probe);
        self.last_scan_at = Some(now);
        Ok(TickOutcome::Scanned(outcome))
    }

    /// The wait before the next probe (RT-0756): the probe interval while SMILE changes,
    /// doubling per unchanged probe up to `probe_idle_max`.
    pub fn next_probe_delay(&self) -> std::time::Duration {
        self.settings.probe_delay_after_idle(self.idle_probes)
    }

    /// Back to the base pace: a skipped (lock-busy) tick catches up at once.
    pub fn reset_pace(&mut self) {
        self.idle_probes = 0;
    }

    /// `ScanOnceAsync`.
    pub fn scan_once(
        &mut self,
        src: &dyn AvailabilitySource,
        hub: &dyn HubApi,
        sync: Option<&SyncConfig>,
        tracker: &Tracker,
        today: Date,
    ) -> Result<ScanOutcome, AvailabilityError> {
        let started = Instant::now();
        let Some(config) = sync.filter(|c| c.availability_sync_enabled) else {
            return Ok(ScanOutcome::SyncOff);
        };
        if !self.gate.allows(
            config,
            tracker.current().as_ref(),
            tracker.foreign_age_minutes(),
        ) {
            return Ok(ScanOutcome::GateClosed);
        }
        let end = self.horizon_end(today);
        let smile = AvailabilityError::Smile;
        let stays = src
            .scan_occupancy(today, end, self.settings.max_folios)
            .map_err(smile)?;
        let rooms = src.scan_rooms().map_err(smile)?;
        let blocks = src.scan_room_blocks(today, end).map_err(smile)?;
        let group_blocks = src.scan_group_blocks(today, end).map_err(smile)?;
        let beds = self.read_extra_beds(src, today, end, &self.extra_bed_code(sync));
        if i64::try_from(stays.len()).unwrap_or(i64::MAX) >= self.settings.max_folios {
            log::error(
                "availability scan dropped: the folio read hit max_folios, so the counts are short",
                &[("max_folios", V::I(self.settings.max_folios))],
            );
            return Ok(ScanOutcome::OverCap);
        }
        let by_type = RoomsByType::from_rooms(&rooms);
        if by_type.is_empty() {
            log::warn(
                "availability scan dropped: SMILE returned no room (check the SQL Server connection)",
                &[],
            );
            return Ok(ScanOutcome::NoRooms);
        }
        let mut sold = count::count_sold(&stays, today, end, &by_type);
        let unpicked =
            count::add_unpicked_group_rooms(&mut sold, &stays, &group_blocks, today, end, &by_type);
        let type_by_room = count::map_room_to_type(&rooms);
        let raised =
            count::raise_to_physical(&mut sold, &stays, &type_by_room, today, end, &by_type);
        if raised > 0 {
            log::info(
                "room-nights raised to physical occupancy (guests placed in another type)",
                &[("room_nights", V::I(raised))],
            );
        }
        let blocked = count::count_blocked(&blocks, today, end, &by_type);
        let full = self.scans_since_full <= 0;
        let lines = count::build_lines(
            &sold,
            &blocked,
            &by_type,
            today,
            end,
            full,
            &self.last_pushed,
        );
        let usage = count::build_extra_bed_lines(beds.as_ref(), today, end, full, &self.last_usage);
        let mut batches: Vec<Vec<_>> = lines
            .chunks(MAX_LINES_PER_REQUEST)
            .map(<[_]>::to_vec)
            .collect();
        if batches.is_empty() {
            batches.push(Vec::new());
        }
        let count = batches.len();
        for (i, batch) in batches.into_iter().enumerate() {
            let last = i + 1 == count;
            let req = AvailabilityRequest {
                lines: batch,
                scan_elapsed_ms: Some(
                    i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX),
                ),
                scan_complete: last,
                extra_bed_usage: if last { usage.clone() } else { None },
            };
            hub.post_availability(&req)
                .map_err(AvailabilityError::Hub)?;
        }
        if let Some(usage) = &usage {
            for l in usage {
                self.last_usage.insert(l.date, l.smile_beds);
            }
            self.last_usage.retain(|d, _| *d >= today);
        }
        for l in &lines {
            self.last_pushed.insert(
                (l.room_type_code.clone(), l.date),
                (l.rooms_sold, l.rooms_total),
            );
        }
        self.last_pushed.retain(|(_, d), _| *d >= today);
        self.scans_since_full = if full {
            i64::from(self.settings.full_push_every.max(1)) - 1
        } else {
            self.scans_since_full - 1
        };
        if !lines.is_empty() {
            log::info(
                "availability pushed",
                &[
                    ("lines", V::I(lines.len() as i64)),
                    ("full", V::B(full)),
                    ("stays", V::I(stays.len() as i64)),
                    ("unpicked_group_room_nights", V::I(unpicked)),
                ],
            );
        }
        Ok(ScanOutcome::Pushed {
            lines: lines.len(),
            stays: stays.len(),
            full,
        })
    }

    /// `ReadExtraBedUsageAsync`: a failure only drops the extra-bed part of this scan.
    fn read_extra_beds(
        &self,
        src: &dyn AvailabilitySource,
        from: Date,
        to: Date,
        code: &str,
    ) -> Option<BTreeMap<Date, i64>> {
        match src.extra_bed_usage(from, to, self.settings.max_folios, code) {
            Ok(ExtraBedUsage::Beds(beds)) => Some(beds),
            Ok(ExtraBedUsage::Unknown(problem)) => {
                log::warn(
                    "SMILE extra beds not reported this scan",
                    &[("problem", V::S(&problem))],
                );
                None
            }
            Err(e) => {
                log::warn(
                    "SMILE extra-bed read failed; extra beds skipped, availability still pushed",
                    &[("error", V::S(&e.to_string()))],
                );
                None
            }
        }
    }
}
