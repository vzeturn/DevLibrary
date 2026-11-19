//! The command loops of the running agent (`AgentHost.AddAgentRuntime` registrations of
//! `SqlSiteCommandStore`, `ResnapshotCommandHandler`, `BootstrapGrantsRepair`,
//! `OrderGrantsMonitor`, `TableSchemaGuard`, `SiteCommandExecutor`, `CommandChannel`, and
//! the hosted `CommandService` and `OrderGrantsService`).

use std::sync::Arc;

use super::channel::{CommandChannel, CommandChannelParts};
use super::executor::SiteCommandExecutor;
use super::grants::{BootstrapGrantsRepair, GrantsRepair, OrderGrantsMonitor, OrderGrantsService};
use super::resnapshot::ResnapshotCommandHandler;
use super::schema_guard::{LogOnlyEvents, TableSchemaGuard};
use super::sql_store::{ConfigSource, SqlSiteCommandStore};
use crate::bootstrap::{SqlAdminCredential, SqlOrderGrantsProbe};
use crate::contracts::SyncCatalog;
use crate::hosting::cli::Unhandled;
use crate::hosting::host::AgentServices;
use crate::hosting::services::{LoopService, ServiceError};
use crate::outbox::CaptureStatus;
use crate::reporting::{Logger, SiteEvents};
use crate::util::ctx::Ctx;

/// `CommandService`: the command channel as a resilient loop (`Commands`).
pub struct CommandService {
    channel: Arc<CommandChannel>,
}

impl CommandService {
    /// The loop of `channel`.
    pub fn new(channel: Arc<CommandChannel>) -> CommandService {
        CommandService { channel }
    }
}

impl LoopService for CommandService {
    fn name(&self) -> &'static str {
        "Commands"
    }

    fn run_loop(&mut self, ctx: &Ctx) -> Result<(), ServiceError> {
        self.channel
            .run(ctx)
            .map_err(|e| Box::new(e) as ServiceError)
    }
}

/// What the commands need from the host beyond [`AgentServices`]: the site events (the
/// schema guard and the grants monitor report through them) and the capture nudge
/// (read-your-writes). `None` falls back to logging the events and not nudging.
#[derive(Default, Clone)]
pub struct CommandExtras {
    /// The host's site events.
    pub events: Option<Arc<dyn SiteEvents>>,
    /// The capture runner's nudge.
    pub nudge: Option<Arc<dyn crate::capture::CaptureNudge>>,
}

/// The executor, channel and grants check of the running agent.
pub fn build(
    s: &AgentServices,
    extras: &CommandExtras,
) -> Result<Vec<Box<dyn LoopService>>, Unhandled> {
    let log: &Logger = &s.log;
    let events: Arc<dyn SiteEvents> = extras
        .events
        .clone()
        .unwrap_or_else(|| Arc::new(LogOnlyEvents(log.for_category(super::executor::CATEGORY))));

    let identity = s.identity.clone();
    let secrets = s.secrets.clone();
    let config: ConfigSource = Arc::new(move || {
        crate::config::sql::agent_connection_for(&identity, &*secrets).map_err(Into::into)
    });
    let store = Arc::new(SqlSiteCommandStore::new(config));

    let catalog = SyncCatalog::load().map_err(|e| e.to_string())?;
    let resnapshot = Arc::new(ResnapshotCommandHandler::new(
        s.outbox.clone(),
        s.clock.clone(),
        log,
        catalog,
    ));
    let repair: Option<Arc<dyn GrantsRepair>> = match crate::hosting::enroll::real_bootstrapper(
        s.secrets.clone(),
        SqlAdminCredential::windows(),
        log,
    ) {
        Ok(b) => Some(Arc::new(BootstrapGrantsRepair::new(
            Arc::new(b),
            s.identity.clone(),
            log,
        ))),
        Err(e) => return Err(e.to_string().into()),
    };
    let grants = Arc::new(OrderGrantsMonitor::new(
        events.clone(),
        Arc::new(SqlOrderGrantsProbe),
        s.identity.clone(),
        s.secrets.clone(),
        s.clock.clone(),
        log,
        repair,
    ));
    let schema = Arc::new(TableSchemaGuard::new(events, log));
    let mut executor = SiteCommandExecutor::new(store, s.clock.clone(), log)
        .with_resnapshot(resnapshot)
        .with_grants(grants.clone())
        .with_schema(schema);
    if let Some(n) = &extras.nudge {
        executor = executor.with_nudge(n.clone());
    }
    let channel = Arc::new(CommandChannel::new(
        CommandChannelParts {
            http: s.http.clone(),
            tokens: s.tokens.clone(),
            executor: Arc::new(executor),
            retry: s.retry.clone(),
            options: s.options.clone(),
            clock: s.clock.clone(),
            tenant_id: Some(s.identity.tenant_id.clone()),
        },
        log,
    ));
    Ok(vec![
        Box::new(CommandService::new(channel)),
        Box::new(OrderGrantsService::new(grants)),
    ])
}

/// The [`RuntimeHooks::before_capture`](crate::hosting::host::RuntimeHooks) factory:
/// `CommandService` and `OrderGrantsService`. The hook passes no site events or capture
/// nudge yet, so events are logged and capture is not nudged (see [`build`]).
pub fn services(
    s: &AgentServices,
    _capture: &Arc<dyn CaptureStatus>,
) -> Result<Vec<Box<dyn LoopService>>, Unhandled> {
    build(s, &CommandExtras::default())
}
