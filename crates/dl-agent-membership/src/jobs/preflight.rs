//! The `SMILE_SPECIAL` preflight (`Smile/ExtraBedPreflight.cs`) and the decision for one
//! room (`FolioWriter.DecideExtraBed`): every check in spec order; the first failure folds
//! the room exactly like `FOLD_LEGACY` and names the reason. When every check holds, the
//! bed is one agent-marked `Folio_Special` row per price band and `FolioRate` carries
//! `NetExclBed`.

use dl_decimal::{Decimal, RoundingMode};

use super::bed_terms::ExtraBedTerms;
use super::beds::{
    BedDecision, FOLD_FINGERPRINT_DRIFT, FOLD_FINGERPRINT_UNVERIFIED, FOLD_HOTEL_ID_MISMATCH,
    FOLD_PAYLOAD_LEGACY, FOLD_SPECIAL_BY_DATE_OFF, FOLD_SPECIAL_MISSING, FOLD_SPECIAL_UNUSABLE,
    MODE_SMILE_SPECIAL, SpecialBedRow, decide_fold,
};
use super::payload::Room;

/// `ExtraBedPreflightInputs`: what the preflight checks for one booking write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightInputs {
    /// `ConfiguredMode`.
    pub configured_mode: String,
    /// `VerifiedFingerprint` (job payload).
    pub verified_fingerprint: Option<String>,
    /// `Terms` (`None`: they could not be read).
    pub terms: Option<ExtraBedTerms>,
    /// `ConfiguredSmileHotelId` (`Smile:SmileHotelId`).
    pub configured_smile_hotel_id: Option<String>,
}

/// `SmileSpecialPrice.From`: on a "++" hotel the pre-tax amount grossed up compound like
/// SMILE, `round(amount × (1 + SC%) × (1 + VAT%))`, whole VND, half away from zero.
/// `None` only when the arithmetic overflows (.NET would throw `OverflowException`).
pub fn special_price(
    amount: Decimal,
    plus_plus: bool,
    sc_percent: Decimal,
    vat_percent: Decimal,
) -> Option<Decimal> {
    if !plus_plus {
        return Some(amount);
    }
    let sc = Decimal::ONE.checked_add(sc_percent.checked_div(Decimal::ONE_HUNDRED)?)?;
    let vat = Decimal::ONE.checked_add(vat_percent.checked_div(Decimal::ONE_HUNDRED)?)?;
    Some(
        amount
            .checked_mul(sc)?
            .checked_mul(vat)?
            .round_dp(0, RoundingMode::HalfUp),
    )
}

/// `FolioWriter.DecideExtraBed`: the preflight when inputs were read, else `FOLD_LEGACY`
/// with the configured mode's reason.
pub fn decide_extra_bed(
    room: &Room,
    inputs: Option<&PreflightInputs>,
    configured_mode: &str,
) -> BedDecision {
    match inputs {
        None => decide_fold(room, configured_mode),
        Some(i) => decide(room, i),
    }
}

fn folded(room: &Room, mode: &str, reason: &'static str) -> BedDecision {
    BedDecision {
        fold_reason: Some(reason),
        ..decide_fold(room, mode)
    }
}

/// `ExtraBedPreflight.Decide`.
pub fn decide(room: &Room, inputs: &PreflightInputs) -> BedDecision {
    // Rules 1 and 2: the configured mode and a payload carrying every field.
    let fold = decide_fold(room, &inputs.configured_mode);
    if fold.fold_reason != Some(FOLD_FINGERPRINT_UNVERIFIED) {
        return fold;
    }
    let Some(bands) = bands(room) else {
        return folded(room, &inputs.configured_mode, FOLD_PAYLOAD_LEGACY);
    };
    if let Some(reason) = check(inputs) {
        return folded(room, &inputs.configured_mode, reason);
    }
    let Some(terms) = inputs.terms.as_ref() else {
        return folded(room, &inputs.configured_mode, FOLD_FINGERPRINT_DRIFT);
    };
    let Some(special) = terms.special.as_ref() else {
        return folded(room, &inputs.configured_mode, FOLD_SPECIAL_MISSING);
    };
    let beds = room
        .occupancy
        .as_ref()
        .and_then(|o| o.extra_beds)
        .unwrap_or(0)
        .max(0);
    let plus_plus = terms.rate_code_plus_plus == Some(true);
    let mut rows = Vec::new();
    if beds > 0 {
        for (amount, from_day, to_day) in bands {
            let price = if terms.pms_priced {
                special.amount_charge
            } else {
                special_price(
                    amount,
                    plus_plus,
                    terms.special_sc_percent.unwrap_or(Decimal::ZERO),
                    terms.special_vat_percent.unwrap_or(Decimal::ZERO),
                )
            };
            // A PMS-priced special is usable only with AmountCharge > 0, and the price
            // overflows only on absurd amounts: either way the bed cannot be priced.
            let Some(price) = price else {
                return folded(room, &inputs.configured_mode, FOLD_SPECIAL_UNUSABLE);
            };
            rows.push(SpecialBedRow {
                qty: beds,
                price,
                from_day,
                to_day,
            });
        }
    }
    // Rule 13: SMILE's own surcharge would bill Folio.ExtraBed a second time at night
    // audit, so the count stays 0 while the row carries the bed.
    let surcharge_on = terms.surcharge_amount.is_some_and(|s| s > Decimal::ZERO);
    BedDecision {
        extra_bed_to_write: if surcharge_on { 0 } else { beds },
        use_net_excl_bed: true,
        bed_note: None,
        mode: MODE_SMILE_SPECIAL,
        fold_reason: None,
        special_rows: rows,
        special_code: Some(special.code.clone()),
        native_surcharge_active: surcharge_on && beds > 0,
    }
}

/// `Check`: rules 3–7 in spec order; `None` when every one holds.
fn check(inputs: &PreflightInputs) -> Option<&'static str> {
    let terms = inputs.terms.as_ref();
    // ADR-0109: the hotel's own pick (PMS-priced) needs no VM proof.
    let proven = terms.is_some_and(|t| t.pms_priced);
    let verified = inputs
        .verified_fingerprint
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty());
    if !proven && verified.is_none() {
        return Some(FOLD_FINGERPRINT_UNVERIFIED);
    }
    let Some(terms) = terms else {
        return Some(FOLD_FINGERPRINT_DRIFT);
    };
    if !proven && !verified.is_some_and(|v| terms.structural_fingerprint.eq_ignore_ascii_case(v)) {
        return Some(FOLD_FINGERPRINT_DRIFT);
    }
    if terms.special.is_none() {
        return Some(FOLD_SPECIAL_MISSING);
    }
    if !terms.special_usable() || !terms.special_priceable() {
        return Some(FOLD_SPECIAL_UNUSABLE);
    }
    if terms.special_by_date != Some(true) {
        return Some(FOLD_SPECIAL_BY_DATE_OFF);
    }
    if let Some(hotel) = inputs
        .configured_smile_hotel_id
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
        && !hotel.eq_ignore_ascii_case(&terms.smile_hotel_id)
    {
        return Some(FOLD_HOTEL_ID_MISMATCH);
    }
    None
}

/// `Bands`: runs of equal per-bed nightly price, `FromDay` from 1, the last `ToDay` the
/// number of nights. `ExtraBedNights` when sent, else `ExtraBedNightly` every night.
/// `None` when the per-night list does not match the nights.
pub fn bands(room: &Room) -> Option<Vec<(Decimal, i32, i32)>> {
    let occ = room.occupancy.as_ref()?;
    if occ.extra_beds.unwrap_or(0) <= 0 {
        return Some(Vec::new());
    }
    let nights = room.nights.len();
    let amounts: Vec<Decimal> = match occ.extra_bed_nights.as_ref().filter(|l| !l.is_empty()) {
        Some(per_night) => {
            if per_night.len() != nights {
                return None;
            }
            per_night.clone()
        }
        None => vec![occ.extra_bed_nightly?; nights],
    };
    let mut out: Vec<(Decimal, i32, i32)> = Vec::new();
    for (i, amount) in amounts.into_iter().enumerate() {
        let day = i32::try_from(i + 1).ok()?;
        match out.last_mut() {
            Some(last) if last.0 == amount => last.2 = day,
            _ => out.push((amount, day, day)),
        }
    }
    Some(out)
}

#[cfg(test)]
#[path = "preflight_tests.rs"]
pub(crate) mod tests;
