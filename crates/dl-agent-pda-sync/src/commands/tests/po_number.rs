//! `order.po_number` store SQL (RT-1413, `TableCommandSqlTests` po-number cases): the agent's
//! batch is the C# `OrderCommandSql.SetPoNumber` byte for byte, it is bound exactly its free
//! parameters, and it writes `TmpInvoice.PONumber` and nothing else of the check.

use dl_tds::ParamValue;

use super::sql::{bound, cs_consts, free, has_word};
use crate::commands::params as p;
use crate::commands::schema_columns;
use crate::commands::sql_text as q;
use crate::contracts::wire::{OrderPoNumberPayload, table_command_types};

const CS_PO_NUMBER: &str =
    include_str!("../../../testdata/cs/Commands/OrderCommandSql.PoNumber.cs");

fn payload(text: &str) -> OrderPoNumberPayload {
    OrderPoNumberPayload {
        rvc_no: 3,
        check_no: 7001,
        po_number: Some(text.to_owned()),
    }
}

#[test]
fn the_po_number_batch_is_the_csharp_text_byte_for_byte() {
    let cs = cs_consts(CS_PO_NUMBER);
    assert_eq!(
        cs.get("SetPoNumber").map(String::as_str),
        Some(q::SET_PO_NUMBER)
    );
}

#[test]
fn the_po_number_batch_is_bound_exactly_its_free_parameters_with_unicode_text() {
    let params = p::po_number_parameters(&payload("101-Nguyễn Văn An"));
    assert_eq!(bound(&params), free(q::SET_PO_NUMBER, true));
    let value = |name: &str| params.iter().find(|p| p.name() == name).unwrap().value();
    assert_eq!(value("CheckNo"), &ParamValue::I64(Some(7001)));
    assert_eq!(
        value("PONumber"),
        &ParamValue::String(Some("101-Nguyễn Văn An".into()))
    );
}

#[test]
fn the_po_number_batch_writes_only_the_column_encoded_and_cut_to_its_length() {
    let sql = q::SET_PO_NUMBER;
    assert_eq!(sql.matches("UPDATE").count(), 1);
    assert!(sql.contains("SET PONumber = @PoText"));
    assert!(sql.contains("dbo.Unicode2TCVN(NULLIF(LTRIM(RTRIM(@PONumber)), N''))"));
    assert!(sql.contains("N'PONumber', 'CharMaxLen'"));
    assert!(sql.contains("WHERE CheckNo = @CheckNo AND RVC = @RVCNo;"));
    assert!(!has_word(
        sql,
        &["TableNo", "NGuest", "INSERT", "DELETE", "EXEC", "MERGE"]
    ));
}

#[test]
fn the_po_number_verb_probes_the_header_columns_it_touches() {
    let cols: Vec<String> = schema_columns(table_command_types::PO_NUMBER)
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        cols,
        [
            "tmpInvoice.PONumber",
            "tmpInvoice.CheckNo",
            "tmpInvoice.RVC"
        ]
    );
}
