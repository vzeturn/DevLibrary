//! `JobResult`, `AckJobRequest`, `RoomFolioMap` and the error classification
//! (`SmileErrorClassifier` on the agent, `SyncErrorClassifier` on the hub).

use dl_decimal::Decimal;
use dl_uuid::Uuid;

use crate::stj::J;

/// `JobResult.ManualPrefix`.
pub const MANUAL_PREFIX: &str = "MANUAL:";
/// `FOLIO_NOT_FOUND`.
pub const FOLIO_NOT_FOUND: &str = "FOLIO_NOT_FOUND";
/// `POSSIBLE_DUPLICATE`.
pub const POSSIBLE_DUPLICATE: &str = "POSSIBLE_DUPLICATE";
/// `LINK_TARGET_CHANGED`.
pub const LINK_TARGET_CHANGED: &str = "LINK_TARGET_CHANGED";
/// `PROFILE_UNAVAILABLE`.
pub const PROFILE_UNAVAILABLE: &str = "PROFILE_UNAVAILABLE";
/// `MANUAL`.
pub const MANUAL: &str = "MANUAL";
/// `EXTRA_BED_LOCKED`.
pub const EXTRA_BED_LOCKED: &str = "EXTRA_BED_LOCKED";
/// `APPLOCK_TIMEOUT`.
pub const APPLOCK_TIMEOUT: &str = "APPLOCK_TIMEOUT";
/// `NETWORK`.
pub const NETWORK: &str = "NETWORK";
/// `UNKNOWN`.
pub const UNKNOWN: &str = "UNKNOWN";

/// The wire error classes (`SyncErrorClass` in `SCREAMING_SNAKE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// Retry later.
    Transient,
    /// The PMS database or its permissions.
    PmsEnvironment,
    /// A person has to act.
    NeedsOperator,
    /// The data does not fit SMILE.
    DataContract,
    /// Anything else.
    Unknown,
}

impl ErrorClass {
    /// The wire name.
    pub fn wire(self) -> &'static str {
        match self {
            ErrorClass::Transient => "TRANSIENT",
            ErrorClass::PmsEnvironment => "PMS_ENVIRONMENT",
            ErrorClass::NeedsOperator => "NEEDS_OPERATOR",
            ErrorClass::DataContract => "DATA_CONTRACT",
            ErrorClass::Unknown => "UNKNOWN",
        }
    }
}

/// `SmileErrorClassifier.Classify` for a SQL Server error number: `SQL_<n>` and its class.
pub fn classify_sql(number: i32) -> (String, ErrorClass) {
    let class = match number {
        208 | 207 | 2812 | 229 | 230 | 916 | 4060 | 18456 | 3906 => ErrorClass::PmsEnvironment,
        1205 | 1222 | -2 | 53 | 64 | 233 | 10053 | 10054 | 11001 => ErrorClass::Transient,
        8152 | 515 | 245 | 8114 | 2627 | 2601 => ErrorClass::DataContract,
        _ => ErrorClass::Unknown,
    };
    (format!("SQL_{number}"), class)
}

/// `SmileErrorClassifier.FromCode`: the class of a named code (trimmed, upper-cased and
/// capped at 64 characters first).
pub fn class_of_code(code: &str) -> (String, ErrorClass) {
    let c: String = code.trim().to_uppercase().chars().take(64).collect();
    if let Some(n) = c.strip_prefix("SQL_").and_then(|n| n.parse::<i32>().ok()) {
        return classify_sql(n);
    }
    let class = match c.as_str() {
        "PMS_IDENTITY_MISMATCH" | "PMS_STALE" | PROFILE_UNAVAILABLE => ErrorClass::PmsEnvironment,
        NETWORK | APPLOCK_TIMEOUT => ErrorClass::Transient,
        MANUAL | FOLIO_NOT_FOUND | POSSIBLE_DUPLICATE | LINK_TARGET_CHANGED | EXTRA_BED_LOCKED => {
            ErrorClass::NeedsOperator
        }
        _ => ErrorClass::Unknown,
    };
    (c, class)
}

/// `RoomFolioMap`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomFolioMap {
    /// `RoomItemId`.
    pub room_item_id: Uuid,
    /// `FolioNum`.
    pub folio_num: Decimal,
    /// `ExtraBedWritten`.
    pub extra_bed_written: Option<i32>,
    /// `ExtraBedFound`.
    pub extra_bed_found: Option<i32>,
    /// `ExtraBedSpecialQtyWritten`.
    pub extra_bed_special_qty_written: Option<i32>,
    /// `ExtraBedSpecialQtyFound`.
    pub extra_bed_special_qty_found: Option<i32>,
}

impl RoomFolioMap {
    /// A map with no extra-bed fields.
    pub fn new(room_item_id: Uuid, folio_num: Decimal) -> RoomFolioMap {
        RoomFolioMap {
            room_item_id,
            folio_num,
            extra_bed_written: None,
            extra_bed_found: None,
            extra_bed_special_qty_written: None,
            extra_bed_special_qty_found: None,
        }
    }

    fn to_j(&self) -> J {
        let i = |v: Option<i32>| J::opt_int(v.map(i64::from));
        J::Obj(vec![
            ("RoomItemId", J::uuid(self.room_item_id)),
            ("FolioNum", J::Dec(self.folio_num)),
            ("ExtraBedWritten", i(self.extra_bed_written)),
            ("ExtraBedFound", i(self.extra_bed_found)),
            (
                "ExtraBedSpecialQtyWritten",
                i(self.extra_bed_special_qty_written),
            ),
            (
                "ExtraBedSpecialQtyFound",
                i(self.extra_bed_special_qty_found),
            ),
        ])
    }
}

/// `AckJobRequest` (`POST api/agent/jobs/ack`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ack {
    /// `JobId`.
    pub job_id: String,
    /// `Success`.
    pub success: bool,
    /// `Error`.
    pub error: Option<String>,
    /// `SmileConfirmNum`.
    pub smile_confirm_num: Option<Decimal>,
    /// `Folios`.
    pub folios: Option<Vec<RoomFolioMap>>,
    /// `RewardGrantId`.
    pub reward_grant_id: Option<Uuid>,
    /// `ErrorDetail`.
    pub error_detail: Option<String>,
    /// `Attempts`: the lease the hub handed out, echoed back.
    pub attempts: Option<i64>,
    /// `ErrorCode`.
    pub error_code: Option<String>,
    /// `ErrorClass`.
    pub error_class: Option<&'static str>,
}

impl Ack {
    fn base(job_id: &str, success: bool, error: Option<String>) -> Ack {
        Ack {
            job_id: job_id.to_owned(),
            success,
            error,
            smile_confirm_num: None,
            folios: None,
            reward_grant_id: None,
            error_detail: None,
            attempts: None,
            error_code: None,
            error_class: None,
        }
    }

    /// `JobResult.Ok`.
    pub fn ok(job_id: &str) -> Ack {
        Ack::base(job_id, true, None)
    }

    /// `JobResult.Ok` with the confirmation number and folios.
    pub fn ok_with(
        job_id: &str,
        confirm: Option<Decimal>,
        folios: Option<Vec<RoomFolioMap>>,
    ) -> Ack {
        Ack {
            smile_confirm_num: confirm,
            folios,
            ..Ack::ok(job_id)
        }
    }

    /// `JobResult.Ok(jobId, rewardGrantId: g)`.
    pub fn ok_reward(job_id: &str, grant: Uuid) -> Ack {
        Ack {
            reward_grant_id: Some(grant),
            ..Ack::ok(job_id)
        }
    }

    /// `JobResult.Fail`.
    pub fn fail(job_id: &str, error: &str, code: Option<String>, class: Option<ErrorClass>) -> Ack {
        Ack {
            error_code: code,
            error_class: class.map(ErrorClass::wire),
            ..Ack::base(job_id, false, Some(error.to_owned()))
        }
    }

    /// `JobResult.Manual`.
    pub fn manual(job_id: &str, reason: &str) -> Ack {
        Ack::needs_operator(job_id, MANUAL, reason)
    }

    /// `JobResult.NeedsOperator`.
    pub fn needs_operator(job_id: &str, code: &str, reason: &str) -> Ack {
        Ack {
            error_code: Some(code.to_owned()),
            error_class: Some(ErrorClass::NeedsOperator.wire()),
            ..Ack::base(job_id, false, Some(format!("{MANUAL_PREFIX} {reason}")))
        }
    }

    /// The ack with `Attempts` stamped (`ack with { Attempts = job.Attempts }`).
    pub fn with_attempts(self, attempts: i64) -> Ack {
        Ack {
            attempts: Some(attempts),
            ..self
        }
    }

    /// The body, in `AckJobRequest` declaration order.
    pub fn to_j(&self) -> J {
        J::Obj(vec![
            ("JobId", J::str(&self.job_id)),
            ("Success", J::Bool(self.success)),
            ("Error", J::opt_str(self.error.as_deref())),
            ("SmileConfirmNum", J::opt_dec(self.smile_confirm_num)),
            (
                "Folios",
                self.folios.as_ref().map_or(J::Null, |f| {
                    J::Arr(f.iter().map(RoomFolioMap::to_j).collect())
                }),
            ),
            ("RewardGrantId", J::opt_uuid(self.reward_grant_id)),
            ("ErrorDetail", J::opt_str(self.error_detail.as_deref())),
            ("Attempts", J::opt_int(self.attempts)),
            ("ErrorCode", J::opt_str(self.error_code.as_deref())),
            ("ErrorClass", J::opt_str(self.error_class)),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_numbers_map_to_the_hub_classes() {
        assert_eq!(
            classify_sql(208),
            ("SQL_208".into(), ErrorClass::PmsEnvironment)
        );
        assert_eq!(classify_sql(1205).1, ErrorClass::Transient);
        assert_eq!(classify_sql(-2).1, ErrorClass::Transient);
        assert_eq!(classify_sql(2627).1, ErrorClass::DataContract);
        assert_eq!(classify_sql(50000).1, ErrorClass::Unknown);
    }

    #[test]
    fn named_codes_are_normalised_then_classed() {
        assert_eq!(
            class_of_code(" pms_stale "),
            ("PMS_STALE".into(), ErrorClass::PmsEnvironment)
        );
        assert_eq!(class_of_code("APPLOCK_TIMEOUT").1, ErrorClass::Transient);
        assert_eq!(
            class_of_code("EXTRA_BED_LOCKED").1,
            ErrorClass::NeedsOperator
        );
        assert_eq!(class_of_code("SQL_1222").1, ErrorClass::Transient);
        assert_eq!(class_of_code("whatever").1, ErrorClass::Unknown);
        assert_eq!(class_of_code(&"X".repeat(80)).0.len(), 64);
    }

    #[test]
    fn manual_ack_matches_the_dotnet_shape() {
        let a = Ack::manual("j1", "folio đã khoá").with_attempts(3);
        assert_eq!(
            a.to_j().to_json(),
            "{\"JobId\":\"j1\",\"Success\":false,\"Error\":\"MANUAL: folio \\u0111\\u00E3 kho\\u00E1\",\
             \"SmileConfirmNum\":null,\"Folios\":null,\"RewardGrantId\":null,\"ErrorDetail\":null,\
             \"Attempts\":3,\"ErrorCode\":\"MANUAL\",\"ErrorClass\":\"NEEDS_OPERATOR\"}"
        );
    }

    #[test]
    fn ok_ack_carries_folios_in_declaration_order() {
        let id: Uuid = "6f1c2d3e-0000-4000-8000-000000000001".parse().unwrap();
        let mut m = RoomFolioMap::new(id, "1234".parse().unwrap());
        m.extra_bed_found = Some(1);
        let a = Ack::ok_with("j2", Some("77".parse().unwrap()), Some(vec![m]));
        assert_eq!(
            a.to_j().to_json(),
            "{\"JobId\":\"j2\",\"Success\":true,\"Error\":null,\"SmileConfirmNum\":77,\"Folios\":[{\
             \"RoomItemId\":\"6f1c2d3e-0000-4000-8000-000000000001\",\"FolioNum\":1234,\
             \"ExtraBedWritten\":null,\"ExtraBedFound\":1,\"ExtraBedSpecialQtyWritten\":null,\
             \"ExtraBedSpecialQtyFound\":null}],\"RewardGrantId\":null,\"ErrorDetail\":null,\
             \"Attempts\":null,\"ErrorCode\":null,\"ErrorClass\":null}"
        );
    }
}
