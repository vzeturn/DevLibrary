//! SMILE_FO bills: the FO branch of `FetchCollected` after the rows are scanned, with
//! `deriveFOLineVAT`, `apportionFODetailTax`, `recalcFOLineTax`, `SnapDetailToFolio`,
//! `CorrectDetailNetGross`, `eliminateFOVoidPairs` and `enrichFOPOSCharges`.

use dl_time::DateTime;

use crate::config::Rules;
use crate::domain::{
    CollectedFlags, CollectedInvoice, DetailItem, InvoiceHeader, InvoiceLine, LangText, LineOrigin,
    MergedLine, TaxBreakdown, VAT_STATUS_KCT, VAT_STATUS_TAXABLE,
};
use crate::log::{self, V};
use crate::money::{abs64, apportion, derive_legal_rate, parse_rate_label, recalc_tax_into_sc};
use crate::pos::PosError;

/// sp_FetchFO result set 1.
#[derive(Debug, Clone, Default, PartialEq)]
#[allow(missing_docs)] // fields mirror the Go struct one for one; see its doc
pub struct FoHeaderRow {
    pub seller_tax_code: String,
    pub company_name: String,
    pub customer_tax: String,
    pub customer_addr: String,
    pub customer_email: String,
    pub customer_tel: String,
    pub invoice_type: String,
    pub code_mode: String,
    pub ky_hieu: String,
    pub mau_so: String,
    pub inv_date: Option<DateTime>,
    pub subtotal: i64,
    pub service_charge: i64,
    pub tax_amount: i64,
    pub total_amount: i64,
    pub guest_name: String,
    pub address2: String,
    pub inv_content: String,
    pub confirm_no: String,
    pub arrival: String,
    pub departure: String,
    pub room_no: String,
}

/// sp_FetchFO result set 2.
#[derive(Debug, Clone, Default, PartialEq)]
#[allow(missing_docs)] // fields mirror the Go struct one for one; see its doc
pub struct FoLineRow {
    pub line_key: String,
    pub source_ref: String,
    pub trn_code: String,
    pub trn_sub_code: String,
    pub desc: String,
    pub unit: String,
    pub qty: f64,
    pub unit_price: i64,
    pub amount: i64,
    pub tax_code: String,
    pub tax_amount: i64,
    pub stax_code: String,
    pub stax_amount: i64,
    pub service_charge_code: String,
    pub service_charge_amount: i64,
    pub rvc: String,
    pub outlet: String,
    pub folio_link: String,
    pub origin_folio_number: String,
    pub trn_type: String,
    pub raw_discount: i64,
    pub trn_group: String,
    pub is_hk_post: bool,
    pub tax_rate: String,
    pub origin_room: String,
    pub room_code: String,
    pub transaction_date: String,
    pub post_to_acc: bool,
    pub number_cover: i64,
    pub xref: String,
    pub origin_currency: String,
    pub origin_amount: i64,
    pub rpt_ex_rate: String,
}

/// sp_FetchFO result set 3.
#[derive(Debug, Clone, Default, PartialEq)]
#[allow(missing_docs)] // fields mirror the Go struct one for one; see its doc
pub struct FoDetailRow {
    pub parent_line_key: String,
    pub line_key: String,
    pub icode: String,
    pub name: String,
    pub group: String,
    pub unit: String,
    pub qty: f64,
    pub unit_price: i64,
    pub amount: i64,
    pub tax_code: String,
    pub tax_amount: i64,
    pub stax_code: String,
    pub stax_amount: i64,
    pub service_charge_code: String,
    pub service_charge_amount: i64,
}

/// The rows of one sp_FetchFO call.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FoRows {
    /// The first result set: one row per bill.
    pub header: Vec<FoHeaderRow>,
    /// The second result set: the printed lines.
    pub lines: Vec<FoLineRow>,
    /// The third result set: the postings behind the lines.
    pub detail: Vec<FoDetailRow>,
}

/// `ServiceChargeRateTolerance`.
const SC_RATE_TOLERANCE: f64 = 0.006;

fn texts(s: &str) -> Vec<LangText> {
    if s.is_empty() {
        Vec::new()
    } else {
        vec![LangText {
            lang: "vi".into(),
            text: s.to_owned(),
        }]
    }
}

/// `deriveFOLineVAT`.
pub fn derive_line_vat(
    t: &mut TaxBreakdown,
    sub_amount: i64,
    trn_code: &str,
    rules: &Rules,
) -> Result<(), String> {
    if rules.is_exempt_code(trn_code) {
        t.vat_status = VAT_STATUS_KCT.into();
        t.tax_code = "0".into();
        return Ok(());
    }
    let base = sub_amount + t.service_charge_amount;
    let rate = derive_legal_rate(base, t.tax_amount)?;
    t.vat_status = VAT_STATUS_TAXABLE.into();
    t.tax_code = rate.to_string();
    Ok(())
}

/// `recalcFOLineTax`.
fn recalc_line_tax(ln: &mut InvoiceLine, rules: &Rules) -> Result<(), String> {
    if !rules.recalc_tax_into_sc || rules.is_exempt_code(&ln.merged.trn_code) {
        return Ok(());
    }
    let Some(rate) = parse_rate_label(&ln.merged.tax_rate) else {
        return Ok(());
    };
    let (t, s) = recalc_tax_into_sc(
        ln.merged.amount,
        ln.merged.tax.tax_amount,
        ln.merged.tax.service_charge_amount,
        rate,
    )
    .map_err(|e| format!("line {} recalc: {e}", ln.merged.line_key))?;
    ln.merged.tax.tax_amount = t;
    ln.merged.tax.service_charge_amount = s;
    for d in &mut ln.detail {
        d.tax.tax_amount = 0;
        d.tax.service_charge_amount = 0;
    }
    Ok(())
}

/// `CorrectDetailNetGross`.
fn correct_net_gross(d: &mut DetailItem, rate: f64) {
    if rate <= 0.0 || d.amount <= 0 || d.tax.service_charge_amount <= 0 {
        return;
    }
    let expected = d.amount as f64 * rate;
    let actual = d.tax.service_charge_amount as f64;
    if actual >= expected * (1.0 - SC_RATE_TOLERANCE) {
        return;
    }
    let deduction = d.tax.tax_amount + d.tax.stax_amount + d.tax.service_charge_amount;
    if deduction <= 0 || deduction >= d.amount {
        return;
    }
    d.amount -= deduction;
    if d.qty > 0.0 {
        d.unit_price -= (deduction as f64 / d.qty + 0.5) as i64;
    } else {
        d.unit_price = d.amount;
    }
}

type Bucket = fn(&mut DetailItem) -> &mut i64;

/// `SnapDetailToFolio`.
fn snap_detail_to_folio(ln: &mut InvoiceLine) {
    let n = ln.detail.len();
    if n == 0 {
        return;
    }
    let weights: Vec<i64> = ln.detail.iter().map(|d| abs64(d.amount)).collect();
    let buckets: [(i64, Bucket); 4] = [
        (ln.merged.amount, |d| &mut d.amount),
        (ln.merged.tax.tax_amount, |d| &mut d.tax.tax_amount),
        (ln.merged.tax.stax_amount, |d| &mut d.tax.stax_amount),
        (ln.merged.tax.service_charge_amount, |d| {
            &mut d.tax.service_charge_amount
        }),
    ];
    for (merged, get) in buckets {
        let sum: i64 = ln.detail.iter_mut().map(|d| *get(d)).sum();
        if sum == 0 || sum == merged {
            continue;
        }
        let gap = merged - sum;
        if gap > n as i64 || gap < -(n as i64) {
            continue;
        }
        let Ok(shares) = apportion(merged, &weights) else {
            continue;
        };
        for (d, s) in ln.detail.iter_mut().zip(shares) {
            *get(d) = s;
        }
    }
}

/// `apportionFODetailTax`.
fn apportion_detail_tax(lines: &mut [InvoiceLine], rules: &Rules) -> Result<(), String> {
    for ln in lines.iter_mut() {
        recalc_line_tax(ln, rules)?;
        if ln.detail.is_empty() {
            continue;
        }
        for d in &mut ln.detail {
            correct_net_gross(d, rules.standard_service_charge_rate);
        }
        snap_detail_to_folio(ln);
        let weights: Vec<i64> = ln.detail.iter().map(|d| abs64(d.amount)).collect();
        let m = ln.merged.clone();
        type Get = fn(&mut TaxBreakdown) -> &mut i64;
        let buckets: [(&str, i64, Get, Option<String>, bool); 3] = [
            ("tax", m.tax.tax_amount, |t| &mut t.tax_amount, None, false),
            (
                "stax",
                m.tax.stax_amount,
                |t| &mut t.stax_amount,
                Some(m.tax.stax_code.clone()),
                true,
            ),
            (
                "service_charge",
                m.tax.service_charge_amount,
                |t| &mut t.service_charge_amount,
                Some(m.tax.service_charge_code.clone()),
                false,
            ),
        ];
        for (name, merged, get, code, is_stax) in buckets {
            let sum: i64 = ln.detail.iter_mut().map(|d| *get(&mut d.tax)).sum();
            if sum != 0 || merged == 0 {
                continue;
            }
            let shares = apportion(merged, &weights)
                .map_err(|e| format!("line {} {name}: {e}", m.line_key))?;
            for (d, s) in ln.detail.iter_mut().zip(shares) {
                *get(&mut d.tax) = s;
                if let Some(c) = &code {
                    if is_stax {
                        d.tax.stax_code = c.clone();
                    } else {
                        d.tax.service_charge_code = c.clone();
                    }
                }
            }
        }
        for d in &mut ln.detail {
            let amount = d.amount;
            derive_line_vat(&mut d.tax, amount, &m.trn_code, rules)
                .map_err(|e| format!("line {} item {}: {e}", m.line_key, d.line_key))?;
        }
    }
    Ok(())
}

/// `eliminateFOVoidPairs`: drops ± mirror pairs; returns whether any was dropped.
fn eliminate_void_pairs(lines: Vec<InvoiceLine>) -> (Vec<InvoiceLine>, bool) {
    let mut dropped = vec![false; lines.len()];
    let mut eliminated = false;
    for i in 0..lines.len() {
        if dropped[i] {
            continue;
        }
        let a = &lines[i].merged;
        for j in i + 1..lines.len() {
            if dropped[j] {
                continue;
            }
            let b = &lines[j].merged;
            if a.trn_code == b.trn_code
                && a.trn_sub_code == b.trn_sub_code
                && a.amount == b.amount.wrapping_neg()
                && a.amount != 0
                && a.tax.tax_amount == b.tax.tax_amount.wrapping_neg()
                && a.tax.stax_amount == b.tax.stax_amount.wrapping_neg()
                && a.tax.service_charge_amount == b.tax.service_charge_amount.wrapping_neg()
            {
                dropped[i] = true;
                dropped[j] = true;
                eliminated = true;
                log::info(
                    "agent: fo void pair eliminated",
                    &[
                        ("line", V::S(&a.line_key)),
                        ("mirror", V::S(&b.line_key)),
                        ("trn_code", V::S(&a.trn_code)),
                        ("amount", V::I(a.amount)),
                    ],
                );
                break;
            }
        }
    }
    let out = lines
        .into_iter()
        .zip(dropped)
        .filter(|(_, d)| !d)
        .map(|(l, _)| l)
        .collect();
    (out, eliminated)
}

/// Resolves and normalises one POS check for the enrich step.
pub type CheckFetcher<'a> = dyn FnMut(&str) -> Result<CollectedInvoice, PosError> + 'a;

/// `enrichFOPOSCharges`: swaps each POS-charge group for the items of its check when
/// every gate passes. Returns whether any group was swapped.
fn enrich_pos_charges(
    lines: Vec<InvoiceLine>,
    fetch: &mut CheckFetcher<'_>,
) -> (Vec<InvoiceLine>, bool) {
    let mut order: Vec<String> = Vec::new();
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, ln) in lines.iter().enumerate() {
        let r = &ln.merged.source_ref;
        if r.is_empty() {
            continue;
        }
        match groups.iter_mut().find(|(k, _)| k == r) {
            Some((_, v)) => v.push(i),
            None => {
                order.push(r.clone());
                groups.push((r.clone(), vec![i]));
            }
        }
    }
    if groups.is_empty() {
        return (lines, false);
    }
    let mut replaced: Vec<(String, Vec<InvoiceLine>)> = Vec::new();
    for check_no in &order {
        let idxs = &groups
            .iter()
            .find(|(k, _)| k == check_no)
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        let (mut fo_amt, mut fo_tax, mut fo_sc) = (0i64, 0i64, 0i64);
        for &i in idxs {
            fo_amt += lines[i].merged.amount;
            fo_tax += lines[i].merged.tax.tax_amount;
            fo_sc += lines[i].merged.tax.service_charge_amount;
        }
        let Ok(pos) = fetch(check_no) else { continue };
        let h = &pos.header;
        if h.folio_num <= 0 {
            log::warn(
                "agent: fo pos-charge enrich skipped (not room-charge payment mode)",
                &[
                    ("check_no", V::S(check_no)),
                    ("folio_num", V::I(h.folio_num)),
                ],
            );
            continue;
        }
        let bad = [
            ("net", fo_amt, h.subtotal - h.service_charge),
            ("tax", fo_tax, h.tax_amount),
            ("service_charge", fo_sc, h.service_charge),
        ]
        .into_iter()
        .find(|(_, got, want)| !(-1..=1).contains(&(got - want)));
        if let Some((bucket, got, want)) = bad {
            log::warn(
                "agent: fo pos-charge enrich skipped (reconcile failed)",
                &[
                    ("check_no", V::S(check_no)),
                    ("bucket", V::S(bucket)),
                    ("fo", V::I(got)),
                    ("pos", V::I(want)),
                ],
            );
            continue;
        }
        let first = &lines[idxs[0]];
        let mut repl = Vec::with_capacity(pos.lines.len());
        for pl in &pos.lines {
            let mut ln = pl.clone();
            ln.merged.trn_code = first.merged.trn_code.clone();
            ln.merged.trn_sub_code = first.merged.trn_sub_code.clone();
            ln.merged.line_key = format!("P{check_no}-{}", pl.merged.line_key);
            ln.merged.source_ref = check_no.clone();
            for (d, pd) in ln.detail.iter_mut().zip(&pl.detail) {
                d.line_key = format!("P{check_no}-{}", pd.line_key);
            }
            let mut org = LineOrigin {
                src_code: "FO".into(),
                pos_check_no: check_no.clone(),
                ..Default::default()
            };
            if let Some(po) = &pl.origin {
                org.rvc = po.rvc.clone();
                org.outlet = po.outlet.clone();
            }
            if let Some(fo) = &first.origin {
                org.folio_link = fo.folio_link.clone();
                // Keep the folio the charge posted to (printed as FolioNo).
                org.origin_folio_number = fo.origin_folio_number.clone();
                org.number_cover = fo.number_cover;
            }
            ln.origin = Some(org);
            repl.push(ln);
        }
        log::info(
            "agent: fo pos-charge enriched",
            &[
                ("check_no", V::S(check_no)),
                ("fo_lines", V::I(idxs.len() as i64)),
                ("item_lines", V::I(repl.len() as i64)),
                ("net", V::I(fo_amt)),
            ],
        );
        replaced.push((check_no.clone(), repl));
    }
    if replaced.is_empty() {
        return (lines, false);
    }
    let mut out = Vec::with_capacity(lines.len());
    let mut spliced: Vec<String> = Vec::new();
    for ln in lines {
        let r = ln.merged.source_ref.clone();
        if let Some((_, repl)) = replaced.iter().find(|(k, _)| *k == r) {
            if !spliced.contains(&r) {
                out.extend(repl.iter().cloned());
                spliced.push(r);
            }
            continue;
        }
        out.push(ln);
    }
    (out, true)
}

/// The FO branch of `FetchCollected`, from scanned rows to the collected invoice.
pub fn collect(
    id: &str,
    rows: &FoRows,
    rules: &Rules,
    fetch: &mut CheckFetcher<'_>,
) -> Result<CollectedInvoice, String> {
    let source_ref = format!("FO:{id}");
    let mut inv = CollectedInvoice {
        source: "FO".into(),
        source_ref: source_ref.clone(),
        vat_status: "printed".into(),
        ..Default::default()
    };
    for r in &rows.header {
        inv.header = InvoiceHeader {
            seller_tax_code: r.seller_tax_code.clone(),
            company_name: r.company_name.clone(),
            customer_tax: r.customer_tax.clone(),
            customer_addr: r.customer_addr.clone(),
            customer_email: r.customer_email.clone(),
            customer_tel: r.customer_tel.clone(),
            invoice_type: r.invoice_type.clone(),
            code_mode: r.code_mode.clone(),
            ky_hieu: r.ky_hieu.clone(),
            mau_so: r.mau_so.clone(),
            inv_date: r.inv_date,
            subtotal: r.subtotal,
            service_charge: r.service_charge,
            tax_amount: r.tax_amount,
            total_amount: r.total_amount,
            guest_name: r.guest_name.clone(),
            address2: r.address2.clone(),
            inv_content: r.inv_content.clone(),
            confirm_no: r.confirm_no.clone(),
            arrival: (!r.arrival.is_empty()).then(|| r.arrival.clone()),
            departure: (!r.departure.is_empty()).then(|| r.departure.clone()),
            room_no: r.room_no.clone(),
            ..Default::default()
        };
    }
    let mut keys: Vec<(String, usize)> = Vec::new();
    for r in &rows.lines {
        let mut ml = MergedLine {
            line_key: r.line_key.clone(),
            source_ref: r.source_ref.clone(),
            trn_code: r.trn_code.clone(),
            trn_sub_code: r.trn_sub_code.clone(),
            unit: r.unit.clone(),
            qty: r.qty,
            unit_price: r.unit_price,
            amount: r.amount,
            tax: TaxBreakdown {
                tax_code: r.tax_code.clone(),
                tax_amount: r.tax_amount,
                stax_code: r.stax_code.clone(),
                stax_amount: r.stax_amount,
                service_charge_code: r.service_charge_code.clone(),
                service_charge_amount: r.service_charge_amount,
                ..Default::default()
            },
            trn_group: r.trn_group.clone(),
            is_hk_post: r.is_hk_post,
            tax_rate: r.tax_rate.clone(),
            discount_amount: abs64(r.raw_discount),
            desc_defaults: texts(&r.desc),
        };
        let org = LineOrigin {
            rvc: r.rvc.clone(),
            outlet: r.outlet.clone(),
            folio_link: r.folio_link.clone(),
            origin_folio_number: r.origin_folio_number.clone(),
            trn_type: r.trn_type.clone(),
            origin_room: r.origin_room.clone(),
            room_code: r.room_code.clone(),
            transaction_date: r.transaction_date.clone(),
            post_to_acc: r.post_to_acc,
            number_cover: r.number_cover,
            xref: r.xref.clone(),
            origin_currency: r.origin_currency.clone(),
            origin_amount: r.origin_amount,
            rpt_ex_rate: r.rpt_ex_rate.clone(),
            src_code: "FO".into(),
            bill_id: id.to_owned(),
            ..Default::default()
        };
        let amount = ml.amount;
        let code = ml.trn_code.clone();
        derive_line_vat(&mut ml.tax, amount, &code, rules).map_err(|e| {
            format!(
                "agent: derive fo vat ({source_ref}) line {}: {e}",
                r.line_key
            )
        })?;
        // Go keeps the LAST index for a repeated key (map assignment).
        match keys.iter_mut().find(|(k, _)| *k == r.line_key) {
            Some((_, idx)) => *idx = inv.lines.len(),
            None => keys.push((r.line_key.clone(), inv.lines.len())),
        }
        inv.lines.push(InvoiceLine {
            merged: ml,
            detail: Vec::new(),
            origin: Some(org),
        });
    }
    for r in &rows.detail {
        let d = DetailItem {
            line_key: r.line_key.clone(),
            icode: r.icode.clone(),
            group: r.group.clone(),
            unit: r.unit.clone(),
            qty: r.qty,
            unit_price: r.unit_price,
            amount: r.amount,
            tax: TaxBreakdown {
                tax_code: r.tax_code.clone(),
                tax_amount: r.tax_amount,
                stax_code: r.stax_code.clone(),
                stax_amount: r.stax_amount,
                service_charge_code: r.service_charge_code.clone(),
                service_charge_amount: r.service_charge_amount,
                ..Default::default()
            },
            name_defaults: texts(&r.name),
            ..Default::default()
        };
        // An orphaned item (no parent line) is dropped; the caller logs it.
        if let Some((_, idx)) = keys.iter().find(|(k, _)| *k == r.parent_line_key) {
            inv.lines[*idx].detail.push(d);
        }
    }
    apportion_detail_tax(&mut inv.lines, rules)
        .map_err(|e| format!("agent: apportion fo detail tax ({source_ref}): {e}"))?;
    let (lines, had_hk) = eliminate_void_pairs(std::mem::take(&mut inv.lines));
    if had_hk {
        inv.flags = Some(CollectedFlags {
            has_hk_line: true,
            has_pos_charge: false,
        });
    }
    let (lines, had_pos) = enrich_pos_charges(lines, fetch);
    inv.lines = lines;
    if had_pos {
        inv.flags
            .get_or_insert_with(CollectedFlags::default)
            .has_pos_charge = true;
    }
    inv.header.total_discount = inv.lines.iter().map(|l| l.merged.discount_amount).sum();
    crate::tcvn3::convert_invoice(&mut inv);
    Ok(inv)
}
