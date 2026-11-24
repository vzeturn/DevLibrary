//! SMILE_FO, read side: the row shapes the scanners use, the source traits they read
//! through (one per scanner, so tests use small hand-written fakes), and [`TdsSmile`],
//! which implements them all on `dl-tds`.
//!
//! Every statement is parameterized; the only spliced text is a column name that passed
//! [`safe_identifier`] (the guest-phone column) or a fixed fragment chosen by a schema
//! probe. Errors carry the SQL Server error number, never the statement text or data.

pub(crate) mod folio_sql;
mod occupancy_sql;
mod rows;
mod tds;

use std::collections::BTreeMap;
use std::fmt;

use dl_decimal::Decimal;
use dl_time::{Date, DateTime, Offset, OffsetDateTime};

pub use tds::{SmileTarget, TdsSmile, normalize_connection_string};

use crate::folio::earn::EarnPair;
use crate::wire::RoomTypeLine;

/// `BackgroundRead.LockTimeoutMs` (RT-0756): the row-lock wait of every background read.
/// A scan that waits behind a front-desk folio lock gives up after this and skips its tick,
/// instead of holding the lock queue for up to the 30 s command timeout. No `NOLOCK`: a
/// dirty read would make the occupancy fingerprint and the sold-room counts wrong.
pub const BACKGROUND_LOCK_TIMEOUT_MS: i32 = 2_000;

/// SQL Server 1222, "Lock request time out period exceeded".
pub const LOCK_TIMEOUT_ERROR: i32 = 1222;

/// The hotel's UTC offset (`HotelClock.Offset`): SMILE stores local wall-clock times.
pub fn hotel_offset() -> Offset {
    Offset::VIETNAM
}

/// The hotel's wall clock now (`HotelClock.Now.DateTime`).
pub fn hotel_now() -> DateTime {
    OffsetDateTime::now_utc()
        .to_offset(hotel_offset())
        .datetime()
}

/// SMILE local time as UTC (`FolioObservationReader.ToUtc`).
pub fn to_utc(pms_local: DateTime) -> OffsetDateTime {
    OffsetDateTime::new(pms_local, hotel_offset()).to_offset(Offset::UTC)
}

/// A SMILE failure, safe to log and report: no SQL text, no data, no connection string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmileError {
    /// No usable connection string.
    Config(String),
    /// The server could not be reached or refused the login.
    Unavailable(String),
    /// A statement failed on the server (SQL Server error number).
    Server(i32),
    /// A column did not have the expected shape.
    Decode(&'static str),
    /// The setting is not usable (for example an invalid phone column name).
    Invalid(String),
}

impl SmileError {
    /// The SQL Server error number, for server errors.
    pub fn number(&self) -> Option<i32> {
        match self {
            SmileError::Server(n) => Some(*n),
            _ => None,
        }
    }

    /// `BackgroundRead.IsLockTimeout`: the read gave up on a row lock (SQL 1222). For a
    /// background read this means "SMILE is busy, skip this tick", not a failure.
    pub fn is_lock_timeout(&self) -> bool {
        self.number() == Some(LOCK_TIMEOUT_ERROR)
    }

    /// Whether SMILE itself is down (as opposed to one statement failing).
    pub fn is_unavailable(&self) -> bool {
        matches!(self, SmileError::Unavailable(_) | SmileError::Config(_))
    }

    pub(crate) fn from_tds(e: &dl_tds::Error) -> SmileError {
        match e.number() {
            // Login failures are "SMILE down" for the module: nothing can be read.
            // 4063: the login has no user in the requested database.
            Some(n @ (18456 | 4060 | 4063 | 18452)) => {
                SmileError::Unavailable(format!("login refused ({n})"))
            }
            Some(n) if e.is_server() => SmileError::Server(n),
            // Keep dl-tds's own text (OS error code included): "I/O: uncategorized error"
            // alone could not be diagnosed on a site. A rejected connection string is not
            // echoed, as its text may carry the password.
            _ => SmileError::Unavailable(match e {
                dl_tds::Error::Config { .. } => "connection string rejected".into(),
                _ if e.is_tls() => format!("TLS handshake failed: {e}"),
                dl_tds::Error::Io(io) => format!("I/O: {io}"),
                _ => format!("connection failed: {e}"),
            }),
        }
    }
}

impl fmt::Display for SmileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SmileError::Config(m) => write!(f, "SMILE not configured: {m}"),
            SmileError::Unavailable(m) => write!(f, "SMILE unavailable: {m}"),
            SmileError::Server(n) => write!(f, "SMILE query failed (SQL error {n})"),
            SmileError::Decode(c) => write!(f, "SMILE column {c} has an unexpected value"),
            SmileError::Invalid(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for SmileError {}

/// `FolioWriter.SafeIdentifier`: a bracketed column name, or an error.
pub fn safe_identifier(name: &str) -> Result<String, SmileError> {
    let bad = || SmileError::Invalid(format!("invalid guest phone column name: '{name}'"));
    let first = name.chars().next().ok_or_else(bad)?;
    if name.trim().is_empty() || !(first.is_alphabetic() || first == '_') {
        return Err(bad());
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(bad());
    }
    Ok(format!("[{name}]"))
}

/// `OccupancyFingerprint`: a cheap digest of everything the availability scan reads.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)] // columns of the probe, one to one
pub struct OccupancyFingerprint {
    pub folio_count: i64,
    pub folio_hash: i64,
    pub folio_sum: i64,
    pub room_count: i64,
    pub room_hash: i64,
    pub pms_date: Date,
    pub ooo_count: i64,
    pub ooo_hash: i64,
    pub oos_count: i64,
    pub oos_hash: i64,
    pub group_block_count: i64,
    pub group_block_hash: i64,
    pub group_block_sum: i64,
    pub hotel_date: Option<DateTime>,
}

/// `OccupancyStay`: one live folio in the horizon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OccupancyStay {
    /// `RoomTypeBooked`, trimmed.
    pub room_type_code: String,
    /// Arrival night.
    pub arrival: Date,
    /// Departure day.
    pub departure: Date,
    /// The group, when the folio belongs to one.
    pub group_code: Option<String>,
    /// The assigned room, if any.
    pub room_code: Option<String>,
}

/// `RoomInventoryLine`: one physical room as SMILE has it now.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[allow(missing_docs)] // columns of the room scan, one to one
pub struct RoomRow {
    pub room_code: String,
    pub room_name: Option<String>,
    pub room_type_code: String,
    pub floor: Option<i64>,
    pub sts_code: Option<i64>,
    pub hskp_clean: Option<bool>,
    pub inspected: Option<bool>,
    pub touched_up: Option<i64>,
    pub out_of_service: bool,
    pub out_of_service_reason: Option<String>,
    pub is_occupied: bool,
    pub arrival_due_today: bool,
    pub departure_due_today: bool,
    pub out_of_order: bool,
    pub out_of_order_reason: Option<String>,
    pub out_of_order_until: Option<Date>,
}

/// `RoomBlock`: a room out of service or out of order for `[from, to_exclusive)`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct RoomBlock {
    pub room_code: String,
    pub room_type_code: String,
    pub from: Date,
    pub to_exclusive: Date,
}

/// `GroupBlock`: rooms a live group holds for `[from, to_exclusive)`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct GroupBlock {
    pub group_code: String,
    pub room_type_code: String,
    pub from: Date,
    pub to_exclusive: Date,
    pub num_room: i64,
}

/// `ExtraBedUsageReadResult`: beds per night, or why they could not be counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtraBedUsage {
    /// Beds per night (nights with none are absent).
    Beds(BTreeMap<Date, i64>),
    /// Not counted this time (schema, configuration or row cap), with a log-safe reason.
    Unknown(String),
}

/// Which optional occupancy columns this SMILE has (`OccupancyColumns`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OccupancyColumns {
    /// `Folio.NumEnf`.
    pub num_enf: bool,
    /// `Folio.ExtraBed`.
    pub extra_bed: bool,
    /// `Folio_Special(FolioNum, SpecialCode, Qty)`.
    pub has_special: bool,
    /// `Folio_Special.CancelNum`.
    pub special_has_cancel_num: bool,
}

/// One changed folio (`FolioObservationReader.FolioRow`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)] // columns of the folio scan, one to one
pub struct FolioRow {
    pub folio_num: Decimal,
    pub folio_sub_num: Decimal,
    pub folio_id: i64,
    pub folio_status: i64,
    pub check_in_time: Option<DateTime>,
    pub check_out_time: Option<DateTime>,
    pub last_change_time: DateTime,
    pub arrival_date: Option<DateTime>,
    pub departure_date: Option<DateTime>,
    pub confirm_num: Option<Decimal>,
    pub external_confirm_number: Option<String>,
    pub cancel_num: Option<Decimal>,
    pub cancel_time: Option<DateTime>,
    pub no_show: bool,
    pub room_code: Option<String>,
    pub user_info1: Option<String>,
    pub last_name: Option<String>,
    pub room_type_booked: Option<String>,
    pub room_type_code: Option<String>,
    pub num_adult: Option<i64>,
    pub num_child: Option<i64>,
    pub num_enf: Option<i64>,
    pub extra_bed: Option<i64>,
    pub origin_code: Option<String>,
    pub market_segment_code: Option<String>,
    pub room_rate_code: Option<String>,
}

/// Qualifying revenue of one folio (`RevenueRow`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RevenueRow {
    /// `FolioNum` as SMILE's varchar, trimmed.
    pub folio_num: String,
    /// `MAX(TransactionID)`.
    pub last_txn_id: Decimal,
    /// Rows counted.
    pub txn_count: i64,
    /// `SUM(TransactionAmount)`.
    pub total_amount: Decimal,
}

/// The first non-cancelled additional name of a folio (`GuestRow`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestRow {
    /// The folio.
    pub folio_num: Decimal,
    /// The phone, trimmed (empty when none).
    pub phone: Option<String>,
    /// First and last name, trimmed.
    pub name: Option<String>,
}

/// One `(code, sub)` row of a catalogue query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeRow {
    /// The code, trimmed.
    pub code: String,
    /// The sub-code, trimmed.
    pub sub: Option<String>,
}

/// The five result sets of `EarnPairReader.CatalogSql`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EarnCatalog {
    /// `TransactionCode` with `AutoPost` (`(code, sub, auto_post)`).
    pub transaction_codes: Vec<(CodeRow, i64)>,
    /// `RoomType.TrnCode/TrnSubCode`.
    pub room_types: Vec<CodeRow>,
    /// `PackageCode.TrnCode/TrnSubCode`.
    pub packages: Vec<CodeRow>,
    /// `GlobalParameter` `EBedSurChgTrnCode` / `EBedSurChgTrnSub` (`code` = name).
    pub globals: Vec<CodeRow>,
    /// The extra-bed `SpecialService` charge code.
    pub specials: Vec<CodeRow>,
}

/// Whether SMILE answers (the heartbeat's `SmileOk`).
pub trait SmileHealth: Send + Sync {
    /// Opens (or reuses) a connection and runs a trivial statement.
    fn ping(&self) -> Result<(), SmileError>;
}

/// What the room-inventory scan reads.
pub trait RoomSource: Send + Sync {
    /// `FolioWriter.ScanRoomInventoryAsync`.
    fn scan_rooms(&self) -> Result<Vec<RoomRow>, SmileError>;
    /// `RoomTypeCatalogReader.ReadAsync`.
    fn room_types(&self) -> Result<Vec<RoomTypeLine>, SmileError>;
}

/// What the availability scan reads (on top of the rooms).
pub trait AvailabilitySource: RoomSource {
    /// `ProbeOccupancyAsync` over `[from, to)`.
    fn probe_occupancy(&self, from: Date, to: Date) -> Result<OccupancyFingerprint, SmileError>;
    /// `ScanOccupancyAsync`: at most `max` live folios in `[from, to)`.
    fn scan_occupancy(
        &self,
        from: Date,
        to: Date,
        max: i64,
    ) -> Result<Vec<OccupancyStay>, SmileError>;
    /// `ScanRoomBlocksAsync`.
    fn scan_room_blocks(&self, from: Date, to: Date) -> Result<Vec<RoomBlock>, SmileError>;
    /// `ScanGroupBlocksAsync`.
    fn scan_group_blocks(&self, from: Date, to: Date) -> Result<Vec<GroupBlock>, SmileError>;
    /// `ExtraBedUsageReader.ReadAsync`.
    fn extra_bed_usage(
        &self,
        from: Date,
        to: Date,
        max_rows: i64,
        special_code: &str,
    ) -> Result<ExtraBedUsage, SmileError>;
}

/// What the folio-observation scan reads.
pub trait FolioSource: Send + Sync {
    /// `EarnPairReader.CatalogSql`.
    fn earn_catalog(&self, extra_bed_code: &str) -> Result<EarnCatalog, SmileError>;
    /// `ReadRoomChargeTrnAsync(ChangedSince)`.
    fn room_charge_trn_since(&self, watermark: DateTime) -> Result<Vec<i64>, SmileError>;
    /// `ProbeOccupancyColumnsAsync`.
    fn occupancy_columns(&self) -> Result<OccupancyColumns, SmileError>;
    /// The changed-folio scan, at most `max` rows from `watermark` on.
    fn changed_folios(
        &self,
        watermark: DateTime,
        max: i64,
        columns: OccupancyColumns,
    ) -> Result<Vec<FolioRow>, SmileError>;
    /// Qualifying revenue of `folio_nums` (varchar form) for `pairs` (non-empty).
    fn revenues(
        &self,
        folio_nums: &[String],
        pairs: &[EarnPair],
    ) -> Result<Vec<RevenueRow>, SmileError>;
    /// The guest of each folio; `phone_column` is already bracketed.
    fn guests(
        &self,
        folio_nums: &[Decimal],
        phone_column: &str,
    ) -> Result<Vec<GuestRow>, SmileError>;
    /// Extra beds sold as a special service per folio.
    fn extra_bed_special_qty(
        &self,
        folio_nums: &[Decimal],
        code: &str,
        with_cancel_filter: bool,
    ) -> Result<Vec<(Decimal, i64)>, SmileError>;
}

#[cfg(test)]
mod tests {
    use super::SmileError;

    #[test]
    fn io_errors_keep_the_os_code() {
        // RT-0164 test PC: only "I/O: uncategorized error" reached the hub.
        let e = dl_tds::Error::Io(std::io::Error::from_raw_os_error(64));
        let SmileError::Unavailable(text) = SmileError::from_tds(&e) else {
            panic!("an I/O failure means SMILE is unavailable");
        };
        assert!(text.starts_with("I/O: "), "{text}");
        assert!(text.contains("os error 64"), "{text}");
    }

    #[test]
    fn a_rejected_connection_string_is_not_echoed() {
        let e = dl_tds::Error::Config {
            key: "password".into(),
            message: "secret-ish".into(),
        };
        assert!(!SmileError::from_tds(&e).to_string().contains("secret-ish"));
    }
}
