//! The availability and room reads (`FolioWriter.ScanOccupancy/RoomBlocks/GroupBlocks/
//! RoomInventory`, `ProbeOccupancyAsync`, `RoomTypeCatalogReader`, `ExtraBedUsageReader`).
//!
//! The statement bodies are the .NET ones. .NET binds dates as `datetime` (`DbType.DateTime`)
//! while dl-tds binds `datetime2`, so each statement starts by declaring the `datetime`
//! variables the body uses from the `datetime2` parameters; comparisons stay
//! `datetime` against `datetime`, as on the .NET agent.

use std::collections::{BTreeMap, HashMap, HashSet};

use dl_tds::{Param, ParamValue, Row};
use dl_time::Date;

use super::{
    AvailabilitySource, ExtraBedUsage, GroupBlock, OccupancyFingerprint, OccupancyStay, RoomBlock,
    RoomRow, RoomSource, SmileError, TdsSmile, rows,
};
use crate::wire::RoomTypeLine;

const DECLARE_RANGE: &str =
    "DECLARE @From datetime = CAST(@FromP AS datetime), @To datetime = CAST(@ToP AS datetime);\n";

const PROBE_SQL: &str = "\
SELECT fo.FolioCount, ISNULL(fo.FolioHash, 0) AS FolioHash, ISNULL(fo.FolioSum, 0) AS FolioSum,
       fo.OooCount, ISNULL(fo.OooHash, 0) AS OooHash,
       ro.RoomCount, ISNULL(ro.RoomHash, 0) AS RoomHash,
       oo.OosCount, ISNULL(oo.OosHash, 0) AS OosHash,
       gb.GroupBlockCount, ISNULL(gb.GroupBlockHash, 0) AS GroupBlockHash,
       ISNULL(gb.GroupBlockSum, 0) AS GroupBlockSum,
       (SELECT TOP (1) HotelDate FROM HotelParameter) AS HotelDate,
       CAST(CAST(GETDATE() AS date) AS datetime) AS PmsDate
FROM (
    SELECT COUNT_BIG(k.Guest) AS FolioCount,
           CHECKSUM_AGG(k.Guest) AS FolioHash,
           SUM(CAST(k.Guest AS bigint)) AS FolioSum,
           COUNT_BIG(k.Ooo) AS OooCount,
           CHECKSUM_AGG(k.Ooo) AS OooHash
    FROM Folio f
    CROSS APPLY (
        SELECT CASE WHEN f.RoomTypeBooked IS NOT NULL
                     AND LTRIM(RTRIM(f.RoomTypeBooked)) NOT IN ('', '**')
                     AND ISNULL(f.FolioStatus, 0) NOT IN (4, 5)
                    THEN CHECKSUM(f.FolioNum, f.FolioSubNum, f.RoomTypeBooked, f.ArrivalDate, f.DepartureDate,
                                  f.GroupCode, f.RoomCode)
               END AS Guest,
               CASE WHEN f.FolioStatus IN (4, 5)
                     AND f.RoomCode IS NOT NULL
                    THEN CHECKSUM(f.FolioNum, f.FolioSubNum, f.RoomCode, f.ArrivalDate, f.DepartureDate)
               END AS Ooo
    ) k
    WHERE ISNULL(f.CancelNum, 0) = 0
      AND f.ArrivalDate < @To
      AND f.DepartureDate > @From
) fo
CROSS JOIN (
    SELECT COUNT_BIG(*) AS RoomCount,
           CHECKSUM_AGG(CHECKSUM(r.RoomCode, r.RoomTypeCode)) AS RoomHash
    FROM Room r
    WHERE ISNULL(r.RoomTypeCode, '') <> '**'
      AND r.RoomCode IS NOT NULL
) ro
CROSS JOIN (
    SELECT COUNT_BIG(*) AS OosCount,
           CHECKSUM_AGG(CHECKSUM(o.nSeq, o.RoomCode, o.FromDate, o.ToDate)) AS OosHash
    FROM RoomOOS o
    WHERE ISNULL(o.isCleared, 0) = 0
      AND o.ClearTime IS NULL
      AND o.FromDate < @To
      AND (o.ToDate IS NULL OR o.ToDate >= @From)
) oo
CROSS JOIN (
    SELECT COUNT_BIG(*) AS GroupBlockCount,
           CHECKSUM_AGG(x.h) AS GroupBlockHash,
           SUM(CAST(x.h AS bigint)) AS GroupBlockSum
    FROM GroupBlockDetail b
    LEFT JOIN GroupFolio g ON g.GroupCode = b.AllocationID
    CROSS APPLY (
        SELECT CHECKSUM(b.ID, b.AllocationID, b.RoomTypeCode, b.FromDate, b.ToDate, b.NumRoom,
                        b.Cancelled, b.DReleaseDate, b.DReleaseDays,
                        g.IsCanceled, g.GroupStatusCode, g.ConfirmStatus, g.ReleaseDate) AS h
    ) x
    WHERE b.FromDate < @To
      AND b.ToDate > @From
) gb
OPTION (MAXDOP 1)";

const OCCUPANCY_SQL: &str = "\
SELECT TOP (@Max)
       RoomTypeBooked AS RoomTypeCode,
       ArrivalDate,
       DepartureDate,
       NULLIF(LTRIM(RTRIM(GroupCode)), '') AS GroupCode,
       NULLIF(LTRIM(RTRIM(RoomCode)), '') AS RoomCode
FROM Folio
WHERE ISNULL(CancelNum, 0) = 0
  AND RoomTypeBooked IS NOT NULL
  AND LTRIM(RTRIM(RoomTypeBooked)) NOT IN ('', '**')
  AND ISNULL(FolioStatus, 0) NOT IN (4, 5)
  AND ArrivalDate < @To
  AND DepartureDate > @From
ORDER BY ArrivalDate";

const ROOM_BLOCKS_SQL: &str = "\
SELECT LTRIM(RTRIM(r.RoomCode)) AS RoomCode,
       LTRIM(RTRIM(r.RoomTypeCode)) AS RoomTypeCode,
       CAST(CAST(o.FromDate AS date) AS datetime) AS FromDate,
       CASE WHEN o.ToDate IS NULL OR CAST(o.ToDate AS date) >= CAST(@To AS date)
            THEN @To
            ELSE CAST(DATEADD(day, 1, CAST(o.ToDate AS date)) AS datetime)
       END AS ToExclusive
FROM RoomOOS o
JOIN Room r ON r.RoomCode = o.RoomCode
WHERE ISNULL(o.isCleared, 0) = 0
  AND o.ClearTime IS NULL
  AND o.FromDate < @To
  AND (o.ToDate IS NULL OR o.ToDate >= @From)
  AND ISNULL(r.RoomTypeCode, '') <> '**'
  AND r.RoomCode IS NOT NULL
UNION ALL
SELECT LTRIM(RTRIM(r.RoomCode)),
       LTRIM(RTRIM(r.RoomTypeCode)),
       CAST(CAST(f.ArrivalDate AS date) AS datetime),
       CAST(CAST(f.DepartureDate AS date) AS datetime)
FROM Folio f
JOIN Room r ON r.RoomCode = f.RoomCode
WHERE f.FolioStatus IN (4, 5)
  AND ISNULL(f.CancelNum, 0) = 0
  AND f.ArrivalDate < @To
  AND f.DepartureDate > @From
  AND ISNULL(r.RoomTypeCode, '') <> '**'
  AND r.RoomCode IS NOT NULL
OPTION (MAXDOP 1)";

const GROUP_BLOCKS_SQL: &str = "\
DECLARE @HotelDate datetime = ISNULL((SELECT TOP (1) HotelDate FROM HotelParameter), @From);
SELECT LTRIM(RTRIM(b.AllocationID)) AS GroupCode,
       LTRIM(RTRIM(b.RoomTypeCode)) AS RoomTypeCode,
       b.FromDate,
       b.ToDate,
       b.NumRoom
FROM GroupBlockDetail b
JOIN GroupFolio g ON g.GroupCode = b.AllocationID
WHERE ISNULL(b.Cancelled, 0) = 0
  AND ISNULL(g.IsCanceled, 0) = 0
  AND b.NumRoom > 0
  AND b.AllocationID IS NOT NULL
  AND b.RoomTypeCode IS NOT NULL
  AND LTRIM(RTRIM(b.RoomTypeCode)) NOT IN ('', '**')
  AND b.FromDate < @To
  AND b.ToDate > @From
  AND (b.DReleaseDate IS NULL OR b.DReleaseDate > @HotelDate)
  AND (   (g.GroupStatusCode IN ('0', '1')
           AND (ISNULL(g.ConfirmStatus, 0) > 0 OR COALESCE(g.ReleaseDate, b.FromDate) > @HotelDate))
       OR (g.GroupStatusCode = '3'
           AND b.FromDate >= DATEADD(day, ISNULL(b.DReleaseDays, 0), @HotelDate)))
OPTION (MAXDOP 1)";

const ROOMS_SQL: &str = "\
SELECT r.RoomCode, r.RoomName, r.RoomTypeCode, r.Floor,
       r.RStatus AS StsCode,
       r.HSKPClean, r.Inspected, r.TouchedUp,
       CASE WHEN oos.RoomCode IS NULL THEN 0 ELSE 1 END AS OutOfService,
       oos.Reason AS OutOfServiceReason,
       CASE WHEN occ.RoomCode IS NULL THEN 0 ELSE 1 END AS IsOccupied,
       CASE WHEN arr.RoomCode IS NULL THEN 0 ELSE 1 END AS ArrivalDueToday,
       CASE WHEN dep.RoomCode IS NULL THEN 0 ELSE 1 END AS DepartureDueToday,
       CASE WHEN ooo.RoomCode IS NULL THEN 0 ELSE 1 END AS OutOfOrder,
       ooo.Reason AS OutOfOrderReason,
       ooo.OutOfOrderUntilRaw AS OutOfOrderUntil
FROM Room r
OUTER APPLY (
    SELECT TOP 1 o.RoomCode,
           NULLIF(LTRIM(RTRIM(COALESCE(o.HSKPNotes, o.ENGNotes, ''))), '') AS Reason
    FROM RoomOOS o
    WHERE o.RoomCode = r.RoomCode
      AND CAST(GETDATE() AS date) BETWEEN CAST(o.FromDate AS date) AND CAST(ISNULL(o.ToDate, '9999-12-31') AS date)
      AND ISNULL(o.isCleared, 0) = 0
      AND o.ClearTime IS NULL
    ORDER BY o.FromDate DESC, o.nSeq DESC
) oos
OUTER APPLY (
    SELECT TOP 1 f.RoomCode
    FROM Folio f
    WHERE f.RoomCode = r.RoomCode
      AND f.FolioStatus = 2
      AND ISNULL(f.CancelNum, 0) = 0
) occ
OUTER APPLY (
    SELECT TOP 1 f.RoomCode
    FROM Folio f
    WHERE f.RoomCode = r.RoomCode
      AND f.FolioStatus = 1
      AND ISNULL(f.CancelNum, 0) = 0
      AND ISNULL(f.NoShowFlag, 0) = 0
      AND CAST(f.ArrivalDate AS date) = CAST(GETDATE() AS date)
) arr
OUTER APPLY (
    SELECT TOP 1 f.RoomCode
    FROM Folio f
    WHERE f.RoomCode = r.RoomCode
      AND f.FolioStatus = 2
      AND ISNULL(f.CancelNum, 0) = 0
      AND CAST(f.DepartureDate AS date) <= CAST(GETDATE() AS date)
) dep
OUTER APPLY (
    SELECT TOP 1 f.RoomCode,
           NULLIF(LTRIM(RTRIM(f.FirstName)), '') AS Reason,
           f.DepartureDate AS OutOfOrderUntilRaw
    FROM Folio f
    WHERE f.RoomCode = r.RoomCode
      AND f.FolioStatus IN (4, 5)
      AND ISNULL(f.CancelNum, 0) = 0
      AND CAST(f.ArrivalDate AS date) <= CAST(GETDATE() AS date)
      AND CAST(GETDATE() AS date) < CAST(f.DepartureDate AS date)
    ORDER BY f.DepartureDate DESC
) ooo
WHERE ISNULL(r.RoomTypeCode, '') <> '**'
  AND r.RoomCode IS NOT NULL";

const ROOM_TYPES_SQL: &str = "\
SELECT t.RoomTypeCode, t.Description, t.NumRoom, t.DisplayOrder
FROM dbo.RoomType t
WHERE LTRIM(RTRIM(t.RoomTypeCode)) <> '**'
ORDER BY t.DisplayOrder, t.RoomTypeCode";

const EXTRA_BED_SCHEMA_SQL: &str = "\
SELECT OBJECT_NAME(c.object_id) AS TableName, c.name AS ColumnName
FROM sys.columns c
WHERE c.object_id IN (OBJECT_ID(N'dbo.GlobalParameter'), OBJECT_ID(N'dbo.Folio'), OBJECT_ID(N'dbo.Folio_Special'))";

const SPECIAL_BY_DATE_SQL: &str = "\
SELECT DISTINCT TOP (2) LTRIM(RTRIM(ISNULL(ParaString, ''))) AS Value
FROM dbo.GlobalParameter
WHERE LTRIM(RTRIM(ParaName)) = @Name
  AND (@HotelId IS NULL OR LTRIM(RTRIM(HotelID)) = @HotelId)";

const SPECIAL_ROWS_SQL: &str = "\
SELECT TOP (@Max) f.ArrivalDate, f.DepartureDate,
       CAST(s.Qty AS int) AS Qty, CAST(s.FromDay AS int) AS FromDay, CAST(s.ToDay AS int) AS ToDay
FROM dbo.Folio_Special s
JOIN (SELECT FolioNum, MIN(ArrivalDate) AS ArrivalDate, MAX(DepartureDate) AS DepartureDate
FROM dbo.Folio
WHERE ISNULL(CancelNum, 0) = 0
  AND ISNULL(FolioStatus, 0) NOT IN (4, 5)
  AND ArrivalDate IS NOT NULL AND DepartureDate IS NOT NULL
  AND ArrivalDate < @To
  AND (DepartureDate > @From OR ArrivalDate >= @From)
GROUP BY FolioNum) f ON f.FolioNum = s.FolioNum
WHERE LTRIM(RTRIM(s.SpecialCode)) = @Code
  AND s.Qty > 0";

const EXTRA_BED_ROWS_SQL: &str = "\
SELECT TOP (@Max) ArrivalDate, DepartureDate, CAST(ExtraBed AS int) AS Qty,
       CAST(NULL AS int) AS FromDay, CAST(NULL AS int) AS ToDay
FROM dbo.Folio
WHERE ISNULL(CancelNum, 0) = 0
  AND ISNULL(FolioStatus, 0) NOT IN (4, 5)
  AND ArrivalDate IS NOT NULL AND DepartureDate IS NOT NULL
  AND ArrivalDate < @To
  AND (DepartureDate > @From OR ArrivalDate >= @From)
  AND ExtraBed > 0";

fn range(from: Date, to: Date) -> [Param; 2] {
    [
        Param::new("FromP", from.midnight()),
        Param::new("ToP", to.midnight()),
    ]
}

fn with_range(body: &str) -> String {
    format!("{DECLARE_RANGE}{body}")
}

/// Clamps `[start, end)` into `[from, to)`; `None` when nothing is left.
fn clamp(start: Date, end: Date, from: Date, to: Date) -> Option<(Date, Date)> {
    let s = start.max(from);
    let e = end.min(to);
    (e > s).then_some((s, e))
}

/// One extra-bed source row (`UsageRow`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UsageRow {
    pub arrival: Date,
    pub departure: Date,
    pub qty: i64,
    pub from_day: Option<i64>,
    pub to_day: Option<i64>,
}

/// `ExtraBedUsageReader.Count`: beds per night in `[from, to)`.
pub(crate) fn count_extra_beds(rows: &[UsageRow], from: Date, to: Date) -> BTreeMap<Date, i64> {
    let mut beds = BTreeMap::new();
    for row in rows.iter().filter(|r| r.qty > 0) {
        let last = if row.departure > row.arrival {
            row.departure.pred()
        } else {
            row.arrival
        };
        let mut night = row.arrival;
        while night <= last && night < to {
            if night >= from {
                let day_no = i64::from(row.arrival.days_until(night)) + 1;
                let before = row.from_day.is_some_and(|first| day_no < first);
                let after = row.to_day.is_some_and(|end| day_no > end);
                if !before && !after {
                    *beds.entry(night).or_insert(0) += row.qty;
                }
            }
            night = night.succ();
        }
    }
    beds
}

impl TdsSmile {
    fn schema(&self) -> Result<HashMap<String, HashSet<String>>, SmileError> {
        let mut schema: HashMap<String, HashSet<String>> = HashMap::new();
        for row in self.rows(EXTRA_BED_SCHEMA_SQL, &[])? {
            let table = rows::text(&row, "TableName")?.to_lowercase();
            let column = rows::text(&row, "ColumnName")?.to_lowercase();
            schema.entry(table).or_default().insert(column);
        }
        Ok(schema)
    }

    /// `ReadSpecialByDateAsync`: `Ok(on)` or a log-safe problem.
    fn special_by_date(
        &self,
        schema: &HashMap<String, HashSet<String>>,
    ) -> Result<Result<bool, String>, SmileError> {
        if !has(
            schema,
            "GlobalParameter",
            &["HotelID", "ParaName", "ParaString"],
        ) {
            return Ok(Ok(false));
        }
        let values = self.rows(
            SPECIAL_BY_DATE_SQL,
            &[
                Param::new("Name", "SpecialByDate"),
                Param::new("HotelId", ParamValue::String(None)),
            ],
        )?;
        let values = values
            .iter()
            .map(|r| rows::text(r, "Value"))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(match values.as_slice() {
            [] => Ok(false),
            [one] => Ok(one == "1"),
            _ => Err("GlobalParameter SpecialByDate differs between HotelIDs".into()),
        })
    }
}

fn has(schema: &HashMap<String, HashSet<String>>, table: &str, columns: &[&str]) -> bool {
    schema
        .get(&table.to_lowercase())
        .is_some_and(|present| columns.iter().all(|c| present.contains(&c.to_lowercase())))
}

fn room_row(r: &Row) -> Result<RoomRow, SmileError> {
    Ok(RoomRow {
        room_code: rows::text(r, "RoomCode")?,
        room_name: rows::opt_str(r, "RoomName")?,
        room_type_code: rows::text(r, "RoomTypeCode")?,
        floor: rows::opt_int(r, "Floor")?,
        sts_code: rows::opt_int(r, "StsCode")?,
        hskp_clean: rows::opt_bool(r, "HSKPClean")?,
        inspected: rows::opt_bool(r, "Inspected")?,
        touched_up: rows::opt_int(r, "TouchedUp")?,
        out_of_service: rows::boolean(r, "OutOfService")?,
        out_of_service_reason: rows::opt_str(r, "OutOfServiceReason")?,
        is_occupied: rows::boolean(r, "IsOccupied")?,
        arrival_due_today: rows::boolean(r, "ArrivalDueToday")?,
        departure_due_today: rows::boolean(r, "DepartureDueToday")?,
        out_of_order: rows::boolean(r, "OutOfOrder")?,
        out_of_order_reason: rows::opt_str(r, "OutOfOrderReason")?,
        out_of_order_until: rows::opt_datetime(r, "OutOfOrderUntil")?.map(|d| d.date()),
    })
}

impl RoomSource for TdsSmile {
    fn scan_rooms(&self) -> Result<Vec<RoomRow>, SmileError> {
        self.rows(ROOMS_SQL, &[])?.iter().map(room_row).collect()
    }

    fn room_types(&self) -> Result<Vec<RoomTypeLine>, SmileError> {
        let mut out = Vec::new();
        for r in self.rows(ROOM_TYPES_SQL, &[])? {
            let Some(code) = rows::trimmed(&r, "RoomTypeCode")? else {
                continue;
            };
            out.push(RoomTypeLine {
                pms_room_type_code: code,
                pms_name: rows::trimmed(&r, "Description")?,
                num_room: rows::opt_int(&r, "NumRoom")?,
                display_order: rows::opt_int(&r, "DisplayOrder")?,
            });
        }
        Ok(out)
    }
}

impl AvailabilitySource for TdsSmile {
    fn probe_occupancy(&self, from: Date, to: Date) -> Result<OccupancyFingerprint, SmileError> {
        let found = self.rows(&with_range(PROBE_SQL), &range(from, to))?;
        let r = found.first().ok_or(SmileError::Decode("FolioCount"))?;
        Ok(OccupancyFingerprint {
            folio_count: rows::int(r, "FolioCount")?,
            folio_hash: rows::int(r, "FolioHash")?,
            folio_sum: rows::int(r, "FolioSum")?,
            room_count: rows::int(r, "RoomCount")?,
            room_hash: rows::int(r, "RoomHash")?,
            pms_date: rows::date(r, "PmsDate")?,
            ooo_count: rows::int(r, "OooCount")?,
            ooo_hash: rows::int(r, "OooHash")?,
            oos_count: rows::int(r, "OosCount")?,
            oos_hash: rows::int(r, "OosHash")?,
            group_block_count: rows::int(r, "GroupBlockCount")?,
            group_block_hash: rows::int(r, "GroupBlockHash")?,
            group_block_sum: rows::int(r, "GroupBlockSum")?,
            hotel_date: rows::opt_datetime(r, "HotelDate")?,
        })
    }

    fn scan_occupancy(
        &self,
        from: Date,
        to: Date,
        max: i64,
    ) -> Result<Vec<OccupancyStay>, SmileError> {
        let [f, t] = range(from, to);
        let found = self.rows(&with_range(OCCUPANCY_SQL), &[f, t, Param::new("Max", max)])?;
        found
            .iter()
            .map(|r| {
                Ok(OccupancyStay {
                    room_type_code: rows::text(r, "RoomTypeCode")?.trim().to_owned(),
                    arrival: rows::date(r, "ArrivalDate")?,
                    departure: rows::date(r, "DepartureDate")?,
                    group_code: rows::opt_str(r, "GroupCode")?,
                    room_code: rows::opt_str(r, "RoomCode")?,
                })
            })
            .collect()
    }

    fn scan_room_blocks(&self, from: Date, to: Date) -> Result<Vec<RoomBlock>, SmileError> {
        if to <= from {
            return Ok(Vec::new());
        }
        let mut blocks = Vec::new();
        for r in self.rows(&with_range(ROOM_BLOCKS_SQL), &range(from, to))? {
            let room_type_code = rows::text(&r, "RoomTypeCode")?;
            if room_type_code.is_empty() {
                continue;
            }
            let start = rows::date(&r, "FromDate")?;
            let end = rows::date(&r, "ToExclusive")?;
            if let Some((from, to_exclusive)) = clamp(start, end, from, to) {
                blocks.push(RoomBlock {
                    room_code: rows::text(&r, "RoomCode")?,
                    room_type_code,
                    from,
                    to_exclusive,
                });
            }
        }
        Ok(blocks)
    }

    fn scan_group_blocks(&self, from: Date, to: Date) -> Result<Vec<GroupBlock>, SmileError> {
        if to <= from {
            return Ok(Vec::new());
        }
        let mut blocks = Vec::new();
        for r in self.rows(&with_range(GROUP_BLOCKS_SQL), &range(from, to))? {
            let group_code = rows::text(&r, "GroupCode")?;
            let room_type_code = rows::text(&r, "RoomTypeCode")?;
            if group_code.is_empty() || room_type_code.is_empty() {
                continue;
            }
            let start = rows::date(&r, "FromDate")?;
            let end = rows::date(&r, "ToDate")?;
            if let Some((from, to_exclusive)) = clamp(start, end, from, to) {
                blocks.push(GroupBlock {
                    group_code,
                    room_type_code,
                    from,
                    to_exclusive,
                    num_room: rows::int(&r, "NumRoom")?,
                });
            }
        }
        Ok(blocks)
    }

    fn extra_bed_usage(
        &self,
        from: Date,
        to: Date,
        max_rows: i64,
        special_code: &str,
    ) -> Result<ExtraBedUsage, SmileError> {
        let schema = self.schema()?;
        let folio = [
            "FolioNum",
            "ArrivalDate",
            "DepartureDate",
            "CancelNum",
            "FolioStatus",
        ];
        if !has(&schema, "Folio", &folio) {
            return Ok(ExtraBedUsage::Unknown(
                "SMILE lacks the Folio columns needed to count extra beds per night".into(),
            ));
        }
        let on = match self.special_by_date(&schema)? {
            Ok(on) => on,
            Err(problem) => return Ok(ExtraBedUsage::Unknown(problem)),
        };
        let sql = if on {
            let special = ["FolioNum", "SpecialCode", "Qty", "FromDay", "ToDay"];
            if !has(&schema, "Folio_Special", &special) {
                return Ok(ExtraBedUsage::Unknown(
                    "SpecialByDate = 1 but SMILE has no Folio_Special(FolioNum, SpecialCode, Qty, FromDay, ToDay)"
                        .into(),
                ));
            }
            SPECIAL_ROWS_SQL
        } else {
            if !has(&schema, "Folio", &["ExtraBed"]) {
                return Ok(ExtraBedUsage::Unknown(
                    "SpecialByDate is off but SMILE has no Folio.ExtraBed column".into(),
                ));
            }
            EXTRA_BED_ROWS_SQL
        };
        let [f, t] = range(from, to);
        let found = self.rows(
            &with_range(sql),
            &[
                f,
                t,
                Param::new("Max", max_rows),
                Param::new("Code", special_code),
            ],
        )?;
        if i64::try_from(found.len()).unwrap_or(i64::MAX) >= max_rows {
            return Ok(ExtraBedUsage::Unknown(format!(
                "hit the {max_rows}-row cap while counting extra beds; not reported this time"
            )));
        }
        let usage = found
            .iter()
            .map(|r| {
                Ok(UsageRow {
                    arrival: rows::date(r, "ArrivalDate")?,
                    departure: rows::date(r, "DepartureDate")?,
                    qty: rows::int(r, "Qty")?,
                    from_day: rows::opt_int(r, "FromDay")?,
                    to_day: rows::opt_int(r, "ToDay")?,
                })
            })
            .collect::<Result<Vec<_>, SmileError>>()?;
        Ok(ExtraBedUsage::Beds(count_extra_beds(&usage, from, to)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(day: u8) -> Date {
        Date::new(2026, 3, day).unwrap()
    }

    #[test]
    fn extra_beds_count_nights_and_respect_the_day_window() {
        let rows = [
            UsageRow {
                arrival: d(1),
                departure: d(4),
                qty: 1,
                from_day: None,
                to_day: None,
            },
            UsageRow {
                arrival: d(2),
                departure: d(5),
                qty: 2,
                from_day: Some(2),
                to_day: Some(2),
            },
            UsageRow {
                arrival: d(3),
                departure: d(3),
                qty: 1,
                from_day: None,
                to_day: None,
            },
            UsageRow {
                arrival: d(1),
                departure: d(9),
                qty: 0,
                from_day: None,
                to_day: None,
            },
        ];
        let beds = count_extra_beds(&rows, d(2), d(4));
        assert_eq!(beds.get(&d(1)), None, "before the window");
        assert_eq!(beds.get(&d(2)), Some(&1));
        assert_eq!(
            beds.get(&d(3)),
            Some(&(1 + 2 + 1)),
            "day 2 of the second stay; day-use stay"
        );
        assert_eq!(beds.get(&d(4)), None, "the window is half-open");
    }

    #[test]
    fn blocks_are_clamped_to_the_horizon() {
        assert_eq!(clamp(d(1), d(5), d(2), d(4)), Some((d(2), d(4))));
        assert_eq!(clamp(d(4), d(6), d(2), d(4)), None);
    }
}
