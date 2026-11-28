//! The PMS audit wire types (Go `internal/jobmodel/pmsaudit.go`, RT-0702/RT-1480): the hub
//! asks for a read-only snapshot of the live Hotel Link folios (a date window, or every
//! folio of a few PMS keys) and classifies the bookings itself.

use dl_json::Value;

use super::decode::{i, s};
use crate::gojson::{Obj, array, string};

/// The agent capability for the read-only PMS audit; the hub sends audit requests only
/// to agents that report it.
pub const CAP_PMS_AUDIT: &str = "pms-audit";

/// `PMSAuditRequest`: one audit the hub wants. `keys` set = targeted audit.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuditRequest {
    /// `id`.
    pub id: String,
    /// `windowDays`: arrival >= hotel date - windowDays.
    pub window_days: i64,
    /// `maxRows`: folio row cap (the agent clamps too).
    pub max_rows: i64,
    /// `logRows`: newest FolioLog rows per folio (0 = none).
    pub log_rows: i64,
    /// `keys`: ExternalConfirmNumber/Booker values of a targeted audit.
    pub keys: Vec<String>,
}

impl AuditRequest {
    pub(crate) fn from_value(v: &Value) -> AuditRequest {
        AuditRequest {
            id: s(v, "id"),
            window_days: i(v, "windowDays"),
            max_rows: i(v, "maxRows"),
            log_rows: i(v, "logRows"),
            keys: v
                .get("keys")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

/// `PMSAuditLogRow`: one SMILE FolioLog row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuditLogRow {
    /// `field`.
    pub field: String,
    /// `oldValue`.
    pub old_value: String,
    /// `newValue`.
    pub new_value: String,
    /// `clerk`.
    pub clerk: String,
    /// `at`: yyyy-MM-dd HH:mm:ss (PMS server time).
    pub at: String,
}

/// `PMSAuditRate`: one FolioRate row (nightly price).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuditRate {
    /// `date`: yyyy-MM-dd.
    pub date: String,
    /// `amount`.
    pub amount: f64,
}

/// `PMSAuditFolio`: per-folio evidence. Live = `cancel_num` 0.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuditFolio {
    /// `folioNum`.
    pub folio_num: f64,
    /// `confirmNum`.
    pub confirm_num: f64,
    /// `externalConfirmNumber`.
    pub external_conf: String,
    /// `booker`.
    pub booker: String,
    /// `bookStatus`.
    pub book_status: i64,
    /// `folioStatus`.
    pub folio_status: i64,
    /// `cancelNum`.
    pub cancel_num: f64,
    /// `roomCode`.
    pub room_code: String,
    /// `roomTypeCode`.
    pub room_type_code: String,
    /// `numRoom`.
    pub num_room: i64,
    /// `arrivalDate`: yyyy-MM-dd.
    pub arrival_date: String,
    /// `departureDate`: yyyy-MM-dd.
    pub departure_date: String,
    /// `itemKey`: UserInfo1 (HL room item key).
    pub item_key: String,
    /// `guestName`.
    pub guest_name: String,
    /// `hlOrigin`: created by the HL agent.
    pub hl_origin: bool,
    /// `cancelledBy` (omitempty): "", "agent", "staff" or "unknown".
    pub cancelled_by: String,
    /// `reservationClerk`.
    pub reservation_clerk: String,
    /// `lastChangeClerkId`.
    pub last_change_clerk_id: String,
    /// `lastChangeTime`.
    pub last_change_time: String,
    /// `log` (omitempty).
    pub log: Vec<AuditLogRow>,
    /// `rates` (omitempty): FolioRate rows, targeted audits only.
    pub rates: Vec<AuditRate>,
}

/// `PMSAuditSnapshot`: the agent's answer to one request. `error`, `log_error` and
/// `warning` are never silent: the hub shows them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuditSnapshot {
    /// `requestId`.
    pub request_id: String,
    /// `takenAt`: RFC 3339 UTC.
    pub taken_at: String,
    /// `hotelDate`.
    pub hotel_date: String,
    /// `from`: first arrival date scanned.
    pub from: String,
    /// `windowDays`.
    pub window_days: i64,
    /// `keys` (omitempty).
    pub keys: Vec<String>,
    /// `truncated`: max rows reached.
    pub truncated: bool,
    /// `lastArrival`: newest arrival in `folios` when truncated.
    pub last_arrival: String,
    /// `folios`.
    pub folios: Vec<AuditFolio>,
    /// `error` (omitempty): the scan failed.
    pub error: String,
    /// `logError` (omitempty): FolioLog/FolioRate could not be read.
    pub log_error: String,
    /// `warning` (omitempty).
    pub warning: String,
}

fn folio_json(out: &mut String, x: &AuditFolio) {
    let mut o = Obj::new(out);
    o.float("folioNum", x.folio_num)
        .float("confirmNum", x.confirm_num)
        .str("externalConfirmNumber", &x.external_conf)
        .str("booker", &x.booker)
        .int("bookStatus", x.book_status)
        .int("folioStatus", x.folio_status)
        .float("cancelNum", x.cancel_num)
        .str("roomCode", &x.room_code)
        .str("roomTypeCode", &x.room_type_code)
        .int("numRoom", x.num_room)
        .str("arrivalDate", &x.arrival_date)
        .str("departureDate", &x.departure_date)
        .str("itemKey", &x.item_key)
        .str("guestName", &x.guest_name)
        .bool("hlOrigin", x.hl_origin)
        .str_omit("cancelledBy", &x.cancelled_by)
        .str("reservationClerk", &x.reservation_clerk)
        .str("lastChangeClerkId", &x.last_change_clerk_id)
        .str("lastChangeTime", &x.last_change_time);
    if !x.log.is_empty() {
        o.raw("log", |out| {
            array(out, Some(&x.log), |out, r| {
                Obj::new(out)
                    .str("field", &r.field)
                    .str("oldValue", &r.old_value)
                    .str("newValue", &r.new_value)
                    .str("clerk", &r.clerk)
                    .str("at", &r.at)
                    .end();
            })
        });
    }
    if !x.rates.is_empty() {
        o.raw("rates", |out| {
            array(out, Some(&x.rates), |out, r| {
                Obj::new(out)
                    .str("date", &r.date)
                    .float("amount", r.amount)
                    .end();
            })
        });
    }
    o.end();
}

impl AuditSnapshot {
    /// `json.Marshal(snapshot)`.
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        let mut o = Obj::new(&mut out);
        o.str("requestId", &self.request_id)
            .str("takenAt", &self.taken_at)
            .str("hotelDate", &self.hotel_date)
            .str("from", &self.from)
            .int("windowDays", self.window_days);
        if !self.keys.is_empty() {
            o.raw("keys", |out| {
                array(out, Some(&self.keys), |out, k| string(out, k))
            });
        }
        o.bool("truncated", self.truncated)
            .str("lastArrival", &self.last_arrival)
            .raw("folios", |out| array(out, Some(&self.folios), folio_json))
            .str_omit("error", &self.error)
            .str_omit("logError", &self.log_error)
            .str_omit("warning", &self.warning)
            .end();
        out
    }
}
