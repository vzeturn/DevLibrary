//! The earn reads over `Folio` / `FolioTransaction` (`FolioWriter.Revenue`,
//! `FolioWriter.StayClassification`, `FolioWriter.CheckoutTime`, `FolioRevenueReads`,
//! `EarnPairReader.ReadRoomChargeTrnAsync`), on a [`Session`] so the recorder pins every
//! statement. The texts are the .NET ones; a Dapper `IN @List` is expanded by hand into
//! numbered parameters (500 at a time, like the .NET chunking).

use dl_decimal::Decimal;
use dl_tds::{Client, Param};
use dl_time::OffsetDateTime;

use super::wire::RevenueLine;
use crate::folio::earn::{EarnPair, filter};
use crate::log::{self, V};
use crate::smile::folio_sql::{CATALOG_SQL, IN_LIMIT, catalog_from_sets, catalog_params, in_list};
use crate::smile::{EarnCatalog, SmileError, to_utc};
use crate::wire::StayClassification;
use crate::writer::session::{Rec, Session, decimal, nvarchar};

/// A [`Session`] that can also read the earn catalogue (five result sets in one batch).
pub trait EarnDb: Session {
    /// `EarnPairReader.CatalogSql` with `@ExtraBedCode`.
    fn earn_catalog(&mut self, extra_bed_code: &str) -> Result<EarnCatalog, SmileError>;
}

impl EarnDb for Client {
    fn earn_catalog(&mut self, extra_bed_code: &str) -> Result<EarnCatalog, SmileError> {
        let sets: Vec<Vec<dl_tds::Row>> = self
            .query(CATALOG_SQL, &catalog_params(extra_bed_code))
            .map_err(|e| SmileError::from_tds(&e))?
            .into_sets()
            .into_iter()
            .map(dl_tds::ResultSet::into_rows)
            .collect();
        catalog_from_sets(&sets)
    }
}

/// The recorder answers the catalogue with its first set only (`TransactionCode` rows:
/// `Code`, `Sub`, `AutoPost`); the other four sets are empty.
#[cfg(test)]
impl EarnDb for crate::writer::recorder::Recorder {
    fn earn_catalog(&mut self, extra_bed_code: &str) -> Result<EarnCatalog, SmileError> {
        let rows = Session::rows(self, CATALOG_SQL, &catalog_params(extra_bed_code))?;
        let transaction_codes = rows
            .iter()
            .map(|r| {
                Ok((
                    crate::smile::CodeRow {
                        code: r.opt_str("Code")?.unwrap_or_default(),
                        sub: r.opt_str("Sub")?,
                    },
                    r.int("AutoPost")?,
                ))
            })
            .collect::<Result<Vec<_>, SmileError>>()?;
        Ok(EarnCatalog {
            transaction_codes,
            ..EarnCatalog::default()
        })
    }
}

/// `EarnFolioScope`: the folios whose `RmChgTrn` feeds the earn pairs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolioScope {
    /// `Bookings(ExternalConfirmNumbers)`.
    Bookings(Vec<String>),
    /// `WalkIns(ConfirmNums)`.
    WalkIns(Vec<Decimal>),
}

/// One stay: a Membership booking (`ExternalConfirmNumber`) or a walk-in group
/// (`ConfirmNum`, never a Membership folio).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stay {
    /// `BookingScope`.
    Booking(String),
    /// `WalkInScope`.
    WalkIn(Decimal),
}

impl Stay {
    fn revenue_scope(&self) -> &'static str {
        match self {
            Stay::Booking(_) => "f.ExternalConfirmNumber = @Ext",
            Stay::WalkIn(_) => "f.ConfirmNum = @Conf AND f.ExternalConfirmNumber IS NULL",
        }
    }

    /// The group key under `name` (`Ext`/`Code`/`Key` nvarchar, `Conf`/`Key` decimal).
    fn key(&self, booking_name: &str, walk_in_name: &str) -> Param {
        match self {
            Stay::Booking(code) => Param::new(booking_name, nvarchar(Some(code))),
            Stay::WalkIn(n) => Param::new(walk_in_name, decimal(*n)),
        }
    }
}

const FOLIO_TRANSACTION_JOIN: &str =
    "JOIN FolioTransaction t ON t.FolioNum = CONVERT(varchar(20), f.FolioNum)";

const CHECKED_OUT_CHARGE_FILTER: &str = "  AND ISNULL(f.CancelNum, 0) = 0
  AND f.FolioStatus = 3
  AND t.FolioBalanceCode = 'A'";

/// `RevenueSumSql`.
pub fn revenue_sum_sql(scope: &str, pair_predicate: &str) -> String {
    format!(
        "SELECT ISNULL(SUM(t.TransactionAmount), 0)
FROM Folio f
{FOLIO_TRANSACTION_JOIN}
WHERE {scope}
{CHECKED_OUT_CHARGE_FILTER}
  AND {pair_predicate}"
    )
}

/// `RevenueLinesSql`.
pub fn revenue_lines_sql(scope: &str, qualifies: Option<&str>) -> String {
    let flag = qualifies.map_or(String::new(), |q| {
        format!(",\n       CASE WHEN {q} THEN 1 ELSE 0 END AS Qualifies")
    });
    format!(
        "SELECT t.TransactionCode,
       (SELECT K FROM dbo.SA_TrnKey(t.TransactionSubCode)) AS TransactionSubCode,
       tc.Description, t.TransactionAmount AS Amount{flag}
FROM Folio f
{FOLIO_TRANSACTION_JOIN}
OUTER APPLY dbo.SA_TrnCode_Resolve(t.TransactionCode, t.TransactionSubCode, 0, 0) tc
WHERE {scope}
{CHECKED_OUT_CHARGE_FILTER}"
    )
}

const BOOKING_CLASSIFICATION_SQL: &str = "\
SELECT 'O' AS K, LTRIM(RTRIM(f.OriginCode)) AS V FROM Folio f
  WHERE f.ExternalConfirmNumber = @Key AND ISNULL(f.CancelNum, 0) = 0
UNION
SELECT 'S', LTRIM(RTRIM(f.MarketSegmentCode)) FROM Folio f
  WHERE f.ExternalConfirmNumber = @Key AND ISNULL(f.CancelNum, 0) = 0
UNION
SELECT 'R', LTRIM(RTRIM(f.RoomRateCode)) FROM Folio f
  WHERE f.ExternalConfirmNumber = @Key AND ISNULL(f.CancelNum, 0) = 0";

const WALK_IN_CLASSIFICATION_SQL: &str = "\
SELECT 'O' AS K, LTRIM(RTRIM(f.OriginCode)) AS V FROM Folio f
  WHERE f.ConfirmNum = @Key AND f.ExternalConfirmNumber IS NULL AND ISNULL(f.CancelNum, 0) = 0
UNION
SELECT 'S', LTRIM(RTRIM(f.MarketSegmentCode)) FROM Folio f
  WHERE f.ConfirmNum = @Key AND f.ExternalConfirmNumber IS NULL AND ISNULL(f.CancelNum, 0) = 0
UNION
SELECT 'R', LTRIM(RTRIM(f.RoomRateCode)) FROM Folio f
  WHERE f.ConfirmNum = @Key AND f.ExternalConfirmNumber IS NULL AND ISNULL(f.CancelNum, 0) = 0";

const CHECKED_OUT_AT_SQL: &str = "\
SELECT MAX(CheckOutTime)
FROM Folio
WHERE ISNULL(CancelNum, 0) = 0
  AND ExternalConfirmNumber = @Code";

fn rm_chg_trn_sql(predicate: &str) -> String {
    format!(
        "SELECT DISTINCT f.RmChgTrn
FROM dbo.Folio f
WHERE {predicate}
  AND ISNULL(f.RmChgTrn, 0) <> 0"
    )
}

/// `EarnPairReader.ReadRoomChargeTrnAsync` for a booking or walk-in scope: the distinct
/// non-zero `Folio.RmChgTrn`, keys deduplicated and read 500 at a time.
pub fn room_charge_trn(db: &mut dyn Session, scope: &FolioScope) -> Result<Vec<i64>, SmileError> {
    let mut out: Vec<i64> = Vec::new();
    let mut add = |rows: Vec<Rec>| -> Result<(), SmileError> {
        for r in rows {
            let v = r.int("RmChgTrn")?;
            if !out.contains(&v) {
                out.push(v);
            }
        }
        Ok(())
    };
    match scope {
        FolioScope::Bookings(codes) => {
            let mut keys: Vec<String> = Vec::new();
            for c in codes {
                if !keys.contains(c) {
                    keys.push(c.clone());
                }
            }
            for chunk in keys.chunks(IN_LIMIT) {
                let (list, params) =
                    in_list("Keys", chunk, |c: &String| nvarchar(Some(c.as_str())));
                add(db.rows(
                    &rm_chg_trn_sql(&format!("f.ExternalConfirmNumber IN ({list})")),
                    &params,
                )?)?;
            }
        }
        FolioScope::WalkIns(nums) => {
            let mut keys: Vec<Decimal> = Vec::new();
            for n in nums {
                if !keys.contains(n) {
                    keys.push(*n);
                }
            }
            for chunk in keys.chunks(IN_LIMIT) {
                let (list, params) = in_list("Keys", chunk, |n| decimal(*n));
                add(db.rows(
                    &rm_chg_trn_sql(&format!(
                        "f.ConfirmNum IN ({list}) AND f.ExternalConfirmNumber IS NULL"
                    )),
                    &params,
                )?)?;
            }
        }
    }
    Ok(out)
}

fn with_pairs(stay: &Stay, pairs: &[EarnPair]) -> Result<(Vec<Param>, String), SmileError> {
    let (predicate, mut params) = filter(pairs).map_err(SmileError::Invalid)?;
    params.insert(0, stay.key("Ext", "Conf"));
    Ok((params, predicate))
}

/// `SumQualifyingRevenueAsync` / `SumWalkInRevenueAsync`: 0 without a pair (no SQL).
pub fn revenue_sum(
    db: &mut dyn Session,
    stay: &Stay,
    pairs: &[EarnPair],
) -> Result<Decimal, SmileError> {
    if pairs.is_empty() {
        return Ok(Decimal::ZERO);
    }
    let (params, predicate) = with_pairs(stay, pairs)?;
    let rows = db.rows(&revenue_sum_sql(stay.revenue_scope(), &predicate), &params)?;
    Ok(rows
        .first()
        .and_then(Rec::first)
        .map(|r| r.opt_dec("_"))
        .transpose()?
        .flatten()
        .unwrap_or(Decimal::ZERO))
}

fn line(r: &Rec) -> Result<RevenueLine, SmileError> {
    Ok(RevenueLine {
        transaction_code: r.opt_str("TransactionCode")?.unwrap_or_default(),
        transaction_sub_code: r.opt_str("TransactionSubCode")?.unwrap_or_default(),
        description: r.opt_str("Description")?,
        amount: r.dec("Amount")?,
    })
}

/// `ScanFolioRevenueAsync` / `ScanWalkInFolioRevenueAsync`: every line plus the total of
/// the lines the earn-pair predicate flags (no pair: no flag column, total 0).
pub fn revenue_scan(
    db: &mut dyn Session,
    stay: &Stay,
    pairs: &[EarnPair],
) -> Result<(Decimal, Vec<RevenueLine>), SmileError> {
    let (params, qualifies) = if pairs.is_empty() {
        (vec![stay.key("Ext", "Conf")], None)
    } else {
        let (p, q) = with_pairs(stay, pairs)?;
        (p, Some(q))
    };
    let rows = db.rows(
        &revenue_lines_sql(stay.revenue_scope(), qualifies.as_deref()),
        &params,
    )?;
    let mut total = Decimal::ZERO;
    let mut lines = Vec::with_capacity(rows.len());
    for r in &rows {
        let l = line(r)?;
        if qualifies.is_some() && r.opt_int("Qualifies")? == Some(1) {
            total += l.amount;
        }
        lines.push(l);
    }
    Ok((total, lines))
}

/// `ScanFolioTransactionsAsync` / `ScanWalkInFolioTransactionsAsync`: every line, no flag.
pub fn folio_transactions(
    db: &mut dyn Session,
    stay: &Stay,
) -> Result<Vec<RevenueLine>, SmileError> {
    db.rows(
        &revenue_lines_sql(stay.revenue_scope(), None),
        &[stay.key("Ext", "Conf")],
    )?
    .iter()
    .map(line)
    .collect()
}

/// `FolioRevenueReads.ReadBookingRevenueAsync` / `ReadWalkInRevenueAsync`: total and
/// breakdown in one read; when that read fails, the total alone with no breakdown (a
/// failure of the fallback is the caller's failure).
pub fn read_revenue(
    db: &mut dyn Session,
    stay: &Stay,
    pairs: &[EarnPair],
    label: &str,
) -> Result<(Decimal, Option<Vec<RevenueLine>>), SmileError> {
    match revenue_scan(db, stay, pairs) {
        Ok((total, lines)) => Ok((total, Some(lines))),
        Err(e) => {
            log::warn(
                "folio breakdown read failed; the hub uses the whitelisted revenue",
                &[("stay", V::S(label)), ("error", V::S(&e.to_string()))],
            );
            Ok((revenue_sum(db, stay, pairs)?, None))
        }
    }
}

/// `ToRevenueLines`: an empty or unavailable breakdown is sent as `null`.
pub fn wire_lines(lines: Option<Vec<RevenueLine>>) -> Option<Vec<RevenueLine>> {
    lines.filter(|l| !l.is_empty())
}

/// `ReadStayClassificationAsync` / `ReadWalkInStayClassificationAsync`: every non-blank,
/// non-`'0'` code of the stay's live folios.
pub fn classification(db: &mut dyn Session, stay: &Stay) -> Result<StayClassification, SmileError> {
    let sql = match stay {
        Stay::Booking(_) => BOOKING_CLASSIFICATION_SQL,
        Stay::WalkIn(_) => WALK_IN_CLASSIFICATION_SQL,
    };
    let mut c = StayClassification::default();
    for r in db.rows(sql, &[stay.key("Key", "Key")])? {
        let Some(v) = r.opt_str("V")?.filter(|v| !v.is_empty() && v != "0") else {
            continue;
        };
        match r.opt_str("K")?.as_deref() {
            Some("O") => c.origin_codes.push(v),
            Some("S") => c.segment_codes.push(v),
            Some("R") => c.rate_codes.push(v),
            _ => return Err(SmileError::Decode("K")),
        }
    }
    Ok(c)
}

/// `ReadCheckedOutAtAsync`: `MAX(CheckOutTime)` of the booking's live folios, in UTC.
pub fn checked_out_at(
    db: &mut dyn Session,
    confirmation_code: &str,
) -> Result<Option<OffsetDateTime>, SmileError> {
    let rows = db.rows(
        CHECKED_OUT_AT_SQL,
        &[Param::new("Code", nvarchar(Some(confirmation_code)))],
    )?;
    Ok(rows
        .first()
        .and_then(Rec::first)
        .map(|r| r.opt_datetime("_"))
        .transpose()?
        .flatten()
        .map(to_utc))
}

/// `EarnCheckoutTime.Resolve`: the PMS checkout time when it wrote one, else the agent
/// clock, with the matching `CheckedOutAtSource`.
pub fn resolve_checkout_time(pms: Option<OffsetDateTime>) -> (OffsetDateTime, &'static str) {
    match pms {
        Some(at) => (at, super::wire::SOURCE_PMS_CHECKOUT_TIME),
        None => (OffsetDateTime::now_utc(), super::wire::SOURCE_AGENT_CLOCK),
    }
}
