//! The money batches (ADR 0174, RT-1297) against the C# text and the parameters the SQL
//! unit binds (`SqlOrderCommandUnit.Money.cs`).

use dl_decimal::Decimal;
use dl_tds::ParamValue;

use super::sql::{bound, cs_consts, free, param};
use crate::commands::batches_money as b;
use crate::commands::params as p;
use crate::contracts::wire::{
    OrderCheckClosePayload, OrderTenderAddPayload, OrderTenderRemovePayload,
};

const CS_MONEY: &str =
    include_str!("../../../testdata/cs/Commands/OrderCommandSql.BatchesMoney.cs");

fn tender() -> OrderTenderAddPayload {
    OrderTenderAddPayload {
        rvc_no: 3,
        check_no: 7001,
        cashier: "clerk-7".into(),
        payment_type: " CASH ".into(),
        amount: Decimal::from(250_000),
        tendered: Decimal::from(300_000),
        reference: None,
        card_type: None,
        bill_no: Some("   ".into()),
        trn_code: None,
        trn_sub: None,
    }
}

#[test]
fn every_money_batch_is_the_csharp_text_byte_for_byte() {
    let cs = cs_consts(CS_MONEY);
    let rust: &[(&str, &str)] = &[
        ("TenderAdd", b::TENDER_ADD),
        ("TenderRemove", b::TENDER_REMOVE),
        ("CheckClose", b::CHECK_CLOSE),
    ];
    assert_eq!(cs.len(), rust.len(), "every C# batch has its Rust constant");
    for (name, text) in rust {
        assert_eq!(cs.get(*name).map(String::as_str), Some(*text), "{name}");
    }
}

#[test]
fn each_money_batch_is_bound_exactly_its_free_parameters() {
    let remove = OrderTenderRemovePayload {
        rvc_no: 3,
        check_no: 7001,
        trn_seq: 4,
        cashier: "clerk-7".into(),
    };
    let close = OrderCheckClosePayload {
        rvc_no: 3,
        check_no: 7001,
        cashier: "clerk-7".into(),
    };
    let cases = [
        ("tender", b::TENDER_ADD, p::tender_add_parameters(&tender())),
        (
            "untender",
            b::TENDER_REMOVE,
            p::tender_remove_parameters(&remove),
        ),
        ("close", b::CHECK_CLOSE, p::check_close_parameters(&close)),
    ];
    for (name, sql, params) in cases {
        let mut free = free(sql, true);
        if name == "untender" {
            // The C# unit binds @Cashier on the remove too; the batch does not read it.
            assert!(free.insert("cashier".into()));
        }
        assert_eq!(bound(&params), free, "{name}");
    }
}

#[test]
fn the_tender_binds_the_trimmed_type_an_empty_reference_and_no_blank_bill_number() {
    let params = p::tender_add_parameters(&tender());
    assert_eq!(
        param(&params, "PaymentType"),
        &ParamValue::String(Some("CASH".into()))
    );
    assert_eq!(
        param(&params, "Reference"),
        &ParamValue::String(Some(String::new()))
    );
    assert_eq!(param(&params, "BillNo"), &ParamValue::VarChar(None));
    assert_eq!(param(&params, "CardType"), &ParamValue::I32(None));
    assert_eq!(param(&params, "CfgTrnCode"), &ParamValue::I32(None));
    assert_eq!(param(&params, "CfgTrnSub"), &ParamValue::I32(None));

    let mut with_bill = tender();
    with_bill.bill_no = Some(" 0000123 ".into());
    with_bill.card_type = Some(2);
    with_bill.trn_code = Some(400);
    with_bill.trn_sub = Some(1);
    let params = p::tender_add_parameters(&with_bill);
    assert_eq!(param(&params, "CfgTrnCode"), &ParamValue::I32(Some(400)));
    assert_eq!(param(&params, "CfgTrnSub"), &ParamValue::I32(Some(1)));
    assert_eq!(
        param(&params, "BillNo"),
        &ParamValue::VarChar(Some("0000123".into()))
    );
    assert_eq!(param(&params, "CardType"), &ParamValue::I32(Some(2)));
}
