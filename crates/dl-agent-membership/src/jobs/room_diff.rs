//! `RoomDiffCalculator` and `ExistingFolio`: what a MODIFY does to each room's folio.

use dl_decimal::Decimal;
use dl_time::Date;
use dl_uuid::Uuid;

use super::beds::{BedDecision, ExistingBeds, night_rate, plan_guests};
use super::guest::{blank, smile_gender};
use super::payload::Room;
use crate::writer::tcvn3;

/// `ExistingFolioGuest`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ExistingGuest {
    /// `NumAdult`.
    pub num_adult: i32,
    /// `NumChild`.
    pub num_child: i32,
    /// `Folio.LastName` (TCVN3).
    pub last_name: Option<String>,
    /// `Folio.FirstName` (TCVN3).
    pub first_name: Option<String>,
    /// `Folio.TelNumM`.
    pub phone: Option<String>,
    /// `Folio.MailM`.
    pub email: Option<String>,
    /// First live `AdditionalName` birth date.
    pub birth_date: Option<Date>,
    /// `AdditionalName` gender (true = male).
    pub gender: Option<bool>,
    /// `AdditionalName.NationalityCode`.
    pub nationality: Option<String>,
}

impl std::fmt::Debug for ExistingGuest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ExistingFolioGuest { *** }")
    }
}

/// `ExistingFolio`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingFolio {
    /// `FolioNum`.
    pub folio_num: Decimal,
    /// `UserInfo1`.
    pub room_item_id: Uuid,
    /// `RoomTypeBooked`.
    pub room_type_booked: String,
    /// `ArrivalDate`.
    pub arrival: Date,
    /// `DepartureDate`.
    pub departure: Date,
    /// `RateAmount`.
    pub rate_amount: Decimal,
    /// `RoomCode` (empty until a room is assigned).
    pub room_code: String,
    /// `FolioStatus` (1 booked, above 1 checked in or out).
    pub folio_status: i32,
    /// `CancelNum` (above 0 once cancelled).
    pub cancel_num: Decimal,
    /// Guest fields, when read.
    pub guest: Option<ExistingGuest>,
    /// Bed fields, when read.
    pub beds: Option<ExistingBeds>,
}

/// `RoomDiffKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    /// A new folio.
    Create,
    /// An updated folio.
    Update,
    /// A cancelled folio.
    Cancel,
    /// Nothing to write.
    Unchanged,
}

/// `RoomDiff`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomDiff {
    /// `Kind`.
    pub kind: DiffKind,
    /// `RoomItemId`.
    pub room_item_id: Uuid,
    /// `Payload`.
    pub payload: Option<Room>,
    /// `Existing`.
    pub existing: Option<ExistingFolio>,
}

/// `RoomDiffResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffResult {
    /// `Diffs`.
    pub diffs: Vec<RoomDiff>,
    /// `RequiresManual`.
    pub requires_manual: bool,
    /// `ManualReason`.
    pub manual_reason: Option<String>,
    /// `BedLocked`.
    pub bed_locked: bool,
}

impl DiffResult {
    /// The diffs of one kind.
    pub fn of(&self, kind: DiffKind) -> impl Iterator<Item = &RoomDiff> {
        self.diffs.iter().filter(move |d| d.kind == kind)
    }

    fn blocked(reason: String, bed_locked: bool) -> DiffResult {
        DiffResult {
            diffs: Vec::new(),
            requires_manual: true,
            manual_reason: Some(reason),
            bed_locked,
        }
    }
}

fn locked(f: &ExistingFolio) -> Option<String> {
    if !f.room_code.is_empty() {
        return Some(format!(
            "folio {} đã xếp phòng ({})",
            f.folio_num, f.room_code
        ));
    }
    (f.folio_status > 1).then(|| {
        format!(
            "folio {} đã check-in (status {})",
            f.folio_num, f.folio_status
        )
    })
}

fn has_mm_rows(f: &ExistingFolio) -> bool {
    f.beds.as_ref().is_some_and(|b| !b.mm_rows.is_empty())
}

/// `RoomDiffCalculator.Diff`.
pub fn diff(
    existing: &[ExistingFolio],
    rooms: &[Room],
    decide: &dyn Fn(&Room) -> BedDecision,
) -> DiffResult {
    // Live folios, first per room, in first-seen order.
    let mut by_room: Vec<&ExistingFolio> = Vec::new();
    for f in existing.iter().filter(|f| f.cancel_num.is_zero()) {
        if !by_room.iter().any(|x| x.room_item_id == f.room_item_id) {
            by_room.push(f);
        }
    }
    let find = |id: Uuid| by_room.iter().copied().find(|f| f.room_item_id == id);
    for room in rooms {
        if let Some(f) = find(room.room_item_id)
            && let Some(reason) = locked(f)
        {
            let bed = has_mm_rows(f) || (!room.removed && decide(room).is_special());
            return DiffResult::blocked(reason, bed);
        }
    }
    let mut diffs = Vec::new();
    for room in rooms {
        let Some(f) = find(room.room_item_id) else {
            if !room.removed {
                diffs.push(RoomDiff {
                    kind: DiffKind::Create,
                    room_item_id: room.room_item_id,
                    payload: Some(room.clone()),
                    existing: None,
                });
            }
            continue;
        };
        let kind = if room.removed {
            DiffKind::Cancel
        } else if has_changed(f, room, &decide(room)) {
            DiffKind::Update
        } else {
            DiffKind::Unchanged
        };
        diffs.push(RoomDiff {
            kind,
            room_item_id: room.room_item_id,
            payload: Some(room.clone()),
            existing: Some(f.clone()),
        });
    }
    for f in &by_room {
        if rooms.iter().any(|r| r.room_item_id == f.room_item_id) {
            continue;
        }
        if let Some(reason) = locked(f) {
            return DiffResult::blocked(reason, has_mm_rows(f));
        }
        diffs.push(RoomDiff {
            kind: DiffKind::Cancel,
            room_item_id: f.room_item_id,
            payload: None,
            existing: Some((*f).clone()),
        });
    }
    DiffResult {
        diffs,
        requires_manual: false,
        manual_reason: None,
        bed_locked: false,
    }
}

fn has_changed(f: &ExistingFolio, room: &Room, bed: &BedDecision) -> bool {
    if f.arrival != room.arrival || f.departure != room.departure {
        return true;
    }
    let total: Decimal = room
        .nights
        .iter()
        .map(|n| night_rate(n, bed.use_net_excl_bed))
        .sum();
    if f.rate_amount != total {
        return true;
    }
    if !f
        .room_type_booked
        .eq_ignore_ascii_case(&room.room_type_code)
        && f.room_type_booked.to_lowercase() != room.room_type_code.to_lowercase()
    {
        return true;
    }
    f.guest.as_ref().is_some_and(|g| guest_changed(g, room)) || bed_changed(f, room, bed)
}

fn bed_changed(f: &ExistingFolio, room: &Room, bed: &BedDecision) -> bool {
    let Some(beds) = &f.beds else {
        return false;
    };
    if bed.is_special() {
        let mut have = beds.mm_rows.clone();
        have.sort_by_key(|r| r.from_day);
        let mut want = bed.special_rows.clone();
        want.sort_by_key(|r| r.from_day);
        return have != want
            || beds
                .extra_bed
                .is_some_and(|eb| eb != bed.extra_bed_to_write)
            || beds.note_beds.is_some();
    }
    if !beds.mm_rows.is_empty() {
        return true;
    }
    let wanted = bed.bed_note.as_ref().map(|_| {
        room.occupancy
            .as_ref()
            .and_then(|o| o.extra_beds)
            .unwrap_or(0)
            .max(0)
    });
    beds.note_beds != wanted
}

fn same_text(a: Option<&str>, b: Option<&str>, ignore_case: bool) -> bool {
    let a = blank(a).unwrap_or_default();
    let b = blank(b).unwrap_or_default();
    if ignore_case {
        a.to_lowercase() == b.to_lowercase()
    } else {
        a == b
    }
}

fn changed_when_given(existing: Option<&str>, payload: Option<&str>, ignore_case: bool) -> bool {
    blank(payload).is_some() && !same_text(existing, payload, ignore_case)
}

fn guest_changed(g: &ExistingGuest, room: &Room) -> bool {
    let counts = plan_guests(room, true);
    if g.num_adult != counts.num_adult || g.num_child != counts.num_child {
        return true;
    }
    let last = tcvn3::encode(&room.guest_last_name);
    if !same_text(g.last_name.as_deref(), Some(&last), false) {
        return true;
    }
    let first = blank(Some(&room.guest_first_name)).map(|s| tcvn3::encode(&s));
    let guest = room.guest.as_ref();
    changed_when_given(g.first_name.as_deref(), first.as_deref(), false)
        || changed_when_given(
            g.phone.as_deref(),
            guest.and_then(|c| c.phone.as_deref()),
            false,
        )
        || changed_when_given(
            g.email.as_deref(),
            guest.and_then(|c| c.email.as_deref()),
            true,
        )
        || changed_when_given(
            g.nationality.as_deref(),
            guest.and_then(|c| c.nationality.as_deref()),
            true,
        )
        || guest
            .and_then(|c| c.date_of_birth)
            .is_some_and(|dob| g.birth_date != Some(dob))
        || smile_gender(guest.and_then(|c| c.gender.as_deref()))
            .is_some_and(|sg| g.gender != Some(sg == 1))
}

#[cfg(test)]
#[path = "room_diff_tests.rs"]
mod tests;
