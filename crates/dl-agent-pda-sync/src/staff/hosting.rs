//! Port of `Staff/StaffSyncHosting.cs`: the staff sync loop and its wiring.

use super::cloud::{StaffCloud, StaffCloudClient};
use super::pin_key::PinKeyProvider;
use super::runner::{StaffSyncParts, StaffSyncRunner};
use super::source::{SqlStaffSource, StaffSource};
use crate::hosting::cli::Unhandled;
use crate::hosting::host::AgentServices;
use crate::hosting::services::{LoopService, ServiceError};
use crate::outbox::CaptureStatus;
use crate::util::ctx::Ctx;
use std::sync::Arc;
use std::time::Duration;

/// The state file in the data folder (`StaffSyncRegistration.StateFileName`).
pub const STATE_FILE_NAME: &str = "staff-sync.json";
/// The first cycle runs this long after start.
pub const FIRST_DELAY: Duration = Duration::from_secs(30);
/// Then every this long.
pub const INTERVAL: Duration = Duration::from_secs(120);

/// The staff sync loop (`StaffSyncService`, ADR 0101): a first cycle shortly after start,
/// then every [`INTERVAL`]. A failing cycle restarts the loop with backoff.
pub struct StaffSyncService {
    runner: Arc<StaffSyncRunner>,
    first_delay: Duration,
    interval: Duration,
}

impl std::fmt::Debug for StaffSyncService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaffSyncService").finish_non_exhaustive()
    }
}

impl StaffSyncService {
    /// The loop over `runner` with the C# delays.
    pub fn new(runner: Arc<StaffSyncRunner>) -> StaffSyncService {
        StaffSyncService::with_delays(runner, FIRST_DELAY, INTERVAL)
    }

    /// The loop with other delays (tests).
    pub fn with_delays(
        runner: Arc<StaffSyncRunner>,
        first_delay: Duration,
        interval: Duration,
    ) -> StaffSyncService {
        StaffSyncService {
            runner,
            first_delay,
            interval,
        }
    }
}

impl LoopService for StaffSyncService {
    fn name(&self) -> &'static str {
        "StaffSync"
    }

    fn run_loop(&mut self, ctx: &Ctx) -> Result<(), ServiceError> {
        if !ctx.sleep(self.first_delay) {
            return Ok(());
        }
        while !ctx.is_done() {
            self.runner.run_once(ctx)?;
            if !ctx.sleep(self.interval) {
                return Ok(());
            }
        }
        Ok(())
    }
}

/// `AddStaffSync`: the runner over the runtime's signed client, token provider and secret
/// store, as a [`RuntimeHooks::after_capture`](crate::hosting::host::RuntimeHooks) factory.
pub fn services(
    s: &AgentServices,
    _capture: &Arc<dyn CaptureStatus>,
) -> Result<Vec<Box<dyn LoopService>>, Unhandled> {
    Ok(vec![Box::new(StaffSyncService::new(Arc::new(runner(s))))])
}

/// The staff sync runner of the running agent.
pub fn runner(s: &AgentServices) -> StaffSyncRunner {
    let identity = s.identity.clone();
    let secrets = s.secrets.clone();
    let source: Arc<dyn StaffSource> = Arc::new(SqlStaffSource::new(move || {
        crate::config::sql::agent_connection_for(&identity, &*secrets)
            .map_err(|e| Box::new(e) as super::StaffError)
    }));
    let cloud: Arc<dyn StaffCloud> =
        Arc::new(StaffCloudClient::new(s.http.clone(), s.tokens.clone()));
    let keys = Arc::new(PinKeyProvider::new(s.secrets.clone(), cloud.clone()));
    StaffSyncRunner::new(
        StaffSyncParts {
            source,
            cloud,
            keys,
            state_file: s.paths.data_dir().join(STATE_FILE_NAME),
            agent_id: s.identity.agent_id.to_string(),
            agent_version: crate::hosting::agent_version(),
            clock: s.clock.clone(),
        },
        &s.log,
    )
}
