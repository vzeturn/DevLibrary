//! Response bodies from the hub. Property names match case-insensitively
//! (`PropertyNameCaseInsensitive = true` on the .NET side); enums arrive as names or
//! numbers.

use dl_decimal::Decimal;
use dl_json::{ParseOptions, Value};
use dl_time::{DateTime, OffsetDateTime};
use dl_uuid::Uuid;

use super::DbIdentity;

/// A body the agent could not understand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError(pub String);

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "hub response: {}", self.0)
    }
}

/// Parses a JSON body, keeping numbers as text so decimals keep their digits.
pub fn json(body: &[u8]) -> Result<Value, DecodeError> {
    let text = std::str::from_utf8(body).map_err(|_| DecodeError("not UTF-8".into()))?;
    dl_json::parse_with(
        text,
        ParseOptions {
            keep_raw_numbers: true,
            ..ParseOptions::default()
        },
    )
    .map_err(|e| DecodeError(format!("invalid JSON: {e}")))
}

/// The property `name` of an object, matched case-insensitively.
pub fn prop<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    let obj = v.as_object()?;
    obj.get(name).or_else(|| {
        obj.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v)
    })
}

fn non_null<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    prop(v, name).filter(|x| !x.is_null())
}

/// A string property (`None` when missing or `null`).
pub fn str_prop(v: &Value, name: &str) -> Option<String> {
    non_null(v, name).and_then(Value::as_str).map(str::to_owned)
}

/// A flag property (`false` when missing).
pub fn bool_prop(v: &Value, name: &str) -> bool {
    non_null(v, name).and_then(Value::as_bool).unwrap_or(false)
}

/// An integer property.
pub fn int_prop(v: &Value, name: &str) -> Option<i64> {
    let x = non_null(v, name)?;
    x.as_i64().or_else(|| match x {
        Value::Number(n) => n.as_raw_str().and_then(|s| s.parse().ok()),
        _ => None,
    })
}

/// A decimal property.
pub fn dec_prop(v: &Value, name: &str) -> Option<Decimal> {
    match non_null(v, name)? {
        Value::Number(n) => n
            .as_raw_str()
            .map(str::to_owned)
            .or_else(|| n.as_i64().map(|i| i.to_string()))
            .and_then(|s| s.parse().ok()),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// A GUID property.
pub fn uuid_prop(v: &Value, name: &str) -> Option<Uuid> {
    str_prop(v, name).and_then(|s| s.trim().parse().ok())
}

/// A `DateTime` property; an offset, if any, is dropped (the local time is kept).
pub fn datetime_prop(v: &Value, name: &str) -> Option<DateTime> {
    let s = str_prop(v, name)?;
    DateTime::parse_iso(&s)
        .ok()
        .or_else(|| OffsetDateTime::parse_rfc3339(&s).ok().map(|o| o.datetime()))
}

/// `PmsWriteGateDto`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteGate {
    /// `Off`, `AlertOnly` or `Block`.
    pub mode: Option<String>,
    /// Hours without foreign activity after which the database counts as stale.
    pub threshold_hours: i64,
}

/// `AgentSyncConfigDto` (`GET api/agent/sync-config`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncConfig {
    /// Whether the room-inventory scanner may push.
    pub room_inventory_sync_enabled: bool,
    /// Whether the availability scanner may push.
    pub availability_sync_enabled: bool,
    /// The SMILE database the hub accepted for this hotel.
    pub baseline_identity: Option<DbIdentity>,
    /// The stale-database gate.
    pub write_gate: Option<WriteGate>,
    /// The hotel's extra-bed special-service code.
    pub extra_bed_special_code: Option<String>,
}

fn identity(v: &Value) -> Option<DbIdentity> {
    Some(DbIdentity {
        server: str_prop(v, "Server")?,
        database: str_prop(v, "Database")?,
        create_date: datetime_prop(v, "CreateDate")?,
        database_guid: uuid_prop(v, "DatabaseGuid"),
        family_guid: uuid_prop(v, "FamilyGuid"),
        service_broker_guid: uuid_prop(v, "ServiceBrokerGuid"),
    })
}

/// Parses the sync-config body.
pub fn sync_config(body: &[u8]) -> Result<SyncConfig, DecodeError> {
    let v = json(body)?;
    if v.as_object().is_none() {
        return Err(DecodeError("sync-config is not an object".into()));
    }
    let baseline_identity = match non_null(&v, "BaselineIdentity") {
        None => None,
        Some(b) => Some(identity(b).ok_or_else(|| DecodeError("bad BaselineIdentity".into()))?),
    };
    let write_gate = non_null(&v, "WriteGate").map(|g| WriteGate {
        mode: str_prop(g, "Mode"),
        threshold_hours: int_prop(g, "ThresholdHours").unwrap_or(0),
    });
    Ok(SyncConfig {
        room_inventory_sync_enabled: bool_prop(&v, "RoomInventorySyncEnabled"),
        availability_sync_enabled: bool_prop(&v, "AvailabilitySyncEnabled"),
        baseline_identity,
        write_gate,
        extra_bed_special_code: str_prop(&v, "ExtraBedSpecialCode"),
    })
}

/// One pulled job (`SyncJobDto`).
#[derive(Clone, PartialEq, Eq)]
pub struct Job {
    /// `JobId`.
    pub job_id: String,
    /// `Action`, as the hub sent it (name or number).
    pub action: String,
    /// `BookingId`.
    pub booking_id: Option<Uuid>,
    /// `PayloadJson` (guest data: never logged).
    pub payload_json: String,
    /// `Attempts` (the delivery's lease token).
    pub attempts: i64,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job")
            .field("job_id", &self.job_id)
            .field("action", &self.action)
            .field("booking_id", &self.booking_id)
            .field("attempts", &self.attempts)
            .finish_non_exhaustive()
    }
}

/// Parses a jobs list (`[{JobId, Action, BookingId, PayloadJson, Attempts}]`).
pub fn jobs(body: &[u8]) -> Result<Vec<Job>, DecodeError> {
    let v = json(body)?;
    let items = v
        .as_array()
        .ok_or_else(|| DecodeError("jobs is not an array".into()))?;
    Ok(items
        .iter()
        .map(|j| Job {
            job_id: str_prop(j, "JobId").unwrap_or_default(),
            action: match non_null(j, "Action") {
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            },
            booking_id: uuid_prop(j, "BookingId"),
            payload_json: str_prop(j, "PayloadJson").unwrap_or_default(),
            attempts: int_prop(j, "Attempts").unwrap_or(0),
        })
        .collect())
}

/// One item of `AgentFolioObservationResult.Items`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationItem {
    /// `FolioNum`.
    pub folio_num: Decimal,
    /// `FolioSubNum`.
    pub folio_sub_num: Decimal,
    /// The outcome was `Rejected` (name or the number 3).
    pub rejected: bool,
}

/// `AgentFolioObservationResult`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObservationResult {
    /// Observations the hub accepted.
    pub accepted: i64,
    /// Observations that earned points.
    pub earned: i64,
    /// Observations that reversed points.
    pub reversed: i64,
    /// Observations that changed nothing.
    pub unchanged: i64,
    /// Per-folio outcomes.
    pub items: Vec<ObservationItem>,
}

/// Parses a folio-observation result. An empty body is an error.
pub fn observation_result(body: &[u8]) -> Result<ObservationResult, DecodeError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Err(DecodeError("empty folio-observations result".into()));
    }
    let v = json(body)?;
    if v.as_object().is_none() {
        return Err(DecodeError(
            "folio-observations result is not an object".into(),
        ));
    }
    let items = non_null(&v, "Items")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|i| {
                    let rejected = match non_null(i, "Outcome") {
                        Some(Value::String(s)) => s.trim().eq_ignore_ascii_case("Rejected"),
                        Some(Value::Number(n)) => {
                            n.as_i64().or_else(|| n.as_raw_str()?.parse().ok()) == Some(3)
                        }
                        _ => false,
                    };
                    Some(ObservationItem {
                        folio_num: dec_prop(i, "FolioNum")?,
                        folio_sub_num: dec_prop(i, "FolioSubNum")?,
                        rejected,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(ObservationResult {
        accepted: int_prop(&v, "Accepted").unwrap_or(0),
        earned: int_prop(&v, "Earned").unwrap_or(0),
        reversed: int_prop(&v, "Reversed").unwrap_or(0),
        unchanged: int_prop(&v, "Unchanged").unwrap_or(0),
        items,
    })
}

/// `AgentEnrollResponse`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollResponse {
    /// The hotel this agent now belongs to.
    pub hotel_id: Uuid,
    /// The agent token (secret).
    pub agent_token: String,
    /// The hub address the agent must use from now on.
    pub hub_base_url: String,
    /// The SMILE origin code for bookings the hub writes.
    pub origin_code: String,
    /// The enrolment id.
    pub enrollment_id: Option<Uuid>,
    /// The hotel was enrolled on another machine before.
    pub machine_changed: bool,
    /// That machine's name.
    pub previous_machine_name: Option<String>,
}

/// Parses the enrolment response.
pub fn enroll_response(body: &[u8]) -> Result<EnrollResponse, DecodeError> {
    let v = json(body)?;
    let need =
        |name: &str| str_prop(&v, name).ok_or_else(|| DecodeError(format!("missing {name}")));
    Ok(EnrollResponse {
        hotel_id: uuid_prop(&v, "HotelId").ok_or_else(|| DecodeError("missing HotelId".into()))?,
        agent_token: need("AgentToken")?,
        hub_base_url: need("HubBaseUrl")?,
        origin_code: str_prop(&v, "OriginCode").unwrap_or_default(),
        enrollment_id: uuid_prop(&v, "EnrollmentId"),
        machine_changed: bool_prop(&v, "MachineChanged"),
        previous_machine_name: str_prop(&v, "PreviousMachineName"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_config_matches_case_insensitively() {
        let c = sync_config(
            br#"{"roomInventorySyncEnabled":true,"AVAILABILITYSYNCENABLED":false,
            "baselineIdentity":{"server":"S","database":"D","createDate":"2019-05-06T07:08:09.1"},
            "writeGate":{"mode":"Block","thresholdHours":24},"extraBedSpecialCode":"EB2"}"#,
        )
        .unwrap();
        assert!(c.room_inventory_sync_enabled);
        assert!(!c.availability_sync_enabled);
        let b = c.baseline_identity.unwrap();
        assert_eq!((b.server.as_str(), b.database.as_str()), ("S", "D"));
        assert_eq!(c.write_gate.unwrap().threshold_hours, 24);
        assert_eq!(c.extra_bed_special_code.as_deref(), Some("EB2"));
    }

    #[test]
    fn observation_result_reads_outcomes_as_names_or_numbers() {
        let r = observation_result(
            br#"{"accepted":2,"earned":1,"reversed":0,"unchanged":1,"items":[
            {"folioNum":12.0,"folioSubNum":1,"outcome":"Rejected"},
            {"folioNum":13,"folioSubNum":0,"outcome":3},
            {"folioNum":14,"folioSubNum":0,"outcome":"Applied"}]}"#,
        )
        .unwrap();
        assert_eq!(r.accepted, 2);
        let rejected: Vec<bool> = r.items.iter().map(|i| i.rejected).collect();
        assert_eq!(rejected, [true, true, false]);
        assert_eq!(r.items[0].folio_num.to_string(), "12.0");
        assert!(observation_result(b"  ").is_err());
    }

    #[test]
    fn jobs_keep_ids_and_actions() {
        let j = jobs(br#"[{"JobId":"a","Action":"Create","Attempts":2},{"jobId":"b","action":4}]"#)
            .unwrap();
        assert_eq!(j.len(), 2);
        assert_eq!((j[0].action.as_str(), j[0].attempts), ("Create", 2));
        assert_eq!(j[1].action, "4");
    }
}
