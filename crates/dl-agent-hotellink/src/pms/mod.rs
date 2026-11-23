//! SMILE FO data access (Go `internal/pms`): the rows, the repository contract the
//! reconciler depends on, and the SQL Server implementation.

mod entities;
pub(crate) mod insert;
mod mapping;
mod repo;
mod rows;
#[cfg(test)]
mod sql_tests;

use std::collections::BTreeMap;

use dl_time::{Date, DateTime};

pub use entities::{Folio, Val, sql_time, ymd, zero_time};
pub use mapping::validate_mapping;
pub use repo::SqlRepo;
pub use rows::{AdditionalName, ClientContact, ClientSts, FolioRate, SmileClient};

use crate::error::Result;
use crate::jobmodel::{AvailRow, MappingRow};

/// `LastChangeClerkID` the agent writes when it cancels a folio, so a later
/// [`PmsRepo::has_user_cancelled_folio`] tells a user's cancel from its own.
pub const AGENT_CANCEL_CLERK_ID: &str = "HLAGENT-CANCEL";

/// `LastChangeClerkID` the agent stamps on a folio it updates (Go `AgentClerkID`). Three
/// characters: `Folio.LastChangeClerkID` is varchar(3) on some SMILE installs, and the
/// trigger `triggerFolioUpdate` concatenates it into its FolioLog insert as
/// `ChangedClerk`, so a NULL clerk writes no history row at all (RT-0651). It differs
/// from [`AGENT_CANCEL_CLERK_ID`], so [`PmsRepo::has_user_cancelled_folio`] is unaffected.
pub const AGENT_CLERK_ID: &str = "HLA";

/// The columns `UpdateFolioFields` may set.
pub const ALLOWED_UPDATE_COLS: &[&str] = &[
    "ArrivalDate",
    "DepartureDate",
    "NumAdult",
    "NumChild",
    "LastName",
    "TelNumM",
    "MailM",
    "RateAmount",
    "IncludeBF",
    "BreakfastCode",
    "NumGuestBF",
    "NumChildBF",
    "ExternalConfirmNumber",
    "RoomTypeCode",
    "RoomTypeBooked",
    "RoomCode",
];

/// The repository the reconciler uses (Go `agent.PMSRepo`).
pub trait PmsRepo: Send + Sync {
    /// Live folios whose `ExternalConfirmNumber` is `ext`.
    fn get_folios(&self, ext: &str) -> Result<Vec<Folio>>;
    /// Live folios whose `Booker` is `booker` (none for a blank booker).
    fn get_folios_by_booker(&self, booker: &str) -> Result<Vec<Folio>>;
    /// Whether a user (not the agent) has cancelled a folio of this booking.
    fn has_user_cancelled_folio(&self, booker: &str, booking_id: &str) -> Result<bool>;
    /// `EXEC GetConfirmNum`.
    fn get_new_confirm_num(&self) -> Result<f64>;
    /// `EXEC GetFolioNum`.
    fn get_next_folio_num(&self) -> Result<f64>;
    /// Inserts the folio, its rates and its additional name in one transaction.
    fn insert_folio_with_rates(
        &self,
        f: &Folio,
        rates: &[FolioRate],
        an: &AdditionalName,
    ) -> Result<()>;
    /// Cancels the folios in one transaction.
    fn cancel_folios(&self, folios: &[Folio]) -> Result<()>;
    /// Updates the given columns of a live folio, stamping `LastChangeTime` and
    /// `LastChangeClerkID` ([`AGENT_CLERK_ID`]).
    fn update_folio_fields(&self, folio_num: f64, set: &BTreeMap<&'static str, Val>) -> Result<()>;
    /// The folio's nightly rates, by date.
    fn get_folio_rates(&self, folio_num: f64) -> Result<Vec<FolioRate>>;
    /// Replaces the folio's nightly rates in one transaction.
    fn replace_folio_rates(&self, folio_num: f64, rates: &[FolioRate]) -> Result<()>;
    /// Updates the guest name, e-mail and stay dates of the folio's additional name. The
    /// dates are written explicitly: SMILE's Folio trigger only narrows them (RT-0651).
    fn update_additional_name(
        &self,
        folio_num: f64,
        last_name: &str,
        email: &str,
        arrival: DateTime,
        departure: DateTime,
    ) -> Result<()>;
    /// Stamps `LastChangeTime`/`LastChangeClerkID` on a live folio when only a child row
    /// (FolioRate, AdditionalName) changed and no Folio column did (RT-0651).
    fn stamp_folio_change(&self, folio_num: f64) -> Result<()>;
    /// Appends one SMILE `FolioLog` row for a change the Folio trigger cannot see
    /// (FolioRate). Best-effort at the caller (RT-0651).
    fn append_folio_log(&self, folio_num: f64, field: &str, old: &str, new: &str) -> Result<()>;
    /// `GlobalParameter.ParaString` as an integer (0 when absent).
    fn get_global_param_int(&self, name: &str) -> Result<i64>;
    /// Inserts the global parameter unless it exists.
    fn seed_global_param_if_absent(&self, name: &str, value: &str) -> Result<()>;
    /// `CLIENT` by name.
    fn get_client_by_name(&self, name: &str) -> Result<Option<SmileClient>>;
    /// `CLIENT` by folio number.
    fn get_client_by_folio_num(&self, num: f64) -> Result<Option<SmileClient>>;
    /// `ClientContacts` by contact name.
    fn get_client_contact_by_name(&self, name: &str) -> Result<Option<ClientContact>>;
    /// `ClientSts` by client code.
    fn get_client_sts(&self, code: f64) -> Result<Option<ClientSts>>;
    /// `HotelParameter.HotelDate`.
    fn get_hotel_date(&self) -> Result<DateTime>;
}

/// Everything the run loops use (Go `agent.FullRepo`).
pub trait FullRepo: PmsRepo {
    /// `SELECT 1`.
    fn test_connection(&self) -> Result<()>;
    /// Creates `RoomTypeMapped` if missing and seeds it from `RoomType` when empty;
    /// returns the number of seeded rows.
    fn ensure_room_type_mapped(&self) -> Result<i64>;
    /// All `RoomTypeMapped` rows.
    fn get_room_type_mapped(&self) -> Result<Vec<MappingRow>>;
    /// `EXEC AvailabilityChart` for `days` days from `from`.
    fn get_availability(&self, from: Date, days: i64) -> Result<Vec<AvailRow>>;
    /// Adds a mapping row.
    fn insert_mapping(&self, m: &MappingRow) -> Result<()>;
    /// Replaces the row equal to `orig` with `m`.
    fn update_mapping(&self, orig: &MappingRow, m: &MappingRow) -> Result<()>;
    /// Deletes the row equal to `orig`.
    fn delete_mapping(&self, orig: &MappingRow) -> Result<()>;
}

/// `computeAvailable`: rooms minus every kind of use (negative when overbooked).
#[allow(clippy::too_many_arguments)]
pub fn compute_available(
    num_room: i64,
    definite: i64,
    tentative: i64,
    seri: i64,
    waiting: i64,
    allot: i64,
    ooo: i64,
    ooi: i64,
    hu: i64,
    phu: i64,
) -> i64 {
    num_room - (definite + tentative + seri + waiting + allot + ooo + ooi + hu + phu)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_available_cases() {
        // avail_test.go
        let cases: [(&str, [i64; 10], i64); 5] = [
            ("all zero", [10, 0, 0, 0, 0, 0, 0, 0, 0, 0], 10),
            ("some booked", [10, 2, 1, 0, 0, 0, 1, 0, 0, 0], 6),
            ("fully booked", [5, 2, 1, 1, 1, 0, 0, 0, 0, 0], 0),
            (
                "overbooking allowed negative",
                [3, 2, 1, 1, 0, 0, 0, 0, 0, 0],
                -1,
            ),
            ("all fields", [10, 1, 1, 1, 1, 1, 1, 1, 1, 1], 1),
        ];
        for (name, a, want) in cases {
            let got = compute_available(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9]);
            assert_eq!(got, want, "{name}");
        }
    }
}
