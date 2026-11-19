//! The folio-observation reads (`FolioObservationReader`, `EarnPairReader`).
//!
//! Dapper expands `IN @List`; here the list is expanded by hand into numbered parameters,
//! 500 at a time like the .NET chunking. `FolioTransaction.FolioNum` is `varchar(10)`, so
//! its parameters go as `varchar` (Dapper sent `nvarchar`, which converts the column side
//! and loses the index); every other value keeps the .NET type.

use std::collections::HashSet;

use dl_decimal::Decimal;
use dl_tds::{Param, ParamValue, Row};
use dl_time::DateTime;

use super::{
    CodeRow, EarnCatalog, FolioRow, FolioSource, GuestRow, OccupancyColumns, RevenueRow,
    SmileError, TdsSmile, rows,
};
use crate::folio::earn::{EarnPair, filter};

/// `SqlServerInParamLimit`.
pub(crate) const IN_LIMIT: usize = 500;

const CATALOG_SQL: &str = "\
SELECT LTRIM(RTRIM(TransactionCode)) AS Code, LTRIM(RTRIM(TransactionSubCode)) AS Sub,
       ISNULL(AutoPost, 0) AS AutoPost
FROM dbo.TransactionCode;

SELECT DISTINCT CONVERT(varchar(12), TrnCode) AS Code, CONVERT(varchar(12), ISNULL(TrnSubCode, 0)) AS Sub
FROM dbo.RoomType
WHERE ISNULL(TrnCode, 0) <> 0;

SELECT DISTINCT LTRIM(RTRIM(TrnCode)) AS Code, LTRIM(RTRIM(TrnSubCode)) AS Sub
FROM dbo.PackageCode
WHERE NULLIF(LTRIM(RTRIM(TrnCode)), '') IS NOT NULL;

SELECT LTRIM(RTRIM(ParaName)) AS Code, LTRIM(RTRIM(ParaString)) AS Sub
FROM dbo.GlobalParameter
WHERE ParaName IN ('EBedSurChgTrnCode', 'EBedSurChgTrnSub');

SELECT LTRIM(RTRIM(TrnCode)) AS Code, LTRIM(RTRIM(TrnSubCode)) AS Sub
FROM dbo.SpecialService
WHERE SpecialServiceCode = @ExtraBedCode
  AND ISNULL(IncludeInRoomCharge, 0) = 0
  AND NULLIF(LTRIM(RTRIM(TrnCode)), '') IS NOT NULL;";

const DECLARE_WATERMARK: &str = "DECLARE @Watermark datetime = CAST(@WatermarkP AS datetime);\n";

const RM_CHG_TRN_SQL: &str = "\
SELECT DISTINCT f.RmChgTrn
FROM dbo.Folio f
WHERE f.LastChangeTime >= @Watermark AND f.FolioStatus IN (1,2,3)
  AND ISNULL(f.RmChgTrn, 0) <> 0";

const OCCUPANCY_SCHEMA_SQL: &str = "\
SELECT OBJECT_NAME(c.object_id) AS TableName, c.name AS ColumnName
FROM sys.columns c
WHERE c.object_id IN (OBJECT_ID(N'dbo.Folio'), OBJECT_ID(N'dbo.Folio_Special'))
  AND c.name IN ('NumEnf', 'ExtraBed', 'FolioNum', 'SpecialCode', 'Qty', 'CancelNum')";

fn folio_sql(columns: OccupancyColumns) -> String {
    let num_enf = if columns.num_enf {
        "f.NumEnf"
    } else {
        "CAST(NULL AS int)"
    };
    let extra_bed = if columns.extra_bed {
        "f.ExtraBed"
    } else {
        "CAST(NULL AS int)"
    };
    format!(
        "{DECLARE_WATERMARK}\
SELECT TOP (@MaxRow)
       f.FolioNum, f.FolioSubNum, CAST(f.ID AS bigint) AS FolioId, f.FolioStatus,
       f.CheckInTime, f.CheckOutTime, f.LastChangeTime,
       f.ArrivalDate, f.DepartureDate,
       f.ConfirmNum, f.ExternalConfirmNumber,
       f.CancelNum, f.CancelTime, ISNULL(f.NoShowFlag, 0) AS NoShowFlag,
       f.RoomCode, f.UserInfo1, f.LastName,
       f.RoomTypeBooked, f.RoomTypeCode,
       f.NumAdult, f.NumChild, CAST({num_enf} AS int) AS NumEnf, CAST({extra_bed} AS int) AS ExtraBed,
       f.OriginCode, f.MarketSegmentCode, f.RoomRateCode
FROM dbo.Folio f
WHERE f.LastChangeTime >= @Watermark
  AND f.FolioStatus IN (1,2,3)
ORDER BY f.LastChangeTime, f.FolioNum, f.FolioSubNum"
    )
}

fn guests_sql(column: &str, list: &str) -> String {
    format!(
        "\
SELECT FolioNum, Phone, Name
FROM (
    SELECT y.FolioNum, y.Phone, y.Name,
           ROW_NUMBER() OVER (
               PARTITION BY y.FolioNum
               ORDER BY CASE WHEN y.Phone = '' THEN 1 ELSE 0 END, y.IdAddition) AS Rn
    FROM (
        SELECT a.FolioNum,
               a.IdAddition,
               LTRIM(RTRIM(COALESCE(
                   NULLIF(LTRIM(RTRIM(a.{column})), ''),
                   NULLIF(LTRIM(RTRIM(a.TeleNum)), ''),
                   ''))) AS Phone,
               LTRIM(RTRIM(ISNULL(a.FirstName, '') + ' ' + ISNULL(a.LastName, ''))) AS Name
        FROM dbo.AdditionalName a
        WHERE a.FolioNum IN ({list})
          AND ISNULL(a.ACancelNum, 0) = 0
    ) y
) x
WHERE x.Rn = 1"
    )
}

/// `IN (@F0,@F1,…)` for `values` and the matching parameters.
pub(crate) fn in_list<T: Clone>(
    prefix: &str,
    values: &[T],
    to_param: impl Fn(&T) -> ParamValue,
) -> (String, Vec<Param>) {
    let mut names = Vec::with_capacity(values.len());
    let mut params = Vec::with_capacity(values.len());
    for (i, v) in values.iter().enumerate() {
        let name = format!("@{prefix}{i}");
        params.push(Param::new(&name, to_param(v)));
        names.push(name);
    }
    (names.join(","), params)
}

fn code_rows(set: Option<&Vec<Row>>) -> Result<Vec<CodeRow>, SmileError> {
    set.map_or(&[][..], Vec::as_slice)
        .iter()
        .map(|r| {
            Ok(CodeRow {
                code: rows::text(r, "Code")?.trim().to_owned(),
                sub: rows::opt_str(r, "Sub")?.map(|s| s.trim().to_owned()),
            })
        })
        .collect()
}

fn folio_row(r: &Row) -> Result<FolioRow, SmileError> {
    Ok(FolioRow {
        folio_num: rows::dec(r, "FolioNum")?,
        folio_sub_num: rows::dec(r, "FolioSubNum")?,
        folio_id: rows::int(r, "FolioId")?,
        folio_status: rows::int(r, "FolioStatus")?,
        check_in_time: rows::opt_datetime(r, "CheckInTime")?,
        check_out_time: rows::opt_datetime(r, "CheckOutTime")?,
        last_change_time: rows::datetime(r, "LastChangeTime")?,
        arrival_date: rows::opt_datetime(r, "ArrivalDate")?,
        departure_date: rows::opt_datetime(r, "DepartureDate")?,
        confirm_num: rows::opt_dec(r, "ConfirmNum")?,
        external_confirm_number: rows::opt_str(r, "ExternalConfirmNumber")?,
        cancel_num: rows::opt_dec(r, "CancelNum")?,
        cancel_time: rows::opt_datetime(r, "CancelTime")?,
        no_show: rows::boolean(r, "NoShowFlag")?,
        room_code: rows::opt_str(r, "RoomCode")?,
        user_info1: rows::opt_str(r, "UserInfo1")?,
        last_name: rows::opt_str(r, "LastName")?,
        room_type_booked: rows::opt_str(r, "RoomTypeBooked")?,
        room_type_code: rows::opt_str(r, "RoomTypeCode")?,
        num_adult: rows::opt_int(r, "NumAdult")?,
        num_child: rows::opt_int(r, "NumChild")?,
        num_enf: rows::opt_int(r, "NumEnf")?,
        extra_bed: rows::opt_int(r, "ExtraBed")?,
        origin_code: rows::opt_str(r, "OriginCode")?,
        market_segment_code: rows::opt_str(r, "MarketSegmentCode")?,
        room_rate_code: rows::opt_str(r, "RoomRateCode")?,
    })
}

impl FolioSource for TdsSmile {
    fn earn_catalog(&self, extra_bed_code: &str) -> Result<EarnCatalog, SmileError> {
        let sets = self.sets(
            CATALOG_SQL,
            &[Param::new(
                "ExtraBedCode",
                ParamValue::VarChar(Some(extra_bed_code.to_owned())),
            )],
        )?;
        let transaction_codes = sets
            .first()
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .map(|r| {
                Ok((
                    CodeRow {
                        code: rows::text(r, "Code")?,
                        sub: rows::opt_str(r, "Sub")?,
                    },
                    rows::int(r, "AutoPost")?,
                ))
            })
            .collect::<Result<Vec<_>, SmileError>>()?;
        Ok(EarnCatalog {
            transaction_codes,
            room_types: code_rows(sets.get(1))?,
            packages: code_rows(sets.get(2))?,
            globals: code_rows(sets.get(3))?,
            specials: code_rows(sets.get(4))?,
        })
    }

    fn room_charge_trn_since(&self, watermark: DateTime) -> Result<Vec<i64>, SmileError> {
        let sql = format!("{DECLARE_WATERMARK}{RM_CHG_TRN_SQL}");
        self.rows(&sql, &[Param::new("WatermarkP", watermark)])?
            .iter()
            .map(|r| rows::int(r, "RmChgTrn"))
            .collect()
    }

    fn occupancy_columns(&self) -> Result<OccupancyColumns, SmileError> {
        let mut present = HashSet::new();
        for r in self.rows(OCCUPANCY_SCHEMA_SQL, &[])? {
            let table = rows::text(&r, "TableName")?;
            let column = rows::text(&r, "ColumnName")?;
            present.insert(format!("{table}.{column}").to_lowercase());
        }
        let has = |name: &str| present.contains(&name.to_lowercase());
        Ok(OccupancyColumns {
            num_enf: has("Folio.NumEnf"),
            extra_bed: has("Folio.ExtraBed"),
            has_special: has("Folio_Special.FolioNum")
                && has("Folio_Special.SpecialCode")
                && has("Folio_Special.Qty"),
            special_has_cancel_num: has("Folio_Special.CancelNum"),
        })
    }

    fn changed_folios(
        &self,
        watermark: DateTime,
        max: i64,
        columns: OccupancyColumns,
    ) -> Result<Vec<FolioRow>, SmileError> {
        let max = i32::try_from(max).unwrap_or(i32::MAX);
        self.rows(
            &folio_sql(columns),
            &[
                Param::new("MaxRow", max),
                Param::new("WatermarkP", watermark),
            ],
        )?
        .iter()
        .map(folio_row)
        .collect()
    }

    fn revenues(
        &self,
        folio_nums: &[String],
        pairs: &[EarnPair],
    ) -> Result<Vec<RevenueRow>, SmileError> {
        let mut out = Vec::new();
        for chunk in folio_nums.chunks(IN_LIMIT) {
            let (list, mut params) =
                in_list("FolioNum", chunk, |n| ParamValue::VarChar(Some(n.clone())));
            let (earn, earn_params) = filter(pairs).map_err(SmileError::Invalid)?;
            params.extend(earn_params);
            let sql = format!(
                "\
SELECT t.FolioNum,
       MAX(t.TransactionID)     AS LastTxnId,
       COUNT(*)                 AS TxnCount,
       SUM(t.TransactionAmount) AS TotalAmount
FROM dbo.FolioTransaction t
WHERE t.FolioNum IN ({list})
  AND t.FolioBalanceCode = 'A'
  AND {earn}
GROUP BY t.FolioNum"
            );
            for r in self.rows(&sql, &params)? {
                out.push(RevenueRow {
                    folio_num: rows::text(&r, "FolioNum")?.trim().to_owned(),
                    last_txn_id: rows::dec(&r, "LastTxnId")?,
                    txn_count: rows::int(&r, "TxnCount")?,
                    total_amount: rows::dec(&r, "TotalAmount")?,
                });
            }
        }
        Ok(out)
    }

    fn guests(
        &self,
        folio_nums: &[Decimal],
        phone_column: &str,
    ) -> Result<Vec<GuestRow>, SmileError> {
        let mut out = Vec::new();
        for chunk in folio_nums.chunks(IN_LIMIT) {
            let (list, params) = in_list("FolioNum", chunk, |n| ParamValue::Decimal(Some(*n)));
            for r in self.rows(&guests_sql(phone_column, &list), &params)? {
                out.push(GuestRow {
                    folio_num: rows::dec(&r, "FolioNum")?,
                    phone: rows::opt_str(&r, "Phone")?,
                    name: rows::opt_str(&r, "Name")?,
                });
            }
        }
        Ok(out)
    }

    fn extra_bed_special_qty(
        &self,
        folio_nums: &[Decimal],
        code: &str,
        with_cancel_filter: bool,
    ) -> Result<Vec<(Decimal, i64)>, SmileError> {
        let cancel = if with_cancel_filter {
            "AND ISNULL(s.CancelNum, 0) = 0"
        } else {
            ""
        };
        let mut out = Vec::new();
        for chunk in folio_nums.chunks(IN_LIMIT) {
            let (list, mut params) = in_list("FolioNum", chunk, |n| ParamValue::Decimal(Some(*n)));
            params.push(Param::new("Code", code));
            let sql = format!(
                "\
SELECT s.FolioNum, CAST(SUM(s.Qty) AS int) AS Qty
FROM dbo.Folio_Special s
WHERE s.FolioNum IN ({list})
  AND LTRIM(RTRIM(s.SpecialCode)) = @Code
  AND s.Qty > 0
  {cancel}
GROUP BY s.FolioNum"
            );
            for r in self.rows(&sql, &params)? {
                out.push((rows::dec(&r, "FolioNum")?, rows::int(&r, "Qty")?));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_lists_number_their_parameters() {
        let (list, params) = in_list("F", &["1".to_owned(), "2".to_owned()], |s| {
            ParamValue::VarChar(Some(s.clone()))
        });
        assert_eq!(list, "@F0,@F1");
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn missing_occupancy_columns_read_as_null() {
        let sql = folio_sql(OccupancyColumns::default());
        assert!(sql.contains("CAST(CAST(NULL AS int) AS int) AS NumEnf"));
        let sql = folio_sql(OccupancyColumns {
            num_enf: true,
            extra_bed: true,
            ..OccupancyColumns::default()
        });
        assert!(
            sql.contains("CAST(f.NumEnf AS int) AS NumEnf, CAST(f.ExtraBed AS int) AS ExtraBed")
        );
    }
}
