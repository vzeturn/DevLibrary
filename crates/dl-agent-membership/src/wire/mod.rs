//! Request bodies sent to the hub, field for field in the order of the .NET records
//! (`MM.Membership.Application` DTOs), serialized through [`crate::stj`].

pub mod parse;

use dl_decimal::Decimal;
use dl_time::{Date, DateTime, OffsetDateTime};
use dl_uuid::Uuid;

use crate::stj::J;

/// `PmsDbIdentityDto`: which SMILE database the agent is reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbIdentity {
    /// `@@SERVERNAME`, trimmed.
    pub server: String,
    /// `DB_NAME()`, trimmed.
    pub database: String,
    /// `sys.databases.create_date`.
    pub create_date: DateTime,
    /// `sys.database_recovery_status.database_guid`.
    pub database_guid: Option<Uuid>,
    /// `sys.database_recovery_status.family_guid`.
    pub family_guid: Option<Uuid>,
    /// `sys.databases.service_broker_guid`.
    pub service_broker_guid: Option<Uuid>,
}

impl DbIdentity {
    /// The JSON value.
    pub fn to_j(&self) -> J {
        J::Obj(vec![
            ("Server", J::str(&self.server)),
            ("Database", J::str(&self.database)),
            ("CreateDate", J::datetime(self.create_date)),
            ("DatabaseGuid", J::opt_uuid(self.database_guid)),
            ("FamilyGuid", J::opt_uuid(self.family_guid)),
            ("ServiceBrokerGuid", J::opt_uuid(self.service_broker_guid)),
        ])
    }
}

/// `HeartbeatRequest` (`POST api/agent/heartbeat`).
#[derive(Debug, Clone, Default)]
pub struct Heartbeat {
    /// The agent version string.
    pub agent_version: String,
    /// Jobs waiting in the agent.
    pub pending_jobs: i64,
    /// Whether a SMILE connection opened.
    pub smile_ok: bool,
    /// Walk-in earn health; `None` while the walk-in path is not ported.
    pub walk_in_earn_ok: Option<bool>,
    /// The folio-observation watermark (PMS local time).
    pub folio_scan_watermark: Option<DateTime>,
    /// How far the watermark trails the PMS clock.
    pub folio_scan_lag_seconds: Option<i64>,
    /// When the folio scanner last ran (UTC).
    pub folio_scan_at: Option<OffsetDateTime>,
    /// The SMILE database identity.
    pub db_identity: Option<DbIdentity>,
    /// Minutes since someone other than the agent last changed a folio.
    pub foreign_activity_age_minutes: Option<i64>,
    /// The PMS clock (+07:00).
    pub pms_now: Option<OffsetDateTime>,
    /// What this agent can do.
    pub capabilities: Option<Vec<String>>,
}

impl Heartbeat {
    /// The JSON value. `Diagnostics` is always `null`: the diagnostics probe is not
    /// ported.
    pub fn to_j(&self) -> J {
        J::Obj(vec![
            ("AgentVersion", J::str(&self.agent_version)),
            ("PendingJobs", J::Int(self.pending_jobs)),
            ("SmileOk", J::Bool(self.smile_ok)),
            ("WalkInEarnOk", J::opt_bool(self.walk_in_earn_ok)),
            (
                "FolioScanWatermark",
                J::opt_datetime(self.folio_scan_watermark),
            ),
            (
                "FolioScanLagSeconds",
                J::opt_int(self.folio_scan_lag_seconds),
            ),
            ("FolioScanAt", J::opt_dto(self.folio_scan_at)),
            ("Diagnostics", J::Null),
            (
                "DbIdentity",
                self.db_identity.as_ref().map_or(J::Null, DbIdentity::to_j),
            ),
            (
                "ForeignActivityAgeMinutes",
                J::opt_int(self.foreign_activity_age_minutes),
            ),
            ("PmsNow", J::opt_dto(self.pms_now)),
            (
                "Capabilities",
                self.capabilities.as_deref().map_or(J::Null, J::strs),
            ),
        ])
    }
}

/// `AgentOccupancyLine`: rooms sold and sellable for one room type and night.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OccupancyLine {
    /// The SMILE room-type code.
    pub room_type_code: String,
    /// The night.
    pub date: Date,
    /// Rooms sold that night.
    pub rooms_sold: i64,
    /// Physical rooms minus blocked rooms.
    pub rooms_total: i64,
}

/// `AgentExtraBedUsageLine`: extra beds in use one night.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtraBedUsageLine {
    /// The night.
    pub date: Date,
    /// Extra beds SMILE has on that night.
    pub smile_beds: i64,
}

/// `AgentAvailabilityRequest` (`POST api/agent/availability`).
#[derive(Debug, Clone, Default)]
pub struct AvailabilityRequest {
    /// The lines of this request.
    pub lines: Vec<OccupancyLine>,
    /// Scan time so far.
    pub scan_elapsed_ms: Option<i64>,
    /// Whether this is the last request of the scan.
    pub scan_complete: bool,
    /// Extra-bed usage, on the last request only.
    pub extra_bed_usage: Option<Vec<ExtraBedUsageLine>>,
}

impl AvailabilityRequest {
    /// The JSON value.
    pub fn to_j(&self) -> J {
        let lines = self
            .lines
            .iter()
            .map(|l| {
                J::Obj(vec![
                    ("RoomTypeCode", J::str(&l.room_type_code)),
                    ("Date", J::date(l.date)),
                    ("RoomsSold", J::Int(l.rooms_sold)),
                    ("RoomsTotal", J::Int(l.rooms_total)),
                ])
            })
            .collect();
        let usage = self.extra_bed_usage.as_ref().map_or(J::Null, |u| {
            J::Arr(
                u.iter()
                    .map(|l| {
                        J::Obj(vec![
                            ("Date", J::date(l.date)),
                            ("SmileBeds", J::Int(l.smile_beds)),
                        ])
                    })
                    .collect(),
            )
        });
        J::Obj(vec![
            ("Lines", J::Arr(lines)),
            ("ScanElapsedMs", J::opt_int(self.scan_elapsed_ms)),
            ("ScanComplete", J::Bool(self.scan_complete)),
            ("ExtraBedUsage", usage),
        ])
    }
}

/// `AgentRoomLine`: one physical room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomLine {
    /// `Room.RoomCode`, untrimmed.
    pub pms_room_code: String,
    /// `Room.RoomName`.
    pub pms_room_name: Option<String>,
    /// `Room.RoomTypeCode`, untrimmed.
    pub room_type_code: String,
    /// `Room.Floor`.
    pub floor: Option<i64>,
    /// `INSPECTED`, `CLEAN`, `TOUCH`, `DIRTY`, `STOP_SALE` or `UNKNOWN`.
    pub housekeeping_status: String,
    /// An open `RoomOOS` row covers today.
    pub out_of_service: bool,
    /// Its housekeeping or engineering note.
    pub out_of_service_reason: Option<String>,
    /// A checked-in folio sits in the room.
    pub is_occupied: bool,
    /// A reservation arrives today.
    pub arrival_due_today: bool,
    /// A checked-in folio departs today or earlier.
    pub departure_due_today: bool,
    /// An out-of-order folio (status 4/5) covers today.
    pub out_of_order: bool,
    /// Its reason.
    pub out_of_order_reason: Option<String>,
    /// Its departure date.
    pub out_of_order_until: Option<Date>,
}

/// `AgentRoomInventoryRequest` (`POST api/agent/room-inventory`).
#[derive(Debug, Clone)]
pub struct RoomInventoryRequest {
    /// Every room.
    pub rooms: Vec<RoomLine>,
    /// Whether the hub may treat rooms missing from the list as gone.
    pub is_full_push: bool,
    /// When the rooms were read (UTC).
    pub as_of: OffsetDateTime,
}

impl RoomInventoryRequest {
    /// The JSON value.
    pub fn to_j(&self) -> J {
        let rooms = self
            .rooms
            .iter()
            .map(|r| {
                J::Obj(vec![
                    ("PmsRoomCode", J::str(&r.pms_room_code)),
                    ("PmsRoomName", J::opt_str(r.pms_room_name.as_deref())),
                    ("RoomTypeCode", J::str(&r.room_type_code)),
                    ("Floor", J::opt_int(r.floor)),
                    ("HousekeepingStatus", J::str(&r.housekeeping_status)),
                    ("OutOfService", J::Bool(r.out_of_service)),
                    (
                        "OutOfServiceReason",
                        J::opt_str(r.out_of_service_reason.as_deref()),
                    ),
                    ("IsOccupied", J::Bool(r.is_occupied)),
                    ("ArrivalDueToday", J::Bool(r.arrival_due_today)),
                    ("DepartureDueToday", J::Bool(r.departure_due_today)),
                    ("OutOfOrder", J::Bool(r.out_of_order)),
                    (
                        "OutOfOrderReason",
                        J::opt_str(r.out_of_order_reason.as_deref()),
                    ),
                    ("OutOfOrderUntil", J::opt_date(r.out_of_order_until)),
                ])
            })
            .collect();
        J::Obj(vec![
            ("Rooms", J::Arr(rooms)),
            ("IsFullPush", J::Bool(self.is_full_push)),
            ("AsOf", J::dto(self.as_of)),
        ])
    }
}

/// `AgentRoomTypeLine`: one room type of the catalogue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomTypeLine {
    /// `RoomType.RoomTypeCode`, trimmed.
    pub pms_room_type_code: String,
    /// `RoomType.Description`, trimmed.
    pub pms_name: Option<String>,
    /// `RoomType.NumRoom`.
    pub num_room: Option<i64>,
    /// `RoomType.DisplayOrder`.
    pub display_order: Option<i64>,
}

/// `AgentRoomTypeCatalogRequest` (`POST api/agent/room-types`).
#[derive(Debug, Clone)]
pub struct RoomTypeCatalogRequest {
    /// The room types.
    pub room_types: Vec<RoomTypeLine>,
    /// When they were read (UTC).
    pub as_of: OffsetDateTime,
}

impl RoomTypeCatalogRequest {
    /// The JSON value.
    pub fn to_j(&self) -> J {
        let types = self
            .room_types
            .iter()
            .map(|t| {
                J::Obj(vec![
                    ("PmsRoomTypeCode", J::str(&t.pms_room_type_code)),
                    ("PmsName", J::opt_str(t.pms_name.as_deref())),
                    ("NumRoom", J::opt_int(t.num_room)),
                    ("DisplayOrder", J::opt_int(t.display_order)),
                ])
            })
            .collect();
        J::Obj(vec![
            ("RoomTypes", J::Arr(types)),
            ("AsOf", J::dto(self.as_of)),
        ])
    }
}

/// `AgentStayClassification`: origin, segment and rate codes of a stay.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StayClassification {
    /// `Folio.OriginCode`.
    pub origin_codes: Vec<String>,
    /// `Folio.MarketSegmentCode`.
    pub segment_codes: Vec<String>,
    /// `Folio.RoomRateCode`.
    pub rate_codes: Vec<String>,
}

/// `AgentFolioObservation`: what the agent saw of one folio.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)] // field names mirror the .NET record one to one
pub struct FolioObservation {
    pub folio_num: Decimal,
    pub folio_sub_num: Decimal,
    pub pms_folio_id: Option<i64>,
    pub folio_status: i64,
    pub checkout_epoch: i64,
    pub checked_out_at: Option<OffsetDateTime>,
    pub checked_in_at: Option<OffsetDateTime>,
    pub observed_at: OffsetDateTime,
    pub arrival_date: Date,
    pub departure_date: Date,
    pub confirm_num: Option<Decimal>,
    pub external_confirm_number: Option<String>,
    pub is_cancelled: bool,
    pub cancelled_at: Option<OffsetDateTime>,
    pub is_no_show: bool,
    pub room_code: Option<String>,
    pub room_item_id_hint: Option<Uuid>,
    pub guest_phone: Option<String>,
    pub guest_name: Option<String>,
    pub qualifying_revenue: Decimal,
    pub revenue_fingerprint: String,
    pub booked_room_type_code: Option<String>,
    pub actual_room_type_code: Option<String>,
    pub num_adult: Option<i64>,
    pub num_child: Option<i64>,
    pub classification: StayClassification,
    pub num_enf: Option<i64>,
    pub extra_bed: Option<i64>,
    pub extra_bed_special_qty: Option<i64>,
}

impl FolioObservation {
    /// The JSON value. `FolioLines` is always `null`, as in the .NET agent.
    pub fn to_j(&self) -> J {
        let c = &self.classification;
        J::Obj(vec![
            ("FolioNum", J::Dec(self.folio_num)),
            ("FolioSubNum", J::Dec(self.folio_sub_num)),
            ("PmsFolioId", J::opt_int(self.pms_folio_id)),
            ("FolioStatus", J::Int(self.folio_status)),
            ("CheckoutEpoch", J::Int(self.checkout_epoch)),
            ("CheckedOutAt", J::opt_dto(self.checked_out_at)),
            ("CheckedInAt", J::opt_dto(self.checked_in_at)),
            ("ObservedAt", J::dto(self.observed_at)),
            ("ArrivalDate", J::date(self.arrival_date)),
            ("DepartureDate", J::date(self.departure_date)),
            ("ConfirmNum", J::opt_dec(self.confirm_num)),
            (
                "ExternalConfirmNumber",
                J::opt_str(self.external_confirm_number.as_deref()),
            ),
            ("IsCancelled", J::Bool(self.is_cancelled)),
            ("CancelledAt", J::opt_dto(self.cancelled_at)),
            ("IsNoShow", J::Bool(self.is_no_show)),
            ("RoomCode", J::opt_str(self.room_code.as_deref())),
            ("RoomItemIdHint", J::opt_uuid(self.room_item_id_hint)),
            ("GuestPhone", J::opt_str(self.guest_phone.as_deref())),
            ("GuestName", J::opt_str(self.guest_name.as_deref())),
            ("QualifyingRevenue", J::Dec(self.qualifying_revenue)),
            ("RevenueFingerprint", J::str(&self.revenue_fingerprint)),
            ("FolioLines", J::Null),
            (
                "BookedRoomTypeCode",
                J::opt_str(self.booked_room_type_code.as_deref()),
            ),
            (
                "ActualRoomTypeCode",
                J::opt_str(self.actual_room_type_code.as_deref()),
            ),
            ("NumAdult", J::opt_int(self.num_adult)),
            ("NumChild", J::opt_int(self.num_child)),
            (
                "Classification",
                J::Obj(vec![
                    ("OriginCodes", J::strs(&c.origin_codes)),
                    ("SegmentCodes", J::strs(&c.segment_codes)),
                    ("RateCodes", J::strs(&c.rate_codes)),
                ]),
            ),
            ("NumEnf", J::opt_int(self.num_enf)),
            ("ExtraBed", J::opt_int(self.extra_bed)),
            ("ExtraBedSpecialQty", J::opt_int(self.extra_bed_special_qty)),
        ])
    }
}

/// `AgentFolioObservationBatch` (`POST api/agent/folio-observations`).
pub fn observation_batch(
    observations: &[FolioObservation],
    agent_version: &str,
    db_identity_fingerprint: Option<&str>,
) -> J {
    J::Obj(vec![
        (
            "Observations",
            J::Arr(observations.iter().map(FolioObservation::to_j).collect()),
        ),
        ("AgentVersion", J::str(agent_version)),
        ("DbIdentityFingerprint", J::opt_str(db_identity_fingerprint)),
    ])
}

/// `AgentEnrollRequest` (`POST api/agent/enroll`).
#[derive(Debug, Clone)]
pub struct EnrollRequest {
    /// The one-time enrolment code from the hub admin.
    pub code: String,
    /// This machine's name.
    pub machine_name: String,
    /// The agent version string.
    pub agent_version: String,
    /// The SMILE server (`Data Source`), for the hub admin's display.
    pub smile_data_source: Option<String>,
    /// The SMILE database name.
    pub smile_database: Option<String>,
}

impl EnrollRequest {
    /// The JSON value.
    pub fn to_j(&self) -> J {
        J::Obj(vec![
            ("Code", J::str(&self.code)),
            ("MachineName", J::str(&self.machine_name)),
            ("AgentVersion", J::str(&self.agent_version)),
            (
                "SmileDataSource",
                J::opt_str(self.smile_data_source.as_deref()),
            ),
            ("SmileDatabase", J::opt_str(self.smile_database.as_deref())),
        ])
    }
}
