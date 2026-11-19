//! `FolioObservationReader.ReadAsync`: changed folios since a watermark, with their
//! qualifying revenue, guest contact and extra beds, as hub observations.

use std::collections::HashMap;

use dl_decimal::Decimal;
use dl_hash::{Digest, Sha256};
use dl_time::DateTime;

use super::earn::EarnPair;
use crate::smile::{
    FolioRow, FolioSource, GuestRow, RevenueRow, SmileError, safe_identifier, to_utc,
};
use crate::wire::{FolioObservation, StayClassification};

/// What one read returns (`FolioObservationRead`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolioRead {
    /// The observations, in scan order.
    pub observations: Vec<FolioObservation>,
    /// The latest `LastChangeTime` read (PMS local time).
    pub max_last_change_time: Option<DateTime>,
}

/// Reads at most `max_row` folios changed at or after `watermark`. An invalid
/// `phone_column` is [`SmileError::Invalid`]; a schema mismatch is the SQL error 207/208.
pub fn read(
    src: &dyn FolioSource,
    phone_column: &str,
    watermark: DateTime,
    max_row: i64,
    pairs: &[EarnPair],
    extra_bed_code: &str,
) -> Result<FolioRead, SmileError> {
    let column = safe_identifier(phone_column)?;
    let columns = src.occupancy_columns()?;
    let rows = src.changed_folios(watermark, max_row, columns)?;
    if rows.is_empty() {
        return Ok(FolioRead::default());
    }
    let revenues = read_revenues(src, &rows, pairs)?;
    let mut nums: Vec<Decimal> = Vec::new();
    for r in &rows {
        if !nums.contains(&r.folio_num) {
            nums.push(r.folio_num);
        }
    }
    let guests: HashMap<Decimal, GuestRow> = src
        .guests(&nums, &column)?
        .into_iter()
        .map(|g| (g.folio_num, g))
        .collect();
    let special: Option<HashMap<Decimal, i64>> = if columns.has_special {
        Some(
            src.extra_bed_special_qty(&nums, extra_bed_code, columns.special_has_cancel_num)?
                .into_iter()
                .collect(),
        )
    } else {
        None
    };
    let observations = rows
        .iter()
        .map(|r| {
            let mut o = to_observation(r, &revenues, &guests);
            o.num_enf = r.num_enf;
            o.extra_bed = r.extra_bed;
            o.extra_bed_special_qty = special
                .as_ref()
                .map(|m| m.get(&r.folio_num).copied().unwrap_or(0));
            o
        })
        .collect();
    Ok(FolioRead {
        observations,
        max_last_change_time: rows.iter().map(|r| r.last_change_time).max(),
    })
}

fn read_revenues(
    src: &dyn FolioSource,
    rows: &[FolioRow],
    pairs: &[EarnPair],
) -> Result<HashMap<String, RevenueRow>, SmileError> {
    if pairs.is_empty() {
        return Ok(HashMap::new());
    }
    let mut nums: Vec<String> = Vec::new();
    for r in rows {
        let n = folio_num_as_varchar(r.folio_num);
        if !nums.contains(&n) {
            nums.push(n);
        }
    }
    Ok(src
        .revenues(&nums, pairs)?
        .into_iter()
        .map(|r| (r.folio_num.trim().to_owned(), r))
        .collect())
}

/// `FolioNumAsVarchar`: the integer part, as `FolioTransaction.FolioNum` stores it.
pub fn folio_num_as_varchar(folio_num: Decimal) -> String {
    format!("{}", folio_num.trunc(0))
}

/// `Fingerprint`: lowercase hex SHA-256 of `lastTxnId:count:total` (`"0"`, `"F2"`).
pub fn fingerprint(revenue: Option<&RevenueRow>) -> String {
    let last = revenue.map_or(Decimal::ZERO, |r| r.last_txn_id);
    let count = revenue.map_or(0, |r| r.txn_count);
    let total = revenue.map_or(Decimal::ZERO, |r| r.total_amount);
    let raw = format!("{last:.0}:{count}:{total:.2}");
    dl_encoding::hex::encode(Sha256::digest(raw.as_bytes()))
}

fn trimmed(v: Option<&str>) -> Option<String> {
    v.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn class_code(raw: Option<&str>) -> Vec<String> {
    trimmed(raw).filter(|v| v != "0").into_iter().collect()
}

fn to_observation(
    r: &FolioRow,
    revenues: &HashMap<String, RevenueRow>,
    guests: &HashMap<Decimal, GuestRow>,
) -> FolioObservation {
    let revenue = revenues.get(&folio_num_as_varchar(r.folio_num));
    let guest = guests.get(&r.folio_num);
    let checked_out_at = r.check_out_time.map(to_utc);
    FolioObservation {
        folio_num: r.folio_num,
        folio_sub_num: r.folio_sub_num,
        pms_folio_id: Some(r.folio_id),
        folio_status: r.folio_status,
        checkout_epoch: checked_out_at.map_or(0, |o| o.unix_seconds()),
        checked_out_at,
        checked_in_at: r.check_in_time.map(to_utc),
        observed_at: to_utc(r.last_change_time),
        arrival_date: r.arrival_date.unwrap_or(r.last_change_time).date(),
        departure_date: r.departure_date.unwrap_or(r.last_change_time).date(),
        confirm_num: r.confirm_num,
        external_confirm_number: trimmed(r.external_confirm_number.as_deref()),
        is_cancelled: r.cancel_num.is_some_and(|c| c != Decimal::ZERO),
        cancelled_at: r.cancel_time.map(to_utc),
        is_no_show: r.no_show,
        room_code: trimmed(r.room_code.as_deref()),
        room_item_id_hint: r.user_info1.as_deref().and_then(|s| s.trim().parse().ok()),
        guest_phone: guest
            .and_then(|g| g.phone.clone())
            .filter(|p| !p.is_empty()),
        guest_name: guest
            .and_then(|g| g.name.clone())
            .filter(|n| !n.is_empty())
            .or_else(|| trimmed(r.last_name.as_deref())),
        qualifying_revenue: revenue.map_or(Decimal::ZERO, |v| v.total_amount),
        revenue_fingerprint: fingerprint(revenue),
        booked_room_type_code: trimmed(r.room_type_booked.as_deref()),
        actual_room_type_code: trimmed(r.room_type_code.as_deref()),
        num_adult: r.num_adult,
        num_child: r.num_child,
        classification: StayClassification {
            origin_codes: class_code(r.origin_code.as_deref()),
            segment_codes: class_code(r.market_segment_code.as_deref()),
            rate_codes: class_code(r.room_rate_code.as_deref()),
        },
        num_enf: None,
        extra_bed: None,
        extra_bed_special_qty: None,
    }
}

#[cfg(test)]
mod tests {
    use dl_time::Date;

    use super::*;

    #[test]
    fn fingerprint_matches_dotnet_formatting() {
        // "0:0:0.00", as FolioObservationReader.Fingerprint(null) hashes it.
        assert_eq!(
            fingerprint(None),
            dl_encoding::hex::encode(Sha256::digest(b"0:0:0.00"))
        );
        let r = RevenueRow {
            folio_num: "12".into(),
            last_txn_id: "105.5".parse().unwrap(),
            txn_count: 3,
            total_amount: "1500000.005".parse().unwrap(),
        };
        assert_eq!(
            fingerprint(Some(&r)),
            dl_encoding::hex::encode(Sha256::digest(b"106:3:1500000.01"))
        );
        assert_eq!(folio_num_as_varchar("1234.9".parse().unwrap()), "1234");
    }

    #[test]
    fn rows_map_like_to_observation() {
        let at = Date::new(2026, 3, 2).unwrap().midnight();
        let row = FolioRow {
            folio_num: Decimal::from(7),
            folio_sub_num: Decimal::from(1),
            folio_id: 70,
            folio_status: 3,
            check_in_time: None,
            check_out_time: Some(at),
            last_change_time: at,
            arrival_date: None,
            departure_date: None,
            confirm_num: None,
            external_confirm_number: None,
            cancel_num: Some(Decimal::ZERO),
            cancel_time: None,
            no_show: false,
            room_code: None,
            user_info1: Some(" 6f9619ff-8b86-d011-b42d-00c04fc964ff ".into()),
            last_name: Some(" Nguyen ".into()),
            room_type_booked: None,
            room_type_code: None,
            num_adult: None,
            num_child: None,
            num_enf: None,
            extra_bed: None,
            origin_code: Some(" OTA ".into()),
            market_segment_code: Some("0".into()),
            room_rate_code: None,
        };
        let o = to_observation(&row, &HashMap::new(), &HashMap::new());
        assert_eq!(o.checkout_epoch, to_utc(at).unix_seconds());
        assert_eq!(o.arrival_date, at.date());
        assert!(!o.is_cancelled);
        assert!(o.room_item_id_hint.is_some());
        assert_eq!(o.guest_name.as_deref(), Some("Nguyen"));
        assert!(o.classification.segment_codes.is_empty());
        assert_eq!(o.classification.origin_codes, vec!["OTA".to_owned()]);
        assert_eq!(o.revenue_fingerprint, fingerprint(None));
    }
}
