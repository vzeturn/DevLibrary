//! The earn bodies (`MM.Membership.Application` `AgentDtos`): field for field in the order
//! of the .NET records, serialized through [`crate::stj`] the way `HubJson.Options` writes
//! them (PascalCase names, `null` written, enums as PascalCase names).

use dl_decimal::Decimal;
use dl_time::{Date, OffsetDateTime};
use dl_uuid::Uuid;

use crate::stj::J;
use crate::wire::StayClassification;
use crate::wire::parse::{self, DecodeError, bool_prop, int_prop, prop, str_prop, uuid_prop};

/// `CheckedOutAtSources.PmsCheckoutTime`.
pub const SOURCE_PMS_CHECKOUT_TIME: &str = "PMS_CHECKOUT_TIME";
/// `CheckedOutAtSources.AgentClock`.
pub const SOURCE_AGENT_CLOCK: &str = "AGENT_CLOCK";

/// `AgentFolioRevenueLine` (also `FolioTransactionLine`, the same four columns).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevenueLine {
    /// `TransactionCode`.
    pub transaction_code: String,
    /// `TransactionSubCode`, `'0'` for a blank one.
    pub transaction_sub_code: String,
    /// `TransactionCode.Description`.
    pub description: Option<String>,
    /// `TransactionAmount`.
    pub amount: Decimal,
}

impl RevenueLine {
    fn to_j(&self) -> J {
        J::Obj(vec![
            ("TransactionCode", J::str(&self.transaction_code)),
            ("TransactionSubCode", J::str(&self.transaction_sub_code)),
            ("Description", J::opt_str(self.description.as_deref())),
            ("Amount", J::Dec(self.amount)),
        ])
    }
}

fn lines_j(lines: Option<&[RevenueLine]>) -> J {
    lines.map_or(J::Null, |l| {
        J::Arr(l.iter().map(RevenueLine::to_j).collect())
    })
}

/// `AgentStayClassification`.
pub fn classification_j(c: Option<&StayClassification>) -> J {
    c.map_or(J::Null, |c| {
        J::Obj(vec![
            ("OriginCodes", J::strs(&c.origin_codes)),
            ("SegmentCodes", J::strs(&c.segment_codes)),
            ("RateCodes", J::strs(&c.rate_codes)),
        ])
    })
}

/// `AgentEarnReport` (`POST api/agent/earn`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EarnReport {
    /// `BookingId`.
    pub booking_id: Uuid,
    /// `FolioRef` (the booking's confirmation code).
    pub folio_ref: String,
    /// `QualifyingRevenue`.
    pub qualifying_revenue: Decimal,
    /// `CheckedOutAt`.
    pub checked_out_at: OffsetDateTime,
    /// `FolioLines` (`None` when the breakdown is empty or failed).
    pub folio_lines: Option<Vec<RevenueLine>>,
    /// `Classification`.
    pub classification: Option<StayClassification>,
    /// `DbIdentityFingerprint` (filled by the hub client when `None`).
    pub db_identity_fingerprint: Option<String>,
    /// `CheckedOutAtSource`.
    pub checked_out_at_source: Option<String>,
}

impl EarnReport {
    /// The JSON value.
    pub fn to_j(&self) -> J {
        J::Obj(vec![
            ("BookingId", J::uuid(self.booking_id)),
            ("FolioRef", J::str(&self.folio_ref)),
            ("QualifyingRevenue", J::Dec(self.qualifying_revenue)),
            ("CheckedOutAt", J::dto(self.checked_out_at)),
            ("FolioLines", lines_j(self.folio_lines.as_deref())),
            (
                "Classification",
                classification_j(self.classification.as_ref()),
            ),
            (
                "DbIdentityFingerprint",
                J::opt_str(self.db_identity_fingerprint.as_deref()),
            ),
            (
                "CheckedOutAtSource",
                J::opt_str(self.checked_out_at_source.as_deref()),
            ),
        ])
    }
}

/// `AgentWalkInEarnReport` (`POST api/agent/earn/walk-in`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkInEarnReport {
    /// `GuestPhone` (raw, as SMILE holds it).
    pub guest_phone: String,
    /// `FolioRef` (`CONF:{ConfirmNum}`).
    pub folio_ref: String,
    /// `QualifyingRevenue`.
    pub qualifying_revenue: Decimal,
    /// `CheckedOutAt`.
    pub checked_out_at: OffsetDateTime,
    /// `GuestName`.
    pub guest_name: Option<String>,
    /// `FolioLines`.
    pub folio_lines: Option<Vec<RevenueLine>>,
    /// `Classification`.
    pub classification: Option<StayClassification>,
    /// `DbIdentityFingerprint`.
    pub db_identity_fingerprint: Option<String>,
}

impl WalkInEarnReport {
    /// The JSON value.
    pub fn to_j(&self) -> J {
        J::Obj(vec![
            ("GuestPhone", J::str(&self.guest_phone)),
            ("FolioRef", J::str(&self.folio_ref)),
            ("QualifyingRevenue", J::Dec(self.qualifying_revenue)),
            ("CheckedOutAt", J::dto(self.checked_out_at)),
            ("GuestName", J::opt_str(self.guest_name.as_deref())),
            ("FolioLines", lines_j(self.folio_lines.as_deref())),
            (
                "Classification",
                classification_j(self.classification.as_ref()),
            ),
            (
                "DbIdentityFingerprint",
                J::opt_str(self.db_identity_fingerprint.as_deref()),
            ),
        ])
    }
}

/// `AgentFolioRevenueReport` (`POST api/agent/folio-revenue`).
pub fn folio_revenue_j(folio_ref: &str, lines: &[RevenueLine]) -> J {
    J::Obj(vec![
        ("FolioRef", J::str(folio_ref)),
        ("Lines", lines_j(Some(lines))),
    ])
}

/// `AgentPmsCancelReport` (`POST api/agent/bookings/pms-cancelled`).
pub fn pms_cancel_j(booking_id: Uuid, confirmation_code: &str) -> J {
    J::Obj(vec![
        ("BookingId", J::uuid(booking_id)),
        ("ConfirmationCode", J::str(confirmation_code)),
    ])
}

/// `AgentCheckoutScanSummaryReport` (`POST api/agent/checkout-scan-summary`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanSummary {
    /// `BusinessDate` (hotel calendar).
    pub business_date: Date,
    /// `FoliosSeen`.
    pub folios_seen: i64,
    /// `FoliosReported`.
    pub folios_reported: i64,
    /// `FoliosSkipped`.
    pub folios_skipped: i64,
    /// `SkipReasons`, in first-seen order.
    pub skip_reasons: Vec<(String, i64)>,
    /// `AgentVersion`.
    pub agent_version: Option<String>,
}

impl ScanSummary {
    /// The JSON value.
    pub fn to_j(&self) -> J {
        J::Obj(vec![
            ("BusinessDate", J::date(self.business_date)),
            ("FoliosSeen", J::Int(self.folios_seen)),
            ("FoliosReported", J::Int(self.folios_reported)),
            ("FoliosSkipped", J::Int(self.folios_skipped)),
            (
                "SkipReasons",
                J::Map(
                    self.skip_reasons
                        .iter()
                        .map(|(k, n)| (k.clone(), J::Int(*n)))
                        .collect(),
                ),
            ),
            ("AgentVersion", J::opt_str(self.agent_version.as_deref())),
        ])
    }
}

/// `AgentPendingBooking`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingBooking {
    /// `BookingId`.
    pub booking_id: Uuid,
    /// `ConfirmationCode`.
    pub confirmation_code: String,
}

/// `GET api/agent/bookings/pending-checkout`: a `null` or empty body is an empty list.
pub fn pending_bookings(body: &[u8]) -> Result<Vec<PendingBooking>, DecodeError> {
    let body = body.trim_ascii();
    if body.is_empty() || body == b"null" {
        return Ok(Vec::new());
    }
    let v = parse::json(body)?;
    let items = v
        .as_array()
        .ok_or_else(|| DecodeError("pending-checkout is not an array".into()))?;
    items
        .iter()
        .map(|item| {
            Ok(PendingBooking {
                booking_id: uuid_prop(item, "BookingId")
                    .ok_or_else(|| DecodeError("pending booking without BookingId".into()))?,
                confirmation_code: str_prop(item, "ConfirmationCode").ok_or_else(|| {
                    DecodeError("pending booking without ConfirmationCode".into())
                })?,
            })
        })
        .collect()
}

/// `AgentPmsCancelResult.Cancelled`; an empty body is an error (`"trả thân rỗng"`).
pub fn pms_cancel_result(body: &[u8]) -> Result<bool, DecodeError> {
    let body = body.trim_ascii();
    if body.is_empty() || body == b"null" {
        return Err(DecodeError(
            "bookings/pms-cancelled answered an empty body".into(),
        ));
    }
    Ok(bool_prop(&parse::json(body)?, "Cancelled"))
}

/// `AgentWalkInEarnOutcome`, in .NET order (the numbers are its integer values).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkInOutcome {
    /// 0: points awarded.
    Awarded,
    /// 1: this folio already earned.
    Duplicate,
    /// 2: no longer returned by the hub.
    PhoneNotRecognized,
    /// 3: not a callable number.
    InvalidPhone,
    /// 4: too little revenue (or before the hotel's earn start).
    NoPoints,
    /// 5: parked in `pending_earn` until the phone's owner signs up.
    Parked,
}

const OUTCOMES: [(WalkInOutcome, &str); 6] = [
    (WalkInOutcome::Awarded, "Awarded"),
    (WalkInOutcome::Duplicate, "Duplicate"),
    (WalkInOutcome::PhoneNotRecognized, "PhoneNotRecognized"),
    (WalkInOutcome::InvalidPhone, "InvalidPhone"),
    (WalkInOutcome::NoPoints, "NoPoints"),
    (WalkInOutcome::Parked, "Parked"),
];

impl WalkInOutcome {
    /// The PascalCase name.
    pub fn name(self) -> &'static str {
        OUTCOMES
            .iter()
            .find(|(o, _)| *o == self)
            .map_or("?", |(_, n)| n)
    }

    /// `WireEnumConverterFactory` read: the name in PascalCase or UPPER_SNAKE, any case,
    /// or the integer value.
    pub fn parse(v: &dl_json::Value) -> Option<WalkInOutcome> {
        if let Some(s) = v.as_str() {
            let folded: String = s.chars().filter(|c| *c != '_').collect();
            return OUTCOMES
                .iter()
                .find(|(_, n)| n.eq_ignore_ascii_case(folded.trim()))
                .map(|(o, _)| *o);
        }
        let n = v.as_i64().or_else(|| match v {
            dl_json::Value::Number(n) => n.as_raw_str().and_then(|s| s.parse().ok()),
            _ => None,
        })?;
        OUTCOMES.get(usize::try_from(n).ok()?).map(|(o, _)| *o)
    }
}

/// `AgentWalkInEarnResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalkInResult {
    /// `Outcome`.
    pub outcome: WalkInOutcome,
    /// `PointsAwarded`.
    pub points_awarded: i64,
}

/// The walk-in answer; an empty or unreadable body is an error (the group is asked
/// again rather than marked done).
pub fn walk_in_result(body: &[u8]) -> Result<WalkInResult, DecodeError> {
    let body = body.trim_ascii();
    if body.is_empty() || body == b"null" {
        return Err(DecodeError("earn/walk-in answered an empty body".into()));
    }
    let v = parse::json(body)?;
    let outcome = prop(&v, "Outcome")
        .and_then(WalkInOutcome::parse)
        .ok_or_else(|| DecodeError("earn/walk-in: unknown Outcome".into()))?;
    Ok(WalkInResult {
        outcome,
        points_awarded: int_prop(&v, "PointsAwarded").unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walk_in_outcomes_read_like_the_wire_enum_converter() {
        for (body, want) in [
            (
                r#"{"outcome":"Parked","pointsAwarded":0}"#,
                WalkInOutcome::Parked,
            ),
            (
                r#"{"Outcome":"AWARDED","PointsAwarded":12}"#,
                WalkInOutcome::Awarded,
            ),
            (
                r#"{"outcome":"PHONE_NOT_RECOGNIZED"}"#,
                WalkInOutcome::PhoneNotRecognized,
            ),
            (r#"{"outcome":4}"#, WalkInOutcome::NoPoints),
        ] {
            assert_eq!(
                walk_in_result(body.as_bytes()).unwrap().outcome,
                want,
                "{body}"
            );
        }
        assert_eq!(
            walk_in_result(br#"{"Outcome":"Awarded","PointsAwarded":12}"#)
                .unwrap()
                .points_awarded,
            12
        );
        assert!(walk_in_result(br#"{"outcome":"Later"}"#).is_err());
        assert!(walk_in_result(b"").is_err());
    }

    #[test]
    fn pending_and_cancel_answers_parse() {
        let p = pending_bookings(
            br#"[{"bookingId":"6f1c2d3e-0000-4000-8000-000000000001","confirmationCode":"MM-1"}]"#,
        )
        .unwrap();
        assert_eq!(p[0].confirmation_code, "MM-1");
        assert!(pending_bookings(b"null").unwrap().is_empty());
        assert!(pms_cancel_result(br#"{"cancelled":true}"#).unwrap());
        assert!(pms_cancel_result(b"").is_err());
    }
}
