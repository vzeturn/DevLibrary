//! The counting half of `AvailabilityScanner` (and its `GroupBlocks`, `PhysicalRooms`
//! and `ExtraBedUsage` parts): rooms sold and rooms available per room type and night.
//!
//! Room-type and room codes compare ignoring case, like the .NET
//! `StringComparer.OrdinalIgnoreCase` dictionaries; a room type keeps the spelling of the
//! first room read with it.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use dl_time::Date;

use crate::smile::{GroupBlock, OccupancyStay, RoomBlock, RoomRow};
use crate::wire::{ExtraBedUsageLine, OccupancyLine};

/// Per room type (folded), per night.
pub type ByTypeNight = HashMap<String, BTreeMap<Date, i64>>;

fn fold(s: &str) -> String {
    s.trim().to_uppercase()
}

/// Physical rooms per room type, in first-seen order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoomsByType {
    order: Vec<(String, i64)>,
    index: HashMap<String, usize>,
}

impl RoomsByType {
    /// Counts `rooms` by trimmed `RoomTypeCode`.
    pub fn from_rooms(rooms: &[RoomRow]) -> RoomsByType {
        let mut out = RoomsByType::default();
        for r in rooms {
            let code = r.room_type_code.trim();
            let key = fold(code);
            match out.index.get(&key) {
                Some(&i) => out.order[i].1 += 1,
                None => {
                    out.index.insert(key, out.order.len());
                    out.order.push((code.to_owned(), 1));
                }
            }
        }
        out
    }

    /// Whether no room was read.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// The room types, in first-seen order, with their room counts.
    pub fn iter(&self) -> impl Iterator<Item = (&str, i64)> {
        self.order.iter().map(|(c, n)| (c.as_str(), *n))
    }

    fn has(&self, code: &str) -> bool {
        self.index.contains_key(&fold(code))
    }
}

/// `MapRoomToType`: trimmed room code (folded) to trimmed room type.
pub fn map_room_to_type(rooms: &[RoomRow]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for r in rooms {
        let code = r.room_code.trim();
        if !code.is_empty() {
            map.insert(fold(code), r.room_type_code.trim().to_owned());
        }
    }
    map
}

/// `OccupiedNights`: the nights of a stay inside `[from, to)` (a day use counts its
/// arrival night).
pub fn occupied_nights(arrival: Date, departure: Date, from: Date, to: Date) -> Vec<Date> {
    let last = if departure > arrival {
        departure.pred()
    } else {
        arrival
    };
    let mut night = arrival.max(from);
    let mut out = Vec::new();
    while night <= last && night < to {
        out.push(night);
        night = night.succ();
    }
    out
}

fn nights(s: &OccupancyStay, from: Date, to: Date) -> Vec<Date> {
    occupied_nights(s.arrival, s.departure, from, to)
}

fn bump(map: &mut ByTypeNight, key: &str, night: Date, by: i64) {
    *map.entry(fold(key)).or_default().entry(night).or_insert(0) += by;
}

/// `CountSoldPerNight`: stays of known room types, per night.
pub fn count_sold(
    stays: &[OccupancyStay],
    from: Date,
    to: Date,
    rooms: &RoomsByType,
) -> ByTypeNight {
    let mut sold = ByTypeNight::new();
    for s in stays.iter().filter(|s| rooms.has(&s.room_type_code)) {
        for night in nights(s, from, to) {
            bump(&mut sold, &s.room_type_code, night, 1);
        }
    }
    sold
}

/// `AddUnpickedGroupRooms`: rooms a group holds but has not picked up yet count as sold.
/// Returns the room-nights added.
pub fn add_unpicked_group_rooms(
    sold: &mut ByTypeNight,
    stays: &[OccupancyStay],
    blocks: &[GroupBlock],
    from: Date,
    to: Date,
    rooms: &RoomsByType,
) -> i64 {
    let mut held: HashMap<(String, String), BTreeMap<Date, i64>> = HashMap::new();
    for b in blocks {
        if b.num_room <= 0 || !rooms.has(&b.room_type_code) {
            continue;
        }
        let by = held
            .entry((fold(&b.group_code), fold(&b.room_type_code)))
            .or_default();
        let mut night = b.from.max(from);
        while night < b.to_exclusive && night < to {
            *by.entry(night).or_insert(0) += b.num_room;
            night = night.succ();
        }
    }
    if held.is_empty() {
        return 0;
    }
    let mut picked: HashMap<(String, String), BTreeMap<Date, i64>> = HashMap::new();
    for s in stays {
        let Some(group) = s.group_code.as_deref().filter(|g| !g.trim().is_empty()) else {
            continue;
        };
        let key = (fold(group), fold(&s.room_type_code));
        if !held.contains_key(&key) {
            continue;
        }
        let by = picked.entry(key).or_default();
        for night in nights(s, from, to) {
            *by.entry(night).or_insert(0) += 1;
        }
    }
    let mut total = 0;
    for (key, by_night) in &held {
        let picked_by = picked.get(key);
        for (night, rooms_held) in by_night {
            let unpicked = rooms_held - picked_by.and_then(|p| p.get(night)).copied().unwrap_or(0);
            if unpicked > 0 {
                bump(sold, &key.1, *night, unpicked);
                total += unpicked;
            }
        }
    }
    total
}

/// `RaiseToPhysicalOccupancy`: a room type is at least as sold as its physical rooms are
/// occupied (guests booked as another type but put in this one). Returns the room-nights
/// added.
pub fn raise_to_physical(
    sold: &mut ByTypeNight,
    stays: &[OccupancyStay],
    type_by_room: &HashMap<String, String>,
    from: Date,
    to: Date,
    rooms: &RoomsByType,
) -> i64 {
    if type_by_room.is_empty() {
        return 0;
    }
    let mut assigned: HashMap<String, BTreeMap<Date, BTreeSet<String>>> = HashMap::new();
    let mut unassigned = ByTypeNight::new();
    for s in stays {
        let room = s
            .room_code
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty());
        if let Some(r) = room
            && let Some(physical) = type_by_room.get(&fold(r))
        {
            if !rooms.has(physical) {
                continue;
            }
            let by = assigned.entry(fold(physical)).or_default();
            for night in nights(s, from, to) {
                by.entry(night).or_default().insert(fold(r));
            }
            continue;
        }
        if !rooms.has(&s.room_type_code) {
            continue;
        }
        for night in nights(s, from, to) {
            bump(&mut unassigned, &s.room_type_code, night, 1);
        }
    }
    let mut raised = 0;
    for (ty, by_night) in &assigned {
        let extra = unassigned.get(ty);
        let sold_by = sold.entry(ty.clone()).or_default();
        for (night, codes) in by_night {
            let physical =
                codes.len() as i64 + extra.and_then(|e| e.get(night)).copied().unwrap_or(0);
            let booked = sold_by.get(night).copied().unwrap_or(0);
            if physical > booked {
                sold_by.insert(*night, physical);
                raised += physical - booked;
            }
        }
    }
    raised
}

/// `CountBlockedPerNight`: out-of-service / out-of-order rooms per night, each room once.
pub fn count_blocked(
    blocks: &[RoomBlock],
    from: Date,
    to: Date,
    rooms: &RoomsByType,
) -> ByTypeNight {
    let mut by_room: HashMap<String, (String, BTreeSet<Date>)> = HashMap::new();
    for b in blocks {
        if !rooms.has(&b.room_type_code) {
            continue;
        }
        let entry = by_room
            .entry(fold(&b.room_code))
            .or_insert_with(|| (b.room_type_code.trim().to_owned(), BTreeSet::new()));
        let mut night = b.from.max(from);
        while night < b.to_exclusive && night < to {
            entry.1.insert(night);
            night = night.succ();
        }
    }
    let mut blocked = ByTypeNight::new();
    for (ty, nights) in by_room.values() {
        for night in nights {
            bump(&mut blocked, ty, *night, 1);
        }
    }
    blocked
}

/// The last pushed `(sold, total)` per room type and night.
pub type LastPushed = HashMap<(String, Date), (i64, i64)>;

/// `BuildLines`: every room type and night, or only what changed since the last push.
pub fn build_lines(
    sold: &ByTypeNight,
    blocked: &ByTypeNight,
    rooms: &RoomsByType,
    from: Date,
    to: Date,
    full: bool,
    last: &LastPushed,
) -> Vec<OccupancyLine> {
    let mut lines = Vec::new();
    for (code, physical) in rooms.iter() {
        let key = fold(code);
        let sold_by = sold.get(&key);
        let blocked_by = blocked.get(&key);
        let mut date = from;
        while date < to {
            let rooms_sold = sold_by.and_then(|m| m.get(&date)).copied().unwrap_or(0);
            let rooms_total =
                (physical - blocked_by.and_then(|m| m.get(&date)).copied().unwrap_or(0)).max(0);
            let unchanged = last.get(&(code.to_owned(), date)) == Some(&(rooms_sold, rooms_total));
            if full || !unchanged {
                lines.push(OccupancyLine {
                    room_type_code: code.to_owned(),
                    date,
                    rooms_sold,
                    rooms_total,
                });
            }
            date = date.succ();
        }
    }
    lines
}

/// `BuildExtraBedUsageLines`: `None` when the beds could not be counted.
pub fn build_extra_bed_lines(
    beds: Option<&BTreeMap<Date, i64>>,
    from: Date,
    to: Date,
    full: bool,
    last: &BTreeMap<Date, i64>,
) -> Option<Vec<ExtraBedUsageLine>> {
    let beds = beds?;
    let mut lines = Vec::new();
    let mut night = from;
    while night < to {
        let count = beds.get(&night).copied().unwrap_or(0);
        if full || last.get(&night) != Some(&count) {
            lines.push(ExtraBedUsageLine {
                date: night,
                smile_beds: count,
            });
        }
        night = night.succ();
    }
    Some(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(day: u8) -> Date {
        Date::new(2026, 4, day).unwrap()
    }

    fn room(code: &str, ty: &str) -> RoomRow {
        RoomRow {
            room_code: code.into(),
            room_type_code: ty.into(),
            ..RoomRow::default()
        }
    }

    fn stay(ty: &str, a: u8, dep: u8, group: Option<&str>, room: Option<&str>) -> OccupancyStay {
        OccupancyStay {
            room_type_code: ty.into(),
            arrival: d(a),
            departure: d(dep),
            group_code: group.map(Into::into),
            room_code: room.map(Into::into),
        }
    }

    #[test]
    fn nights_cover_the_stay_inside_the_window_and_day_use_counts_once() {
        assert_eq!(occupied_nights(d(1), d(4), d(2), d(10)), vec![d(2), d(3)]);
        assert_eq!(occupied_nights(d(5), d(5), d(1), d(10)), vec![d(5)]);
        assert!(occupied_nights(d(9), d(12), d(1), d(9)).is_empty());
    }

    #[test]
    fn sold_groups_physical_rooms_and_blocks_combine_into_lines() {
        let rooms = [room("101", "DLX "), room("102", "dlx"), room("201", "STD")];
        let by_type = RoomsByType::from_rooms(&rooms);
        assert_eq!(
            by_type.iter().collect::<Vec<_>>(),
            vec![("DLX", 2), ("STD", 1)]
        );
        let stays = [
            stay("STD", 1, 3, None, Some("101")), // booked STD, sits in a DLX room
            stay("DLX", 1, 2, Some("G1"), None),
            stay("XXX", 1, 2, None, None), // unknown type: ignored
        ];
        let (from, to) = (d(1), d(3));
        let mut sold = count_sold(&stays, from, to, &by_type);
        let groups = [GroupBlock {
            group_code: "g1".into(),
            room_type_code: "DLX".into(),
            from: d(1),
            to_exclusive: d(3),
            num_room: 2,
        }];
        assert_eq!(
            add_unpicked_group_rooms(&mut sold, &stays, &groups, from, to, &by_type),
            3
        );
        let raised = raise_to_physical(
            &mut sold,
            &stays,
            &map_room_to_type(&rooms),
            from,
            to,
            &by_type,
        );
        assert_eq!(raised, 0, "group hold already covers the DLX nights");
        let blocks = [RoomBlock {
            room_code: "201".into(),
            room_type_code: "STD".into(),
            from: d(2),
            to_exclusive: d(5),
        }];
        let blocked = count_blocked(&blocks, from, to, &by_type);
        let lines = build_lines(
            &sold,
            &blocked,
            &by_type,
            from,
            to,
            true,
            &LastPushed::new(),
        );
        let got: Vec<_> = lines
            .iter()
            .map(|l| {
                (
                    l.room_type_code.as_str(),
                    l.date.day(),
                    l.rooms_sold,
                    l.rooms_total,
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("DLX", 1, 2, 2),
                ("DLX", 2, 2, 2),
                ("STD", 1, 1, 1),
                ("STD", 2, 1, 0)
            ]
        );
        let mut last = LastPushed::new();
        for l in &lines {
            last.insert(
                (l.room_type_code.clone(), l.date),
                (l.rooms_sold, l.rooms_total),
            );
        }
        assert!(build_lines(&sold, &blocked, &by_type, from, to, false, &last).is_empty());
    }

    #[test]
    fn physical_occupancy_raises_a_type_booked_elsewhere() {
        let rooms = [room("101", "DLX"), room("201", "STD")];
        let by_type = RoomsByType::from_rooms(&rooms);
        let stays = [stay("STD", 1, 2, None, Some("101"))];
        let mut sold = count_sold(&stays, d(1), d(2), &by_type);
        let raised = raise_to_physical(
            &mut sold,
            &stays,
            &map_room_to_type(&rooms),
            d(1),
            d(2),
            &by_type,
        );
        assert_eq!(raised, 1);
        assert_eq!(sold["DLX"][&d(1)], 1);
        assert_eq!(sold["STD"][&d(1)], 1);
    }

    #[test]
    fn extra_bed_lines_are_deltas_unless_full() {
        let beds = BTreeMap::from([(d(1), 2)]);
        let last = BTreeMap::from([(d(1), 2), (d(2), 0)]);
        assert_eq!(
            build_extra_bed_lines(Some(&beds), d(1), d(3), false, &last).unwrap(),
            vec![]
        );
        assert_eq!(
            build_extra_bed_lines(Some(&beds), d(1), d(3), true, &last)
                .unwrap()
                .len(),
            2
        );
        assert!(build_extra_bed_lines(None, d(1), d(3), true, &last).is_none());
    }
}
