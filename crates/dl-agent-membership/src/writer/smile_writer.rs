//! [`PmsWriter`] over the SMILE connection: every operation runs on the shared
//! [`TdsSmile`] session (connect, command timeout and broken-session handling).

use std::sync::Arc;

use dl_decimal::Decimal;
use dl_uuid::Uuid;

use super::folio::FolioWriter;
use super::profile::ProfileWriter;
use super::travel_agent;
use crate::jobs::payload::LinkFolioPayload;
use crate::jobs::plan::{BookingPlan, PmsWriter, ProfileRequest, WriteError, Written};
use crate::jobs::room_diff::ExistingFolio;
use crate::jobs::travel_agent::TravelAgent;
use crate::smile::TdsSmile;
use crate::smile::hotel_now;

/// The SMILE writer the dispatcher drives.
#[derive(Debug)]
pub struct SmileWriter {
    smile: Arc<TdsSmile>,
    folio: FolioWriter,
    profile: ProfileWriter,
}

impl SmileWriter {
    /// A writer on `smile` with the folio options.
    pub fn new(smile: Arc<TdsSmile>, folio: FolioWriter) -> SmileWriter {
        SmileWriter {
            smile,
            folio,
            profile: ProfileWriter::default(),
        }
    }
}

impl PmsWriter for SmileWriter {
    fn lookup_by_confirmation(&self, ext: &str) -> Result<Vec<ExistingFolio>, WriteError> {
        self.smile
            .session(None, |c| self.folio.lookup_by_confirmation(c, ext))
    }

    fn lookup_by_folio_nums(&self, nums: &[Decimal]) -> Result<Vec<ExistingFolio>, WriteError> {
        self.smile
            .session(None, |c| self.folio.lookup_by_folio_nums(c, nums))
    }

    fn write_booking(&self, plan: &BookingPlan) -> Result<Written, WriteError> {
        self.smile
            .session(None, |c| self.folio.write_booking(c, plan))
    }

    fn cancel_folio(&self, folio_num: Decimal) -> Result<(), WriteError> {
        self.smile
            .session(None, |c| self.folio.cancel_folio(c, folio_num).map(|_| ()))
    }

    fn has_posted_ref(&self, folios: &[Decimal], reference: &str) -> Result<bool, WriteError> {
        self.smile
            .session(None, |c| self.folio.has_posted_ref(c, folios, reference))
    }

    fn post_discount(
        &self,
        folio_num: Decimal,
        amount: Decimal,
        code: &str,
        reference: &str,
    ) -> Result<(), WriteError> {
        self.smile.session(None, |c| {
            self.folio
                .post_discount(c, hotel_now(), folio_num, amount, code, reference)
        })
    }

    fn link_folios(&self, link: &LinkFolioPayload) -> Result<(), WriteError> {
        self.smile
            .session(None, |c| self.folio.link_folios(c, link))
    }

    fn ensure_profile(&self, req: &ProfileRequest) -> Result<Decimal, WriteError> {
        self.smile
            .session(None, |c| self.profile.ensure_profile(c, req))
    }

    fn find_travel_agent(&self, code: i64) -> Result<Option<TravelAgent>, WriteError> {
        self.smile.session(None, |c| travel_agent::find(c, code))
    }

    fn report_earn(&self, _booking_id: Uuid, _ext: &str) -> Result<(), WriteError> {
        Err(WriteError::NotPorted("earn"))
    }
}
