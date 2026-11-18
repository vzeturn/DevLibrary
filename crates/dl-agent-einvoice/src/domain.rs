//! The collected invoice and its canonical JSON: a port of
//! `internal/ingest/domain` (collected.go, collected_hash.go, classify.go) and
//! `internal/invoice/domain/push.go`. Field order and `omitempty` rules follow the Go
//! struct tags exactly, because the payload bytes are hashed.

use dl_time::DateTime;

use crate::gojson::{self, Obj, array};

/// VAT category of an ordinary taxable line (rate derived from amounts).
pub const VAT_STATUS_TAXABLE: &str = "";
/// Không chịu thuế GTGT.
pub const VAT_STATUS_KCT: &str = "KCT";

/// One candidate text in a language (`LangText`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LangText {
    /// Language code (`vi`, `en`).
    pub lang: String,
    /// The text in that language.
    pub text: String,
}

/// Per-line or per-item tax split (`TaxBreakdown`).
#[derive(Debug, Clone, PartialEq, Default)]
#[allow(missing_docs)] // fields mirror the Go struct one for one; see its doc
pub struct TaxBreakdown {
    pub tax_code: String,
    pub tax_amount: i64,
    pub vat_status: String,
    pub stax_code: String,
    pub stax_amount: i64,
    pub service_charge_code: String,
    pub service_charge_amount: i64,
}

/// One expanded item (`DetailItem`).
#[derive(Debug, Clone, PartialEq, Default)]
#[allow(missing_docs)] // fields mirror the Go struct one for one; see its doc
pub struct DetailItem {
    pub line_key: String,
    pub icode: String,
    /// Empty encodes as `null` (a nil Go slice).
    pub name_defaults: Vec<LangText>,
    pub group: String,
    pub unit: String,
    pub qty: f64,
    pub unit_price: i64,
    pub amount: i64,
    pub discount_amount: i64,
    pub tax: TaxBreakdown,
}

/// The folio/check summary line (`MergedLine`).
#[derive(Debug, Clone, PartialEq, Default)]
#[allow(missing_docs)] // fields mirror the Go struct one for one; see its doc
pub struct MergedLine {
    pub trn_code: String,
    pub trn_sub_code: String,
    pub line_key: String,
    pub source_ref: String,
    /// Empty encodes as `null`.
    pub desc_defaults: Vec<LangText>,
    pub unit: String,
    pub qty: f64,
    pub unit_price: i64,
    pub amount: i64,
    pub discount_amount: i64,
    pub tax: TaxBreakdown,
    pub trn_group: String,
    pub is_hk_post: bool,
    pub tax_rate: String,
}

/// POS/FO traceability for a line (`LineOrigin`).
#[derive(Debug, Clone, PartialEq, Default)]
#[allow(missing_docs)] // fields mirror the Go struct one for one; see its doc
pub struct LineOrigin {
    pub pos_check_no: String,
    pub src_code: String,
    pub rvc: String,
    pub outlet: String,
    pub folio_link: String,
    pub origin_folio_number: String,
    pub trn_type: String,
    pub origin_room: String,
    pub room_code: String,
    pub transaction_date: String,
    pub bill_id: String,
    pub post_to_acc: bool,
    pub number_cover: i64,
    pub xref: String,
    pub origin_currency: String,
    pub origin_amount: i64,
    pub rpt_ex_rate: String,
}

/// A line with both representations (`InvoiceLine`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InvoiceLine {
    /// The line as printed on the invoice.
    pub merged: MergedLine,
    /// The source items folded into it.
    pub detail: Vec<DetailItem>,
    /// Where it came from in SMILE, when known.
    pub origin: Option<LineOrigin>,
}

/// The collected invoice header (`InvoiceHeader`).
#[derive(Debug, Clone, PartialEq, Default)]
#[allow(missing_docs)] // fields mirror the Go struct one for one; see its doc
pub struct InvoiceHeader {
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
    pub total_discount: i64,
    pub rvc: String,
    pub folio: String,
    pub room_no: String,
    pub arrival: Option<String>,
    pub departure: Option<String>,
    pub num_guests: i64,
    pub confirm_no: String,
    pub guest_name: String,
    pub address2: String,
    pub buyer_contact_name: String,
    pub buyer_citizen_id: String,
    pub buyer_id_number: String,
    pub buyer_passport: String,
    pub buyer_budget_code: String,
    pub inv_content: String,
    pub currency: String,
    pub exchange_rate: f64,
    pub folio_num: i64,
}

/// Collect-time signals (`CollectedFlags`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CollectedFlags {
    /// The bill carried at least one housekeeping / minibar posting.
    pub has_hk_line: bool,
    /// The bill carried at least one line transferred from a POS check.
    pub has_pos_charge: bool,
}

/// One invoice read from SMILE (`CollectedInvoice`).
#[derive(Debug, Clone, PartialEq, Default)]
#[allow(missing_docs)] // fields mirror the Go struct one for one; see its doc
pub struct CollectedInvoice {
    pub source: String,
    pub source_ref: String,
    pub vat_status: String,
    pub header: InvoiceHeader,
    pub lines: Vec<InvoiceLine>,
    pub flags: Option<CollectedFlags>,
}

fn lang_texts(out: &mut String, v: &[LangText]) {
    let v = if v.is_empty() { None } else { Some(v) };
    array(out, v, |out, t| {
        Obj::new(out)
            .str("lang", &t.lang)
            .str("text", &t.text)
            .end();
    });
}

fn tax(out: &mut String, t: &TaxBreakdown) {
    Obj::new(out)
        .str("tax_code", &t.tax_code)
        .int("tax_amount", t.tax_amount)
        .str_omit("vat_status", &t.vat_status)
        .str_omit("stax_code", &t.stax_code)
        .int_omit("stax_amount", t.stax_amount)
        .str_omit("service_charge_code", &t.service_charge_code)
        .int_omit("service_charge_amount", t.service_charge_amount)
        .end();
}

fn detail_item(out: &mut String, d: &DetailItem) {
    Obj::new(out)
        .str_omit("line_key", &d.line_key)
        .str("icode", &d.icode)
        .raw("name_defaults", |o| lang_texts(o, &d.name_defaults))
        .str_omit("group", &d.group)
        .str_omit("unit", &d.unit)
        .float("qty", d.qty)
        .int("unit_price", d.unit_price)
        .int("amount", d.amount)
        .int_omit("discount_amount", d.discount_amount)
        .raw("tax", |o| tax(o, &d.tax))
        .end();
}

fn merged_line(out: &mut String, m: &MergedLine) {
    Obj::new(out)
        .str_omit("trn_code", &m.trn_code)
        .str_omit("trn_sub_code", &m.trn_sub_code)
        .str_omit("line_key", &m.line_key)
        .str_omit("source_ref", &m.source_ref)
        .raw("desc_defaults", |o| lang_texts(o, &m.desc_defaults))
        .str_omit("unit", &m.unit)
        .float("qty", m.qty)
        .int("unit_price", m.unit_price)
        .int("amount", m.amount)
        .int_omit("discount_amount", m.discount_amount)
        .raw("tax", |o| tax(o, &m.tax))
        .str_omit("trn_group", &m.trn_group)
        .bool_omit("is_hk_post", m.is_hk_post)
        .str_omit("tax_rate", &m.tax_rate)
        .end();
}

fn origin(out: &mut String, o: &LineOrigin) {
    Obj::new(out)
        .str_omit("pos_check_no", &o.pos_check_no)
        .str_omit("src_code", &o.src_code)
        .str_omit("rvc", &o.rvc)
        .str_omit("outlet", &o.outlet)
        .str_omit("folio_link", &o.folio_link)
        .str_omit("origin_folio_number", &o.origin_folio_number)
        .str_omit("trn_type", &o.trn_type)
        .str_omit("origin_room", &o.origin_room)
        .str_omit("room_code", &o.room_code)
        .str_omit("transaction_date", &o.transaction_date)
        .str_omit("bill_id", &o.bill_id)
        .bool_omit("post_to_acc", o.post_to_acc)
        .int_omit("number_cover", o.number_cover)
        .str_omit("xref", &o.xref)
        .str_omit("origin_currency", &o.origin_currency)
        .int_omit("origin_amount", o.origin_amount)
        .str_omit("rpt_ex_rate", &o.rpt_ex_rate)
        .end();
}

fn header(out: &mut String, h: &InvoiceHeader) {
    Obj::new(out)
        .str("seller_tax_code", &h.seller_tax_code)
        .str_omit("company_name", &h.company_name)
        .str_omit("customer_tax", &h.customer_tax)
        .str_omit("customer_addr", &h.customer_addr)
        .str_omit("customer_email", &h.customer_email)
        .str_omit("customer_tel", &h.customer_tel)
        .str("invoice_type", &h.invoice_type)
        .str("code_mode", &h.code_mode)
        .str_omit("ky_hieu", &h.ky_hieu)
        .str_omit("mau_so", &h.mau_so)
        .time_omit("inv_date", h.inv_date)
        .int("subtotal", h.subtotal)
        .int("service_charge", h.service_charge)
        .int("tax_amount", h.tax_amount)
        .int("total_amount", h.total_amount)
        .int_omit("total_discount", h.total_discount)
        .str_omit("rvc", &h.rvc)
        .str_omit("folio", &h.folio)
        .str_omit("room_no", &h.room_no)
        .opt_str("arrival", h.arrival.as_deref())
        .opt_str("departure", h.departure.as_deref())
        .int_omit("num_guests", h.num_guests)
        .str_omit("confirm_no", &h.confirm_no)
        .str_omit("guest_name", &h.guest_name)
        .str_omit("address2", &h.address2)
        .str_omit("buyer_contact_name", &h.buyer_contact_name)
        .str_omit("buyer_citizen_id", &h.buyer_citizen_id)
        .str_omit("buyer_id_number", &h.buyer_id_number)
        .str_omit("buyer_passport", &h.buyer_passport)
        .str_omit("buyer_budget_code", &h.buyer_budget_code)
        .str_omit("inv_content", &h.inv_content)
        .str_omit("currency", &h.currency)
        .float_omit("exchange_rate", h.exchange_rate)
        .int_omit("folio_num", h.folio_num)
        .end();
}

/// Detail items ordered by (ICode, original index), as `sortedDetail` does.
fn sorted_detail(items: &[DetailItem]) -> Vec<&DetailItem> {
    let mut v: Vec<&DetailItem> = items.iter().collect();
    // Vec::sort_by is stable, which keeps the original index as the tie-break.
    v.sort_by(|a, b| a.icode.cmp(&b.icode));
    v
}

impl CollectedInvoice {
    /// `CanonicalCollectedJSON`: the push payload and the input of the payload hash.
    /// Flags are not part of it (the Go canonical struct has no flags field).
    pub fn canonical_json(&self) -> String {
        let mut out = String::with_capacity(1024);
        Obj::new(&mut out)
            .str("source", &self.source)
            .str("source_ref", &self.source_ref)
            .str("vat_status", &self.vat_status)
            .raw("header", |o| header(o, &self.header))
            .raw("lines", |o| {
                array(o, Some(&self.lines), |o, ln| {
                    let mut obj = Obj::new(o);
                    obj.raw("merged", |o| merged_line(o, &ln.merged))
                        .raw("detail", |o| {
                            let sorted = sorted_detail(&ln.detail);
                            array(o, Some(&sorted), |o, d| detail_item(o, d));
                        });
                    if let Some(org) = &ln.origin {
                        obj.raw("origin", |o| origin(o, org));
                    }
                    obj.end();
                });
            })
            .end();
        out
    }

    /// `Validate`: for every line with detail, the merged net amount equals the sum of
    /// the details, and each tax bucket the detail carries matches too.
    pub fn validate(&self) -> Result<(), String> {
        for (i, ln) in self.lines.iter().enumerate() {
            if ln.detail.is_empty() {
                continue;
            }
            let (mut amt, mut tx, mut stx, mut svc) = (0i64, 0i64, 0i64, 0i64);
            for d in &ln.detail {
                amt = amt.wrapping_add(d.amount);
                tx = tx.wrapping_add(d.tax.tax_amount);
                stx = stx.wrapping_add(d.tax.stax_amount);
                svc = svc.wrapping_add(d.tax.service_charge_amount);
            }
            for (bucket, merged, total, tax_bucket) in [
                ("amount", ln.merged.amount, amt, false),
                ("tax", ln.merged.tax.tax_amount, tx, true),
                ("stax", ln.merged.tax.stax_amount, stx, true),
                (
                    "service_charge",
                    ln.merged.tax.service_charge_amount,
                    svc,
                    true,
                ),
            ] {
                if tax_bucket && total == 0 {
                    continue;
                }
                if merged != total {
                    return Err(format!(
                        "agent: {} line {i} {bucket} mismatch: merged={merged}, sum(detail)={total}",
                        self.source_ref
                    ));
                }
            }
        }
        Ok(())
    }
}

/// `ClassifyBuyer` result.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BuyerClassification {
    /// The buyer company, when the bill is a company invoice.
    pub company_name: String,
    /// The buyer person's name.
    pub guest_name: String,
    /// The buyer's tax code.
    pub tax_code: String,
    /// The buyer's citizen ID.
    pub citizen_id: String,
}

/// Exactly 12 ASCII digits (`^\d{12}$` in Go's RE2, where `\d` is ASCII only).
pub fn looks_like_citizen_id(id: &str) -> bool {
    id.len() == 12 && id.bytes().all(|b| b.is_ascii_digit())
}

fn is_placeholder_company_name(name: &str) -> bool {
    matches!(
        name.trim().to_lowercase().as_str(),
        "khach le" | "khách lẻ" | "n/a" | "individual" | "walk-in" | "walk in"
    )
}

/// `ClassifyBuyer`.
pub fn classify_buyer(company: &str, guest: &str, tax_or_citizen: &str) -> BuyerClassification {
    let mut out = BuyerClassification {
        guest_name: guest.trim().to_owned(),
        ..Default::default()
    };
    let name = company.trim();
    if !name.is_empty() && !is_placeholder_company_name(name) {
        out.company_name = name.to_owned();
    }
    let id = tax_or_citizen.trim();
    if !id.is_empty() {
        if looks_like_citizen_id(id) {
            out.citizen_id = id.to_owned();
        } else {
            out.tax_code = id.to_owned();
        }
    }
    out
}

/// One invoice pushed to the platform (`PushItem`).
#[derive(Debug, Clone, PartialEq, Default)]
#[allow(missing_docs)] // fields mirror the Go struct one for one; see its doc
pub struct PushItem {
    pub source_ref: String,
    pub payload_hash: String,
    /// Canonical JSON, embedded as is.
    pub payload: String,
    pub invoice_type: String,
    pub code_mode: String,
    pub ky_hieu: String,
    pub mau_so: String,
    pub subtotal: i64,
    pub tax_amount: i64,
    pub total_amount: i64,
    pub buyer_name: Option<String>,
    pub buyer_tax_code: Option<String>,
    pub buyer_address: Option<String>,
    pub buyer_email: Option<String>,
}

impl PushItem {
    /// `json.Marshal(PushItem)`.
    pub fn write_json(&self, out: &mut String) {
        Obj::new(out)
            .str("source_ref", &self.source_ref)
            .str("payload_hash", &self.payload_hash)
            .raw("payload", |o| o.push_str(&self.payload))
            .str("invoice_type", &self.invoice_type)
            .str("code_mode", &self.code_mode)
            .str("ky_hieu", &self.ky_hieu)
            .str("mau_so", &self.mau_so)
            .int("subtotal", self.subtotal)
            .int("tax_amount", self.tax_amount)
            .int("total_amount", self.total_amount)
            .opt_str("buyer_name", self.buyer_name.as_deref())
            .opt_str("buyer_tax_code", self.buyer_tax_code.as_deref())
            .opt_str("buyer_address", self.buyer_address.as_deref())
            .opt_str("buyer_email", self.buyer_email.as_deref())
            .end();
    }

    /// The JSON as a string.
    pub fn to_json(&self) -> String {
        let mut s = String::new();
        self.write_json(&mut s);
        s
    }
}

fn join_address(l1: &str, l2: &str) -> String {
    match (l1.is_empty(), l2.is_empty()) {
        (true, _) => l2.to_owned(),
        (_, true) => l1.to_owned(),
        _ => format!("{l1}, {l2}"),
    }
}

/// `ToPushItemCollected`: classify the buyer, validate, embed the canonical JSON and
/// lift the indexed header fields.
pub fn to_push_item(mut c: CollectedInvoice) -> Result<PushItem, String> {
    let cls = classify_buyer(
        &c.header.company_name,
        &c.header.guest_name,
        &c.header.customer_tax,
    );
    if !cls.citizen_id.is_empty() && c.header.buyer_citizen_id.is_empty() {
        c.header.buyer_citizen_id = cls.citizen_id.clone();
    }
    if !cls.guest_name.is_empty() && c.header.buyer_contact_name.is_empty() {
        c.header.buyer_contact_name = cls.guest_name.clone();
    }
    c.validate()?;
    let payload = c.canonical_json();
    let hash = <dl_hash::Sha256 as dl_hash::Digest>::digest(payload.as_bytes());
    let mut item = PushItem {
        source_ref: c.source_ref.clone(),
        payload_hash: dl_encoding::hex::encode(hash),
        payload,
        invoice_type: c.header.invoice_type.clone(),
        code_mode: c.header.code_mode.clone(),
        ky_hieu: c.header.ky_hieu.clone(),
        mau_so: c.header.mau_so.clone(),
        subtotal: c.header.subtotal,
        tax_amount: c.header.tax_amount,
        total_amount: c.header.total_amount,
        ..Default::default()
    };
    if !cls.company_name.is_empty() {
        item.buyer_name = Some(cls.company_name);
        if !cls.tax_code.is_empty() {
            item.buyer_tax_code = Some(cls.tax_code);
        }
    } else if !cls.guest_name.is_empty() {
        item.buyer_name = Some(cls.guest_name);
    }
    let addr = join_address(&c.header.customer_addr, &c.header.address2);
    if !addr.is_empty() {
        item.buyer_address = Some(addr);
    }
    if !c.header.customer_email.is_empty() {
        item.buyer_email = Some(c.header.customer_email.clone());
    }
    Ok(item)
}

/// Writes a string the way Go's `json.Marshal` would (re-exported for callers).
pub fn json_string(out: &mut String, s: &str) {
    gojson::string(out, s);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify() {
        let c = classify_buyer(" Khách lẻ ", " Nguyen A ", "079123456789");
        assert_eq!(c.company_name, "");
        assert_eq!(c.guest_name, "Nguyen A");
        assert_eq!(c.citizen_id, "079123456789");
        let c = classify_buyer("NAM HOA PHU", "", " 0313168346 ");
        assert_eq!(c.company_name, "NAM HOA PHU");
        assert_eq!(c.tax_code, "0313168346");
        assert!(!looks_like_citizen_id("07912345678٩"));
    }

    #[test]
    fn empty_invoice_shape() {
        let c = CollectedInvoice {
            source: "FO".into(),
            ..Default::default()
        };
        assert_eq!(
            c.canonical_json(),
            r#"{"source":"FO","source_ref":"","vat_status":"","header":{"seller_tax_code":"","invoice_type":"","code_mode":"","subtotal":0,"service_charge":0,"tax_amount":0,"total_amount":0},"lines":[]}"#
        );
    }
}
