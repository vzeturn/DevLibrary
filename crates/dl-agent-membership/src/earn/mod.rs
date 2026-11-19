//! Earn reporting: the `EarnPoints` job (`JobDispatcher.HandleEarnAsync`) and the reads,
//! bodies and hub calls the checkout scanner shares with it.
//!
//! Every SMILE statement here only reads. The reports go to the hub stamped with the
//! current SMILE identity fingerprint (`HubClient` does that in .NET; here the caller
//! fills [`wire::EarnReport::db_identity_fingerprint`] from the identity tracker).

pub mod job;
pub mod pairs;
pub mod sql;
pub mod wire;

#[cfg(test)]
mod tests;

/// What the earn paths need from the running module besides SMILE and the hub.
pub trait EarnEnv: Send + Sync {
    /// `IPmsIdentitySource.CurrentFingerprint`.
    fn fingerprint(&self) -> Option<String>;
    /// The hub's extra-bed special code from the last sync-config (`HubExtraBedSpecialCode`).
    fn hub_extra_bed_code(&self) -> Option<String>;
}

/// The module's [`EarnEnv`]: the identity tracker and the last sync-config.
pub struct ModuleEnv {
    /// The identity tracker.
    pub tracker: std::sync::Arc<crate::identity::Tracker>,
    /// The shared module state (the last sync-config).
    pub shared: std::sync::Arc<crate::module::state::Shared>,
}

impl EarnEnv for ModuleEnv {
    fn fingerprint(&self) -> Option<String> {
        self.tracker.current_fingerprint()
    }

    fn hub_extra_bed_code(&self) -> Option<String> {
        self.shared
            .sync_config()
            .and_then(|s| s.extra_bed_special_code)
            .map(|c| c.trim().to_owned())
            .filter(|c| !c.is_empty())
    }
}
