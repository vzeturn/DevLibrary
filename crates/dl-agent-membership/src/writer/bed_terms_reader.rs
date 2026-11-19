//! `ExtraBedTermsReader` (`Sync/ExtraBedTermsReader.cs`): SELECT-only reads of SMILE's
//! extra-bed terms (spec §9.7). Every table and column is checked in `sys.columns` first,
//! in one round trip: a missing part reads as "unknown", which folds the bed. The SQL text
//! is the .NET text byte for byte (C# raw literals with their indentation removed).

use std::collections::{HashMap, HashSet};

use dl_decimal::Decimal;

use super::dapper_in;
use super::session::{Session, nvarchar, p};
use crate::jobs::bed_terms::{
    ExtraBedTerms, SpecialService, TaxCodes, content_hash, structural_fingerprint,
};
use crate::jobs::plan::WriteError;
use crate::smile::SmileError;

/// `ParaSurcharge`.
pub const PARA_SURCHARGE: &str = "ExtraBedSurChg";
/// `ParaTrnCode`.
pub const PARA_TRN_CODE: &str = "EBedSurChgTrnCode";
/// `ParaTrnSub`.
pub const PARA_TRN_SUB: &str = "EBedSurChgTrnSub";
/// `ParaStock`.
pub const PARA_STOCK: &str = "ExtraBedInv";
/// `ParaSpecialByDate`.
pub const PARA_SPECIAL_BY_DATE: &str = "SpecialByDate";
/// `ParaRateCodePlusPlus`.
pub const PARA_RATE_CODE_PLUS_PLUS: &str = "RateCodePP";

/// `ParaNames`, in .NET order.
pub const PARA_NAMES: [&str; 6] = [
    PARA_SURCHARGE,
    PARA_TRN_CODE,
    PARA_TRN_SUB,
    PARA_STOCK,
    PARA_SPECIAL_BY_DATE,
    PARA_RATE_CODE_PLUS_PLUS,
];

/// `MaxSurcharge`: the hub's `numeric(14,0)` ceiling.
const MAX_SURCHARGE: i64 = 99_999_999_999_999;

/// `SchemaSql`.
pub const SCHEMA_SQL: &str = "SELECT OBJECT_NAME(c.object_id) AS TableName, c.name AS ColumnName
FROM sys.columns c
WHERE c.object_id IN (OBJECT_ID(N'dbo.GlobalParameter'), OBJECT_ID(N'dbo.ExtraBedInv'),
                      OBJECT_ID(N'dbo.TransactionCode'), OBJECT_ID(N'dbo.HotelParameter'),
                      OBJECT_ID(N'dbo.SpecialService'), OBJECT_ID(N'dbo.TaxDef'),
                      OBJECT_ID(N'dbo.ServiceCharge'))";

/// `HotelProbeSql` (before Dapper expands `IN @Names`).
const HOTEL_PROBE_SQL: &str = "SELECT DISTINCT TOP (2) LTRIM(RTRIM(HotelID)) AS HotelID
FROM dbo.GlobalParameter
WHERE ParaName IN {names} AND HotelID IS NOT NULL AND LTRIM(RTRIM(HotelID)) <> ''";

/// `ParametersSql` (before Dapper expands `IN @Names`).
const PARAMETERS_SQL: &str = "SELECT LTRIM(RTRIM(ParaName)) AS ParaName, ParaString
FROM dbo.GlobalParameter
WHERE LTRIM(RTRIM(HotelID)) = @HotelId AND ParaName IN {names}";

/// `StockSql`.
const STOCK_SQL: &str = "SELECT SUM(CAST(NumBed AS bigint)) FROM dbo.ExtraBedInv WHERE LTRIM(RTRIM(HotelID)) = @HotelId";

/// `HotelDateSql`.
const HOTEL_DATE_SQL: &str = "SELECT TOP (1) HotelDate FROM dbo.HotelParameter
WHERE LTRIM(RTRIM(HotelID)) = @HotelId";

/// `SpecialSql`.
pub const SPECIAL_SQL: &str = "SELECT TOP (1) LTRIM(RTRIM(SpecialServiceCode)) AS Code,
       NULLIF(LTRIM(RTRIM(CONVERT(varchar(20), TrnCode))), '') AS TrnCode,
       NULLIF(LTRIM(RTRIM(CONVERT(varchar(20), TrnSubCode))), '') AS TrnSub,
       TRY_CONVERT(int, OpenPrice) AS OpenPrice,
       TRY_CONVERT(int, IncludeInRoomCharge) AS IncludeInRoomCharge,
       TRY_CONVERT(decimal(18, 2), AmountCharge) AS AmountCharge
FROM dbo.SpecialService
WHERE LTRIM(RTRIM(SpecialServiceCode)) = @Code";

/// `TaxColumns`.
pub(crate) const TAX_COLUMNS: &str = "LTRIM(RTRIM(CONVERT(nvarchar(20), TaxCode))) AS TaxCode,
       LTRIM(RTRIM(CONVERT(nvarchar(20), ServiceChargeCode))) AS ServiceChargeCode,";

/// `STaxColumn` / `NoSTaxColumn`.
pub(crate) fn s_tax_column(has_s_tax: bool) -> &'static str {
    if has_s_tax {
        "LTRIM(RTRIM(CONVERT(nvarchar(20), STaxCode))) AS STaxCode"
    } else {
        "CAST(NULL AS nvarchar(20)) AS STaxCode"
    }
}

/// `CodeTaxSql`: one of two constants, chosen by whether the install has `STaxCode`.
pub(crate) fn code_tax_sql(has_s_tax: bool) -> String {
    format!(
        "SELECT TOP (1) {TAX_COLUMNS}
       {}
FROM dbo.TransactionCode
WHERE LTRIM(RTRIM(TransactionCode)) = @Code
  AND (@Sub IS NULL OR LTRIM(RTRIM(CONVERT(varchar(20), TransactionSubCode))) = @Sub)
ORDER BY LTRIM(RTRIM(CONVERT(varchar(20), TransactionSubCode))), TaxCode, ServiceChargeCode",
        s_tax_column(has_s_tax)
    )
}

/// `RoomTaxSql`.
fn room_tax_sql(has_s_tax: bool) -> String {
    format!(
        "SELECT DISTINCT TOP (2) {TAX_COLUMNS}
       {}
FROM dbo.TransactionCode
WHERE AutoPost = 1",
        s_tax_column(has_s_tax)
    )
}

/// `TaxRateSql`.
const TAX_RATE_SQL: &str = "SELECT DISTINCT TOP (2) TRY_CONVERT(decimal(9, 6), TaxRate) AS Rate
FROM dbo.TaxDef
WHERE LTRIM(RTRIM(TaxCode)) = @Code";

/// `ServiceRateSql`.
const SERVICE_RATE_SQL: &str =
    "SELECT DISTINCT TOP (2) TRY_CONVERT(decimal(9, 6), ServiceRate) AS Rate
FROM dbo.ServiceCharge
WHERE LTRIM(RTRIM(ServiceChargeCode)) = @Code";

/// `ExtraBedTermsReadResult`: the terms, or why there are none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermsRead {
    /// `Terms`.
    pub terms: Option<ExtraBedTerms>,
    /// `SkipReason`.
    pub skip_reason: Option<String>,
}

impl TermsRead {
    fn skip(reason: &str) -> TermsRead {
        TermsRead {
            terms: None,
            skip_reason: Some(reason.to_owned()),
        }
    }
}

/// Table → columns found (case-insensitive).
pub(crate) type Schema = HashMap<String, HashSet<String>>;

/// `Has`.
pub(crate) fn has(schema: &Schema, table: &str, columns: &[&str]) -> bool {
    schema
        .get(&table.to_lowercase())
        .is_some_and(|present| columns.iter().all(|c| present.contains(&c.to_lowercase())))
}

/// Reads `sys.columns` of the terms tables.
pub(crate) fn read_schema(s: &mut dyn Session) -> Result<Schema, WriteError> {
    let mut schema: Schema = HashMap::new();
    for r in s.rows(SCHEMA_SQL, &[])? {
        let (Some(t), Some(c)) = (r.opt_str("TableName")?, r.opt_str("ColumnName")?) else {
            continue;
        };
        schema
            .entry(t.to_lowercase())
            .or_default()
            .insert(c.to_lowercase());
    }
    Ok(schema)
}

fn trimmed(v: Option<&str>) -> Option<String> {
    v.map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
}

/// `Flag`: `'1'` is on.
fn flag(v: Option<&str>) -> Option<bool> {
    trimmed(v).map(|f| f == "1")
}

fn names() -> (String, Vec<dl_tds::Param>) {
    dapper_in("Names", PARA_NAMES.iter().map(|n| nvarchar(Some(n))))
}

/// `ReadAsync`. `configured_hotel_id` is `Smile:SmileHotelId` (`None`: the only `HotelID`
/// SMILE has); `special_code` the bed code to read.
pub fn read(
    s: &mut dyn Session,
    configured_hotel_id: Option<&str>,
    special_code: &str,
) -> Result<TermsRead, WriteError> {
    let schema = read_schema(s)?;
    if !has(
        &schema,
        "GlobalParameter",
        &["HotelID", "ParaName", "ParaString"],
    ) {
        return Ok(TermsRead::skip(
            "SMILE không có bảng GlobalParameter(HotelID, ParaName, ParaString).",
        ));
    }
    let hotel_id = match trimmed(configured_hotel_id) {
        Some(h) => h,
        None => {
            let (list, params) = names();
            let probed = s.rows(&HOTEL_PROBE_SQL.replace("{names}", &list), &params)?;
            match probed.as_slice() {
                [] => {
                    return Ok(TermsRead::skip(
                        "GlobalParameter không có dòng giường phụ nào (chưa khai ở SMILE).",
                    ));
                }
                [one] => one.opt_str("HotelID")?.unwrap_or_default(),
                _ => {
                    return Ok(TermsRead::skip(
                        "GlobalParameter có nhiều HotelID — khai Smile:SmileHotelId trong appsettings của Agent.",
                    ));
                }
            }
        }
    };
    let mut warnings = Vec::new();
    let values = read_parameters(s, &hotel_id)?;
    let get = |n: &str| values.get(&n.to_lowercase()).cloned().flatten();
    let surcharge = parse_surcharge(get(PARA_SURCHARGE).as_deref(), &mut warnings);
    let trn_code = trimmed(get(PARA_TRN_CODE).as_deref());
    let trn_sub = trimmed(get(PARA_TRN_SUB).as_deref());
    let by_date = flag(get(PARA_SPECIAL_BY_DATE).as_deref());
    let plus_plus = flag(get(PARA_RATE_CODE_PLUS_PLUS).as_deref());
    let stock = read_stock(
        s,
        &schema,
        &hotel_id,
        get(PARA_STOCK).as_deref(),
        &mut warnings,
    )?;
    let special = read_special(s, &schema, special_code, &mut warnings)?;
    let (special_tax, room_tax) = read_tax_codes(s, &schema, special.as_ref(), &mut warnings)?;
    let hotel_date = if has(&schema, "HotelParameter", &["HotelID", "HotelDate"]) {
        let rows = s.rows(HOTEL_DATE_SQL, &[p("@HotelId", nvarchar(Some(&hotel_id)))])?;
        match rows.first() {
            Some(r) => r.scalar_date()?,
            None => None,
        }
    } else {
        None
    };
    let (mut sc, mut vat) = (None, None);
    if plus_plus == Some(true)
        && let Some(t) = &special_tax
    {
        sc = read_rate_percent(
            s,
            &schema,
            ("ServiceCharge", "ServiceChargeCode", "ServiceRate"),
            SERVICE_RATE_SQL,
            t.service_charge_code.as_deref(),
            &mut warnings,
        )?;
        vat = read_rate_percent(
            s,
            &schema,
            ("TaxDef", "TaxCode", "TaxRate"),
            TAX_RATE_SQL,
            t.tax_code.as_deref(),
            &mut warnings,
        )?;
    }
    let fingerprint = structural_fingerprint(
        &hotel_id,
        special.as_ref(),
        special_tax.as_ref(),
        room_tax.as_ref(),
        by_date,
        plus_plus,
        false,
    );
    let content = content_hash(
        &fingerprint,
        surcharge,
        trn_code.as_deref(),
        trn_sub.as_deref(),
        stock,
    );
    Ok(TermsRead {
        terms: Some(ExtraBedTerms {
            smile_hotel_id: hotel_id,
            surcharge_amount: surcharge,
            trn_code,
            trn_sub,
            stock_total: stock,
            special,
            special_tax,
            room_tax,
            special_by_date: by_date,
            smile_hotel_date: hotel_date,
            structural_fingerprint: fingerprint,
            content_hash: content,
            warnings,
            rate_code_plus_plus: plus_plus,
            special_sc_percent: sc,
            special_vat_percent: vat,
            pms_priced: false,
        }),
        skip_reason: None,
    })
}

/// `ReadParametersAsync`: a duplicated `ParaName` keeps its first non-blank value.
fn read_parameters(
    s: &mut dyn Session,
    hotel_id: &str,
) -> Result<HashMap<String, Option<String>>, WriteError> {
    let (list, mut params) = names();
    params.insert(0, p("@HotelId", nvarchar(Some(hotel_id))));
    let rows = s.rows(&PARAMETERS_SQL.replace("{names}", &list), &params)?;
    let mut values: HashMap<String, Option<String>> = HashMap::new();
    for r in &rows {
        let name = r.opt_str("ParaName")?.unwrap_or_default().to_lowercase();
        let value = r.opt_str("ParaString")?;
        let keep = values
            .get(&name)
            .is_some_and(|e| e.as_deref().is_some_and(|v| !v.trim().is_empty()));
        if !keep {
            values.insert(name, value);
        }
    }
    Ok(values)
}

/// `ParseSurcharge`: digits with at most one decimal point (invariant culture, no sign or
/// thousands separator); 0 is a real value. A fractional, oversized or unparseable amount
/// is `None` with a warning.
pub fn parse_surcharge(raw: Option<&str>, warnings: &mut Vec<String>) -> Option<Decimal> {
    let raw = raw.filter(|r| !r.trim().is_empty())?;
    let t = raw.trim();
    let digits = t.bytes().filter(u8::is_ascii_digit).count();
    let points = t.bytes().filter(|b| *b == b'.').count();
    let parsed = (digits > 0 && points <= 1 && digits + points == t.len())
        .then(|| {
            let text = if t.starts_with('.') {
                format!("0{t}")
            } else {
                t.to_owned()
            };
            text.trim_end_matches('.').parse::<Decimal>().ok()
        })
        .flatten()
        .filter(|a| *a == a.trunc(0) && *a <= Decimal::from(MAX_SURCHARGE));
    if parsed.is_none() {
        warnings.push(format!(
            "GlobalParameter {PARA_SURCHARGE} = '{t}' không phải số tiền nguyên ≥ 0 — gửi NULL."
        ));
    }
    parsed.map(|a| a.trunc(0))
}

fn read_stock(
    s: &mut dyn Session,
    schema: &Schema,
    hotel_id: &str,
    global: Option<&str>,
    warnings: &mut Vec<String>,
) -> Result<Option<i32>, WriteError> {
    if has(schema, "ExtraBedInv", &["HotelID", "NumBed"]) {
        let rows = s.rows(STOCK_SQL, &[p("@HotelId", nvarchar(Some(hotel_id)))])?;
        let sum = match rows.first() {
            Some(r) => r.scalar_int()?,
            None => None,
        };
        if let Some(n) = sum {
            if (0..=i64::from(i16::MAX)).contains(&n) {
                return Ok(i32::try_from(n).ok());
            }
            warnings.push(format!(
                "SUM(ExtraBedInv.NumBed) = {n} ngoài khoảng hợp lệ — gửi tồn kho NULL."
            ));
            return Ok(None);
        }
    }
    let Some(g) = global.map(str::trim).filter(|g| !g.is_empty()) else {
        return Ok(None);
    };
    if g.bytes().all(|b| b.is_ascii_digit())
        && let Ok(n) = g.parse::<i32>()
        && n <= i32::from(i16::MAX)
    {
        return Ok(Some(n));
    }
    warnings.push(format!(
        "GlobalParameter {PARA_STOCK} = '{g}' không phải số giường hợp lệ — gửi NULL."
    ));
    Ok(None)
}

fn read_special(
    s: &mut dyn Session,
    schema: &Schema,
    code: &str,
    warnings: &mut Vec<String>,
) -> Result<Option<SpecialService>, WriteError> {
    let cols = [
        "SpecialServiceCode",
        "TrnCode",
        "TrnSubCode",
        "OpenPrice",
        "IncludeInRoomCharge",
        "AmountCharge",
    ];
    if !has(schema, "SpecialService", &cols) {
        warnings.push("SMILE không có bảng SpecialService(SpecialServiceCode, TrnCode, TrnSubCode, OpenPrice, IncludeInRoomCharge, AmountCharge) — giường phụ gộp vào tiền phòng.".into());
        return Ok(None);
    }
    let code = code.trim();
    let rows = s.rows(SPECIAL_SQL, &[p("@Code", nvarchar(Some(code)))])?;
    let Some(r) = rows.first() else {
        warnings.push(format!(
            "SpecialService không có mã '{code}' — giường phụ gộp vào tiền phòng."
        ));
        return Ok(None);
    };
    Ok(Some(SpecialService {
        code: r.opt_str("Code")?.unwrap_or_default(),
        trn_code: r.opt_str("TrnCode")?,
        trn_sub: r.opt_str("TrnSub")?,
        open_price: opt_i32(r.opt_int("OpenPrice")?, "OpenPrice")?,
        include_in_room_charge: opt_i32(r.opt_int("IncludeInRoomCharge")?, "IncludeInRoomCharge")?,
        amount_charge: r.opt_dec("AmountCharge")?,
    }))
}

fn opt_i32(n: Option<i64>, col: &'static str) -> Result<Option<i32>, WriteError> {
    n.map(|n| i32::try_from(n).map_err(|_| WriteError::Smile(SmileError::Decode(col))))
        .transpose()
}

pub(crate) fn tax_codes(r: &super::session::Rec) -> Result<TaxCodes, WriteError> {
    Ok(TaxCodes {
        tax_code: r.opt_str("TaxCode")?,
        service_charge_code: r.opt_str("ServiceChargeCode")?,
        s_tax_code: r.opt_str("STaxCode")?,
    })
}

type TaxPair = (Option<TaxCodes>, Option<TaxCodes>);

fn read_tax_codes(
    s: &mut dyn Session,
    schema: &Schema,
    special: Option<&SpecialService>,
    warnings: &mut Vec<String>,
) -> Result<TaxPair, WriteError> {
    if !has(
        schema,
        "TransactionCode",
        &[
            "TransactionCode",
            "TransactionSubCode",
            "TaxCode",
            "ServiceChargeCode",
        ],
    ) {
        warnings.push(
            "SMILE không có TransactionCode(TaxCode, ServiceChargeCode) — không so được thuế với mã phòng."
                .into(),
        );
        return Ok((None, None));
    }
    let has_s_tax = has(schema, "TransactionCode", &["STaxCode"]);
    let mut special_tax = None;
    if let Some(sp) = special
        && let Some(code) = &sp.trn_code
    {
        let rows = s.rows(
            &code_tax_sql(has_s_tax),
            &[
                p("@Code", nvarchar(Some(code))),
                p("@Sub", nvarchar(sp.trn_sub.as_deref())),
            ],
        )?;
        match rows.first() {
            Some(r) => special_tax = Some(tax_codes(r)?),
            None => warnings.push(format!(
                "TransactionCode không có dòng cho mã '{code}'/'{}' của {}.",
                sp.trn_sub.as_deref().unwrap_or(""),
                sp.code
            )),
        }
    }
    if !has(schema, "TransactionCode", &["AutoPost"]) {
        warnings.push(
            "TransactionCode không có cột AutoPost — không tìm được mã tiền phòng để so thuế."
                .into(),
        );
        return Ok((special_tax, None));
    }
    let rooms = s.rows(&room_tax_sql(has_s_tax), &[])?;
    if let [one] = rooms.as_slice() {
        return Ok((special_tax, Some(tax_codes(one)?)));
    }
    warnings.push(if rooms.is_empty() {
        "TransactionCode không có mã tiền phòng (AutoPost = 1).".into()
    } else {
        "Các mã tiền phòng (AutoPost = 1) có thuế/phí khác nhau — không so được với mã giường phụ."
            .into()
    });
    Ok((special_tax, None))
}

/// `ReadRatePercentAsync`: a blank code is a real 0 %; a missing table, no row, two rates
/// or a value outside 0..1 is unknown with a warning.
fn read_rate_percent(
    s: &mut dyn Session,
    schema: &Schema,
    (table, code_column, rate_column): (&str, &str, &str),
    sql: &str,
    code: Option<&str>,
    warnings: &mut Vec<String>,
) -> Result<Option<Decimal>, WriteError> {
    let Some(code) = code.map(str::trim).filter(|c| !c.is_empty()) else {
        return Ok(Some(Decimal::ZERO));
    };
    if !has(schema, table, &[code_column, rate_column]) {
        warnings.push(format!(
            "SMILE không có bảng {table}({code_column}, {rate_column}) — không tính được giá giường phụ \"++\"."
        ));
        return Ok(None);
    }
    let rows = s.rows(sql, &[p("@Code", nvarchar(Some(code)))])?;
    if let [one] = rows.as_slice()
        && let Some(rate) = one.opt_dec("Rate")?
        && rate >= Decimal::ZERO
        && rate < Decimal::ONE
    {
        return Ok(Some(rate * Decimal::ONE_HUNDRED));
    }
    warnings.push(format!(
        "{table} không có đúng một tỷ lệ hợp lệ cho mã '{code}' — không tính được giá giường phụ \"++\"."
    ));
    Ok(None)
}

#[cfg(test)]
#[path = "bed_terms_reader_tests.rs"]
pub(crate) mod tests;
