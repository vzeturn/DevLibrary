//! SMILE_POS checks: a port of `normalizePOS` (pos_normalize.go) and
//! `recalcPOSLineTax` (recalc_tax.go).

use dl_time::DateTime;

use crate::domain::{
    CollectedInvoice, DetailItem, InvoiceHeader, InvoiceLine, LangText, LineOrigin, MergedLine,
    TaxBreakdown, VAT_STATUS_KCT, VAT_STATUS_TAXABLE,
};
use crate::money::{LEGAL_RATES, abs64, apportion, atoi, derive_legal_rate, recalc_tax_into_sc};

/// sp_FetchPOS v2 result set 1.
#[derive(Debug, Clone, Default, PartialEq)]
#[allow(missing_docs)] // fields mirror the Go struct one for one; see its doc
pub struct PosRawHeader {
    pub sub_total: i64,
    pub discount: i64,
    pub sur_charge: i64,
    pub before_tax: i64,
    pub tax_total: i64,
    pub stax_total: i64,
    pub total_base: i64,
    pub inv_status: i64,
    pub tax_omit: i64,
    pub rvc: i64,
    pub folio_num: String,
    pub inv_date: Option<DateTime>,
    pub ex_rate: f64,
    pub client_vat: String,
    pub client_name: String,
    pub company: String,
    pub address: String,
    pub inv_content: String,
}

/// sp_FetchPOS v2 result set 2 (one dbo.Trn row).
#[derive(Debug, Clone, Default, PartialEq)]
#[allow(missing_docs)] // fields mirror the Go struct one for one; see its doc
pub struct PosRawLine {
    pub trn_seq: i64,
    pub trn_code: i64,
    pub item_code: String,
    pub trn_desc: String,
    pub unit_name: String,
    pub qty: f64,
    pub unit_price: i64,
    pub base_sub: i64,
    pub base_src: i64,
    pub base_tax: i64,
    pub tax_no: i64,
    pub base_stax: i64,
    pub stax_no: i64,
    pub rvc: i64,
}

/// The whole sp_FetchPOS result.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PosRawCheck {
    /// The check number asked for.
    pub check_no: String,
    /// The header result set.
    pub header: PosRawHeader,
    /// The line result set.
    pub lines: Vec<PosRawLine>,
}

/// Why normalisation failed.
#[derive(Debug, Clone, PartialEq)]
pub enum PosError {
    /// The check number resolved to no POS check (normal in the FO enrich path).
    NotPosCheck,
    /// Anything else; the bill is held.
    Invalid(String),
}

impl std::fmt::Display for PosError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PosError::NotPosCheck => f.write_str("not a POS check"),
            PosError::Invalid(m) => f.write_str(m),
        }
    }
}

/// "0" means "not charged to a folio" (see `normalizeFolioNum`).
fn normalize_folio_num(v: &str) -> String {
    if v == "0" {
        String::new()
    } else {
        v.to_owned()
    }
}

/// `parseFolioNum`: the numeric room-charge signal fo enrichment gates on.
/// "" or anything that is not a whole number is 0; an all-zero fraction
/// ("147105.00", a scaled numeric rendered by SQL Server) is accepted.
fn parse_folio_num(folio: &str) -> i64 {
    let s = folio.trim();
    let s = match s.split_once('.') {
        Some((whole, frac)) => {
            if !frac.trim_matches('0').is_empty() {
                return 0;
            }
            whole
        }
        None => s,
    };
    match s.parse::<i64>() {
        Ok(n) if n >= 0 => n,
        _ => 0,
    }
}

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

/// `normalizePOS`.
pub fn normalize(raw: &PosRawCheck) -> Result<CollectedInvoice, PosError> {
    let h = &raw.header;
    let fail = |m: String| PosError::Invalid(format!("agent: pos {}: {m}", raw.check_no));

    let mut revenue = Vec::new();
    let mut discounts = Vec::new();
    for ln in &raw.lines {
        match ln.trn_code {
            100 => revenue.push(ln),
            110 => discounts.push(ln),
            c => {
                return Err(fail(format!(
                    "unexpected TrnCode {c} on line {} (expected 100|110)",
                    ln.trn_seq
                )));
            }
        }
    }
    if revenue.is_empty() {
        if !discounts.is_empty() {
            return Err(fail(format!(
                "{} discount row(s) but no revenue rows to apportion onto",
                discounts.len()
            )));
        }
        return Err(PosError::NotPosCheck);
    }
    let n = revenue.len();
    let mut rates = Vec::with_capacity(n);
    for ln in &revenue {
        let r = derive_legal_rate(ln.base_sub, ln.base_tax)
            .map_err(|e| fail(format!("line {} ({}): {e}", ln.trn_seq, ln.item_code)))?;
        rates.push(r);
    }
    let net_w: Vec<i64> = revenue.iter().map(|l| abs64(l.base_sub)).collect();
    let tax_w: Vec<i64> = revenue.iter().map(|l| abs64(l.base_tax)).collect();
    let src_w: Vec<i64> = revenue.iter().map(|l| abs64(l.base_src)).collect();
    let mut disc_net = vec![0i64; n];
    let mut disc_tax = vec![0i64; n];
    let mut disc_src = vec![0i64; n];
    for d in &discounts {
        let ns = apportion(d.base_sub, &net_w)
            .map_err(|e| fail(format!("discount row {} net apportionment: {e}", d.trn_seq)))?;
        let ts = apportion(d.base_tax, &tax_w)
            .map_err(|e| fail(format!("discount row {} tax apportionment: {e}", d.trn_seq)))?;
        let ss = apportion(d.base_src, &src_w).map_err(|e| {
            fail(format!(
                "discount row {} service-charge apportionment: {e}",
                d.trn_seq
            ))
        })?;
        for i in 0..n {
            disc_net[i] += ns[i];
            disc_tax[i] += ts[i];
            disc_src[i] += ss[i];
        }
    }
    let sc: Vec<i64> = revenue
        .iter()
        .enumerate()
        .map(|(i, l)| l.base_src + disc_src[i])
        .collect();
    let vat_status = if h.tax_omit == 1 {
        VAT_STATUS_KCT
    } else {
        VAT_STATUS_TAXABLE
    };
    let folio = normalize_folio_num(&h.folio_num);
    let mut lines = Vec::with_capacity(n);
    for (i, ln) in revenue.iter().enumerate() {
        let mut ml = MergedLine {
            line_key: ln.trn_seq.to_string(),
            source_ref: raw.check_no.clone(),
            unit: ln.unit_name.clone(),
            qty: ln.qty,
            unit_price: ln.unit_price,
            amount: ln.base_sub,
            discount_amount: -disc_net[i],
            tax: TaxBreakdown {
                tax_code: rates[i].to_string(),
                tax_amount: ln.base_tax + disc_tax[i],
                vat_status: vat_status.to_owned(),
                service_charge_amount: sc[i],
                ..Default::default()
            },
            ..Default::default()
        };
        if ln.base_stax != 0 {
            ml.tax.stax_code = ln.stax_no.to_string();
            ml.tax.stax_amount = ln.base_stax;
        }
        ml.desc_defaults = texts(&ln.trn_desc);
        let origin = LineOrigin {
            src_code: "POS".into(),
            pos_check_no: raw.check_no.clone(),
            rvc: ln.rvc.to_string(),
            folio_link: folio.clone(),
            ..Default::default()
        };
        let mut detail = Vec::new();
        if !ln.item_code.is_empty() {
            detail.push(DetailItem {
                line_key: ln.trn_seq.to_string(),
                icode: ln.item_code.clone(),
                unit: ln.unit_name.clone(),
                qty: ln.qty,
                unit_price: ln.unit_price,
                amount: ln.base_sub,
                discount_amount: -disc_net[i],
                tax: ml.tax.clone(),
                name_defaults: texts(&ln.trn_desc),
                ..Default::default()
            });
        }
        lines.push(InvoiceLine {
            merged: ml,
            detail,
            origin: Some(origin),
        });
    }

    let (mut s_net, mut s_disc, mut s_tax, mut s_sc, mut s_stax) = (0i64, 0i64, 0i64, 0i64, 0i64);
    for (i, ln) in revenue.iter().enumerate() {
        s_net += ln.base_sub;
        s_disc += disc_net[i];
        s_tax += lines[i].merged.tax.tax_amount;
        s_sc += sc[i];
        s_stax += ln.base_stax;
    }
    for (name, got, want) in [
        ("Σ net vs SubTotal", s_net, h.sub_total),
        ("Σ discount vs Discount", s_disc, h.discount),
        ("Σ line tax vs TaxTotal", s_tax, h.tax_total),
        ("Σ service charge vs SurCharge", s_sc, h.sur_charge),
        ("Σ special tax vs STaxTotal", s_stax, h.stax_total),
        (
            "SubTotal+Discount+SurCharge vs BeforeTax",
            h.sub_total + h.discount + h.sur_charge,
            h.before_tax,
        ),
        (
            "BeforeTax+TaxTotal vs TotalBase",
            h.before_tax + h.tax_total,
            h.total_base,
        ),
    ] {
        let diff = got - want;
        if !(-1..=1).contains(&diff) {
            return Err(fail(format!(
                "header reconcile failed ({name}): got={got} want={want}"
            )));
        }
    }

    Ok(CollectedInvoice {
        source: "POS".into(),
        source_ref: format!("POS:{}", raw.check_no),
        vat_status: "printed".into(),
        header: InvoiceHeader {
            company_name: h.company.clone(),
            customer_tax: h.client_vat.clone(),
            customer_addr: h.address.clone(),
            invoice_type: "GTGT".into(),
            code_mode: "CO_MA".into(),
            subtotal: h.before_tax,
            service_charge: h.sur_charge,
            tax_amount: h.tax_total,
            total_amount: h.total_base,
            total_discount: -h.discount,
            guest_name: h.client_name.clone(),
            inv_content: h.inv_content.clone(),
            folio_num: parse_folio_num(&folio),
            folio,
            rvc: h.rvc.to_string(),
            // Go always sets a pointer to the scanned time, so a zero time is still sent.
            inv_date: Some(h.inv_date.unwrap_or_else(crate::gojson::go_zero_time)),
            ..Default::default()
        },
        lines,
        flags: None,
    })
}

/// `recalcPOSLineTax`.
pub fn recalc_line_tax(inv: &mut CollectedInvoice, recalc: bool) -> Result<(), String> {
    if !recalc {
        return Ok(());
    }
    for ln in &mut inv.lines {
        if ln.merged.tax.vat_status == VAT_STATUS_KCT {
            continue;
        }
        let Some(rate) = atoi(ln.merged.tax.tax_code.trim()) else {
            continue;
        };
        if !LEGAL_RATES.contains(&rate) {
            continue;
        }
        let (t, s) = recalc_tax_into_sc(
            ln.merged.amount,
            ln.merged.tax.tax_amount,
            ln.merged.tax.service_charge_amount,
            rate,
        )
        .map_err(|e| format!("line {} recalc: {e}", ln.merged.line_key))?;
        ln.merged.tax.tax_amount = t;
        ln.merged.tax.service_charge_amount = s;
    }
    Ok(())
}
