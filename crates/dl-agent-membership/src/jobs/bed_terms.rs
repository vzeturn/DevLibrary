//! SMILE's extra-bed terms (`Sync/ExtraBedTerms.cs`): the `SpecialService` row of the bed
//! code, its tax codes, the AutoPost room code's tax codes and the `GlobalParameter` flags,
//! with the hashes the hub stores (`ExtraBedTermsHashing`). Read-only: the agent never
//! writes `GlobalParameter`, `ExtraBedInv` or the `SpecialService` catalogue.

use dl_decimal::{Decimal, RoundingMode};
use dl_hash::{Digest, Sha256};
use dl_time::Date;

/// `ExtraBedSpecialRow`: the `SpecialService` catalogue row of the bed code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecialService {
    /// `Code` (`SpecialServiceCode`, trimmed).
    pub code: String,
    /// `TrnCode`.
    pub trn_code: Option<String>,
    /// `TrnSub`.
    pub trn_sub: Option<String>,
    /// `OpenPrice` (`None`: unreadable).
    pub open_price: Option<i32>,
    /// `IncludeInRoomCharge`.
    pub include_in_room_charge: Option<i32>,
    /// `AmountCharge`.
    pub amount_charge: Option<Decimal>,
}

/// `ExtraBedTaxCodes`: `TransactionCode`'s tax and service-charge codes of one code.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaxCodes {
    /// `TaxCode`.
    pub tax_code: Option<String>,
    /// `ServiceChargeCode`.
    pub service_charge_code: Option<String>,
    /// `STaxCode`.
    pub s_tax_code: Option<String>,
}

/// `ExtraBedTerms`: the terms of one SMILE `HotelID`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraBedTerms {
    /// `SmileHotelId`.
    pub smile_hotel_id: String,
    /// `SurchargeAmount` (native `ExtraBedSurChg`, the rule-13 guard only).
    pub surcharge_amount: Option<Decimal>,
    /// `TrnCode` (`EBedSurChgTrnCode`).
    pub trn_code: Option<String>,
    /// `TrnSub` (`EBedSurChgTrnSub`).
    pub trn_sub: Option<String>,
    /// `StockTotal`.
    pub stock_total: Option<i32>,
    /// `Special` (`None`: `SPECIAL_MISSING`).
    pub special: Option<SpecialService>,
    /// `SpecialTax`.
    pub special_tax: Option<TaxCodes>,
    /// `RoomTax` (the AutoPost room code).
    pub room_tax: Option<TaxCodes>,
    /// `SpecialByDate`.
    pub special_by_date: Option<bool>,
    /// `SmileHotelDate`.
    pub smile_hotel_date: Option<Date>,
    /// `StructuralFingerprint`.
    pub structural_fingerprint: String,
    /// `ContentHash`.
    pub content_hash: String,
    /// `Warnings`.
    pub warnings: Vec<String>,
    /// `RateCodePlusPlus` (`GlobalParameter.RateCodePP = '1'`).
    pub rate_code_plus_plus: Option<bool>,
    /// `SpecialScPercent`.
    pub special_sc_percent: Option<Decimal>,
    /// `SpecialVatPercent`.
    pub special_vat_percent: Option<Decimal>,
    /// `PmsPriced` (ADR-0106: the hotel's admin picked this special on the hub).
    pub pms_priced: bool,
}

impl ExtraBedTerms {
    /// `SpecialCode`.
    pub fn special_code(&self) -> Option<&str> {
        self.special.as_ref().map(|s| s.code.as_str())
    }

    /// `SpecialTaxLabel`: `"<TaxCode>/<ServiceChargeCode>"` of the special's code.
    pub fn special_tax_label(&self) -> Option<String> {
        self.special.as_ref()?;
        let t = self.special_tax.as_ref()?;
        Some(format!(
            "{}/{}",
            t.tax_code.as_deref().unwrap_or(""),
            t.service_charge_code.as_deref().unwrap_or("")
        ))
    }

    /// `SpecialMatchesRoomTax`.
    pub fn special_matches_room_tax(&self) -> Option<bool> {
        self.special.as_ref()?;
        Some(self.special_tax.is_some() && self.special_tax == self.room_tax)
    }

    /// `SpecialUsable`: the catalogue row can carry an agent bed.
    pub fn special_usable(&self) -> bool {
        let Some(s) = &self.special else {
            return false;
        };
        let taxed_like_room = self.special_matches_room_tax() == Some(true);
        if self.pms_priced {
            s.include_in_room_charge == Some(0)
                && s.amount_charge.is_some_and(|a| a > Decimal::ZERO)
                && s.trn_code.is_some()
                && taxed_like_room
        } else {
            s.open_price == Some(1)
                && s.include_in_room_charge == Some(0)
                && s.amount_charge == Some(Decimal::ZERO)
                && s.trn_code.is_some()
                && taxed_like_room
        }
    }

    /// `SpecialPriceable`: no gross-up needed, or both rates known and no `STaxCode`.
    pub fn special_priceable(&self) -> bool {
        self.rate_code_plus_plus != Some(true)
            || (self.special_sc_percent.is_some()
                && self.special_vat_percent.is_some()
                && self
                    .special_tax
                    .as_ref()
                    .and_then(|t| t.s_tax_code.as_deref())
                    .is_none_or(|c| c.trim().is_empty()))
    }

    /// `AsPmsPriced`: the terms on the PMS-priced basis (the fingerprint gains a pricing
    /// marker and drops `AmountCharge`).
    pub fn as_pms_priced(&self) -> ExtraBedTerms {
        if self.pms_priced {
            return self.clone();
        }
        let fingerprint = structural_fingerprint(
            &self.smile_hotel_id,
            self.special.as_ref(),
            self.special_tax.as_ref(),
            self.room_tax.as_ref(),
            self.special_by_date,
            self.rate_code_plus_plus,
            true,
        );
        ExtraBedTerms {
            pms_priced: true,
            content_hash: content_hash(
                &fingerprint,
                self.surcharge_amount,
                self.trn_code.as_deref(),
                self.trn_sub.as_deref(),
                self.stock_total,
            ),
            structural_fingerprint: fingerprint,
            ..self.clone()
        }
    }
}

/// `ExtraBedTermsHashing.NoRateCodeWritten`.
pub const NO_RATE_CODE_WRITTEN: &str = "RATE:NONE";

/// `Field`: length-prefixed (UTF-16 code units, like .NET `string.Length`); `-` for null.
pub(crate) fn field(text: &mut String, name: &str, value: Option<&str>) {
    text.push_str(name);
    text.push('=');
    match value {
        None => text.push('-'),
        Some(v) => {
            text.push_str(&v.encode_utf16().count().to_string());
            text.push(':');
            text.push_str(v);
        }
    }
    text.push('\n');
}

fn tax(text: &mut String, name: &str, t: Option<&TaxCodes>) {
    field(text, name, t.map(|_| "FOUND"));
    let Some(t) = t else { return };
    field(text, &format!("{name}.taxCode"), t.tax_code.as_deref());
    field(
        text,
        &format!("{name}.serviceChargeCode"),
        t.service_charge_code.as_deref(),
    );
    field(text, &format!("{name}.sTaxCode"), t.s_tax_code.as_deref());
}

pub(crate) fn flag(b: Option<bool>) -> Option<&'static str> {
    b.map(|b| if b { "1" } else { "0" })
}

/// `Amount`: `value.ToString("0.##")` (two decimals at most, half away from zero, no
/// trailing zeros).
pub fn amount_text(value: Option<Decimal>) -> Option<String> {
    value.map(|v| v.round_dp(2, RoundingMode::HalfUp).normalize().to_string())
}

pub(crate) fn sha256_hex(text: &str) -> String {
    dl_encoding::hex::encode(Sha256::digest(text.as_bytes()))
}

/// `ExtraBedTermsHashing.StructuralFingerprint` (`extra-bed-fingerprint:v3`).
pub fn structural_fingerprint(
    smile_hotel_id: &str,
    special: Option<&SpecialService>,
    special_tax: Option<&TaxCodes>,
    room_tax: Option<&TaxCodes>,
    special_by_date: Option<bool>,
    rate_code_plus_plus: Option<bool>,
    pms_priced: bool,
) -> String {
    let mut text = String::from("extra-bed-fingerprint:v3\n");
    field(&mut text, "hotel", Some(smile_hotel_id));
    field(&mut text, "special", special.map(|_| "FOUND"));
    if let Some(s) = special {
        field(&mut text, "special.code", Some(&s.code));
        field(&mut text, "special.trnCode", s.trn_code.as_deref());
        field(&mut text, "special.trnSub", s.trn_sub.as_deref());
        field(
            &mut text,
            "special.openPrice",
            s.open_price.map(|n| n.to_string()).as_deref(),
        );
        field(
            &mut text,
            "special.includeInRoomCharge",
            s.include_in_room_charge.map(|n| n.to_string()).as_deref(),
        );
        if !pms_priced {
            field(
                &mut text,
                "special.amountCharge",
                amount_text(s.amount_charge).as_deref(),
            );
        }
    }
    tax(&mut text, "specialTax", special_tax);
    tax(&mut text, "roomTax", room_tax);
    field(&mut text, "specialByDate", flag(special_by_date));
    field(&mut text, "rate", Some(NO_RATE_CODE_WRITTEN));
    field(&mut text, "rateCodePP", flag(rate_code_plus_plus));
    if pms_priced {
        field(&mut text, "pricing", Some("PMS"));
    }
    sha256_hex(&text)
}

/// `ExtraBedTermsHashing.ContentHash` (`extra-bed-content:v2`).
pub fn content_hash(
    structural_fingerprint: &str,
    surcharge: Option<Decimal>,
    trn_code: Option<&str>,
    trn_sub: Option<&str>,
    stock_total: Option<i32>,
) -> String {
    let mut text = String::from("extra-bed-content:v2\n");
    field(&mut text, "fingerprint", Some(structural_fingerprint));
    field(&mut text, "surcharge", amount_text(surcharge).as_deref());
    field(&mut text, "trnCode", trn_code);
    field(&mut text, "trnSub", trn_sub);
    field(
        &mut text,
        "stock",
        stock_total.map(|n| n.to_string()).as_deref(),
    );
    sha256_hex(&text)
}

/// `ExtraBedTermsHashing.WireContentHash` (`extra-bed-wire:v2`): the content plus what the
/// agent derived, so a mode change is posted at once.
pub fn wire_content_hash(
    content_hash: &str,
    mode_configured: &str,
    mode_effective: &str,
    mode_reason: Option<&str>,
    special_earn_covered: Option<bool>,
) -> String {
    let mut text = String::from("extra-bed-wire:v2\n");
    field(&mut text, "content", Some(content_hash));
    field(&mut text, "modeConfigured", Some(mode_configured));
    field(&mut text, "modeEffective", Some(mode_effective));
    field(&mut text, "modeReason", mode_reason);
    field(&mut text, "earnCovered", flag(special_earn_covered));
    sha256_hex(&text)
}
