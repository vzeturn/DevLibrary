//! [`PmsWriter`] over the SMILE connection: every operation runs on the shared
//! [`TdsSmile`] session (connect, command timeout and broken-session handling).

use std::sync::Arc;

use dl_decimal::Decimal;
use dl_uuid::Uuid;

use super::folio::FolioWriter;
use super::profile::ProfileWriter;
use super::travel_agent;
use crate::earn::pairs::PairSource;
use crate::earn::wire::EarnReport;
use crate::earn::{self, EarnEnv};
use crate::hub::earn::EarnHub;
use crate::jobs::payload::LinkFolioPayload;
use crate::jobs::plan::{BookingPlan, PmsWriter, ProfileRequest, WriteError, Written};
use crate::jobs::room_diff::ExistingFolio;
use crate::jobs::travel_agent::TravelAgent;
use crate::log::{self, V};
use crate::smile::TdsSmile;
use crate::smile::hotel_now;

/// The SMILE writer the dispatcher drives.
#[derive(Debug)]
pub struct SmileWriter {
    smile: Arc<TdsSmile>,
    folio: FolioWriter,
    profile: ProfileWriter,
    earn: Option<EarnPath>,
}

/// What `HandleEarnAsync` needs besides SMILE: the earn pairs, the hub and the identity.
pub struct EarnPath {
    /// `IEarnPairSource`.
    pub pairs: PairSource,
    /// `HubClient.ReportEarnAsync`.
    pub hub: Arc<dyn EarnHub>,
    /// The identity fingerprint and the hub's extra-bed code.
    pub env: Arc<dyn EarnEnv>,
}

impl EarnPath {
    /// `HubClient.ReportEarnAsync`: stamps the identity fingerprint and posts the report.
    ///
    /// # Errors
    /// The hub refused it (or could not be reached): the job fails and is retried.
    pub fn report(&self, mut report: EarnReport) -> Result<(), WriteError> {
        if report.db_identity_fingerprint.is_none() {
            report.db_identity_fingerprint = self.env.fingerprint();
        }
        self.hub.report_earn(&report)?;
        log::info(
            "EARN_POINTS: revenue reported to the hub",
            &[
                ("booking", V::S(&report.booking_id.to_string())),
                ("revenue", V::S(&report.qualifying_revenue.to_string())),
            ],
        );
        Ok(())
    }
}

impl std::fmt::Debug for EarnPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EarnPath")
            .field("pairs", &self.pairs)
            .finish_non_exhaustive()
    }
}

impl SmileWriter {
    /// A writer on `smile` with the folio options.
    pub fn new(smile: Arc<TdsSmile>, folio: FolioWriter) -> SmileWriter {
        SmileWriter {
            smile,
            folio,
            profile: ProfileWriter::default(),
            earn: None,
        }
    }

    /// Reports `EarnPoints` jobs through `earn` (without it they are left un-acked).
    pub fn with_earn(mut self, earn: EarnPath) -> SmileWriter {
        self.earn = Some(earn);
        self
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

    /// `HandleEarnAsync` after the external key: SMILE reads on the writer's session, then
    /// `POST api/agent/earn` (204, or 202 held) outside it.
    fn report_earn(&self, booking_id: Uuid, ext: &str) -> Result<(), WriteError> {
        let Some(e) = &self.earn else {
            return Err(WriteError::NotPorted("earn"));
        };
        let hub_code = e.env.hub_extra_bed_code();
        let report = self.smile.session(None, |c| {
            earn::job::read(c, &e.pairs, hub_code.as_deref(), booking_id, ext)
        })?;
        e.report(report)
    }
}
