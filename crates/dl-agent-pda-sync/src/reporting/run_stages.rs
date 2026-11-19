//! What a verb (enroll, legacy-export, legacy-archive) tells the operator and the cloud
//! while it runs (port of `Reporting/RunStages.cs`): console lines, stage events (followed
//! by a best-effort flush) and progress events (queued only).

use dl_json::Map;

/// `SiteEventLevels.Info`.
pub const LEVEL_INFO: &str = "info";
/// `SiteEventLevels.Warn`.
pub const LEVEL_WARN: &str = "warn";
/// `SiteEventLevels.Error`.
pub const LEVEL_ERROR: &str = "error";

/// The reporting surface of a verb run (`IRunStages`).
pub trait RunStages: Send + Sync {
    /// One line on the console (and in the tools log).
    fn line(&self, text: &str);

    /// A stage finished: queue the event, then try to send what is queued (bounded,
    /// never fails).
    fn stage(&self, level: &str, code: &str, message: &str, detail: Option<Map>);

    /// A progress event inside a stage (per table, per section): queued, sent with the
    /// next stage.
    fn progress(&self, level: &str, code: &str, message: &str, detail: Option<Map>);

    /// A refusal with its own code (`warn` for usage problems, `error` otherwise); it
    /// becomes the run's FAILED code.
    fn fail(&self, code: &str, message: &str, level: &str);

    /// [`fail`](RunStages::fail) at level `error` (the C# default argument).
    fn fail_error(&self, code: &str, message: &str) {
        self.fail(code, message, LEVEL_ERROR);
    }
}

/// Reports nothing (tests and internal callers that run without a tool run).
#[derive(Debug, Default, Clone, Copy)]
pub struct NullRunStages;

impl RunStages for NullRunStages {
    fn line(&self, _text: &str) {}

    fn stage(&self, _level: &str, _code: &str, _message: &str, _detail: Option<Map>) {}

    fn progress(&self, _level: &str, _code: &str, _message: &str, _detail: Option<Map>) {}

    fn fail(&self, _code: &str, _message: &str, _level: &str) {}
}
