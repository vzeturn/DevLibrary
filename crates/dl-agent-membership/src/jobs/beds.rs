//! Extra beds (`FolioWriter.ExtraBed`, `ExtraBedWire`) and the guest-count plan
//! (`FolioWriter.GuestCounts`).
//!
//! Only the fold decision is ported: the bed stays in the room rate and a TCVN3 note
//! (`MM: +N giường phụ (đã gộp trong giá phòng)`) records it. The `SMILE_SPECIAL` preflight
//! is not ported, so a hotel configured for it folds with `FINGERPRINT_UNVERIFIED`, which
//! is the .NET agent's own safe fallback when the preflight cannot verify SMILE.

use dl_decimal::Decimal;

use super::payload::{Night, Room};
use crate::writer::tcvn3;

/// `ExtraBedWire.PricingModelOccupancyV1`.
pub const PRICING_OCCUPANCY_V1: &str = "OCCUPANCY_V1";
/// `ExtraBedWire.ModeFoldLegacy`.
pub const MODE_FOLD_LEGACY: &str = "FOLD_LEGACY";
/// `ExtraBedWire.ModeSmileSpecial`.
pub const MODE_SMILE_SPECIAL: &str = "SMILE_SPECIAL";
/// `FoldConfiguredFold`.
pub const FOLD_CONFIGURED: &str = "CONFIGURED_FOLD";
/// `FoldPayloadLegacy`.
pub const FOLD_PAYLOAD_LEGACY: &str = "PAYLOAD_LEGACY";
/// `FoldFingerprintUnverified`.
pub const FOLD_FINGERPRINT_UNVERIFIED: &str = "FINGERPRINT_UNVERIFIED";

const BED_NOTE_TAIL: &str = " giường phụ (đã gộp trong giá phòng)";
const NOTE_SEPARATOR: &str = " | ";

/// `SpecialBedRow`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecialBedRow {
    /// `Qty`.
    pub qty: i32,
    /// `Price`.
    pub price: Decimal,
    /// `FromDay`.
    pub from_day: i32,
    /// `ToDay`.
    pub to_day: i32,
}

/// `ExtraBedWriteDecision` (fold only: no special rows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BedDecision {
    /// `ExtraBedToWrite`.
    pub extra_bed_to_write: i32,
    /// `UseNetExclBed`.
    pub use_net_excl_bed: bool,
    /// `BedNote` (Unicode).
    pub bed_note: Option<String>,
    /// `Mode`.
    pub mode: &'static str,
    /// `FoldReason`.
    pub fold_reason: Option<&'static str>,
    /// `SpecialRows`.
    pub special_rows: Vec<SpecialBedRow>,
}

impl BedDecision {
    /// `IsSpecial`.
    pub fn is_special(&self) -> bool {
        self.mode == MODE_SMILE_SPECIAL
    }
}

/// `ExistingFolioBeds`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingBeds {
    /// `ExtraBed`.
    pub extra_bed: Option<i32>,
    /// Rows the agent wrote (`SPNotes` starts with `MM:{room} `).
    pub mm_rows: Vec<SpecialBedRow>,
    /// Quantity on other rows with the hotel's extra-bed code.
    pub foreign_qty: i32,
    /// Beds in the note marker.
    pub note_beds: Option<i32>,
    /// Whether `Folio_Special` was read.
    pub special_known: bool,
}

impl ExistingBeds {
    /// `SpecialQtyFound`.
    pub fn special_qty_found(&self) -> Option<i32> {
        self.special_known
            .then(|| self.mm_rows.iter().map(|r| r.qty).sum::<i32>() + self.foreign_qty)
    }
}

/// `IsOccupancyPayload`.
pub fn is_occupancy_payload(room: &Room) -> bool {
    room.occupancy.as_ref().is_some_and(|o| {
        o.pricing_model.as_deref() == Some(PRICING_OCCUPANCY_V1)
            && o.extra_beds.is_some()
            && !room.nights.is_empty()
            && room.nights.iter().all(|n| n.net_excl_bed.is_some())
            && (o.extra_beds == Some(0) || o.extra_bed_nightly.is_some())
    })
}

/// `BedNoteText`.
pub fn bed_note_text(beds: i32) -> String {
    format!("MM: +{beds}{BED_NOTE_TAIL}")
}

/// `DecideFold`.
pub fn decide_fold(room: &Room, configured_mode: &str) -> BedDecision {
    let beds = room
        .occupancy
        .as_ref()
        .and_then(|o| o.extra_beds)
        .unwrap_or(0)
        .max(0);
    let reason = if !is_occupancy_payload(room) {
        FOLD_PAYLOAD_LEGACY
    } else if configured_mode != MODE_SMILE_SPECIAL {
        FOLD_CONFIGURED
    } else {
        FOLD_FINGERPRINT_UNVERIFIED
    };
    BedDecision {
        extra_bed_to_write: 0,
        use_net_excl_bed: false,
        bed_note: (beds > 0).then(|| bed_note_text(beds)),
        mode: MODE_FOLD_LEGACY,
        fold_reason: Some(reason),
        special_rows: Vec::new(),
    }
}

/// `NightRate`.
pub fn night_rate(n: &Night, use_net_excl_bed: bool) -> Decimal {
    match n.net_excl_bed {
        Some(x) if use_net_excl_bed => x,
        _ => n.net,
    }
}

fn encoded_tail() -> String {
    tcvn3::encode(BED_NOTE_TAIL)
}

/// The span of the encoded bed marker in `s` (with a leading `" | "` separator when
/// present): `(?:\s*\|\s*)?MM: \+\d+{tail}`.
fn marker_span(s: &str) -> Option<(usize, usize, usize)> {
    let tail = encoded_tail();
    let mut from = 0;
    while let Some(i) = s[from..].find("MM: +") {
        let start = from + i;
        let digits_at = start + 5;
        let digits = s[digits_at..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        if digits > 0 && s[digits_at + digits..].starts_with(&tail) {
            let end = digits_at + digits + tail.len();
            let before = s[..start].trim_end();
            let lead = if let Some(b) = before.strip_suffix('|') {
                b.trim_end().len()
            } else {
                start
            };
            return Some((lead, start, end));
        }
        from = start + 1;
    }
    None
}

/// `StripBedNote`: the encoded note without any bed marker; `None` when nothing is left.
pub fn strip_bed_note(encoded: Option<&str>) -> Option<String> {
    let mut s = encoded.filter(|e| !e.is_empty())?.to_owned();
    while let Some((lead, _, end)) = marker_span(&s) {
        s.replace_range(lead..end, "");
    }
    let rest = s.trim();
    (!rest.is_empty()).then(|| rest.to_owned())
}

/// `ExtractBedMarker`.
pub fn extract_bed_marker(encoded: Option<&str>) -> Option<String> {
    let s = encoded?;
    let (_, start, end) = marker_span(s)?;
    Some(s[start..end].to_owned())
}

/// `NoteBedCount_`: the bed count of the first `MM: +N ` in the note.
pub fn note_bed_count(encoded: Option<&str>) -> Option<i32> {
    let s = encoded?;
    let mut from = 0;
    while let Some(i) = s[from..].find("MM: +") {
        let at = from + i + 5;
        let digits: String = s[at..].chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty() && s[at + digits.len()..].starts_with(' ') {
            return digits.parse().ok();
        }
        from = at;
    }
    None
}

fn cut(s: &str, max: Option<usize>) -> String {
    match max {
        Some(m) if s.chars().count() > m => s.chars().take(m).collect(),
        _ => s.to_owned(),
    }
}

/// `ComposeNote`: the guest note (encoded) with the bed marker appended, cut to fit.
pub fn compose_note(
    encoded_base: Option<&str>,
    bed_note: Option<&str>,
    max_length: Option<usize>,
    encoded_marker: Option<&str>,
) -> Option<String> {
    let marker = encoded_marker
        .map(str::to_owned)
        .or_else(|| bed_note.map(tcvn3::encode));
    let text = strip_bed_note(encoded_base);
    let Some(marker) = marker else {
        return text.map(|t| cut(&t, max_length));
    };
    let Some(text) = text else {
        return Some(cut(&marker, max_length));
    };
    let Some(max) = max_length else {
        return Some(format!("{text}{NOTE_SEPARATOR}{marker}"));
    };
    let room = max as i64 - marker.chars().count() as i64 - NOTE_SEPARATOR.len() as i64;
    if room <= 0 {
        return Some(cut(&marker, max_length));
    }
    Some(format!(
        "{}{NOTE_SEPARATOR}{marker}",
        cut(&text, Some(room as usize)).trim_end()
    ))
}

/// `GuestPlan`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestPlan {
    /// `NumAdult`.
    pub num_adult: i32,
    /// `NumChild`.
    pub num_child: i32,
    /// `NumEnf`.
    pub num_enf: i32,
    /// `NumEnfUpdate`.
    pub num_enf_update: Option<i32>,
    /// `PlaceholderAdults`.
    pub placeholder_adults: i32,
    /// `PlaceholderChildren`.
    pub placeholder_children: i32,
    /// `Mismatch` (`PMS_GUEST_COUNT_MISMATCH`).
    pub mismatch: Option<String>,
}

/// `PMS_GUEST_COUNT_MISMATCH`.
pub const GUEST_COUNT_MISMATCH: &str = "PMS_GUEST_COUNT_MISMATCH";

/// `PlanGuests`.
pub fn plan_guests(room: &Room, can_mark_children: bool) -> GuestPlan {
    let adults = room.num_adult;
    let children = room.num_child;
    let occ = room.occupancy.as_ref();
    let enf = occ.and_then(|o| o.num_enf).unwrap_or(0).max(0);
    let p_adults = adults.max(1);
    let p_children = if can_mark_children {
        children.max(0) + enf
    } else {
        0
    };
    let written = adults + children + enf;
    let mut mismatch = None;
    if can_mark_children && p_adults + p_children != written {
        mismatch = Some(format!(
            "{} placeholders for NumAdult {adults} + NumChild {children} + NumEnf {enf}",
            p_adults + p_children
        ));
    } else if can_mark_children
        && let Some(o) = occ
        && let (Some(a), Some(half), Some(free)) = (o.adults, o.children_half, o.children_free)
        && (a != adults || half + free != children + enf)
    {
        mismatch = Some(format!(
            "payload party {a} adults + {half} + {free} children vs NumAdult {adults} + NumChild {children} + NumEnf {enf}"
        ));
    }
    if mismatch.is_some() {
        return GuestPlan {
            num_adult: adults,
            num_child: children + enf,
            num_enf: 0,
            num_enf_update: occ.map(|_| 0),
            placeholder_adults: p_adults,
            placeholder_children: 0,
            mismatch,
        };
    }
    GuestPlan {
        num_adult: adults,
        num_child: children,
        num_enf: enf,
        num_enf_update: occ.map(|_| enf),
        placeholder_adults: p_adults,
        placeholder_children: p_children,
        mismatch: None,
    }
}

#[cfg(test)]
#[path = "beds_tests.rs"]
mod tests;
