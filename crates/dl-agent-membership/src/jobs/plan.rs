//! What the dispatcher hands the SMILE writers ([`PmsWriter`]) and the pure rules it
//! applies on the way (voucher amount, reward reference, error code and class).

use std::fmt;

use dl_decimal::Decimal;
use dl_uuid::Uuid;

use super::payload::{Contact, LinkFolioPayload, Room};
use super::preflight::PreflightInputs;
use super::result::{
    ErrorClass, NETWORK, PROFILE_UNAVAILABLE, UNKNOWN, class_of_code, classify_sql,
};
use super::room_diff::ExistingFolio;
use super::travel_agent::TravelAgent;
use crate::hub::HubError;
use crate::smile::SmileError;

/// `EarnOptions.MaxTransactionCodeLength`.
pub const MAX_TRANSACTION_CODE_LENGTH: usize = 5;

/// Why a writer call did not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteError {
    /// SMILE failed (the transaction, if any, was rolled back).
    Smile(SmileError),
    /// `PmsWriteRefusedException`: a safety check refused the write.
    Refused {
        /// The stable code (`POSSIBLE_DUPLICATE`, `LINK_TARGET_CHANGED`, ...).
        code: String,
        /// The operator-facing message.
        message: String,
    },
    /// A failure with a named code (`APPLOCK_TIMEOUT`, `PMS_EARN_ROOM_CODE_AMBIGUOUS`).
    Coded {
        /// The code.
        code: String,
        /// The message.
        message: String,
    },
    /// The hub refused a report the job sends.
    Hub(String),
    /// This writer path is not ported yet: leave the job for a later delivery.
    NotPorted(&'static str),
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WriteError::Smile(e) => write!(f, "{e}"),
            WriteError::Refused { code, message } => write!(f, "{code}: {message}"),
            WriteError::Coded { code, message } => write!(f, "{code}: {message}"),
            WriteError::Hub(m) => write!(f, "hub: {m}"),
            WriteError::NotPorted(what) => write!(f, "{what} is not ported to the Rust agent"),
        }
    }
}

impl std::error::Error for WriteError {}

impl From<SmileError> for WriteError {
    fn from(e: SmileError) -> WriteError {
        WriteError::Smile(e)
    }
}

impl From<HubError> for WriteError {
    fn from(e: HubError) -> WriteError {
        WriteError::Hub(e.to_string())
    }
}

/// `FolioWriteContext`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteContext {
    /// `ConfirmationCode` (`ExternalConfirmNumber`, the idempotency key).
    pub confirmation_code: String,
    /// `BookingId` (`ExternalFolioNumber`).
    pub booking_id: Uuid,
    /// `OriginCode`.
    pub origin_code: String,
    /// `MemberId`.
    pub member_id: Option<Uuid>,
    /// `ForceCreate`.
    pub force_create: bool,
    /// `Note` (special requests, Unicode).
    pub note: Option<String>,
    /// `TravelAgent`.
    pub travel_agent: Option<TravelAgent>,
}

/// `BookingWritePlan`: every room of one booking, written in one SMILE transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookingPlan {
    /// `Ctx`.
    pub ctx: WriteContext,
    /// `SharedConfirmNum`.
    pub shared_confirm_num: Option<Decimal>,
    /// `ProfileNumber`.
    pub profile_number: Decimal,
    /// `Creates`.
    pub creates: Vec<Room>,
    /// `Updates` (`FolioNum`, room).
    pub updates: Vec<(Decimal, Room)>,
    /// `Cancels`.
    pub cancels: Vec<Decimal>,
    /// `NoteFolios`: folios whose note is rewritten (`None`: the note is not touched).
    pub note_folios: Option<Vec<Decimal>>,
    /// `ExtraBed`: the `SMILE_SPECIAL` preflight inputs (`None`: every room folds).
    pub extra_bed: Option<PreflightInputs>,
}

/// `BookingWriteResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    /// `Created` (`RoomItemId`, `FolioNum`), in payload order.
    pub created: Vec<(Uuid, Decimal)>,
    /// `ConfirmNum`.
    pub confirm_num: Option<Decimal>,
}

/// `ProfileMemberKey` plus the names and contact `EnsureProfileAsync` writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileRequest {
    /// `MemberId`.
    pub member_id: Uuid,
    /// `MemberCode`.
    pub member_code: Option<String>,
    /// Last name (Unicode).
    pub last_name: String,
    /// First name (Unicode).
    pub first_name: String,
    /// The member's contact.
    pub contact: Option<Contact>,
}

/// Every SMILE operation a job needs (`IFolioWriter`, `IProfileWriter`,
/// `ITravelAgentReader` and the earn reads).
pub trait PmsWriter: Send + Sync {
    /// `LookupByConfirmationAsync`.
    fn lookup_by_confirmation(&self, ext: &str) -> Result<Vec<ExistingFolio>, WriteError>;
    /// `LookupByFolioNumsAsync`.
    fn lookup_by_folio_nums(&self, nums: &[Decimal]) -> Result<Vec<ExistingFolio>, WriteError>;
    /// `ReadExtraBedInputsAsync`: the `SMILE_SPECIAL` preflight inputs, read right before a
    /// booking write; `None` in `FOLD_LEGACY` mode (and for writers without SMILE).
    fn read_extra_bed_inputs(
        &self,
        _verified_fingerprint: Option<&str>,
        _special_code: Option<&str>,
    ) -> Result<Option<PreflightInputs>, WriteError> {
        Ok(None)
    }
    /// `WriteBookingAsync`.
    fn write_booking(&self, plan: &BookingPlan) -> Result<Written, WriteError>;
    /// `CancelFolioAsync`.
    fn cancel_folio(&self, folio_num: Decimal) -> Result<(), WriteError>;
    /// `HasPostedRefAsync`.
    fn has_posted_ref(&self, folios: &[Decimal], reference: &str) -> Result<bool, WriteError>;
    /// `PostDiscountAsync`.
    fn post_discount(
        &self,
        folio_num: Decimal,
        amount: Decimal,
        code: &str,
        reference: &str,
    ) -> Result<(), WriteError>;
    /// `LinkFoliosAsync`.
    fn link_folios(&self, link: &LinkFolioPayload) -> Result<(), WriteError>;
    /// `EnsureProfileAsync`: the member's `ProfileNumber`.
    fn ensure_profile(&self, req: &ProfileRequest) -> Result<Decimal, WriteError>;
    /// `TravelAgentReader.FindAsync`.
    fn find_travel_agent(&self, code: i64) -> Result<Option<TravelAgent>, WriteError>;
    /// `HandleEarnAsync` after the external key: read the stay's revenue and report it.
    fn report_earn(&self, booking_id: Uuid, ext: &str) -> Result<(), WriteError>;
}

/// A profile failure is a [`WriteError`] the dispatcher answers itself.
pub(crate) type ProfileFailure = WriteError;

pub(crate) fn profile_unavailable(sql: i32) -> WriteError {
    WriteError::Coded {
        code: format!("{PROFILE_UNAVAILABLE}#{sql}"),
        message: format!("{PROFILE_UNAVAILABLE}: SQL {sql}"),
    }
}

/// `FolioRefNumbers.Reward`: `RWD:` + the grant's .NET bytes in unpadded base64url.
pub fn reward_ref(grant_id: Uuid) -> String {
    format!(
        "RWD:{}",
        dl_encoding::base64::URL_SAFE_NO_PAD.encode(grant_id.to_bytes_le())
    )
}

/// `VoucherDiscountRule.Compute` for `PercentOff`: `round(total × value)` (whole đồng, half
/// away from zero; `value` is a fraction), capped by the truncated `MaxDiscountAmount`,
/// then clamped to `0..=max(0, total)`.
pub fn percent_discount(value: Decimal, max: Option<Decimal>, total: Decimal) -> Decimal {
    let raw = (total * value).round(0);
    let capped = match max {
        Some(m) => raw.min(m.trunc(0).max(Decimal::ZERO)),
        None => raw,
    };
    capped.max(Decimal::ZERO).min(total.max(Decimal::ZERO))
}

/// `ExtractRuleString`: a string property of the reward rule; `None` on bad JSON.
pub(crate) fn rule_string(rule: Option<&str>, name: &str) -> Option<String> {
    let rule = rule.filter(|r| !r.trim().is_empty())?;
    let v = crate::wire::parse::json(rule.as_bytes()).ok()?;
    match v {
        dl_json::Value::Object(map) => map.iter().find_map(|(k, v)| match v {
            dl_json::Value::String(s) if k == name => Some(s.clone()),
            _ => None,
        }),
        _ => None,
    }
}

/// The code and class of a failure (`SmileErrorClassifier.Classify`).
pub(crate) fn classify(e: &WriteError) -> (String, ErrorClass) {
    match e {
        WriteError::Smile(SmileError::Server(n)) => classify_sql(*n),
        WriteError::Smile(s) if s.is_unavailable() => (NETWORK.into(), ErrorClass::Transient),
        WriteError::Coded { code, .. } if code.starts_with(PROFILE_UNAVAILABLE) => {
            // A deadlock or lock timeout stays its own transient SQL code.
            let n = code
                .split('#')
                .nth(1)
                .and_then(|n| n.parse::<i32>().ok())
                .unwrap_or(0);
            let inner = classify_sql(n);
            if inner.1 == ErrorClass::Transient {
                inner
            } else {
                class_of_code(PROFILE_UNAVAILABLE)
            }
        }
        WriteError::Coded { code, .. } | WriteError::Refused { code, .. } => class_of_code(code),
        WriteError::Hub(_) => (NETWORK.into(), ErrorClass::Transient),
        WriteError::Smile(_) | WriteError::NotPorted(_) => (UNKNOWN.into(), ErrorClass::Unknown),
    }
}

/// `ExceptionDetailFormatter.Summarize` without server text: SQL errors as `SQL n`.
pub(crate) fn summary(e: &WriteError) -> String {
    let s = match e {
        WriteError::Smile(SmileError::Server(n)) => format!("SQL {n}"),
        WriteError::Coded { message, .. } => message.clone(),
        other => other.to_string(),
    };
    s.chars().take(500).collect()
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
