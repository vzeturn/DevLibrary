//! `MapperTests.cs`, `CheckRenameMapperTests.cs`, `ItemClassRenameMapperTests.cs`,
//! `NullKeyDefaultsTests.cs`.

use super::*;
use crate::contracts::catalog::{CatalogError, SyncCatalog};
use crate::contracts::mapping::{CatalogRowMapper, MapperOptions, MappingError, RawValue};

fn invoice_row() -> VendorRow {
    row([
        ("CheckNo", RawValue::I64(500_001_234)),
        ("RVC", RawValue::I32(1)),
        ("OpenTime", dt(2026, 10, 24, 12, 0, 0)),
        ("CloseTime", dt(2026, 10, 24, 13, 30, 0)),
        ("TotalPaid", dec("125000.50")),
        ("SubTotal", dec("100000.00")),
        ("PaymentInfo", RawValue::Bytes(PHO_BO_TCVN3.to_vec())),
        ("PONumber", RawValue::Null),
        ("CardNumber", "4111111111111111".into()),
    ])
}

fn col<'a>(m: &'a StrMap, k: &str) -> Option<&'a str> {
    m.get(k).and_then(|v| v.as_deref())
}

#[test]
fn invoice_golden_mapping() {
    let m = mapper().map("Invoice", &invoice_row()).unwrap();
    assert_eq!(m.pg_target, "app.closed_checks");
    assert_eq!(
        sorted(&m.key),
        sorted(&smap([("check_no", Some("500001234"))]))
    );
    assert!(m.key_aligned);
    assert_eq!(col(&m.columns, "total_paid"), Some("125000.5"));
    assert_eq!(col(&m.columns, "sub_total"), Some("100000"));
    assert_eq!(col(&m.columns, "rvc_no"), Some("1"));
    assert_eq!(m.excluded, ["CardNumber"]);
    assert!(!m.columns.contains_key("card_number"));
    assert_eq!(
        col(&m.columns, "opened_at"),
        Some("2026-10-24T05:00:00.0000000Z")
    );
    assert_eq!(
        col(&m.columns, "closed_at"),
        Some("2026-10-24T06:30:00.0000000Z")
    );
    let extra = crate::util::stj::parse(col(&m.columns, "src_extra").unwrap()).unwrap();
    assert_eq!(
        extra.get("PaymentInfo").and_then(|v| v.as_str()),
        Some("Phở bò")
    );
    assert!(extra.get("PONumber").unwrap().is_null());
}

#[test]
fn mapping_is_deterministic_and_hash_ignores_input_order() {
    let a = mapper().map("Invoice", &invoice_row()).unwrap();
    let mut pairs: Vec<(String, RawValue)> = invoice_row().into_iter().collect();
    pairs.reverse();
    let reordered: VendorRow = pairs.into_iter().collect();
    let b = mapper().map("Invoice", &reordered).unwrap();
    assert_eq!(b.hash, a.hash);
    assert_eq!(sorted(&b.columns), sorted(&a.columns));
}

#[test]
fn hash_changes_when_money_changes_but_not_on_trailing_zeros() {
    let mut base = invoice_row();
    let h0 = mapper().map("Invoice", &base).unwrap().hash;
    base.insert("TotalPaid", dec("125000.5000"));
    assert_eq!(mapper().map("Invoice", &base).unwrap().hash, h0);
    base.insert("TotalPaid", dec("125000.51"));
    assert_ne!(mapper().map("Invoice", &base).unwrap().hash, h0);
}

#[test]
fn excluded_credentials_never_appear_anywhere_and_do_not_affect_the_hash() {
    let r = row([
        ("Initial", "NV01".into()),
        ("FullName", RawValue::Bytes(PHO_BO_TCVN3.to_vec())),
        ("PassWord", "1234".into()),
        ("TouchID", "T".into()),
        ("CardID", "C".into()),
    ]);
    let m = mapper().map("CashierDef", &r).unwrap();
    let mut ex = m.excluded.clone();
    ex.sort();
    assert_eq!(ex, ["CardID", "PassWord", "TouchID"]);
    let json = {
        let mut w = crate::util::stj::Writer::new(crate::util::stj::Style::Compact);
        crate::contracts::json::write_str_map(&mut w, &m.columns);
        w.finish()
    };
    assert!(!json.contains("1234") && !json.contains("PassWord"));
    for k in ["PassWord", "TouchID", "CardID"] {
        assert!(!m.columns.contains_key(k));
    }
    let mut other = r.clone();
    other.insert("PassWord", "9999".into());
    assert_eq!(mapper().map("CashierDef", &other).unwrap().hash, m.hash);
}

#[test]
fn target_without_extra_column_drops_and_lists_unmapped_columns() {
    let m = CatalogRowMapper::new(catalog().clone(), options(Some(|_| false)))
        .map("Invoice", &invoice_row())
        .unwrap();
    assert!(!m.columns.contains_key("src_extra"));
    assert!(m.dropped.iter().any(|d| d == "PaymentInfo"));
    assert!(m.dropped.iter().any(|d| d == "PONumber"));
    assert!(!m.dropped.iter().any(|d| d == "TotalPaid"));
}

#[test]
fn null_and_empty_string_stay_distinct() {
    let mut with_empty = invoice_row();
    with_empty.insert("PONumber", "".into());
    let a = mapper().map("Invoice", &with_empty).unwrap();
    let b = mapper().map("Invoice", &invoice_row()).unwrap();
    assert_ne!(a.hash, b.hash);
}

#[test]
fn composite_key_and_explicit_override_rename() {
    let mut opts = MapperOptions::new(vietnam());
    opts.column_overrides = vec![(
        "Invoice".into(),
        vec![("TotalPaid".into(), "paid_total".into())],
    )];
    let m = CatalogRowMapper::new(catalog().clone(), opts)
        .map("Invoice", &invoice_row())
        .unwrap();
    assert!(m.columns.contains_key("paid_total"));
    assert!(!m.columns.contains_key("total_paid"));

    let tl = mapper()
        .map(
            "TableList",
            &row([("RVC", RawValue::I32(1)), ("TableNum", "A1".into())]),
        )
        .unwrap();
    assert_eq!(
        sorted(&tl.key),
        sorted(&smap([("rvc_no", Some("1")), ("table_no", Some("A1"))]))
    );
}

fn missing_column(r: Result<impl std::fmt::Debug, MappingError>) -> String {
    match r {
        Err(MappingError::MissingKey { column, .. }) => column,
        other => panic!("expected MissingKey, got {other:?}"),
    }
}

#[test]
fn missing_key_column_throws() {
    assert_eq!(
        missing_column(mapper().map("Invoice", &row([("RVC", RawValue::I32(1))]))),
        "CheckNo"
    );
    missing_column(mapper().map("Invoice", &row([("CheckNo", RawValue::Null)])));
}

#[test]
fn a_null_in_a_later_logical_key_column_of_a_raw_table_is_kept_as_a_null_key_value() {
    let reopen = mapper()
        .map(
            "ReOpenLog",
            &row([
                ("CheckNo", RawValue::I32(7)),
                ("OpenTime", dt(2026, 10, 24, 12, 0, 0)),
                ("CloseTime", RawValue::Null),
                ("ReOpenTime", dt(2026, 10, 24, 14, 0, 0)),
            ]),
        )
        .unwrap();
    assert_eq!(reopen.key.get("CloseTime"), Some(&None));
    assert_eq!(col(&reopen.key, "CheckNo"), Some("7"));

    let info = mapper()
        .map(
            "tmpInvoiceInfo",
            &row([
                ("CheckNo", RawValue::I32(7)),
                ("CurPage", RawValue::I32(1)),
                ("CurLine", RawValue::I32(2)),
                ("Printed", RawValue::Null),
                ("Ordered", RawValue::Null),
                ("PrintType", RawValue::Null),
            ]),
        )
        .unwrap();
    assert_eq!(info.key.get("PrintType"), Some(&None));
}

#[test]
fn a_raw_table_still_needs_its_leading_key_column_and_every_key_column_of_the_build() {
    let m = mapper();
    assert_eq!(
        missing_column(m.map(
            "ReOpenLog",
            &row([
                ("CheckNo", RawValue::Null),
                ("OpenTime", dt(2026, 10, 24, 0, 0, 0)),
                ("CloseTime", dt(2026, 10, 24, 0, 0, 0)),
            ])
        )),
        "CheckNo"
    );
    // A column the build lacks is not a NULL.
    assert_eq!(
        missing_column(m.map(
            "ReOpenLog",
            &row([
                ("CheckNo", RawValue::I32(7)),
                ("OpenTime", dt(2026, 10, 24, 0, 0, 0))
            ])
        )),
        "CloseTime"
    );
}

#[test]
fn a_bad_value_names_its_column() {
    let r = mapper().map(
        "Invoice",
        &row([("CheckNo", RawValue::I32(1)), ("SubTotal", "abc".into())]),
    );
    match r {
        Err(MappingError::ValueFormat { column, .. }) => assert_eq!(column, "SubTotal"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn unknown_skipped_and_targetless_tables_are_refused() {
    let m = mapper();
    let r = row([("x", RawValue::I32(1))]);
    assert!(matches!(
        m.map("NoSuchTable", &r),
        Err(MappingError::UnknownTable { .. })
    ));
    let skipped = &catalog()
        .tables
        .iter()
        .find(|t| t.class == "skip")
        .unwrap()
        .table;
    assert!(matches!(
        m.map(skipped, &r),
        Err(MappingError::NotMappable { .. })
    ));
    let unclassified = &catalog().unclassified().next().unwrap().table;
    assert!(matches!(
        m.map(unclassified, &r),
        Err(MappingError::NotMappable { .. })
    ));
    let no_target = catalog().classified().find(|t| !t.has_pg_target()).unwrap();
    let with_keys: VendorRow = no_target
        .keys()
        .iter()
        .map(|k| (k.clone(), RawValue::I32(1)))
        .collect();
    assert!(matches!(
        m.map(&no_target.table, &with_keys),
        Err(MappingError::NotMappable { .. })
    ));
    assert_eq!(
        m.canonicalize(&no_target.table, &with_keys)
            .unwrap()
            .hash
            .len(),
        64
    );
}

#[test]
fn table_lookup_is_case_insensitive() {
    assert_eq!(
        mapper().map("invoice", &invoice_row()).unwrap().pg_target,
        "app.closed_checks"
    );
}

#[test]
fn blob_columns_are_replaced_by_content_hash_under_except_blob_policy() {
    let mut r = row([
        ("MemberID", "M1".into()),
        ("MemberSubID", RawValue::I32(0)),
        ("MemberName", RawValue::Bytes(PHO_BO_TCVN3.to_vec())),
        ("Picture", RawValue::Bytes(vec![1, 2, 3])),
    ]);
    let m = mapper().map("MemberList", &r).unwrap();
    assert_eq!(
        m.blob_hashes.get("Picture").map(String::as_str),
        Some("039058c6f2c0cb492c533b0a4d14ef77cc0f78abccced5287d84a1a2011cfb81")
    );
    assert!(!m.columns.values().any(|v| v.as_deref() == Some("AQID")));
    r.insert("Picture", RawValue::Bytes(vec![9]));
    // Blob content travels by its own hash.
    assert_eq!(mapper().map("MemberList", &r).unwrap().hash, m.hash);
}

#[test]
fn mapped_row_converts_to_wire_row() {
    let m = mapper().map("Invoice", &invoice_row()).unwrap();
    let wire = m.to_sync_row(5);
    assert_eq!(wire.src_ver, 5);
    assert_eq!(wire.hash, m.hash);
    assert_eq!(col(&wire.key, "check_no"), Some("500001234"));
}

// ---- CheckRenameMapperTests / ItemClassRenameMapperTests -------------------------------

/// The catalog before a rename: every new name put back to its old one.
fn pre_rename(tables: &[(&str, &str)], columns: &[(&str, &str)]) -> SyncCatalog {
    let old_table = |t: &str| {
        tables
            .iter()
            .find(|(_, new)| *new == t)
            .map_or(t.to_owned(), |(old, _)| (*old).to_owned())
    };
    let old_col = |c: &str| {
        columns
            .iter()
            .find(|(_, new)| *new == c)
            .map_or(c.to_owned(), |(old, _)| (*old).to_owned())
    };
    let mut c = catalog().clone();
    for t in &mut c.tables {
        t.pg_target = t.pg_target.as_deref().map(old_table);
        t.pg_keys = t
            .pg_keys
            .as_ref()
            .map(|k| k.iter().map(|x| old_col(x)).collect());
        t.pg_columns = t
            .pg_columns
            .as_ref()
            .map(|m| m.iter().map(|(k, v)| (k.to_owned(), old_col(v))).collect());
    }
    c
}

fn renamed(before: &StrMap, columns: &[(&str, &str)]) -> Vec<(String, Option<String>)> {
    let mut v: Vec<(String, Option<String>)> = before
        .iter()
        .map(|(k, v)| {
            let k = columns
                .iter()
                .find(|(old, _)| *old == k)
                .map_or(k, |(_, new)| *new);
            (k.to_owned(), v.clone())
        })
        .collect();
    v.sort();
    v
}

fn rename_case(
    tables: &[(&str, &str)],
    columns: &[(&str, &str)],
    vendor: &str,
    old_target: &str,
    new_target: &str,
    r: &VendorRow,
) -> (
    crate::contracts::mapping::MappedRow,
    crate::contracts::mapping::MappedRow,
) {
    let before = CatalogRowMapper::new(pre_rename(tables, columns), options(None))
        .map(vendor, r)
        .unwrap();
    let after = mapper().map(vendor, r).unwrap();
    assert_eq!(before.pg_target, old_target, "{vendor}");
    assert_eq!(after.pg_target, new_target, "{vendor}");
    assert_eq!(
        sorted(&after.key),
        renamed(&before.key, columns),
        "{vendor}"
    );
    assert_eq!(
        sorted(&after.columns),
        renamed(&before.columns, columns),
        "{vendor}"
    );
    // The row hash is taken over vendor names, so a PG rename never re-syncs a row.
    assert_eq!(after.hash, before.hash, "{vendor}");
    assert_eq!(after.key_aligned, before.key_aligned);
    assert_eq!(after.excluded, before.excluded);
    assert_eq!(after.dropped, before.dropped);
    for k in after.key.keys().chain(after.columns.keys()) {
        assert!(!columns.iter().any(|(old, _)| *old == k), "{vendor}: {k}");
    }
    (before, after)
}

const CHECK_TABLES: &[(&str, &str)] = &[
    ("app.tmp_invoice", "app.open_checks"),
    ("app.tmp_trn", "app.open_check_lines"),
    ("app.invoice", "app.closed_checks"),
    ("app.trn", "app.closed_check_lines"),
    ("app.trn_info", "app.check_line_details"),
    ("app.trn_modifiers", "app.check_line_modifiers"),
    ("app.trn_activities", "app.check_line_activities"),
    ("app.void_trn", "app.voided_check_lines"),
    ("app.invoice_cxl", "app.closed_check_cancellations"),
];
const CHECK_COLUMNS: &[(&str, &str)] = &[("trn_seq", "line_seq"), ("trn_code", "transaction_code")];

fn check_row(vendor: &str) -> VendorRow {
    match vendor {
        "TmpTrn" | "Trn" => row([
            ("CheckNo", RawValue::I64(7)),
            ("TrnSeq", RawValue::I32(3)),
            ("TrnCode", RawValue::I32(100)),
            ("ItemCode", "PHO".into()),
            ("TrnQty", dec("2")),
            ("BaseTTL", dec("90000.00")),
        ]),
        "TmpInvoice" => row([
            ("CheckNo", RawValue::I64(7)),
            ("RVC", RawValue::I32(1)),
            ("SubTotal", dec("10.5")),
        ]),
        "Invoice" => row([
            ("CheckNo", RawValue::I64(7)),
            ("RVC", RawValue::I32(1)),
            ("TotalPaid", dec("125000.50")),
        ]),
        "tmpTrnInfo" => row([
            ("CheckNo", RawValue::I64(7)),
            ("TrnNo", RawValue::I32(3)),
            ("DoneTime", RawValue::Null),
        ]),
        "TrnActivities" => row([
            ("nSeq", RawValue::I64(11)),
            ("RVC", RawValue::I32(1)),
            ("CheckNo", RawValue::I64(7)),
            ("TrnNo", RawValue::I32(3)),
        ]),
        "InvoiceCxl" => row([("CheckNo", RawValue::I64(7)), ("RVC", RawValue::I32(1))]),
        "Payment" => row([
            ("PaymentNo", RawValue::I64(21)),
            ("CheckNo", RawValue::I64(7)),
            ("TrnNo", RawValue::I32(4)),
            ("PmtCode", RawValue::I32(400)),
        ]),
        "TransactionCode" => row([
            ("TrnCode", RawValue::I32(400)),
            ("TrnSubCode", RawValue::I32(1)),
            ("Description", "CARD".into()),
        ]),
        other => panic!("no sample row for {other}"),
    }
}

#[test]
fn a_row_maps_as_before_the_check_rename_once_the_explicit_name_map_is_applied() {
    for (vendor, old, new) in [
        ("TmpInvoice", "app.tmp_invoice", "app.open_checks"),
        ("TmpTrn", "app.tmp_trn", "app.open_check_lines"),
        ("Invoice", "app.invoice", "app.closed_checks"),
        ("Trn", "app.trn", "app.closed_check_lines"),
        ("tmpTrnInfo", "app.trn_info", "app.check_line_details"),
        (
            "TrnActivities",
            "app.trn_activities",
            "app.check_line_activities",
        ),
        (
            "InvoiceCxl",
            "app.invoice_cxl",
            "app.closed_check_cancellations",
        ),
        ("Payment", "app.payment_detail", "app.payment_detail"),
        (
            "TransactionCode",
            "app.transaction_codes",
            "app.transaction_codes",
        ),
    ] {
        rename_case(
            CHECK_TABLES,
            CHECK_COLUMNS,
            vendor,
            old,
            new,
            &check_row(vendor),
        );
    }
}

fn assert_no_renamed_names(tables: &[(&str, &str)], columns: &[(&str, &str)], sqlite_too: bool) {
    let c = catalog();
    let old_t = |t: &Option<String>| tables.iter().any(|(old, _)| Some(*old) == t.as_deref());
    if sqlite_too {
        for t in &c.sqlite_tables {
            assert!(
                !old_t(&t.pg_target),
                "{} must target the new table name",
                t.table
            );
        }
    }
    for t in &c.tables {
        assert!(
            !old_t(&t.pg_target),
            "{} must target the new table name",
            t.table
        );
        let old_c = |x: &str| columns.iter().any(|(old, _)| *old == x);
        assert!(!t.pg_keys.iter().flatten().any(|k| old_c(k)), "{}", t.table);
        assert!(
            !t.pg_columns
                .iter()
                .flat_map(|m| m.values())
                .any(|v| old_c(v)),
            "{}",
            t.table
        );
    }
}

#[test]
fn the_catalog_targets_no_renamed_check_table_or_column() {
    assert_no_renamed_names(CHECK_TABLES, CHECK_COLUMNS, false);
}

const ICLASS_TABLES: &[(&str, &str)] = &[(
    "app.modifier_group_iclasses",
    "app.modifier_group_item_classes",
)];
const ICLASS_COLUMNS: &[(&str, &str)] = &[("iclass", "item_class"), ("p_type", "payment_type")];

#[test]
fn a_row_maps_as_before_the_item_class_rename_once_the_explicit_name_map_is_applied() {
    let item = row([
        ("ICode", "PHO".into()),
        ("IName", "Pho bo".into()),
        ("Price", dec("50000.00")),
        ("IClass", RawValue::I32(901)),
        ("IGroup", "1".into()),
        ("IActive", RawValue::Bool(true)),
        ("RVC", RawValue::I32(1)),
    ]);
    let cg = row([
        ("CGGCode", RawValue::I32(10)),
        ("IClass", RawValue::I32(901)),
    ]);
    for (vendor, old, new, r) in [
        ("ItemDef", "app.items", "app.items", &item),
        (
            "CGGroupIClass",
            "app.modifier_group_iclasses",
            "app.modifier_group_item_classes",
            &cg,
        ),
    ] {
        let (before, _) = rename_case(ICLASS_TABLES, ICLASS_COLUMNS, vendor, old, new, r);
        // The oracle must exercise the renamed column.
        assert!(
            before
                .key
                .keys()
                .chain(before.columns.keys())
                .any(|k| k == "iclass")
        );
    }
}

#[test]
fn the_catalog_targets_no_table_or_column_renamed_by_0230() {
    assert_no_renamed_names(ICLASS_TABLES, ICLASS_COLUMNS, true);
    let pm: Vec<_> = catalog()
        .tables
        .iter()
        .filter(|t| t.table == "PaymentMode")
        .collect();
    assert_eq!(pm.len(), 1);
    assert!(
        pm[0].pg_raw,
        "PaymentMode lands raw, under vendor column names"
    );
}

// ---- NullKeyDefaultsTests --------------------------------------------------------------

fn glob_para(rvc: RawValue) -> VendorRow {
    row([
        ("nSeq", RawValue::I32(7)),
        ("RVC", rvc),
        ("ParaName", "TaxByIClass".into()),
        ("ParaStr", "1".into()),
        ("ParaType", "S".into()),
        ("ParaDesc", "desc".into()),
        ("Status", RawValue::I32(1)),
    ])
}

#[test]
fn only_glob_para_currency_def_and_discount_code_opt_in_to_rvc_0_for_a_null_outlet() {
    let mut with: Vec<&str> = catalog()
        .tables
        .iter()
        .filter(|t| t.null_key_defaults.as_ref().is_some_and(|d| !d.is_empty()))
        .map(|t| t.table.as_str())
        .collect();
    with.sort();
    assert_eq!(with, ["CurrencyDef", "DiscountCode", "GlobPara"]);
    let d = catalog()
        .find("GlobPara")
        .unwrap()
        .null_key_defaults
        .clone()
        .unwrap();
    assert_eq!(sorted(&d), vec![("rvc_no".to_owned(), "0".to_owned())]);
}

#[test]
fn glob_para_with_null_rvc_maps_to_rvc_0_in_key_and_columns() {
    let m = mapper()
        .map("GlobPara", &glob_para(RawValue::Null))
        .unwrap();
    assert_eq!(
        sorted(&m.key),
        sorted(&smap([("rvc_no", Some("0")), ("key", Some("TaxByIClass"))]))
    );
    // The PG column is NOT NULL and must agree with the key.
    assert_eq!(col(&m.columns, "rvc_no"), Some("0"));
    assert_eq!(m.defaulted_keys, ["rvc_no"]);
}

#[test]
fn currency_def_with_null_rvc_maps_to_rvc_0() {
    let m = mapper()
        .map(
            "CurrencyDef",
            &row([
                ("RVC", RawValue::Null),
                ("CrCode", RawValue::I32(2)),
                ("CrSymbol", "USD".into()),
                ("CrName", "Dollar".into()),
                ("ExRate", dec("25000")),
                ("DivideBase", RawValue::Bool(false)),
                ("NoOfDec", RawValue::I32(2)),
                ("IsBase", RawValue::Bool(false)),
            ]),
        )
        .unwrap();
    assert_eq!(col(&m.key, "rvc_no"), Some("0"));
    assert_eq!(col(&m.key, "cr_code"), Some("2"));
    assert_eq!(col(&m.columns, "rvc_no"), Some("0"));
    assert_eq!(m.defaulted_keys, ["rvc_no"]);
}

#[test]
fn discount_code_with_null_rvc_maps_to_rvc_0() {
    let m = mapper()
        .map(
            "DiscountCode",
            &row([
                ("RVC", RawValue::Null),
                ("DscCode", "VIP10".into()),
                ("DscName", "VIP".into()),
                ("DscByAmount", RawValue::Bool(false)),
                ("DscAmount", dec("10")),
                ("isOpen", RawValue::Bool(true)),
                ("IsVoided", RawValue::Bool(false)),
                ("VoidedBy", RawValue::Null),
                ("VoidedAt", RawValue::Null),
            ]),
        )
        .unwrap();
    assert_eq!(
        sorted(&m.key),
        sorted(&smap([("rvc_no", Some("0")), ("dsc_code", Some("VIP10"))]))
    );
    assert_eq!(m.defaulted_keys, ["rvc_no"]);
}

#[test]
fn explicit_rvc_is_not_defaulted_and_hashes_apart_from_the_null_row() {
    let m = mapper();
    let explicit0 = m.map("GlobPara", &glob_para(RawValue::I32(0))).unwrap();
    let null_rvc = m.map("GlobPara", &glob_para(RawValue::Null)).unwrap();
    assert!(explicit0.defaulted_keys.is_empty());
    // Both land on the same PG row; capture picks the explicit one.
    assert_eq!(sorted(&explicit0.key), sorted(&null_rvc.key));
    assert_ne!(explicit0.hash, null_rvc.hash);
}

#[test]
fn table_without_the_option_still_rejects_a_null_rvc() {
    let r = row([
        ("RVC", RawValue::Null),
        ("TableNum", "A1".into()),
        ("TableArea", RawValue::I32(1)),
    ]);
    assert_eq!(missing_column(mapper().map("TableList", &r)), "RVC");
}

#[test]
fn a_key_column_the_build_lacks_is_still_a_reject() {
    let mut r = glob_para(RawValue::I32(1));
    r.remove("RVC");
    assert_eq!(missing_column(mapper().map("GlobPara", &r)), "RVC");
}

/// The embedded catalog JSON with `null_key_defaults` set on one table.
fn with_defaults(table: &str, column: &str, value: &str) -> String {
    let mut root = crate::util::stj::parse(crate::contracts::catalog::CATALOG_JSON).unwrap();
    let tables = root
        .as_object_mut()
        .and_then(|o| o.get_mut("tables"))
        .and_then(|t| t.as_array_mut())
        .unwrap();
    let entry = tables
        .iter_mut()
        .find(|t| t.get("table").and_then(|x| x.as_str()) == Some(table))
        .unwrap();
    let mut d = dl_json::Map::new();
    d.insert(column, value);
    entry
        .as_object_mut()
        .unwrap()
        .insert("null_key_defaults", d);
    let mut w = crate::util::stj::Writer::new(crate::util::stj::Style::Compact);
    w.value(&root);
    w.finish()
}

#[test]
fn loader_refuses_an_invalid_null_key_defaults_entry() {
    for (table, column, value, because) in [
        ("GlobPara", "value", "0", "not a vendor-fed pg_keys column"),
        ("GlobPara", "rvc_no", " ", "non-empty value"),
        ("DefRVCList", "rvc_no", "0", "needs a typed table"),
    ] {
        match SyncCatalog::parse(&with_defaults(table, column, value), None) {
            Err(CatalogError::Invalid(m)) => assert!(m.contains(because), "{m}"),
            other => panic!("{table}: {:?}", other.map(|_| ())),
        }
    }
}

#[test]
fn loader_reads_a_valid_null_key_defaults_entry() {
    let cat = SyncCatalog::parse(&with_defaults("TableList", "rvc_no", "0"), None).unwrap();
    let d = cat.find("TableList").unwrap().null_key_defaults_or_empty();
    assert_eq!(d.get("rvc_no").map(String::as_str), Some("0"));
}
