//! The Rust pipeline must turn the same SQL rows into the same PushItem bytes as the Go
//! agent. Vectors: `testdata/golden.json`, written by
//! `internal/agentcollector/golden_rust_test.go`. `MMEI_GOLDEN=<path>` checks another
//! (for example a larger) vector file.

use dl_agent_einvoice::config::Rules;
use dl_agent_einvoice::fo::{FoDetailRow, FoHeaderRow, FoLineRow, FoRows};
use dl_agent_einvoice::pos::{PosError, PosRawCheck, PosRawHeader, PosRawLine};
use dl_json::Value;
use dl_time::DateTime;

fn s(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}
fn i(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(Value::as_i64).unwrap_or(0)
}
fn f(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(0.0)
}
fn b(v: &Value, k: &str) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(false)
}
fn t(v: &Value, k: &str) -> Option<DateTime> {
    let txt = v.get(k)?.as_str()?;
    let naive = txt.strip_suffix('Z').unwrap_or(txt);
    Some(
        naive
            .parse()
            .unwrap_or_else(|e| panic!("time {txt}: {e:?}")),
    )
}
fn arr<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v.get(k)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn pos_check(v: &Value) -> PosRawCheck {
    let h = v.get("Header").expect("Header");
    PosRawCheck {
        check_no: s(v, "CheckNo"),
        header: PosRawHeader {
            sub_total: i(h, "SubTotal"),
            discount: i(h, "Discount"),
            sur_charge: i(h, "SurCharge"),
            before_tax: i(h, "BeforeTax"),
            tax_total: i(h, "TaxTotal"),
            stax_total: i(h, "STaxTotal"),
            total_base: i(h, "TotalBase"),
            inv_status: i(h, "InvStatus"),
            tax_omit: i(h, "TaxOmit"),
            rvc: i(h, "RVC"),
            folio_num: s(h, "FolioNum"),
            inv_date: t(h, "InvDate"),
            ex_rate: f(h, "ExRate"),
            client_vat: s(h, "ClientVAT"),
            client_name: s(h, "ClientName"),
            company: s(h, "Company"),
            address: s(h, "Address"),
            inv_content: s(h, "InvContent"),
        },
        lines: arr(v, "Lines")
            .iter()
            .map(|l| PosRawLine {
                trn_seq: i(l, "TrnSeq"),
                trn_code: i(l, "TrnCode"),
                item_code: s(l, "ItemCode"),
                trn_desc: s(l, "TrnDesc"),
                unit_name: s(l, "UnitName"),
                qty: f(l, "Qty"),
                unit_price: i(l, "UnitPrice"),
                base_sub: i(l, "BaseSub"),
                base_src: i(l, "BaseSrc"),
                base_tax: i(l, "BaseTax"),
                tax_no: i(l, "TaxNo"),
                base_stax: i(l, "BaseSTax"),
                stax_no: i(l, "STaxNo"),
                rvc: i(l, "RVC"),
            })
            .collect(),
    }
}

fn fo_rows(c: &Value) -> FoRows {
    FoRows {
        header: arr(c, "fo_header")
            .iter()
            .map(|h| FoHeaderRow {
                seller_tax_code: s(h, "SellerTaxCode"),
                company_name: s(h, "CompanyName"),
                customer_tax: s(h, "CustomerTax"),
                customer_addr: s(h, "CustomerAddr"),
                customer_email: s(h, "CustomerEmail"),
                customer_tel: s(h, "CustomerTel"),
                invoice_type: s(h, "InvoiceType"),
                code_mode: s(h, "CodeMode"),
                ky_hieu: s(h, "KyHieu"),
                mau_so: s(h, "MauSo"),
                inv_date: t(h, "InvDate"),
                subtotal: i(h, "Subtotal"),
                service_charge: i(h, "ServiceCharge"),
                tax_amount: i(h, "TaxAmount"),
                total_amount: i(h, "TotalAmount"),
                guest_name: s(h, "GuestName"),
                address2: s(h, "Address2"),
                inv_content: s(h, "InvContent"),
                confirm_no: s(h, "ConfirmNo"),
                arrival: s(h, "Arrival"),
                departure: s(h, "Departure"),
                room_no: s(h, "RoomNo"),
            })
            .collect(),
        lines: arr(c, "fo_lines")
            .iter()
            .map(|l| FoLineRow {
                line_key: s(l, "LineKey"),
                source_ref: s(l, "SourceRef"),
                trn_code: s(l, "TrnCode"),
                trn_sub_code: s(l, "TrnSubCode"),
                desc: s(l, "Desc"),
                unit: s(l, "Unit"),
                qty: f(l, "Qty"),
                unit_price: i(l, "UnitPrice"),
                amount: i(l, "Amount"),
                tax_code: s(l, "TaxCode"),
                tax_amount: i(l, "TaxAmount"),
                stax_code: s(l, "STaxCode"),
                stax_amount: i(l, "STaxAmount"),
                service_charge_code: s(l, "ServiceChargeCode"),
                service_charge_amount: i(l, "ServiceChargeAmount"),
                rvc: s(l, "RVC"),
                outlet: s(l, "Outlet"),
                folio_link: s(l, "FolioLink"),
                origin_folio_number: s(l, "OriginFolioNumber"),
                trn_type: s(l, "TrnType"),
                raw_discount: i(l, "RawDiscount"),
                trn_group: s(l, "TrnGroup"),
                is_hk_post: b(l, "IsHKPost"),
                tax_rate: s(l, "TaxRate"),
                origin_room: s(l, "OriginRoom"),
                room_code: s(l, "RoomCode"),
                transaction_date: s(l, "TransactionDate"),
                post_to_acc: b(l, "PostToACC"),
                number_cover: i(l, "NumberCover"),
                xref: s(l, "XRef"),
                origin_currency: s(l, "OriginCurrency"),
                origin_amount: i(l, "OriginAmount"),
                rpt_ex_rate: s(l, "RptExRate"),
            })
            .collect(),
        detail: arr(c, "fo_detail")
            .iter()
            .map(|d| FoDetailRow {
                parent_line_key: s(d, "ParentLineKey"),
                line_key: s(d, "LineKey"),
                icode: s(d, "ICode"),
                name: s(d, "Name"),
                group: s(d, "Group"),
                unit: s(d, "Unit"),
                qty: f(d, "Qty"),
                unit_price: i(d, "UnitPrice"),
                amount: i(d, "Amount"),
                tax_code: s(d, "TaxCode"),
                tax_amount: i(d, "TaxAmount"),
                stax_code: s(d, "STaxCode"),
                stax_amount: i(d, "STaxAmount"),
                service_charge_code: s(d, "ServiceChargeCode"),
                service_charge_amount: i(d, "ServiceChargeAmount"),
            })
            .collect(),
    }
}

fn rules(c: &Value) -> Rules {
    let cfg = c.get("cfg").expect("cfg");
    Rules {
        recalc_tax_into_sc: b(cfg, "recalc_tax_into_sc"),
        fo_exempt_codes: arr(cfg, "fo_exempt_codes")
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        standard_service_charge_rate: f(cfg, "standard_service_charge_rate"),
    }
}

#[test]
fn push_items_match_go_byte_for_byte() {
    let path = std::env::var("MMEI_GOLDEN").unwrap_or_else(|_| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/golden.json").to_owned()
    });
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let doc = dl_json::parse(&text).expect("golden json");
    let cases = arr(&doc, "cases");
    assert!(!cases.is_empty());
    let (mut ok, mut err, mut bad) = (0, 0, Vec::new());
    for c in cases {
        let name = s(c, "name");
        let rules = rules(c);
        let result = match s(c, "kind").as_str() {
            "pos" => dl_agent_einvoice::collect_pos(&pos_check(c.get("pos").expect("pos")), &rules),
            "fo" => {
                let checks: Vec<(String, PosRawCheck)> = c
                    .get("checks")
                    .and_then(Value::as_object)
                    .map(|m| {
                        m.iter()
                            .filter(|(_, v)| !v.is_null())
                            .map(|(k, v)| (k.to_owned(), pos_check(v)))
                            .collect()
                    })
                    .unwrap_or_default();
                let mut fetch = |no: &str| -> Result<_, PosError> {
                    match checks.iter().find(|(k, _)| k == no) {
                        Some((_, raw)) => dl_agent_einvoice::enrich_check(raw, &rules),
                        None => Err(PosError::Invalid("no such check".into())),
                    }
                };
                dl_agent_einvoice::fo::collect(&s(c, "id"), &fo_rows(c), &rules, &mut fetch)
            }
            k => panic!("{name}: kind {k}"),
        }
        .and_then(dl_agent_einvoice::push_item);
        let want_ok = b(c, "ok");
        match (result, want_ok) {
            (Ok(item), true) => {
                // The exact bytes Go's json.Marshal(PushItem) produced.
                let want = s(c, "push_item");
                let got = item.to_json();
                if got != want {
                    let at = got
                        .bytes()
                        .zip(want.bytes())
                        .position(|(a, b)| a != b)
                        .unwrap_or(got.len().min(want.len()));
                    let from = at.saturating_sub(60);
                    bad.push(format!(
                        "{name}: bytes differ at {at}\n  got  …{}\n  want …{}",
                        got.get(from..(at + 60).min(got.len())).unwrap_or(""),
                        want.get(from..(at + 60).min(want.len())).unwrap_or("")
                    ));
                    continue;
                }
                ok += 1;
            }
            (Err(_), false) => err += 1,
            (Ok(_), false) => bad.push(format!(
                "{name}: Rust succeeded, Go failed: {}",
                s(c, "error")
            )),
            (Err(e), true) => bad.push(format!("{name}: Rust failed ({e}), Go succeeded")),
        }
    }
    eprintln!(
        "golden: {ok} identical push items, {err} matching errors, {} mismatches",
        bad.len()
    );
    assert!(
        bad.is_empty(),
        "{} of {} cases differ:\n{}",
        bad.len(),
        cases.len(),
        bad[..bad.len().min(5)].join("\n")
    );
}
