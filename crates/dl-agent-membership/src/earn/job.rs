//! `JobDispatcher.HandleEarnAsync`: the SMILE reads of an `EarnPoints` job, turned into
//! the `AgentEarnReport` the hub expects at `POST api/agent/earn`.

use dl_uuid::Uuid;

use super::pairs::PairSource;
use super::sql::{
    self, EarnDb, FolioScope, Stay, checked_out_at, classification, read_revenue,
    resolve_checkout_time,
};
use super::wire::EarnReport;
use crate::folio::earn::ROOM_CODE_AMBIGUOUS;
use crate::jobs::plan::WriteError;
use crate::jobs::result::UNKNOWN;

/// Reads everything the earn report carries for the booking `ext` (its external key):
/// the earn pairs, the revenue total and breakdown, the stay classification and the
/// checkout time. `DbIdentityFingerprint` is left for the caller.
///
/// # Errors
/// Ambiguous SMILE room-charge configuration fails the job with the .NET message (code
/// `UNKNOWN`, as the .NET `InvalidOperationException`); any SMILE failure fails it too
/// (the hub retries it later).
pub fn read(
    db: &mut dyn EarnDb,
    pairs: &PairSource,
    hub_extra_bed_code: Option<&str>,
    booking_id: Uuid,
    ext: &str,
) -> Result<EarnReport, WriteError> {
    let scope = FolioScope::Bookings(vec![ext.to_owned()]);
    let Some(earn_pairs) = pairs.resolve(db, &scope, hub_extra_bed_code)? else {
        return Err(WriteError::Coded {
            code: UNKNOWN.into(),
            message: format!(
                "{ROOM_CODE_AMBIGUOUS}: không xác định được mã tiền phòng SMILE — chưa tích điểm {ext}."
            ),
        });
    };
    let stay = Stay::Booking(ext.to_owned());
    let (revenue, lines) = read_revenue(db, &stay, &earn_pairs, ext)?;
    let classification = classification(db, &stay)?;
    let (at, source) = resolve_checkout_time(checked_out_at(db, ext)?);
    Ok(EarnReport {
        booking_id,
        folio_ref: ext.to_owned(),
        qualifying_revenue: revenue,
        checked_out_at: at,
        folio_lines: sql::wire_lines(lines),
        classification: Some(classification),
        db_identity_fingerprint: None,
        checked_out_at_source: Some(source.to_owned()),
    })
}
