//! The command loops of the running agent (`AgentHost.AddAgentRuntime` registrations of
//! `SqlSiteCommandStore`, `ResnapshotCommandHandler`, `VerifyCommandHandler`, `BootstrapGrantsRepair`,
//! `OrderGrantsMonitor`, `TableSchemaGuard`, `SiteCommandExecutor`, `CommandChannel`, and
//! the hosted `CommandService` and `OrderGrantsService`).

use std::sync::Arc;

use super::channel::{CommandChannel, CommandChannelParts};
use super::executor::SiteCommandExecutor;
use super::grants::{BootstrapGrantsRepair, GrantsRepair, OrderGrantsMonitor, OrderGrantsService};
use super::resnapshot::ResnapshotCommandHandler;
use super::schema_guard::{LogOnlyEvents, TableSchemaGuard};
use super::sql_store::{ConfigSource, SqlSiteCommandStore};
use super::verify::{VerifyCommandHandler, VerifyParts};
use super::verify_manifest::HttpCloudManifestSource;
use crate::bootstrap::SqlOrderGrantsProbe;
use crate::contracts::SyncCatalog;
use crate::contracts::time::TimeZone;
use crate::hosting::cli::Unhandled;
use crate::hosting::host::AgentServices;
use crate::hosting::services::{LoopService, ServiceError};
use crate::outbox::CaptureStatus;
use crate::reporting::site_event_store::DEFAULT_CAPACITY;
use crate::reporting::{Logger, SiteEventStore, SiteEvents, SqliteSiteEventStore, Value};
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
    // fo.inhouse_search (RT-1296) reads the RVC's Front Office database on its own
    // connection; it never writes and keeps no command-log row.
    let fo_inhouse = Arc::new(super::fo_inhouse::FoInHouseSearchCommandHandler::new(
        Arc::new(super::fo_inhouse::SqlFoInHouseSource::new(config.clone())),
        s.clock.clone(),
        log,
    ));
    // One idle store connection per command worker (RT-1318).
    let workers = usize::try_from(s.options.command_workers).unwrap_or(1);
    let store = Arc::new(SqlSiteCommandStore::new(config).with_max_idle(workers));

    let catalog = SyncCatalog::load().map_err(|e| e.to_string())?;
    let resnapshot = Arc::new(ResnapshotCommandHandler::new(
        s.outbox.clone(),
        s.clock.clone(),
        log,
        catalog,
    ));
    // sync.verify (ADR 0126) compares the shadow with the cloud manifest; it reads no store
    // table. Its own capture state connection (WAL) reads beside the capture session's and
    // is dropped after each run.
    let state_path = s.paths.capture_state_db();
    let verify = Arc::new(VerifyCommandHandler::new(
        VerifyParts {
            outbox: s.outbox.clone(),
            open_state: Arc::new(move || {
                Ok(
                    Arc::new(crate::capture::SqliteCaptureState::open(&state_path)?)
                        as Arc<dyn crate::capture::CaptureState>,
                )
            }),
            cloud: Arc::new(HttpCloudManifestSource::new(
                s.http.clone(),
                s.tokens.clone(),
            )),
            clock: s.clock.clone(),
            catalog: Arc::new(catalog.clone()),
            config: Some(s.runtime_config.clone()),
        },
        log,
    ));
    // The repair runs as each identity `pda-enroll` tries (Windows, then the SMILE
    // default login) and only ever applies pda_sync's objects and grants (RT-1309).
    let repair: Option<Arc<dyn GrantsRepair>> = Some(Arc::new(BootstrapGrantsRepair::new(
        s.secrets.clone(),
        s.identity.clone(),
        log,
    )));
    let grants = Arc::new(OrderGrantsMonitor::new(
        events.clone(),
        Arc::new(SqlOrderGrantsProbe),
        s.identity.clone(),
        s.secrets.clone(),
        s.clock.clone(),
        log,
        repair,
    ));
    let schema = Arc::new(TableSchemaGuard::new(events.clone(), log));
    let legacy_export = Arc::new(crate::legacy_export::LegacyExportCommandHandler::new(
        legacy_export_env(s, events.clone()),
        log,
    ));
    let mut executor = SiteCommandExecutor::new(store, s.clock.clone(), log)
        .with_resnapshot(resnapshot)
        .with_verify(verify)
        .with_legacy_export(legacy_export)
        .with_fo_inhouse(fo_inhouse)
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

/// The environment of `legacy.export` (RT-0390): the site events go to the queue of the
/// agent under the sources `legacy-export` and `legacy-upload`; when that queue cannot be
/// opened they go through the host's reporter.
fn legacy_export_env(
    s: &AgentServices,
    fallback: Arc<dyn SiteEvents>,
) -> crate::legacy_export::LegacyExportEnv {
    let queue: Option<Arc<dyn SiteEventStore>> = match SqliteSiteEventStore::try_open(
        s.paths.events_db(),
        DEFAULT_CAPACITY,
    ) {
        Ok(q) => Some(Arc::new(q)),
        Err(error) => {
            s.log.warn(
                    "The site event queue {Path} could not be opened for legacy.export ({Error}); its events go through the agent's reporter",
                    &[
                        Value::from(s.paths.events_db().display().to_string()),
                        Value::from(error),
                    ],
                );
            None
        }
    };
    let zone = crate::hosting::host::site_time_zone(s.options.site_time_zone.as_deref())
        .unwrap_or_else(|_| TimeZone::local());
    crate::legacy_export::production_env(crate::legacy_export::ProductionParts {
        work_dir: s.paths.data_dir().join("legacy-export"),
        identity: s.identity.clone(),
        secrets: s.secrets.clone(),
        http: s.http.clone(),
        tokens: s.tokens.clone(),
        clock: s.clock.clone(),
        zone,
        events: crate::legacy_export::events::queue_events_factory(
            queue,
            fallback,
            s.log.clone(),
            s.clock.clone(),
        ),
        exporter: format!("PdaSyncAgent {}", crate::hosting::agent_version()),
    })
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
