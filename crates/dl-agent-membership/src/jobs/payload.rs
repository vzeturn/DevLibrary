//! `JobPayload` and `JobPayloadParser`: the schema inside `SyncJobDto.PayloadJson`.
//!
//! Property names match case-insensitively. A field of the wrong type reads as absent
//! (never a dead job), exactly as the .NET parser does; only invalid JSON fails.

use dl_decimal::Decimal;
use dl_json::Value;
use dl_time::{Date, DateTime, OffsetDateTime};
use dl_uuid::Uuid;

use crate::wire::parse::{self, prop};

/// `default(DateOnly)` (0001-01-01): what .NET reads for a missing or bad date.
pub const DEFAULT_DATE: Date = match Date::new(1, 1, 1) {
    Ok(d) => d,
    Err(_) => Date::UNIX_EPOCH,
};

/// `JobContact`. Never logged (guest data).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Contact {
    /// `FullName`.
    pub full_name: Option<String>,
    /// `Phone`.
    pub phone: Option<String>,
    /// `Email`.
    pub email: Option<String>,
    /// `DateOfBirth`.
    pub date_of_birth: Option<Date>,
    /// `Gender`.
    pub gender: Option<String>,
    /// `Nationality` (or `Country` for the member block).
    pub nationality: Option<String>,
}

impl std::fmt::Debug for Contact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JobContact { *** }")
    }
}

/// `JobVoucher`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Voucher {
    /// `VoucherId`.
    pub voucher_id: Uuid,
    /// `Code`.
    pub code: String,
    /// `Type` (`VoucherType.ToString()`).
    pub kind: String,
    /// `Value`.
    pub value: Decimal,
    /// `MaxDiscountAmount`.
    pub max_discount_amount: Option<Decimal>,
}

/// `JobReward`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reward {
    /// `GrantId`.
    pub grant_id: Uuid,
    /// `RewardId`.
    pub reward_id: Uuid,
    /// `Kind`.
    pub kind: String,
    /// `RuleJson`.
    pub rule_json: Option<String>,
    /// `Value`.
    pub value: Option<Decimal>,
}

/// `JobNight`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Night {
    /// `Date`.
    pub date: Date,
    /// `Net`.
    pub net: Decimal,
    /// `NetExclBed`.
    pub net_excl_bed: Option<Decimal>,
}

/// `JobRoomOccupancy` (ADR-0104).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Occupancy {
    /// `PricingModel`.
    pub pricing_model: Option<String>,
    /// `ExtraBeds`.
    pub extra_beds: Option<i32>,
    /// `ExtraBedNightly`.
    pub extra_bed_nightly: Option<Decimal>,
    /// `ExtraBedNights` (null when any item is not a number).
    pub extra_bed_nights: Option<Vec<Decimal>>,
    /// `Adults`.
    pub adults: Option<i32>,
    /// `ChildrenHalf`.
    pub children_half: Option<i32>,
    /// `ChildrenFree`.
    pub children_free: Option<i32>,
    /// `NumEnf`.
    pub num_enf: Option<i32>,
}

/// `JobRoom`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Room {
    /// `RoomItemId` (`Folio.UserInfo1`).
    pub room_item_id: Uuid,
    /// `RoomTypeCode`.
    pub room_type_code: String,
    /// `Arrival`.
    pub arrival: Date,
    /// `Departure`.
    pub departure: Date,
    /// `NumAdult`.
    pub num_adult: i32,
    /// `NumChild`.
    pub num_child: i32,
    /// `GuestFirstName`.
    pub guest_first_name: String,
    /// `GuestLastName`.
    pub guest_last_name: String,
    /// `Nights`.
    pub nights: Vec<Night>,
    /// `Removed` (MODIFY).
    pub removed: bool,
    /// `Guest`.
    pub guest: Option<Contact>,
    /// `Occupancy`.
    pub occupancy: Option<Occupancy>,
}

/// `JobExpectedFolio`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedFolio {
    /// `RoomItemId` (nil when missing).
    pub room_item_id: Uuid,
    /// `FolioNum`.
    pub folio_num: Decimal,
}

/// `JobPayload`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobPayload {
    /// `Action`.
    pub action: String,
    /// `BookingId` (nil when missing).
    pub booking_id: Uuid,
    /// `ConfirmationCode`.
    pub confirmation_code: String,
    /// `MemberId`.
    pub member_id: Option<Uuid>,
    /// `Voucher`.
    pub voucher: Option<Voucher>,
    /// `Reward`.
    pub reward: Option<Reward>,
    /// `Rooms`.
    pub rooms: Vec<Room>,
    /// `TravelAgentCode` (positive only).
    pub travel_agent_code: Option<i64>,
    /// `Member`.
    pub member: Option<Contact>,
    /// `MemberCode`.
    pub member_code: Option<String>,
    /// `ExpectedFolios` (`None` when empty).
    pub expected_folios: Option<Vec<ExpectedFolio>>,
    /// `SmileConfirmNum` (positive only).
    pub smile_confirm_num: Option<Decimal>,
    /// `ForceCreate`.
    pub force_create: bool,
    /// `SpecialRequests` (PII-capable; never logged).
    pub special_requests: Option<String>,
    /// `ExtraBedVerifiedFingerprint`.
    pub extra_bed_verified_fingerprint: Option<String>,
    /// `ExtraBedSpecialCode`.
    pub extra_bed_special_code: Option<String>,
}

impl JobPayload {
    /// `IsKnownInSmile`.
    pub fn is_known_in_smile(&self) -> bool {
        self.expected_folios.as_ref().is_some_and(|f| !f.is_empty())
            || self.smile_confirm_num.is_some_and(Decimal::is_positive)
    }
}

/// `JobLinkFolio`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkFolio {
    /// `FolioNum`.
    pub folio_num: Decimal,
    /// `BookingRoomId`.
    pub booking_room_id: Uuid,
    /// `RoomLabel`.
    pub room_label: Option<String>,
}

/// `JobLinkFolioPayload`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkFolioPayload {
    /// `BookingId`.
    pub booking_id: Uuid,
    /// `MemberId`.
    pub member_id: Uuid,
    /// `ExternalConfirmNumber`.
    pub external_confirm_number: String,
    /// `ConfirmNum`.
    pub confirm_num: Decimal,
    /// `Folios`.
    pub folios: Vec<LinkFolio>,
}

fn root(json: &str) -> Result<Value, String> {
    parse::json(json.as_bytes()).map_err(|e| e.0)
}

/// `JobPayloadParser.Parse`.
///
/// # Errors
/// Invalid JSON.
pub fn parse_payload(json: &str) -> Result<JobPayload, String> {
    let r = root(json)?;
    let obj = |name| prop(&r, name).filter(|v| v.is_object());
    Ok(JobPayload {
        action: get_string(&r, "action").unwrap_or_default(),
        booking_id: get_guid(&r, "bookingId").unwrap_or(Uuid::NIL),
        confirmation_code: get_string(&r, "confirmationCode").unwrap_or_default(),
        member_id: get_guid(&r, "memberId"),
        voucher: obj("voucher").map(parse_voucher),
        reward: obj("reward").map(parse_reward),
        rooms: prop(&r, "rooms")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(parse_room).collect())
            .unwrap_or_default(),
        travel_agent_code: number(&r, "travelAgentCode")
            .and_then(Decimal::to_i64)
            .filter(|n| *n > 0)
            .filter(|_| is_integer(&r, "travelAgentCode")),
        member: obj("member").map(|m| parse_contact(m, "Country")),
        member_code: trimmed(&r, "memberCode"),
        expected_folios: parse_expected(&r),
        smile_confirm_num: positive_decimal(&r, "smileConfirmNum"),
        force_create: get_bool(&r, "forceCreate"),
        special_requests: trimmed(&r, "specialRequests"),
        extra_bed_verified_fingerprint: trimmed(&r, "extraBedVerifiedFingerprint"),
        extra_bed_special_code: trimmed(&r, "extraBedSpecialCode"),
    })
}

/// `JobPayloadParser.ParseLinkFolio`.
///
/// # Errors
/// Invalid JSON, or the `FormatException` messages of .NET.
pub fn parse_link_folio(json: &str) -> Result<LinkFolioPayload, String> {
    let r = root(json)?;
    let booking_id = get_guid(&r, "bookingId")
        .filter(|g| !g.is_nil())
        .ok_or("LINK_FOLIO payload has no BookingId.")?;
    let member_id = get_guid(&r, "memberId")
        .filter(|g| !g.is_nil())
        .ok_or("LINK_FOLIO payload has no MemberId.")?;
    let ext = trimmed(&r, "externalConfirmNumber")
        .or_else(|| trimmed(&r, "confirmationCode"))
        .ok_or("LINK_FOLIO payload has no ExternalConfirmNumber.")?;
    let confirm_num = positive_decimal(&r, "confirmNum")
        .ok_or("LINK_FOLIO payload has no positive ConfirmNum.")?;
    let mut folios = Vec::new();
    for f in prop(&r, "folios")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let num = f
            .is_object()
            .then(|| positive_decimal(f, "FolioNum"))
            .flatten();
        let room = f
            .is_object()
            .then(|| get_guid(f, "BookingRoomId"))
            .flatten();
        match (num, room) {
            (Some(n), Some(g)) if !g.is_nil() => folios.push(LinkFolio {
                folio_num: n,
                booking_room_id: g,
                room_label: trimmed(f, "RoomLabel"),
            }),
            _ => {
                return Err(
                    "LINK_FOLIO payload has a folio without FolioNum or BookingRoomId.".into(),
                );
            }
        }
    }
    if folios.is_empty() {
        return Err("LINK_FOLIO payload lists no folio.".into());
    }
    Ok(LinkFolioPayload {
        booking_id,
        member_id,
        external_confirm_number: ext,
        confirm_num,
        folios,
    })
}

fn parse_expected(r: &Value) -> Option<Vec<ExpectedFolio>> {
    let items = prop(r, "expectedFolios")?.as_array()?;
    let list: Vec<_> = items
        .iter()
        .filter(|f| f.is_object())
        .filter_map(|f| {
            Some(ExpectedFolio {
                folio_num: positive_decimal(f, "FolioNum")?,
                room_item_id: get_guid(f, "RoomItemId").unwrap_or(Uuid::NIL),
            })
        })
        .collect();
    (!list.is_empty()).then_some(list)
}

fn parse_contact(c: &Value, nationality_key: &str) -> Contact {
    let dob = get_date(c, "DateOfBirth");
    Contact {
        full_name: trimmed(c, "FullName"),
        phone: trimmed(c, "Phone"),
        email: trimmed(c, "Email"),
        date_of_birth: (dob != DEFAULT_DATE).then_some(dob),
        gender: trimmed(c, "Gender"),
        nationality: trimmed(c, nationality_key).or_else(|| trimmed(c, "Nationality")),
    }
}

fn parse_voucher(v: &Value) -> Voucher {
    Voucher {
        voucher_id: get_guid(v, "VoucherId").unwrap_or(Uuid::NIL),
        code: get_string(v, "Code").unwrap_or_default(),
        kind: get_string(v, "Type").unwrap_or_default(),
        value: number(v, "Value").unwrap_or(Decimal::ZERO),
        max_discount_amount: number(v, "MaxDiscountAmount"),
    }
}

fn parse_reward(r: &Value) -> Reward {
    Reward {
        grant_id: get_guid(r, "GrantId").unwrap_or(Uuid::NIL),
        reward_id: get_guid(r, "RewardId").unwrap_or(Uuid::NIL),
        kind: get_string(r, "Kind").unwrap_or_default(),
        rule_json: get_string(r, "RuleJson"),
        value: number(r, "Value"),
    }
}

fn parse_room(r: &Value) -> Room {
    let nights = prop(r, "nights")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|n| Night {
                    date: get_date(n, "Date"),
                    net: number(n, "Net").unwrap_or(Decimal::ZERO),
                    net_excl_bed: number(n, "NetExclBed"),
                })
                .collect()
        })
        .unwrap_or_default();
    Room {
        room_item_id: get_guid(r, "Id")
            .or_else(|| get_guid(r, "RoomItemId"))
            .unwrap_or(Uuid::NIL),
        room_type_code: get_string(r, "RoomTypeCode").unwrap_or_default(),
        arrival: get_date(r, "Arrival"),
        departure: get_date(r, "Departure"),
        num_adult: get_i32(r, "NumAdult").unwrap_or(0),
        num_child: get_i32(r, "NumChild").unwrap_or(0),
        guest_first_name: get_string(r, "GuestFirstName").unwrap_or_default(),
        guest_last_name: get_string(r, "GuestLastName").unwrap_or_default(),
        nights,
        removed: get_bool(r, "Removed"),
        guest: prop(r, "guest")
            .filter(|g| g.is_object())
            .map(|g| parse_contact(g, "Nationality")),
        occupancy: parse_occupancy(r),
    }
}

fn parse_occupancy(r: &Value) -> Option<Occupancy> {
    let model = trimmed(r, "PricingModel");
    let beds = get_i32(r, "ExtraBeds");
    if model.is_none() && beds.is_none() {
        return None;
    }
    let per_night = prop(r, "ExtraBedNights")
        .and_then(Value::as_array)
        .and_then(|a| a.iter().map(as_decimal).collect::<Option<Vec<_>>>());
    Some(Occupancy {
        pricing_model: model,
        extra_beds: beds,
        extra_bed_nightly: number(r, "ExtraBedNightly"),
        extra_bed_nights: per_night,
        adults: get_i32(r, "Adults"),
        children_half: get_i32(r, "ChildrenHalf"),
        children_free: get_i32(r, "ChildrenFree"),
        num_enf: get_i32(r, "NumEnf"),
    })
}

fn get_string(v: &Value, name: &str) -> Option<String> {
    prop(v, name).and_then(Value::as_str).map(str::to_owned)
}

/// `Trimmed`: a non-blank string, trimmed.
fn trimmed(v: &Value, name: &str) -> Option<String> {
    get_string(v, name)
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_owned())
}

fn get_guid(v: &Value, name: &str) -> Option<Uuid> {
    get_string(v, name).and_then(|s| s.trim().parse().ok())
}

fn as_decimal(v: &Value) -> Option<Decimal> {
    match v {
        Value::Number(n) => n
            .as_raw_str()
            .map(str::to_owned)
            .or_else(|| n.as_i64().map(|i| i.to_string()))
            .and_then(|s| s.parse().ok()),
        _ => None,
    }
}

/// A JSON number property as a decimal (`TryGetDecimal`).
fn number(v: &Value, name: &str) -> Option<Decimal> {
    prop(v, name).and_then(as_decimal)
}

fn is_integer(v: &Value, name: &str) -> bool {
    match prop(v, name) {
        Some(Value::Number(n)) => n.as_raw_str().is_none_or(|s| !s.contains(['.', 'e', 'E'])),
        _ => false,
    }
}

fn positive_decimal(v: &Value, name: &str) -> Option<Decimal> {
    number(v, name).filter(|d| d.is_positive())
}

/// `TryGetInt32` on a number property.
fn get_i32(v: &Value, name: &str) -> Option<i32> {
    if !is_integer(v, name) {
        return None;
    }
    number(v, name)?
        .to_i64()
        .and_then(|i| i32::try_from(i).ok())
}

/// `true`, or a string `bool.TryParse` reads as true.
fn get_bool(v: &Value, name: &str) -> bool {
    match prop(v, name) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s.trim().eq_ignore_ascii_case("true"),
        _ => false,
    }
}

/// `GetDate`: `yyyy-MM-dd`, or an ISO date-time; anything else is [`DEFAULT_DATE`].
fn get_date(v: &Value, name: &str) -> Date {
    let Some(s) = get_string(v, name) else {
        return DEFAULT_DATE;
    };
    let s = s.trim();
    Date::parse_iso(s)
        .ok()
        .or_else(|| DateTime::parse_iso(s).ok().map(DateTime::date))
        .or_else(|| {
            OffsetDateTime::parse_rfc3339(s)
                .ok()
                .map(|o| o.datetime().date())
        })
        .unwrap_or(DEFAULT_DATE)
}

#[cfg(test)]
#[path = "payload_tests.rs"]
mod tests;
