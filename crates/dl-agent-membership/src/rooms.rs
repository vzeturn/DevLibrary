//! `RoomInventoryScanner` and `HousekeepingDerivation`: physical rooms (housekeeping,
//! out of service, occupancy today) and the room-type catalogue, pushed to the hub.

use std::fmt;

use dl_time::OffsetDateTime;

use crate::hub::{HubApi, HubError};
use crate::identity::Tracker;
use crate::identity::gate::GatedPush;
use crate::log::{self, V};
use crate::smile::{RoomRow, RoomSource, SmileError};
use crate::wire::parse::SyncConfig;
use crate::wire::{RoomInventoryRequest, RoomLine, RoomTypeCatalogRequest};

/// `MaxRoomsPerScan`: above this the scan is dropped (a truncated list would report rooms
/// as gone).
pub const MAX_ROOMS_PER_SCAN: usize = 5_000;

/// `HousekeepingDerivation.Derive`: `Room.StsCode` when the PMS has it, else the
/// housekeeping flags.
pub fn derive_housekeeping(
    sts_code: Option<i64>,
    hskp_clean: Option<bool>,
    inspected: Option<bool>,
    touched_up: Option<i64>,
) -> &'static str {
    if let Some(sts) = sts_code {
        return match sts {
            0 => "INSPECTED",
            1 => "CLEAN",
            2 => "TOUCH",
            3 => "DIRTY",
            4 => "STOP_SALE",
            _ => "UNKNOWN",
        };
    }
    match (hskp_clean, inspected) {
        (Some(true), Some(true)) => "INSPECTED",
        (Some(true), _) => "CLEAN",
        _ if touched_up.is_some_and(|t| t > 0) => "TOUCH",
        (Some(false), _) => "DIRTY",
        _ => "UNKNOWN",
    }
}

/// The hub line for one SMILE room.
pub fn room_line(r: &RoomRow) -> RoomLine {
    RoomLine {
        pms_room_code: r.room_code.clone(),
        pms_room_name: r.room_name.clone(),
        room_type_code: r.room_type_code.clone(),
        floor: r.floor,
        housekeeping_status: derive_housekeeping(
            r.sts_code,
            r.hskp_clean,
            r.inspected,
            r.touched_up,
        )
        .to_owned(),
        out_of_service: r.out_of_service,
        out_of_service_reason: r.out_of_service_reason.clone(),
        is_occupied: r.is_occupied,
        arrival_due_today: r.arrival_due_today,
        departure_due_today: r.departure_due_today,
        out_of_order: r.out_of_order,
        out_of_order_reason: r.out_of_order_reason.clone(),
        out_of_order_until: r.out_of_order_until,
    }
}

/// Why a room scan failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomError {
    /// Reading SMILE failed.
    Smile(SmileError),
    /// Posting the rooms failed.
    Hub(HubError),
}

impl fmt::Display for RoomError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RoomError::Smile(e) => write!(f, "room scan: {e}"),
            RoomError::Hub(e) => write!(f, "room post: {e}"),
        }
    }
}

impl std::error::Error for RoomError {}

/// What a scan did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomOutcome {
    /// `room_inventory_sync_enabled` is off (or no sync-config yet).
    SyncOff,
    /// The write gate is closed (another or a stale database).
    GateClosed,
    /// More rooms than [`MAX_ROOMS_PER_SCAN`]; nothing pushed.
    OverCap(usize),
    /// Pushed.
    Pushed {
        /// Rooms pushed.
        rooms: usize,
        /// Room types pushed (0 when the catalogue read or post failed).
        room_types: usize,
        /// Whether this was a full push.
        full: bool,
    },
}

/// The room scanner.
#[derive(Debug)]
pub struct RoomScanner {
    full_push_every: u32,
    scan_count: u64,
    gate: GatedPush,
}

impl RoomScanner {
    /// A scanner that marks every `full_push_every`-th push as full.
    pub fn new(full_push_every: u32) -> RoomScanner {
        RoomScanner {
            full_push_every: full_push_every.max(1),
            scan_count: 0,
            gate: GatedPush::new("physical rooms"),
        }
    }

    /// `ScanOnceAsync`.
    pub fn scan_once(
        &mut self,
        src: &dyn RoomSource,
        hub: &dyn HubApi,
        sync: Option<&SyncConfig>,
        tracker: &Tracker,
    ) -> Result<RoomOutcome, RoomError> {
        let Some(config) = sync.filter(|c| c.room_inventory_sync_enabled) else {
            return Ok(RoomOutcome::SyncOff);
        };
        if !self.gate.allows(
            config,
            tracker.current().as_ref(),
            tracker.foreign_age_minutes(),
        ) {
            return Ok(RoomOutcome::GateClosed);
        }
        let rows = src.scan_rooms().map_err(RoomError::Smile)?;
        // The catalogue rides on the same pass; its failure never blocks the rooms.
        let catalog = match src.room_types() {
            Ok(c) => c,
            Err(e) => {
                log::warn(
                    "room-type catalogue read failed; retried next scan",
                    &[("error", V::S(&e.to_string()))],
                );
                Vec::new()
            }
        };
        let mut room_types = 0;
        if !catalog.is_empty() {
            let req = RoomTypeCatalogRequest {
                room_types: catalog,
                as_of: OffsetDateTime::now_utc(),
            };
            match hub.post_room_types(&req) {
                Ok(()) => {
                    room_types = req.room_types.len();
                    log::info("room types pushed", &[("count", V::I(room_types as i64))]);
                }
                Err(e) => log::warn(
                    "room-type catalogue post failed; retried next scan",
                    &[("error", V::S(&e.to_string()))],
                ),
            }
        }
        if rows.len() > MAX_ROOMS_PER_SCAN {
            log::warn(
                "room scan over the cap; whole pass dropped (missing rooms are worse than stale ones)",
                &[
                    ("count", V::I(rows.len() as i64)),
                    ("max", V::I(MAX_ROOMS_PER_SCAN as i64)),
                ],
            );
            return Ok(RoomOutcome::OverCap(rows.len()));
        }
        self.scan_count += 1;
        let full = self
            .scan_count
            .is_multiple_of(u64::from(self.full_push_every.max(1)));
        let req = RoomInventoryRequest {
            rooms: rows.iter().map(room_line).collect(),
            is_full_push: full,
            as_of: OffsetDateTime::now_utc(),
        };
        hub.post_room_inventory(&req).map_err(RoomError::Hub)?;
        log::info(
            "rooms pushed",
            &[("count", V::I(rows.len() as i64)), ("full", V::B(full))],
        );
        Ok(RoomOutcome::Pushed {
            rooms: rows.len(),
            room_types,
            full,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn housekeeping_follows_sts_code_then_flags() {
        let d = derive_housekeeping;
        assert_eq!(d(Some(0), Some(false), None, None), "INSPECTED");
        assert_eq!(d(Some(4), None, None, None), "STOP_SALE");
        assert_eq!(d(Some(9), None, None, None), "UNKNOWN");
        assert_eq!(d(None, Some(true), Some(true), None), "INSPECTED");
        assert_eq!(d(None, Some(true), Some(false), Some(3)), "CLEAN");
        assert_eq!(d(None, Some(false), None, Some(1)), "TOUCH");
        assert_eq!(d(None, Some(false), None, Some(0)), "DIRTY");
        assert_eq!(d(None, None, None, None), "UNKNOWN");
        assert_eq!(d(None, None, None, Some(2)), "TOUCH");
    }
}
