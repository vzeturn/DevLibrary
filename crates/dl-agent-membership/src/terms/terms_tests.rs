//! `ExtraBedTermsSync` and `SpecialServiceCatalogReader` against the recording session and
//! the fake hub. Mirrors MM-MEMBERSHIP `ExtraBedTermsSyncTests` and
//! `SpecialServiceCatalogReaderTests` where they run without SQL Server.

use std::sync::Mutex;

use dl_decimal::Decimal;
use dl_tds::Value;

use super::*;
use crate::jobs::bed_terms::SpecialService;
use crate::jobs::fakes::Hub;
use crate::jobs::preflight::tests::terms;
use crate::writer::bed_terms_reader::tests::{TERMS_TABLES, seed_victory};
use crate::writer::recorder::Recorder;
use crate::writer::session::{Called, Rec};

fn s(v: &str) -> Value {
    Value::String(v.into())
}

/// Victory plus a PMS-priced `XB` at 453,600 and the catalogue's own queries.
fn catalogue() -> Recorder {
    let mut r = Recorder::default();
    seed_victory(&mut r);
    r.on("TYPE_NAME(c.system_type_id)", |_| {
        Ok(Called {
            rows: TERMS_TABLES
                .iter()
                .flat_map(|(t, cols)| {
                    cols.iter().map(|c| {
                        Rec::new(vec![
                            ("TableName", s(t)),
                            ("ColumnName", s(c)),
                            ("TypeName", s("nvarchar")),
                        ])
                    })
                })
                .collect(),
            status: Some(0),
        })
    });
    let special = |code: &str, name: &str, amount: i64, open: i32| {
        Rec::new(vec![
            ("Code", s(code)),
            ("Name", s(name)),
            ("TrnCode", s("110")),
            ("TrnSub", s("9")),
            ("AmountCharge", Value::Decimal(Decimal::from(amount))),
            ("OpenPrice", Value::I32(open)),
            ("IncludeInRoomCharge", Value::I32(0)),
        ])
    };
    r.rows(
        "ORDER BY LTRIM(RTRIM(SpecialServiceCode))",
        vec![
            special("EB", "Extra bed", 0, 1),
            special("XB", "Giường phụ", 453_600, 0),
            special("XB", "duplicate", 1, 0),
        ],
    );
    r.rows(
        "WHERE LTRIM(RTRIM(TransactionCode)) IN",
        vec![
            Rec::new(vec![
                ("Code", s("110")),
                ("Sub", s("9")),
                ("TaxCode", s("V8")),
                ("ServiceChargeCode", s("S5")),
                ("STaxCode", Value::Null),
            ]),
            Rec::new(vec![
                ("Code", s("110")),
                ("Sub", s("1")),
                ("TaxCode", s("V10")),
                ("ServiceChargeCode", s("S5")),
                ("STaxCode", Value::Null),
            ]),
        ],
    );
    let rate = |code: &str, rate: &str| {
        vec![Rec::new(vec![
            ("Code", s(code)),
            ("Rate", Value::Decimal(rate.parse().unwrap())),
        ])]
    };
    r.rows("FROM dbo.TaxDef WHERE", rate("V8", "0.080000"));
    r.rows("FROM dbo.ServiceCharge WHERE", rate("S5", "0.050000"));
    r
}

struct Source(Mutex<Recorder>);

impl TermsSource for Source {
    fn read(&self, hotel_id: Option<&str>, code: &str) -> Result<Readings, WriteError> {
        read_both(&mut *self.0.lock().unwrap(), hotel_id, code)
    }
}

fn opts(mode: &str) -> TermsOptions {
    TermsOptions {
        mode: mode.into(),
        configured_code: "EB".into(),
        hub_code: HubSpecialCode::default(),
        smile_hotel_id: None,
        earn_charge_codes: Vec::new(),
    }
}

#[test]
fn the_catalogue_reads_every_code_once_with_its_bed_price_and_hash() {
    let mut r = catalogue();
    let c = catalog::read(&mut r, Some(true)).unwrap().unwrap();
    assert_eq!(c.items.len(), 2);
    let xb = &c.items[1];
    assert_eq!(xb.name.as_deref(), Some("Giường phụ"));
    assert_eq!(xb.tax_label.as_deref(), Some("V8/S5"));
    assert_eq!(xb.bed_price, Some(Decimal::from(400_000)));
    assert_eq!(xb.open_price, Some(false));
    assert_eq!(c.items[0].bed_price, None, "no catalogue price");
    // Computed outside Rust from the `special-services:v1` text.
    assert_eq!(
        c.content_hash,
        "bd3c4ececd09de944f2a92bd531cd33fc5a8f84fd1724b6613c2272c67b0241f"
    );
    let tax = r
        .log
        .iter()
        .find(|st| st.text.contains("TransactionCode)) IN"))
        .unwrap();
    assert!(tax.text.ends_with("IN (@Codes1)"));
}

#[test]
fn bed_price_backs_the_taxes_out_only_when_the_basis_is_known() {
    let d = |n: i64| Some(Decimal::from(n));
    assert_eq!(bed_price_of(d(453_600), Some(true)), d(400_000));
    assert_eq!(bed_price_of(d(453_600), Some(false)), d(453_600));
    assert_eq!(bed_price_of(d(453_600), None), None);
    assert_eq!(bed_price_of(d(0), Some(false)), None);
    assert_eq!(
        catalog::bed_price(d(453_600), Some(true), d(5), d(8), true),
        None,
        "an STaxCode whose rate is not read"
    );
}

fn bed_price_of(amount: Option<Decimal>, pp: Option<bool>) -> Option<Decimal> {
    catalog::bed_price(
        amount,
        pp,
        Some(Decimal::from(5)),
        Some(Decimal::from(8)),
        false,
    )
}

#[test]
fn decide_mode_names_what_blocks_smile_special() {
    assert_eq!(
        decide_mode(MODE_FOLD_LEGACY, &terms()),
        (MODE_FOLD_LEGACY, None)
    );
    assert_eq!(
        decide_mode(MODE_SMILE_SPECIAL, &terms()),
        (MODE_FOLD_LEGACY, Some(FOLD_FINGERPRINT_UNVERIFIED))
    );
    let missing = ExtraBedTerms {
        special: None,
        ..terms()
    };
    assert_eq!(
        decide_mode(MODE_SMILE_SPECIAL, &missing).1,
        Some(FOLD_SPECIAL_MISSING)
    );
    let off = ExtraBedTerms {
        special_by_date: Some(false),
        ..terms()
    };
    assert_eq!(
        decide_mode(MODE_SMILE_SPECIAL, &off).1,
        Some(FOLD_SPECIAL_BY_DATE_OFF)
    );
    let pms = ExtraBedTerms {
        special: Some(SpecialService {
            open_price: Some(0),
            amount_charge: Some(Decimal::from(500_000)),
            ..terms().special.unwrap()
        }),
        ..terms()
    }
    .as_pms_priced();
    assert_eq!(
        decide_mode(MODE_SMILE_SPECIAL, &pms),
        (MODE_SMILE_SPECIAL, None)
    );
}

#[test]
fn earn_coverage_follows_the_override_or_the_derived_rule() {
    assert_eq!(special_earn_covered(&terms(), &[]), Some(true));
    assert_eq!(special_earn_covered(&terms(), &["500".into()]), Some(false));
    assert_eq!(special_earn_covered(&terms(), &["110".into()]), Some(true));
    let none = ExtraBedTerms {
        special: None,
        ..terms()
    };
    assert_eq!(special_earn_covered(&none, &[]), None);
}

#[test]
fn a_cycle_posts_both_then_waits_and_posts_again_only_on_change() {
    let hub = Arc::new(Hub::default());
    *hub.has_pms_endpoints.lock().unwrap() = true;
    let src = Arc::new(Source(Mutex::new(catalogue())));
    let mut sync = TermsSync::new(src.clone(), hub.clone(), opts(MODE_SMILE_SPECIAL));
    let t0 = Instant::now();
    assert!(sync.run_if_due(t0));
    {
        let posts = hub.pms_posts.lock().unwrap();
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[0].0, "catalog");
        let body = dl_json::parse(&posts[1].1).unwrap();
        assert_eq!(
            body.get("SmileHotelId").and_then(|v| v.as_str()),
            Some("VIC")
        );
        assert_eq!(
            body.get("ModeReason").and_then(|v| v.as_str()),
            Some(FOLD_FINGERPRINT_UNVERIFIED)
        );
        assert_eq!(
            body.get("SpecialTaxLabel").and_then(|v| v.as_str()),
            Some("V8/S5")
        );
        assert_eq!(
            body.get("IsFullSnapshot").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            body.get("SmileHotelDate").and_then(|v| v.as_str()),
            Some("2026-10-20")
        );
    }
    // Within 300 s nothing is read.
    let reads = src.0.lock().unwrap().log.len();
    assert!(!sync.run_if_due(t0 + Duration::from_secs(10)));
    assert_eq!(src.0.lock().unwrap().log.len(), reads);
    // Due again, nothing changed: no post.
    assert!(!sync.run_if_due(t0 + READ_INTERVAL));
    assert_eq!(hub.pms_posts.lock().unwrap().len(), 2);
    // The hub picked a special: PMS-priced terms, posted at once.
    sync.opts.hub_code.set(Some("EB"));
    assert!(sync.run_if_due(t0 + READ_INTERVAL * 2));
    let posts = hub.pms_posts.lock().unwrap();
    let body = dl_json::parse(&posts.last().unwrap().1).unwrap();
    assert_eq!(
        body.get("IsFullSnapshot").and_then(|v| v.as_bool()),
        Some(false)
    );
    // A full snapshot is due after 24 h even without a change.
    drop(posts);
    assert!(sync.run_if_due(t0 + FULL_SNAPSHOT_INTERVAL + READ_INTERVAL * 3));
}

#[test]
fn an_older_hub_or_a_smile_failure_never_fails_the_cycle() {
    let hub = Arc::new(Hub::default());
    let src = Arc::new(Source(Mutex::new(catalogue())));
    let mut sync = TermsSync::new(src, hub.clone(), opts(MODE_SMILE_SPECIAL));
    assert!(!sync.run_if_due(Instant::now()));
    assert!(hub.pms_posts.lock().unwrap().is_empty());

    let mut broken = Recorder::default();
    broken.on("sys.columns", |_| {
        Err(crate::smile::SmileError::Server(208))
    });
    let mut sync = TermsSync::new(
        Arc::new(Source(Mutex::new(broken))),
        hub,
        opts(MODE_SMILE_SPECIAL),
    );
    assert!(!sync.run_if_due(Instant::now()));
}

#[test]
fn the_terms_body_has_the_dotnet_property_names() {
    let req = TermsRequest {
        terms: terms(),
        wire_hash: "w".into(),
        mode_configured: MODE_SMILE_SPECIAL.into(),
        mode_effective: MODE_FOLD_LEGACY,
        mode_reason: Some(FOLD_FINGERPRINT_UNVERIFIED),
        is_full_snapshot: true,
        special_earn_covered: Some(true),
    };
    let body = req.to_j().to_json();
    for key in [
        "SmileHotelId",
        "SurchargeAmount",
        "TrnCode",
        "TrnSub",
        "StockTotal",
        "StructuralFingerprint",
        "ContentHash",
        "ModeConfigured",
        "ModeEffective",
        "ModeReason",
        "IsFullSnapshot",
        "SmileHotelDate",
        "SpecialCode",
        "SpecialTrnCode",
        "SpecialTrnSub",
        "SpecialByDate",
        "SpecialTaxLabel",
        "SpecialMatchesRoomTax",
        "SpecialEarnCovered",
    ] {
        assert!(body.contains(&format!("\"{key}\":")), "{key}");
    }
}
