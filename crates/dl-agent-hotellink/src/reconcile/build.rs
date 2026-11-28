//! Pure helpers of the reconciler: item keys, room merging, folio building and dates.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use dl_time::{Date, DateTime, OffsetDateTime};

use crate::error::{Error, Result};
use crate::gofmt::{quote, scan_int};
use crate::jobmodel::{Job, PmsFolio, Room, sha256_hex};
use crate::pms::{AGENT_CLERK_ID, AdditionalName, ClientSts, Folio, FolioRate, SmileClient, ymd};

/// `itemKey`: the item a folio belongs to, from `UserInfo1` (`<key>#<seq>`); legacy folios
/// without a key fall back to the room type.
pub(crate) fn item_key(f: &Folio) -> String {
    if let Some(i) = f.user_info1.rfind('#') {
        let prefix = &f.user_info1[..i];
        if prefix.is_empty() {
            return room_item_key(&f.room_type_code, "");
        }
        return prefix.to_owned();
    }
    if !f.user_info1.is_empty() {
        return f.user_info1.clone();
    }
    room_item_key(&f.room_type_code, "")
}

/// `roomItemKey`.
pub(crate) fn room_item_key(room_type_code: &str, rate_plan_id: &str) -> String {
    if rate_plan_id.is_empty() {
        format!("rt:{room_type_code}")
    } else {
        format!("rt:{room_type_code}|rp:{rate_plan_id}")
    }
}

/// `roomKey`: the folio item key of a (merged) room, the legacy `room_item_key` plus
/// `|v:<variant>` when the booking has diverging items of that key (RT-1474).
pub(crate) fn room_key(room: &Room) -> String {
    let key = room_item_key(&room.room_type_code, &room.rate_plan_id);
    if room.variant.is_empty() {
        key
    } else {
        format!("{key}|v:{}", room.variant)
    }
}

/// `roomSignature`: the business fields written to a folio (guests, breakfast, nightly
/// rates and extras), formatted as Go's `fmt` does; an equal signature means the rooms are
/// interchangeable.
fn room_signature(r: &Room) -> String {
    let mut s = format!(
        "a{};c{};bf{};{};",
        r.adults, r.children, r.include_bf, r.breakfast_code
    );
    for rn in r.rates() {
        let _ = write!(s, "{}={:.2};", rn.date, rn.rate);
        for ex in &rn.extras {
            let _ = write!(s, "{}:{:.2},", ex.kind, ex.amount);
        }
    }
    s
}

/// `roomSignatureHash`: hex of the first 4 bytes of the signature's sha256.
pub(crate) fn room_signature_hash(r: &Room) -> String {
    let mut hex = sha256_hex(room_signature(r).as_bytes());
    hex.truncate(8);
    hex
}

/// `mergeRoomsByItem`: one room per item key (first item's details, summed quantity), in
/// first-seen order. Items sharing `room_item_key` but differing in guests, breakfast or
/// rates are not merged: each variant becomes its own group keyed `|v:<hash>` (RT-1474).
/// Keys stay legacy unless a booking really has diverging items.
pub(crate) fn merge_rooms_by_item(rooms: &[Room]) -> Vec<Room> {
    let mut variants: HashMap<String, HashSet<String>> = HashMap::new();
    for room in rooms {
        variants
            .entry(room_item_key(&room.room_type_code, &room.rate_plan_id))
            .or_default()
            .insert(room_signature(room));
    }
    let mut out: Vec<Room> = Vec::with_capacity(rooms.len());
    let mut idx: HashMap<String, usize> = HashMap::new();
    for room in rooms {
        let mut room = room.clone();
        if variants
            .get(&room_item_key(&room.room_type_code, &room.rate_plan_id))
            .is_some_and(|v| v.len() > 1)
        {
            room.variant = room_signature_hash(&room);
        }
        let key = room_key(&room);
        if let Some(&i) = idx.get(&key) {
            out[i].quantity += room.quantity;
            continue;
        }
        idx.insert(key, out.len());
        out.push(room);
    }
    out
}

/// `mergeRoomsConflicts`: item keys whose items disagree on guests, breakfast or rates.
pub(crate) fn merge_rooms_conflicts(rooms: &[Room]) -> Vec<String> {
    let sig = |r: &Room| {
        let rates: String = r
            .rates()
            .iter()
            .map(|rn| format!("{}={:.2};", rn.date, rn.rate))
            .collect();
        (
            r.adults,
            r.children,
            r.include_bf,
            r.breakfast_code.clone(),
            rates,
        )
    };
    let mut first = HashMap::new();
    let mut out: Vec<String> = Vec::new();
    for room in rooms {
        let key = room_item_key(&room.room_type_code, &room.rate_plan_id);
        let s = sig(room);
        match first.get(&key) {
            None => {
                first.insert(key, s);
            }
            Some(prev) => {
                if *prev != s && !out.contains(&key) {
                    out.push(key);
                }
            }
        }
    }
    out
}

/// `nextSeq`: one past the highest `#<seq>` among the folios (0 when none).
pub(crate) fn next_seq(folios: &[Folio]) -> i64 {
    let mut max_seq = -1;
    for f in folios {
        if let Some(i) = f.user_info1.rfind('#')
            && let Some(s) = scan_int(&f.user_info1[i + 1..])
            && s > max_seq
        {
            max_seq = s;
        }
    }
    max_seq + 1
}

/// `pickToCancel`: `n` folios, unassigned ones first.
pub(crate) fn pick_to_cancel(folios: &[Folio], n: usize) -> Vec<Folio> {
    let (unassigned, assigned): (Vec<&Folio>, Vec<&Folio>) =
        folios.iter().partition(|f| f.room_code.is_empty());
    unassigned
        .into_iter()
        .chain(assigned)
        .take(n)
        .cloned()
        .collect()
}

/// `ratesEqual`.
pub(crate) fn rates_equal(a: &[FolioRate], b: &[FolioRate]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x.h_date == y.h_date && x.rate_amount == y.rate_amount)
}

/// The nightly rates of `room` as `FolioRate` rows.
pub(crate) fn room_rates(room: &Room, folio_num: f64) -> Result<Vec<FolioRate>> {
    room.rates()
        .iter()
        .map(|rn| {
            let d = parse_day(&rn.date)
                .map_err(|e| e.context(format!("rate date {}: ", quote(&rn.date))))?;
            Ok(FolioRate {
                folio_num,
                h_date: d,
                to_date: d,
                rate_amount: rn.rate,
                rate_override: 1,
                ..FolioRate::default()
            })
        })
        .collect()
}

/// `buildFolio`. `room` must have rates (the caller checks).
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_folio(
    job: &Job,
    room: &Room,
    folio_num: f64,
    confirm_num: f64,
    check_in: DateTime,
    check_out: DateTime,
    ta: Option<&SmileClient>,
    sts: Option<&ClientSts>,
    now: DateTime,
) -> Result<(Folio, Vec<FolioRate>, AdditionalName)> {
    let mut f = Folio::new(now);
    f.folio_num = folio_num;
    // A new folio carries the agent's mark from the start (creator and last changer): the
    // SMILE FolioLog trigger concatenates LastChangeClerkID into its history row, and an
    // empty one makes the next change lose its author (RT-0702).
    f.reservation_clerk_id = AGENT_CLERK_ID.to_owned();
    f.last_change_clerk_id = AGENT_CLERK_ID.to_owned();
    f.arrival_date = check_in;
    f.departure_date = check_out;
    f.room_type_code = room.room_type_code.clone();
    f.room_type_booked = room.room_type_code.clone();
    f.num_adult = room.adults as i32;
    f.num_child = room.children as i32;
    if room.include_bf {
        f.num_guest_bf = room.adults as i32;
        f.num_child_bf = room.children as i32;
    }
    if let Some(ta) = ta {
        f.travel_agent1_code = ta.client_folio_num;
    }
    if let Some(sts) = sts {
        f.origin_code = sts.origin.clone();
        f.market_segment_code = sts.m_segment.clone();
        let [u1, u2, u3, u4, u5, u6, u7, u8] = sts.u_def.clone();
        f.user_define_code1 = u1;
        f.user_define_code2 = u2;
        f.user_define_code3 = u3;
        f.user_define_code4 = u4;
        f.user_define_code5 = u5;
        f.user_define_code6 = u6;
        f.user_define_code7 = u7;
        f.user_define_code8 = u8;
    }
    f.confirm_num = confirm_num;
    f.rate_amount = room.rates().first().map(|r| r.rate).unwrap_or_default();
    f.booker = job.booker.clone();
    f.last_name = job.guest_name.clone();
    f.tel_num_m = job.guest_phone.clone();
    f.mail_m = job.guest_email.clone();
    f.book_status = job.book_status as i32;
    f.breakfast_code = room.breakfast_code.clone();
    f.include_bf = room.include_bf;
    // The OTA's own code (ExtBookingRef) or the BookingID fallback for non-OTA bookings.
    f.external_confirm_number = Some(job.booker.clone());
    if !job.payment_code.is_empty() {
        f.payment_code = job.payment_code.clone();
    }
    let rates = room_rates(room, folio_num)?;
    let mut an = AdditionalName::new();
    an.folio_num = folio_num;
    an.last_name = f.last_name.clone();
    an.arrival_time = check_in;
    an.departure_time = check_out;
    an.confirm_num = confirm_num;
    an.a_origin_code = f.origin_code.clone();
    an.email = job.guest_email.clone();
    Ok((f, rates, an))
}

/// `folioToPMS`.
pub(crate) fn folio_to_pms(f: &Folio) -> PmsFolio {
    PmsFolio {
        folio_num: f.folio_num,
        confirm_num: f.confirm_num,
        external_conf: f.external_confirm_number.clone().unwrap_or_default(),
        book_status: i64::from(f.book_status),
        folio_status: i64::from(f.folio_status),
        cancel_num: f.cancel_num,
        room_code: f.room_code.clone(),
        room_type_code: f.room_type_code.clone(),
        arrival_date: ymd(f.arrival_date),
        departure_date: ymd(f.departure_date),
    }
}

/// `foliosToPMS`.
pub(crate) fn folios_to_pms(fs: &[Folio]) -> Vec<PmsFolio> {
    fs.iter().map(folio_to_pms).collect()
}

/// `parseDay`: `yyyy-MM-dd`, `yyyy-MM-dd HH:mm:ss` or RFC 3339 (its wall-clock time, the
/// way SQL Server `datetime` stores it).
pub(crate) fn parse_day(s: &str) -> Result<DateTime> {
    if s.len() == 10
        && let Ok(d) = Date::parse_iso(s)
    {
        return Ok(d.midnight());
    }
    if let Ok(t) = DateTime::parse(s, "yyyy-MM-dd HH:mm:ss") {
        return Ok(t);
    }
    if let Ok(t) = OffsetDateTime::parse_rfc3339(s) {
        return Ok(t.datetime());
    }
    Err(Error::other(format!("không parse được ngày {}", quote(s))))
}
