//! The checkout reads of `FolioWriter` (`ScanCheckedOutAsync`, `ScanCancelledAsync`,
//! `ScanWalkInCheckoutsAsync`, `ScanWalkInCheckoutsPageAsync`, `BuildPaddedExclusion`,
//! `SafeIdentifier`, `ParseUserInfo2`). Read-only, on a [`Session`].
//!
//! `datetime` parameters: .NET binds `DbType.DateTime`; `dl_tds` binds `datetime2`, so
//! each statement that compares a `datetime` column declares a `datetime` variable from
//! its parameter first (as the folio-observation scan does), keeping the .NET comparison.

use dl_decimal::Decimal;
use dl_tds::Param;
use dl_time::{DateTime, OffsetDateTime};
use dl_uuid::Uuid;

use crate::smile::folio_sql::in_list;
use crate::smile::{SmileError, hotel_offset, to_utc};
use crate::writer::session::{Rec, Session, decimal, int, nvarchar};

/// `FolioWriter.BookingTagLikePattern`.
pub const BOOKING_TAG_LIKE: &str = "MEM:%;BKG:%";

/// `CheckedOutFolio`: one booking whose every live folio is checked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedOut {
    /// `MAX(FolioNum)`.
    pub folio_num: Decimal,
    /// `ExternalConfirmNumber`.
    pub confirmation_code: String,
    /// From the agent's `UserInfo2` tag.
    pub booking_id: Uuid,
    /// From the agent's `UserInfo2` tag (`MEM:-` is none).
    pub member_id: Option<Uuid>,
    /// `MAX(CheckOutTime)` in UTC.
    pub checked_out_at: Option<OffsetDateTime>,
}

/// `WalkInCheckout`: a checked-out stay the Membership app did not create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkIn {
    /// `ConfirmNum`, the group.
    pub confirm_num: Decimal,
    /// The smallest folio of the group (the first guest).
    pub first_folio_num: Decimal,
    /// `MAX(ISNULL(CheckOutTime, DepartureDate))` at the hotel offset (+07:00).
    pub checked_out_at: OffsetDateTime,
    /// The raw phone of the first guest.
    pub guest_phone: String,
    /// `Folio.LastName` of the first folio (`None` when blank).
    pub guest_name: Option<String>,
}

impl WalkIn {
    /// `FolioRef`: `CONF:{ConfirmNum}` (invariant culture, scale kept).
    pub fn folio_ref(&self) -> String {
        format!("CONF:{}", self.confirm_num)
    }
}

/// `WalkInScanCursor`: the raw PMS checkout time and `ConfirmNum` of the last row read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalkInCursor {
    /// `ISNULL(CheckOutTime, DepartureDate)`, PMS local time.
    pub checked_out_at: DateTime,
    /// `ConfirmNum`.
    pub confirm_num: Decimal,
}

/// `WalkInScanPage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkInPage {
    /// The groups of this page.
    pub items: Vec<WalkIn>,
    /// The cursor of the last row when the page was full; `None` at the end.
    pub next: Option<WalkInCursor>,
}

/// `ParseUserInfo2`: `MEM:{guid|-};BKG:{guid}`; a malformed tag gives `(None, nil)`.
pub fn parse_user_info2(tag: Option<&str>) -> (Option<Uuid>, Uuid) {
    let mut member = None;
    let mut booking = Uuid::NIL;
    let Some(tag) = tag.filter(|t| !t.trim().is_empty()) else {
        return (member, booking);
    };
    for part in tag.split(';').filter(|p| !p.is_empty()) {
        let part = part.trim();
        if let Some(m) = part.strip_prefix("MEM:").and_then(|g| g.parse().ok()) {
            member = Some(m);
        } else if let Some(b) = part.strip_prefix("BKG:").and_then(|g| g.parse().ok()) {
            booking = b;
        }
    }
    (member, booking)
}

/// `SafeIdentifier`: letters, digits and `_`, not starting with a digit; bracketed.
pub fn safe_identifier(name: &str) -> Result<String, SmileError> {
    let bad = || SmileError::Invalid(format!("Tên cột SĐT không hợp lệ: '{name}'."));
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return Err(bad()),
    }
    if name.trim().is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(bad());
    }
    Ok(format!("[{name}]"))
}

/// `ExclusionBuckets`.
pub const EXCLUSION_BUCKETS: [usize; 4] = [16, 64, 256, 512];

/// `BuildPaddedExclusion`: `AND ConfirmNum NOT IN (@Ex0, …)` padded to the next bucket by
/// repeating the last value; empty for an empty list; the exact count above 512.
pub fn padded_exclusion(exclude: &[Decimal]) -> (String, Vec<Param>) {
    let Some(last) = exclude.last() else {
        return (String::new(), Vec::new());
    };
    let size = EXCLUSION_BUCKETS
        .iter()
        .copied()
        .find(|b| *b >= exclude.len())
        .unwrap_or(exclude.len());
    let mut names = Vec::with_capacity(size);
    let mut params = Vec::with_capacity(size);
    for i in 0..size {
        let name = format!("@Ex{i}");
        params.push(Param::new(&name, decimal(*exclude.get(i).unwrap_or(last))));
        names.push(name);
    }
    (
        format!("AND ConfirmNum NOT IN ({})", names.join(", ")),
        params,
    )
}

fn checked_out_sql(list: &str) -> String {
    format!(
        "SELECT MAX(FolioNum)          AS FolioNum,
       ExternalConfirmNumber  AS ConfirmationCode,
       MAX(CASE WHEN UserInfo2 LIKE @Tag THEN UserInfo2 END) AS UserInfo2,
       MAX(CheckOutTime)      AS CheckOutTime
FROM Folio
WHERE ISNULL(CancelNum, 0) = 0
  AND ExternalConfirmNumber IN ({list})
GROUP BY ExternalConfirmNumber
HAVING COUNT(*) = SUM(CASE WHEN FolioStatus = 3 THEN 1 ELSE 0 END)"
    )
}

fn cancelled_sql(list: &str) -> String {
    format!(
        "SELECT ExternalConfirmNumber
FROM Folio
WHERE ExternalConfirmNumber IN ({list})
GROUP BY ExternalConfirmNumber
HAVING SUM(CASE WHEN ISNULL(CancelNum, 0) = 0 THEN 1 ELSE 0 END) = 0"
    )
}

fn codes_param(codes: &[String]) -> (String, Vec<Param>) {
    in_list("Codes", codes, |c: &String| nvarchar(Some(c.as_str())))
}

/// `ScanCheckedOutAsync` for one chunk of at most 500 codes: the bookings whose every
/// live folio is `FolioStatus = 3`, with the agent's tag; an untagged booking is dropped.
pub fn scan_checked_out(
    db: &mut dyn Session,
    codes: &[String],
) -> Result<Vec<CheckedOut>, SmileError> {
    if codes.is_empty() {
        return Ok(Vec::new());
    }
    let (list, mut params) = codes_param(codes);
    params.push(Param::new("Tag", nvarchar(Some(BOOKING_TAG_LIKE))));
    let mut out = Vec::new();
    for r in db.rows(&checked_out_sql(&list), &params)? {
        let (member_id, booking_id) = parse_user_info2(r.opt_str("UserInfo2")?.as_deref());
        if booking_id == Uuid::NIL {
            continue;
        }
        out.push(CheckedOut {
            folio_num: r.dec("FolioNum")?,
            confirmation_code: r.opt_str("ConfirmationCode")?.unwrap_or_default(),
            booking_id,
            member_id,
            checked_out_at: r.opt_datetime("CheckOutTime")?.map(to_utc),
        });
    }
    Ok(out)
}

/// `ScanCancelledAsync` for one chunk: the codes whose every folio is cancelled.
pub fn scan_cancelled(db: &mut dyn Session, codes: &[String]) -> Result<Vec<String>, SmileError> {
    if codes.is_empty() {
        return Ok(Vec::new());
    }
    let (list, params) = codes_param(codes);
    db.rows(&cancelled_sql(&list), &params)?
        .iter()
        .map(|r| Ok(r.opt_str("ExternalConfirmNumber")?.unwrap_or_default()))
        .collect()
}

fn walk_in(r: &Rec) -> Result<(WalkIn, DateTime), SmileError> {
    let raw = r
        .opt_datetime("CheckedOutAt")?
        .ok_or(SmileError::Decode("CheckedOutAt"))?;
    Ok((
        WalkIn {
            confirm_num: r.dec("ConfirmNum")?,
            first_folio_num: r.dec("FirstFolioNum")?,
            checked_out_at: OffsetDateTime::new(raw, hotel_offset()),
            guest_phone: r.opt_str("GuestPhone")?.unwrap_or_default(),
            guest_name: r.opt_str("GuestName")?.filter(|n| !n.trim().is_empty()),
        },
        raw,
    ))
}

fn walk_in_sql(column: &str, exclude: &str) -> String {
    format!(
        "DECLARE @Since datetime = CAST(@SinceP AS datetime);
WITH g AS (
    SELECT ConfirmNum,
           MIN(FolioNum)      AS FirstFolioNum,
           MAX(ISNULL(CheckOutTime, DepartureDate)) AS CheckedOutAt,
           COUNT(*)           AS Total,
           SUM(CASE WHEN FolioStatus = 3 THEN 1 ELSE 0 END) AS Done
    FROM Folio
    WHERE ISNULL(CancelNum, 0) = 0
      AND ExternalConfirmNumber IS NULL
      AND (CheckOutTime >= @Since OR (CheckOutTime IS NULL AND DepartureDate >= @Since))
      {exclude}
    GROUP BY ConfirmNum
)
SELECT TOP (@Max)
       g.ConfirmNum     AS ConfirmNum,
       g.FirstFolioNum  AS FirstFolioNum,
       g.CheckedOutAt   AS CheckedOutAt,
       n.GuestPhone     AS GuestPhone,
       f.LastName       AS GuestName
FROM g
JOIN Folio f ON f.FolioNum = g.FirstFolioNum
CROSS APPLY (
    SELECT MIN(COALESCE(
               NULLIF(LTRIM(RTRIM(a.{column})), ''),
               NULLIF(LTRIM(RTRIM(a.TeleNum)), ''))) AS GuestPhone
    FROM AdditionalName a
    WHERE a.FolioNum = g.FirstFolioNum
      AND ISNULL(a.ACancelNum, 0) = 0
) n
WHERE g.Total = g.Done
  AND n.GuestPhone IS NOT NULL
ORDER BY g.CheckedOutAt, g.ConfirmNum"
    )
}

/// `ScanWalkInCheckoutsAsync`: at most `max` checked-out walk-in groups since `since`
/// (oldest first), minus `exclude`. A bad `phone_column` is [`SmileError::Invalid`].
pub fn scan_walk_ins(
    db: &mut dyn Session,
    phone_column: &str,
    since: OffsetDateTime,
    exclude: &[Decimal],
    max: i32,
) -> Result<Vec<WalkIn>, SmileError> {
    let column = safe_identifier(phone_column)?;
    let mut params = vec![
        Param::new("SinceP", since.to_offset(hotel_offset()).datetime()),
        Param::new("Max", int(max)),
    ];
    let (clause, ex) = padded_exclusion(exclude);
    params.extend(ex);
    db.rows(&walk_in_sql(&column, &clause), &params)?
        .iter()
        .map(|r| walk_in(r).map(|(w, _)| w))
        .collect()
}

fn walk_in_page_sql(column: &str, cursor: bool) -> String {
    let (declare, clause) = if cursor {
        (
            "DECLARE @CursorAt datetime = CAST(@CursorAtP AS datetime);\n",
            "AND (g.CheckedOutAt > @CursorAt OR (g.CheckedOutAt = @CursorAt AND g.ConfirmNum > @CursorConf))",
        )
    } else {
        ("", "")
    };
    format!(
        "DECLARE @From datetime = CAST(@FromP AS datetime);
DECLARE @To datetime = CAST(@ToP AS datetime);
{declare}WITH g AS (
    SELECT ConfirmNum,
           MIN(FolioNum)      AS FirstFolioNum,
           MAX(ISNULL(CheckOutTime, DepartureDate)) AS CheckedOutAt,
           COUNT(*)           AS Total,
           SUM(CASE WHEN FolioStatus = 3 THEN 1 ELSE 0 END) AS Done
    FROM Folio
    WHERE ISNULL(CancelNum, 0) = 0
      AND ExternalConfirmNumber IS NULL
    GROUP BY ConfirmNum
),
n AS (
    SELECT FolioNum,
           MIN(LTRIM(RTRIM({column}))) AS GuestPhone
    FROM AdditionalName
    WHERE ISNULL(ACancelNum, 0) = 0
      AND LTRIM(RTRIM(ISNULL({column}, ''))) <> ''
    GROUP BY FolioNum
)
SELECT TOP (@Max)
       g.ConfirmNum     AS ConfirmNum,
       g.FirstFolioNum  AS FirstFolioNum,
       g.CheckedOutAt   AS CheckedOutAt,
       n.GuestPhone     AS GuestPhone,
       f.LastName       AS GuestName
FROM g
JOIN Folio f ON f.FolioNum = g.FirstFolioNum
JOIN n ON n.FolioNum = g.FirstFolioNum
WHERE g.Total = g.Done
  AND g.CheckedOutAt >= @From
  AND g.CheckedOutAt < @To
  {clause}
ORDER BY g.CheckedOutAt, g.ConfirmNum"
    )
}

/// `ScanWalkInCheckoutsPageAsync`: one keyset page of `[from, to)` after `cursor`.
pub fn scan_walk_ins_page(
    db: &mut dyn Session,
    phone_column: &str,
    from: DateTime,
    to: DateTime,
    cursor: Option<WalkInCursor>,
    max: i32,
) -> Result<WalkInPage, SmileError> {
    let column = safe_identifier(phone_column)?;
    let mut params = vec![
        Param::new("FromP", from),
        Param::new("ToP", to),
        Param::new("Max", int(max)),
    ];
    if let Some(c) = cursor {
        params.push(Param::new("CursorAtP", c.checked_out_at));
        params.push(Param::new("CursorConf", decimal(c.confirm_num)));
    }
    let rows = db.rows(&walk_in_page_sql(&column, cursor.is_some()), &params)?;
    let mut items = Vec::with_capacity(rows.len());
    let mut last = None;
    for r in &rows {
        let (w, raw) = walk_in(r)?;
        last = Some(WalkInCursor {
            checked_out_at: raw,
            confirm_num: w.confirm_num,
        });
        items.push(w);
    }
    let full = i32::try_from(rows.len()).unwrap_or(i32::MAX) >= max;
    Ok(WalkInPage {
        items,
        next: if full { last } else { None },
    })
}
