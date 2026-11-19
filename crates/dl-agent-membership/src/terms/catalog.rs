//! `SpecialServiceCatalogReader` (`Sync/SpecialServiceCatalogReader.cs`, ADR-0106): SMILE's
//! whole `SpecialService` catalogue, SELECT only, with the per-bed-night price the hub
//! quotes for a PMS-priced extra bed (`SpecialServiceBedPrice`) and the content hash
//! (`SpecialServiceCatalogHashing`). The agent never writes the catalogue.

use std::collections::HashMap;

use dl_decimal::{Decimal, RoundingMode};

use crate::jobs::bed_terms::{amount_text, field, flag, sha256_hex};
use crate::jobs::plan::WriteError;
use crate::stj::J;
use crate::writer::dapper_in;
use crate::writer::session::{Session, nvarchar};

/// `MaxSpecials`.
pub const MAX_SPECIALS: usize = 2000;

const REQUIRED_COLUMNS: [&str; 6] = [
    "SpecialServiceCode",
    "TrnCode",
    "TrnSubCode",
    "AmountCharge",
    "OpenPrice",
    "IncludeInRoomCharge",
];

/// `SchemaSql`.
const SCHEMA_SQL: &str = "SELECT OBJECT_NAME(c.object_id) AS TableName, c.name AS ColumnName, TYPE_NAME(c.system_type_id) AS TypeName
FROM sys.columns c
WHERE c.object_id IN (OBJECT_ID(N'dbo.SpecialService'), OBJECT_ID(N'dbo.TransactionCode'),
                      OBJECT_ID(N'dbo.TaxDef'), OBJECT_ID(N'dbo.ServiceCharge'))";

/// `SpecialsSql`: the description column is one of two constants.
fn specials_sql(has_description: bool) -> String {
    let name = if has_description {
        "NULLIF(LTRIM(RTRIM(Description)), '')"
    } else {
        "CAST(NULL AS nvarchar(50))"
    };
    format!(
        "SELECT TOP ({MAX_SPECIALS}) LTRIM(RTRIM(SpecialServiceCode)) AS Code,
       {name} AS Name,
       NULLIF(LTRIM(RTRIM(CONVERT(varchar(20), TrnCode))), '') AS TrnCode,
       NULLIF(LTRIM(RTRIM(CONVERT(varchar(20), TrnSubCode))), '') AS TrnSub,
       TRY_CONVERT(decimal(18, 2), AmountCharge) AS AmountCharge,
       TRY_CONVERT(int, OpenPrice) AS OpenPrice,
       TRY_CONVERT(int, IncludeInRoomCharge) AS IncludeInRoomCharge
FROM dbo.SpecialService
WHERE SpecialServiceCode IS NOT NULL AND LTRIM(RTRIM(SpecialServiceCode)) <> ''
ORDER BY LTRIM(RTRIM(SpecialServiceCode))"
    )
}

/// `CodeTaxSql` (before Dapper expands `IN @Codes`).
fn code_tax_sql(has_s_tax: bool, codes: &str) -> String {
    let s_tax = if has_s_tax {
        "LTRIM(RTRIM(CONVERT(nvarchar(20), STaxCode)))"
    } else {
        "CAST(NULL AS nvarchar(20))"
    };
    format!(
        "SELECT LTRIM(RTRIM(TransactionCode)) AS Code,
       LTRIM(RTRIM(CONVERT(varchar(20), TransactionSubCode))) AS Sub,
       LTRIM(RTRIM(CONVERT(nvarchar(20), TaxCode))) AS TaxCode,
       LTRIM(RTRIM(CONVERT(nvarchar(20), ServiceChargeCode))) AS ServiceChargeCode,
       {s_tax} AS STaxCode
FROM dbo.TransactionCode
WHERE LTRIM(RTRIM(TransactionCode)) IN {codes}"
    )
}

/// `TaxRatesSql`.
const TAX_RATES_SQL: &str =
    "SELECT DISTINCT LTRIM(RTRIM(TaxCode)) AS Code, TRY_CONVERT(decimal(9, 6), TaxRate) AS Rate
FROM dbo.TaxDef WHERE LTRIM(RTRIM(TaxCode)) IN {codes}";

/// `ServiceRatesSql`.
const SERVICE_RATES_SQL: &str = "SELECT DISTINCT LTRIM(RTRIM(ServiceChargeCode)) AS Code, TRY_CONVERT(decimal(9, 6), ServiceRate) AS Rate
FROM dbo.ServiceCharge WHERE LTRIM(RTRIM(ServiceChargeCode)) IN {codes}";

/// `AgentSpecialServiceItem`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecialItem {
    /// `Code`.
    pub code: String,
    /// `Name` (Unicode).
    pub name: Option<String>,
    /// `TrnCode`.
    pub trn_code: Option<String>,
    /// `TrnSub`.
    pub trn_sub: Option<String>,
    /// `AmountCharge`.
    pub amount_charge: Option<Decimal>,
    /// `OpenPrice`.
    pub open_price: Option<bool>,
    /// `IncludeInRoomCharge`.
    pub include_in_room_charge: Option<bool>,
    /// `TaxLabel`.
    pub tax_label: Option<String>,
    /// `BedPrice`.
    pub bed_price: Option<Decimal>,
}

impl SpecialItem {
    /// The wire object (property names as `System.Text.Json` writes them).
    pub fn to_j(&self) -> J {
        J::Obj(vec![
            ("Code", J::str(&self.code)),
            ("Name", J::opt_str(self.name.as_deref())),
            ("TrnCode", J::opt_str(self.trn_code.as_deref())),
            ("TrnSub", J::opt_str(self.trn_sub.as_deref())),
            ("AmountCharge", J::opt_dec(self.amount_charge)),
            ("OpenPrice", J::opt_bool(self.open_price)),
            (
                "IncludeInRoomCharge",
                J::opt_bool(self.include_in_room_charge),
            ),
            ("TaxLabel", J::opt_str(self.tax_label.as_deref())),
            ("BedPrice", J::opt_dec(self.bed_price)),
        ])
    }
}

/// `SpecialServiceCatalog`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    /// `Items`.
    pub items: Vec<SpecialItem>,
    /// `RateCodePlusPlus`.
    pub rate_code_plus_plus: Option<bool>,
    /// `ContentHash`.
    pub content_hash: String,
}

/// `SpecialServiceBedPrice.From`: the pre-tax per-bed-night price of a PMS-priced bed.
pub fn bed_price(
    amount_charge: Option<Decimal>,
    plus_plus: Option<bool>,
    sc_percent: Option<Decimal>,
    vat_percent: Option<Decimal>,
    tax_unknown: bool,
) -> Option<Decimal> {
    let amount = amount_charge.filter(|a| *a > Decimal::ZERO)?;
    let positive = |v: Decimal| (v > Decimal::ZERO).then_some(v);
    match plus_plus? {
        false => positive(amount.round_dp(0, RoundingMode::HalfUp)),
        true => {
            if tax_unknown {
                return None;
            }
            let sc = Decimal::ONE.checked_add(sc_percent?.checked_div(Decimal::ONE_HUNDRED)?)?;
            let vat = Decimal::ONE.checked_add(vat_percent?.checked_div(Decimal::ONE_HUNDRED)?)?;
            let net = amount.checked_div(sc.checked_mul(vat)?)?;
            positive(net.round_dp(0, RoundingMode::HalfUp))
        }
    }
}

/// `SpecialServiceCatalogHashing.ContentHash` (`special-services:v1`).
pub fn content_hash(items: &[SpecialItem], rate_code_plus_plus: Option<bool>) -> String {
    let mut text = String::from("special-services:v1\n");
    field(&mut text, "rateCodePP", flag(rate_code_plus_plus));
    let mut sorted: Vec<&SpecialItem> = items.iter().collect();
    sorted.sort_by(|a, b| a.code.cmp(&b.code));
    for i in sorted {
        field(&mut text, "code", Some(&i.code));
        field(&mut text, "name", i.name.as_deref());
        field(&mut text, "trnCode", i.trn_code.as_deref());
        field(&mut text, "trnSub", i.trn_sub.as_deref());
        field(&mut text, "amount", amount_text(i.amount_charge).as_deref());
        field(&mut text, "openPrice", flag(i.open_price));
        field(&mut text, "inRoom", flag(i.include_in_room_charge));
        field(&mut text, "tax", i.tax_label.as_deref());
        field(&mut text, "bedPrice", amount_text(i.bed_price).as_deref());
    }
    sha256_hex(&text)
}

struct TaxRow {
    code: String,
    sub: Option<String>,
    tax_code: Option<String>,
    service_charge_code: Option<String>,
    s_tax_code: Option<String>,
}

/// `TaxOf`: the code's row for the sub (any sub when the special has none), lowest first.
fn tax_of<'a>(
    trn_code: Option<&str>,
    trn_sub: Option<&str>,
    rows: &'a [TaxRow],
) -> Option<&'a TaxRow> {
    let code = trn_code?;
    let mut hits: Vec<&TaxRow> = rows
        .iter()
        .filter(|r| {
            r.code.eq_ignore_ascii_case(code)
                && trn_sub
                    .is_none_or(|s| r.sub.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(s)))
        })
        .collect();
    hits.sort_by(|a, b| {
        (&a.sub, &a.tax_code, &a.service_charge_code).cmp(&(
            &b.sub,
            &b.tax_code,
            &b.service_charge_code,
        ))
    });
    hits.first().copied()
}

fn distinct_ignore_case(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in values {
        if !out.iter().any(|o| o.eq_ignore_ascii_case(&v)) {
            out.push(v);
        }
    }
    out
}

/// `RatesAsync`: code (upper-cased key) → percent; a code without exactly one rate in
/// 0..1 is unknown.
fn rates(
    s: &mut dyn Session,
    sql: &str,
    codes: impl IntoIterator<Item = Option<String>>,
) -> Result<HashMap<String, Option<Decimal>>, WriteError> {
    let wanted = distinct_ignore_case(
        codes
            .into_iter()
            .flatten()
            .map(|c| c.trim().to_owned())
            .filter(|c| !c.is_empty()),
    );
    let mut out: HashMap<String, Option<Decimal>> = HashMap::new();
    if wanted.is_empty() {
        return Ok(out);
    }
    let (list, params) = dapper_in("Codes", wanted.iter().map(|c| nvarchar(Some(c))));
    let mut grouped: HashMap<String, Vec<Option<Decimal>>> = HashMap::new();
    for r in s.rows(&sql.replace("{codes}", &list), &params)? {
        let code = r.opt_str("Code")?.unwrap_or_default().to_uppercase();
        grouped.entry(code).or_default().push(r.opt_dec("Rate")?);
    }
    for (code, list) in grouped {
        let rate = match list.as_slice() {
            [Some(r)] if *r >= Decimal::ZERO && *r < Decimal::ONE => {
                r.checked_mul(Decimal::ONE_HUNDRED)
            }
            _ => None,
        };
        out.insert(code, rate);
    }
    Ok(out)
}

/// `Rate`: a blank code is a real 0 %.
fn rate(rates: &HashMap<String, Option<Decimal>>, code: Option<&str>) -> Option<Decimal> {
    match code.map(str::trim).filter(|c| !c.is_empty()) {
        None => Some(Decimal::ZERO),
        Some(c) => rates.get(&c.to_uppercase()).copied().flatten(),
    }
}

/// `ReadAsync`: `None` when the install has no readable `SpecialService` table.
pub fn read(
    s: &mut dyn Session,
    rate_code_plus_plus: Option<bool>,
) -> Result<Option<Catalog>, WriteError> {
    let mut columns: Vec<(String, String, Option<String>)> = Vec::new();
    for r in s.rows(SCHEMA_SQL, &[])? {
        columns.push((
            r.opt_str("TableName")?.unwrap_or_default(),
            r.opt_str("ColumnName")?.unwrap_or_default(),
            r.opt_str("TypeName")?,
        ));
    }
    let find = |t: &str, c: &str| {
        columns
            .iter()
            .find(|(tn, cn, _)| tn.eq_ignore_ascii_case(t) && cn.eq_ignore_ascii_case(c))
    };
    let has = |t: &str, c: &str| find(t, c).is_some();
    if !REQUIRED_COLUMNS.iter().all(|c| has("SpecialService", c)) {
        return Ok(None);
    }
    let description = find("SpecialService", "Description");
    // A varchar description holds TCVN3 bytes; nvarchar is already Unicode.
    let tcvn = description.is_some_and(|d| matches!(d.2.as_deref(), Some("varchar" | "char")));
    let mut specials = Vec::new();
    for r in s.rows(&specials_sql(description.is_some()), &[])? {
        specials.push(SpecialItem {
            code: r.opt_str("Code")?.unwrap_or_default(),
            name: r.opt_str("Name")?,
            trn_code: r.opt_str("TrnCode")?,
            trn_sub: r.opt_str("TrnSub")?,
            amount_charge: r.opt_dec("AmountCharge")?,
            open_price: r.opt_int("OpenPrice")?.map(|n| n == 1),
            include_in_room_charge: r.opt_int("IncludeInRoomCharge")?.map(|n| n == 1),
            tax_label: None,
            bed_price: None,
        });
    }
    let main_codes = distinct_ignore_case(specials.iter().filter_map(|i| i.trn_code.clone()));
    let mut tax_rows = Vec::new();
    if !main_codes.is_empty()
        && [
            "TransactionCode",
            "TransactionSubCode",
            "TaxCode",
            "ServiceChargeCode",
        ]
        .iter()
        .all(|c| has("TransactionCode", c))
    {
        let (list, params) = dapper_in("Codes", main_codes.iter().map(|c| nvarchar(Some(c))));
        let sql = code_tax_sql(has("TransactionCode", "STaxCode"), &list);
        for r in s.rows(&sql, &params)? {
            tax_rows.push(TaxRow {
                code: r.opt_str("Code")?.unwrap_or_default(),
                sub: r.opt_str("Sub")?,
                tax_code: r.opt_str("TaxCode")?,
                service_charge_code: r.opt_str("ServiceChargeCode")?,
                s_tax_code: r.opt_str("STaxCode")?,
            });
        }
    }
    let taxes: Vec<Option<&TaxRow>> = specials
        .iter()
        .map(|i| tax_of(i.trn_code.as_deref(), i.trn_sub.as_deref(), &tax_rows))
        .collect();
    let (mut vat, mut sc) = (HashMap::new(), HashMap::new());
    if rate_code_plus_plus == Some(true) {
        let found = || taxes.iter().flatten();
        if has("TaxDef", "TaxCode") && has("TaxDef", "TaxRate") {
            vat = rates(s, TAX_RATES_SQL, found().map(|t| t.tax_code.clone()))?;
        }
        if has("ServiceCharge", "ServiceChargeCode") && has("ServiceCharge", "ServiceRate") {
            sc = rates(
                s,
                SERVICE_RATES_SQL,
                found().map(|t| t.service_charge_code.clone()),
            )?;
        }
    }
    let mut items: Vec<SpecialItem> = Vec::new();
    for (mut item, tax) in specials.into_iter().zip(taxes) {
        if items.iter().any(|i| i.code == item.code) {
            continue;
        }
        if tcvn {
            item.name = item
                .name
                .map(|n| dl_vn::tcvn3::decode(&n, dl_vn::tcvn3::CodePage::Cp1252));
        }
        item.tax_label = tax.map(|t| {
            format!(
                "{}/{}",
                t.tax_code.as_deref().unwrap_or(""),
                t.service_charge_code.as_deref().unwrap_or("")
            )
        });
        let tax_unknown = tax.is_none_or(|t| {
            t.s_tax_code
                .as_deref()
                .is_some_and(|c| !c.trim().is_empty())
        });
        item.bed_price = bed_price(
            item.amount_charge,
            rate_code_plus_plus,
            tax.and_then(|t| rate(&sc, t.service_charge_code.as_deref())),
            tax.and_then(|t| rate(&vat, t.tax_code.as_deref())),
            tax_unknown,
        );
        items.push(item);
    }
    let content_hash = content_hash(&items, rate_code_plus_plus);
    Ok(Some(Catalog {
        items,
        rate_code_plus_plus,
        content_hash,
    }))
}
