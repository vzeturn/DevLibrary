//! `ExtraBedTermsReader` against the recording session. Mirrors MM-MEMBERSHIP
//! `tests/MM.Membership.Agent.Tests/Sync/ExtraBedTermsReaderTests.cs` where it can run
//! without SQL Server; the live run is `tests/sql_live_writes.rs`.

use dl_tds::Value;
use dl_time::Date;

use super::*;
use crate::writer::recorder::Recorder;
use crate::writer::session::{Called, Rec};

fn s(v: &str) -> Value {
    Value::String(v.into())
}

fn one(cols: Vec<(&str, Value)>) -> Vec<Rec> {
    vec![Rec::new(cols)]
}

/// The SMILE tables and columns the reader checks (`sys.columns`).
pub(crate) const TERMS_TABLES: &[(&str, &[&str])] = &[
    (
        "GlobalParameter",
        &["HotelID", "ParaName", "ParaString", "Status"],
    ),
    ("ExtraBedInv", &["CruiseCode", "NumBed", "HotelID"]),
    (
        "TransactionCode",
        &[
            "TransactionCode",
            "TransactionSubCode",
            "Description",
            "TaxCode",
            "ServiceChargeCode",
            "STaxCode",
            "AutoPost",
        ],
    ),
    ("HotelParameter", &["HotelID", "HotelDate"]),
    (
        "SpecialService",
        &[
            "SpecialServiceCode",
            "Description",
            "AmountCharge",
            "IncludeInRoomCharge",
            "OpenPrice",
            "TrnCode",
            "TrnSubCode",
        ],
    ),
    ("TaxDef", &["TaxCode", "Description", "TaxRate"]),
    (
        "ServiceCharge",
        &["ServiceChargeCode", "Description", "ServiceRate"],
    ),
];

/// Victory's terms (`SmileFixture.SeedExtraBedSpecialAsync` + `SeedSmileRatesAsync`):
/// `EB` → `110/9` open price, both codes `V8/S5`, `SpecialByDate = 1`, `RateCodePP = 1`,
/// `ExtraBedSurChg = 0`, 48 beds, VAT 8 %, SC 5 %.
pub(crate) fn seed_victory(r: &mut Recorder) {
    r.on("sys.columns", |_| {
        Ok(Called {
            rows: TERMS_TABLES
                .iter()
                .flat_map(|(t, cols)| {
                    cols.iter()
                        .map(|c| Rec::new(vec![("TableName", s(t)), ("ColumnName", s(c))]))
                })
                .collect(),
            status: Some(0),
        })
    });
    r.rows(
        "SELECT DISTINCT TOP (2) LTRIM(RTRIM(HotelID))",
        one(vec![("HotelID", s("VIC"))]),
    );
    r.rows(
        "SELECT LTRIM(RTRIM(ParaName))",
        vec![
            Rec::new(vec![
                ("ParaName", s("ExtraBedSurChg")),
                ("ParaString", s("0")),
            ]),
            Rec::new(vec![
                ("ParaName", s("SpecialByDate")),
                ("ParaString", s("1")),
            ]),
            Rec::new(vec![("ParaName", s("RateCodePP")), ("ParaString", s("1"))]),
        ],
    );
    r.rows("FROM dbo.ExtraBedInv", one(vec![("", Value::I64(48))]));
    r.rows(
        "FROM dbo.SpecialService",
        one(vec![
            ("Code", s("EB")),
            ("TrnCode", s("110")),
            ("TrnSub", s("9")),
            ("OpenPrice", Value::I32(1)),
            ("IncludeInRoomCharge", Value::I32(0)),
            ("AmountCharge", Value::Decimal(Decimal::ZERO)),
        ]),
    );
    let tax = || {
        one(vec![
            ("TaxCode", s("V8")),
            ("ServiceChargeCode", s("S5")),
            ("STaxCode", Value::Null),
        ])
    };
    r.rows(
        "SELECT TOP (1) LTRIM(RTRIM(CONVERT(nvarchar(20), TaxCode)))",
        tax(),
    );
    r.rows("WHERE AutoPost = 1", tax());
    r.rows(
        "FROM dbo.HotelParameter",
        one(vec![(
            "HotelDate",
            Value::Date(Date::new(2026, 10, 20).unwrap()),
        )]),
    );
    r.rows(
        "FROM dbo.TaxDef",
        one(vec![("Rate", Value::Decimal("0.080000".parse().unwrap()))]),
    );
    r.rows(
        "FROM dbo.ServiceCharge",
        one(vec![("Rate", Value::Decimal("0.050000".parse().unwrap()))]),
    );
}

/// The Victory fingerprint, computed from the canonical text outside Rust (Python
/// `hashlib.sha256` over the `extra-bed-fingerprint:v3` lines).
pub(crate) const VICTORY_FINGERPRINT: &str =
    "872409c5dda0487f181793ce3bedf6b6c29a6a6be2593c9fa65fc89ae11c1d33";

#[test]
fn victory_terms_read_with_the_dotnet_fingerprint() {
    let mut r = Recorder::default();
    seed_victory(&mut r);
    let read = read(&mut r, None, "EB").unwrap();
    let t = read.terms.unwrap();
    assert_eq!(t.smile_hotel_id, "VIC");
    assert_eq!(t.surcharge_amount, Some(Decimal::ZERO));
    assert_eq!(t.stock_total, Some(48));
    assert_eq!(t.special_by_date, Some(true));
    assert_eq!(t.rate_code_plus_plus, Some(true));
    assert_eq!(t.special_sc_percent, Some(Decimal::from(5)));
    assert_eq!(t.special_vat_percent, Some(Decimal::from(8)));
    assert_eq!(t.smile_hotel_date, Date::new(2026, 10, 20).ok());
    assert_eq!(t.special_tax_label().as_deref(), Some("V8/S5"));
    assert_eq!(t.special_matches_room_tax(), Some(true));
    assert!(t.special_usable() && t.special_priceable());
    assert_eq!(t.structural_fingerprint, VICTORY_FINGERPRINT);
    assert_eq!(
        t.content_hash,
        "ceb3204b68fd66232f5b4b900641e2ae62511e192d9bfd79c88d22673ddf77ec"
    );
    // Every statement is a SELECT (`ExtraBedTermsReaderTests`).
    assert!(r.texts().iter().all(|t| t.starts_with("SELECT")));
}

#[test]
fn the_in_list_is_expanded_like_dapper() {
    let mut r = Recorder::default();
    seed_victory(&mut r);
    read(&mut r, Some(" VIC "), "EB").unwrap();
    let params = r
        .log
        .iter()
        .find(|s| s.text.contains("ParaName IN"))
        .unwrap();
    assert!(
        params
            .text
            .ends_with("ParaName IN (@Names1,@Names2,@Names3,@Names4,@Names5,@Names6)")
    );
    assert_eq!(params.params[0].2.as_deref(), Some("VIC"));
    assert!(
        !r.texts()
            .iter()
            .any(|t| t.contains("DISTINCT TOP (2) LTRIM(RTRIM(HotelID))"))
    );
}

#[test]
fn skip_reasons_without_parameters_or_with_two_hotels() {
    let mut r = Recorder::default();
    let read_none = read(&mut r, None, "EB").unwrap();
    assert!(read_none.terms.is_none());
    assert!(
        read_none
            .skip_reason
            .unwrap()
            .contains("GlobalParameter(HotelID")
    );

    let mut r = Recorder::default();
    seed_victory(&mut r);
    r.rows(
        "SELECT DISTINCT TOP (2) LTRIM(RTRIM(HotelID))",
        vec![
            Rec::new(vec![("HotelID", s("A"))]),
            Rec::new(vec![("HotelID", s("B"))]),
        ],
    );
    let two = read(&mut r, None, "EB").unwrap();
    assert!(two.skip_reason.unwrap().contains("nhiều HotelID"));
}

#[test]
fn a_missing_catalogue_row_reads_as_none_with_a_warning() {
    let mut r = Recorder::default();
    seed_victory(&mut r);
    r.rows("FROM dbo.SpecialService", Vec::new());
    let t = read(&mut r, None, "XB").unwrap().terms.unwrap();
    assert_eq!(t.special, None);
    assert_eq!(t.special_matches_room_tax(), None);
    assert!(t.warnings.iter().any(|w| w.contains("'XB'")));
}

#[test]
fn surcharge_is_a_whole_amount_or_none() {
    let mut w = Vec::new();
    assert_eq!(
        parse_surcharge(Some(" 250000 "), &mut w),
        Some(Decimal::from(250_000))
    );
    assert_eq!(
        parse_surcharge(Some("12.00"), &mut w),
        Some(Decimal::from(12))
    );
    assert_eq!(parse_surcharge(Some("5."), &mut w), Some(Decimal::from(5)));
    assert!(w.is_empty());
    for bad in ["12.5", "-1", "1,000", "abc", ".", "1e3", "100000000000000"] {
        assert_eq!(parse_surcharge(Some(bad), &mut w), None, "{bad}");
    }
    assert_eq!(w.len(), 7);
    assert_eq!(parse_surcharge(Some("  "), &mut w), None);
    assert_eq!(w.len(), 7);
}

#[test]
fn rates_are_read_only_on_a_plus_plus_hotel() {
    let mut r = Recorder::default();
    seed_victory(&mut r);
    r.rows(
        "SELECT LTRIM(RTRIM(ParaName))",
        one(vec![
            ("ParaName", s("SpecialByDate")),
            ("ParaString", s("1")),
        ]),
    );
    let t = read(&mut r, None, "EB").unwrap().terms.unwrap();
    assert_eq!(t.rate_code_plus_plus, None);
    assert_eq!(t.special_sc_percent, None);
    assert!(!r.texts().iter().any(|t| t.contains("dbo.TaxDef\n")));
}

#[test]
fn sql_text_matches_the_dotnet_constants() {
    assert_eq!(
        code_tax_sql(false),
        "SELECT TOP (1) LTRIM(RTRIM(CONVERT(nvarchar(20), TaxCode))) AS TaxCode,
       LTRIM(RTRIM(CONVERT(nvarchar(20), ServiceChargeCode))) AS ServiceChargeCode,
       CAST(NULL AS nvarchar(20)) AS STaxCode
FROM dbo.TransactionCode
WHERE LTRIM(RTRIM(TransactionCode)) = @Code
  AND (@Sub IS NULL OR LTRIM(RTRIM(CONVERT(varchar(20), TransactionSubCode))) = @Sub)
ORDER BY LTRIM(RTRIM(CONVERT(varchar(20), TransactionSubCode))), TaxCode, ServiceChargeCode"
    );
}
