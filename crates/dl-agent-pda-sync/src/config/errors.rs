//! Stable error codes (port of `Config/AgentErrors.cs`) and the agent's coded failure
//! (`AgentException`).

use std::fmt;
use std::sync::Arc;

/// Stable error codes surfaced to the console / installer (`AgentErrorCodes`).
pub mod codes {
    /// No SQL Server instance found.
    pub const SQL_INSTANCE_NOT_FOUND: &str = "SQL_INSTANCE_NOT_FOUND";
    /// The SQL setup login has no access.
    pub const SQL_NO_ACCESS: &str = "SQL_NO_ACCESS";
    /// The POS database does not exist.
    pub const SQL_DATABASE_NOT_FOUND: &str = "SQL_DATABASE_NOT_FOUND";
    /// The database does not look like SMILE_POS.
    pub const FINGERPRINT_MISMATCH: &str = "FINGERPRINT_MISMATCH";
    /// The enrollment code was refused.
    pub const ENROLL_CODE_INVALID: &str = "ENROLL_CODE_INVALID";
    /// Enrollment failed for another reason.
    pub const ENROLL_FAILED: &str = "ENROLL_FAILED";
    /// The cloud could not be reached.
    pub const CLOUD_UNREACHABLE: &str = "CLOUD_UNREACHABLE";
    /// The secret store failed.
    pub const KEY_STORE_FAILED: &str = "KEY_STORE_FAILED";
    /// This machine is not enrolled.
    pub const NOT_ENROLLED: &str = "NOT_ENROLLED";
    /// The agent must be enrolled again.
    pub const REENROLL_REQUIRED: &str = "REENROLL_REQUIRED";
    /// Invalid command line.
    pub const BAD_ARGUMENTS: &str = "BAD_ARGUMENTS";

    /// Capture: SQL Server cannot be reached.
    pub const SQL_UNREACHABLE: &str = "SQL_UNREACHABLE";
    /// Capture: `pda_sync` cannot sign in.
    pub const SQL_LOGIN_FAILED: &str = "SQL_LOGIN_FAILED";
    /// Capture: SQL Server is busy (timeouts, deadlocks).
    pub const SQL_BUSY: &str = "SQL_BUSY";
    /// Capture: the schema differs from the expected one.
    pub const SQL_SCHEMA_MISMATCH: &str = "SQL_SCHEMA_MISMATCH";
    /// Capture: another SQL error.
    pub const SQL_ERROR: &str = "SQL_ERROR";
    /// Capture paused: the outbox is full.
    pub const OUTBOX_FULL: &str = "OUTBOX_FULL";
    /// The blob queue is full.
    pub const BLOB_QUEUE_FULL: &str = "BLOB_QUEUE_FULL";
    /// A local SQLite store failed.
    pub const LOCAL_STORE_FAILED: &str = "LOCAL_STORE_FAILED";
    /// `Agent:SiteTimeZone` names no known zone.
    pub const SITE_TIME_ZONE_INVALID: &str = "SITE_TIME_ZONE_INVALID";
    /// Capture failed for another reason.
    pub const CAPTURE_FAILED: &str = "CAPTURE_FAILED";

    /// legacy-export: the legacy site is still live (ADR 0085).
    pub const LEGACY_SITE_LIVE: &str = "LEGACY_SITE_LIVE";
    /// legacy-export `take_offline`: no site root given and not exactly one legacy PDA web
    /// app found in IIS.
    pub const LEGACY_SITE_UNRESOLVED: &str = "LEGACY_SITE_UNRESOLVED";
    /// legacy-export `take_offline`: `app_offline.htm` could not be written or removed.
    pub const LEGACY_SITE_OFFLINE_FAILED: &str = "LEGACY_SITE_OFFLINE_FAILED";
    /// legacy-export failed.
    pub const LEGACY_EXPORT_FAILED: &str = "LEGACY_EXPORT_FAILED";
    /// No `pda.db` at the given path (ADR 0093).
    pub const LEGACY_STORE_MISSING: &str = "LEGACY_STORE_MISSING";
    /// The legacy VietQR registry values could not be read or decrypted (ADR 0099).
    pub const LEGACY_VIETQR_UNREADABLE: &str = "LEGACY_VIETQR_UNREADABLE";
    /// `--vietqr-only` without a VietQR account in the legacy registry (ADR 0099).
    pub const LEGACY_VIETQR_NOT_SET: &str = "LEGACY_VIETQR_NOT_SET";

    /// legacy-archive: the bundle is invalid (ADR 0089).
    pub const LEGACY_BUNDLE_INVALID: &str = "LEGACY_BUNDLE_INVALID";
    /// legacy-archive was refused.
    pub const LEGACY_ARCHIVE_REFUSED: &str = "LEGACY_ARCHIVE_REFUSED";
    /// legacy-archive failed.
    pub const LEGACY_ARCHIVE_FAILED: &str = "LEGACY_ARCHIVE_FAILED";

    /// legacy-upload: the cloud refused the upload (ADR 0095).
    pub const LEGACY_UPLOAD_REFUSED: &str = "LEGACY_UPLOAD_REFUSED";
    /// legacy-upload: the bundle is larger than the cloud accepts.
    pub const LEGACY_UPLOAD_TOO_LARGE: &str = "LEGACY_UPLOAD_TOO_LARGE";
    /// legacy-upload failed.
    pub const LEGACY_UPLOAD_FAILED: &str = "LEGACY_UPLOAD_FAILED";
    /// legacy-upload: the cloud answered the commit without committing and without a refusal.
    pub const LEGACY_UPLOAD_NOT_COMMITTED: &str = "LEGACY_UPLOAD_NOT_COMMITTED";

    /// Enroll preflight: the instance accepts Windows logins only.
    pub const SQL_WINDOWS_AUTH_ONLY: &str = "SQL_WINDOWS_AUTH_ONLY";
    /// Enroll preflight: the instance is too old.
    pub const SQL_SERVER_TOO_OLD: &str = "SQL_SERVER_TOO_OLD";
    /// Enroll preflight: `pda_sync` could not sign in or read after the bootstrap.
    pub const SQL_LOGIN_VERIFY_FAILED: &str = "SQL_LOGIN_VERIFY_FAILED";
    /// Enroll preflight: the cloud host name does not resolve.
    pub const CLOUD_DNS_FAILED: &str = "CLOUD_DNS_FAILED";
    /// Enroll preflight: TLS to the cloud failed.
    pub const CLOUD_TLS_FAILED: &str = "CLOUD_TLS_FAILED";
    /// `GET /health/ready` answered, but not with 2xx.
    pub const CLOUD_NOT_READY: &str = "CLOUD_NOT_READY";
    /// Clock differs from the cloud by more than 2 min (warning).
    pub const CLOCK_SKEW: &str = "CLOCK_SKEW";
    /// Clock differs from the cloud by more than 5 min.
    pub const CLOCK_SKEW_TOO_LARGE: &str = "CLOCK_SKEW_TOO_LARGE";
    /// The cloud answer had no `Date` header (warning).
    pub const CLOCK_SKEW_UNKNOWN: &str = "CLOCK_SKEW_UNKNOWN";

    /// `grants` ended without a more specific code.
    pub const GRANTS_FAILED: &str = "GRANTS_FAILED";
    /// `pda_sync` lacks the order-verb grants.
    pub const SQL_GRANTS_MISSING: &str = "SQL_GRANTS_MISSING";
    /// The service applied the missing grants itself (AG1).
    pub const SQL_GRANTS_REPAIRED: &str = "SQL_GRANTS_REPAIRED";
    /// A verb was refused because a store column is missing (OW1e, ADR 0112).
    pub const SCHEMA_UNSUPPORTED: &str = "SCHEMA_UNSUPPORTED";

    /// A site command was executed with an unknown outcome.
    pub const COMMAND_REPLAY_FAILED: &str = "COMMAND_REPLAY_FAILED";
    /// The cloud refused a batch for good; it is quarantined (AD1).
    pub const SYNC_BATCH_QUARANTINED: &str = "SYNC_BATCH_QUARANTINED";
    /// The cloud expects a seq no sendable batch carries (AD1).
    pub const SYNC_STREAM_GAP: &str = "SYNC_STREAM_GAP";
    /// A seq gap was healed by renumbering (AD1).
    pub const SYNC_STREAM_RECOVERED: &str = "SYNC_STREAM_RECOVERED";
    /// Renumbering the stream failed (AD1).
    pub const SYNC_STREAM_RECOVERY_FAILED: &str = "SYNC_STREAM_RECOVERY_FAILED";
    /// A stream waited for parent rows of another stream (ADR 0102).
    pub const SYNC_PARENT_PENDING: &str = "SYNC_PARENT_PENDING";
    /// A targeted resnapshot was scheduled (ADR 0102).
    pub const SYNC_STREAM_RESNAPSHOT: &str = "SYNC_STREAM_RESNAPSHOT";
    /// One capture table fails with a schema error and is skipped (AD1).
    pub const CAPTURE_TABLE_SKIPPED: &str = "CAPTURE_TABLE_SKIPPED";
    /// A skipped capture table reads again (AD1).
    pub const CAPTURE_TABLE_RECOVERED: &str = "CAPTURE_TABLE_RECOVERED";
}

/// The .NET type name of `AgentException`, used where an exception is written to a log.
pub const AGENT_EXCEPTION_TYPE: &str = "MM.PdaOrder.SyncAgent.Config.AgentException";

/// A failure with a stable [`code`](AgentError::code) and a human hint (`AgentException`).
#[derive(Clone)]
pub struct AgentError {
    code: String,
    message: String,
    source: Option<Arc<dyn std::error::Error + Send + Sync>>,
}

impl AgentError {
    /// A coded failure.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> AgentError {
        AgentError {
            code: code.into(),
            message: message.into(),
            source: None,
        }
    }

    /// A coded failure caused by `source` (the C# inner exception).
    pub fn with_source(
        code: impl Into<String>,
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> AgentError {
        AgentError {
            code: code.into(),
            message: message.into(),
            source: Some(Arc::new(source)),
        }
    }

    /// The stable code, e.g. [`codes::CLOUD_UNREACHABLE`].
    pub fn code(&self) -> &str {
        &self.code
    }

    /// The message (the hint shown to the operator).
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Debug for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentError")
            .field("code", &self.code)
            .field("message", &self.message)
            .field("source", &self.source.as_ref().map(|s| s.to_string()))
            .finish()
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AgentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|s| s as &(dyn std::error::Error + 'static))
    }
}
