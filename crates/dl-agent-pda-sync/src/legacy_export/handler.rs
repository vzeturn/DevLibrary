//! Runs `legacy.export` (ADR 0136): the cloud orders the store agent to do what
//! `migrate-legacy.ps1` does by hand. The handler pre-checks, answers at once with
//! `{"accepted":true,"runId":...}` and runs the export, the upload and the cleanup on a
//! thread of its own, so the command channel is never blocked.
//!
//! The run: export into a bundle under the agent's data folder, zip it, upload it as a dry
//! run; with `commit` upload it again as the commit and run a second dry run as the count
//! check; delete the bundle and the zip whatever happened; report every stage as a site
//! event. Reasons a command is rejected, as stored in `sync.commands.last_error`:
//! `invalid_payload: ...`, `contract_version_unsupported`, `legacy_export_already_running`,
//! `legacy_sql_unreachable: ...`, `legacy_site_live: ...`, `legacy_store_missing: ...`,
//! `legacy_vietqr_not_set: ...`, `legacy_vietqr_unreadable: ...`, `legacy_bad_arguments: ...`,
//! `legacy_site_unresolved: ...`, `legacy_site_offline_failed: ...`.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use dl_json::{Map, Value as Json};
use dl_uuid::Uuid;

use super::bundle::{self, ExportParts};
use super::events::{EventsFactory, RunEvents};
use super::offline::{self, LegacySite, Placement};
use super::payload::{LegacyExportPayload, sources};
use super::sources::{
    AppPoolProbe, BrandingSource, ExportError, LegacySqlSource, LegacyStore, Result, VietQrRead,
    VietQrSource, explain_missing_store, maintenance_gate, require_sql_store, stages,
};
use super::sql_source::LegacySqlStoreSource;
use super::sqlite_source::LegacySqliteSource;
use super::upload::{BundleUploader, BundleZip, UploadOutcome, UploadPlan, run_sequence};
use crate::commands::Result as CommandResult;
use crate::config::codes;
use crate::config::time::SharedClock;
use crate::contracts::WireJson;
use crate::contracts::legacy::bundle::VIETQR_ONLY_MODE;
use crate::contracts::legacy::{LegacyBundleMaintenance, builds};
use crate::contracts::time::{DateTimeOffset, TimeZone};
use crate::contracts::wire::{
    CommandStatus, SyncCommand, SyncCommandResult, site_command_contract, site_event_levels,
};
use crate::reporting::{Logger, Value};
use crate::util::ctx::Ctx;

/// Category of the handler's log lines.
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.LegacyExport.LegacyExportCommandHandler";

/// Longest the whole run may take.
pub const RUN_DEADLINE: Duration = Duration::from_secs(2 * 60 * 60);
/// Commands remembered, so a redelivery gets the same answer.
const REMEMBERED: usize = 16;
/// Left-out tables named in the leftover event.
const LEFTOVER_NAMES: usize = 20;

/// Builds the SQL source for a command (the instance and database it names, else the
/// enrolled agent's).
pub type SqlFactory =
    Arc<dyn Fn(&LegacyExportPayload) -> Result<Arc<dyn LegacySqlSource>> + Send + Sync>;
/// The legacy API's version under a site root, when it can be read.
pub type AppVersionProbe = Arc<dyn Fn(Option<&str>) -> Option<String> + Send + Sync>;

/// What the run touches outside the agent's own folders; tests replace the parts.
pub struct LegacyExportEnv {
    /// Where bundles and zips are made (`<data dir>/legacy-export`); emptied of stale runs.
    pub work_dir: PathBuf,
    /// The on-prem store (`%ProgramData%\MM-PDA\pda.db`).
    pub pda_db: PathBuf,
    /// The SQL source of a command.
    pub sql: SqlFactory,
    /// IIS app pool state.
    pub pools: Arc<dyn AppPoolProbe>,
    /// IIS applications and the wait after `app_offline.htm` (`take_offline`).
    pub site: Arc<dyn LegacySite>,
    /// Registry brand values.
    pub branding: Arc<dyn BrandingSource>,
    /// Registry VietQR account.
    pub vietqr: Arc<dyn VietQrSource>,
    /// The cloud upload.
    pub uploader: Arc<dyn BundleUploader>,
    /// Site events of a run.
    pub events: EventsFactory,
    /// The clock.
    pub clock: SharedClock,
    /// The site's time zone, for the oldest builds' local-time columns.
    pub zone: Arc<TimeZone>,
    /// `PdaSyncAgent <version>` in the manifest.
    pub exporter: String,
    /// Largest zip the cloud accepts.
    pub max_bytes: u64,
    /// Reads the legacy API's version.
    pub app_version: AppVersionProbe,
}

#[derive(Default)]
struct State {
    running: Option<String>,
    accepted: VecDeque<(Uuid, String)>,
    thread: Option<JoinHandle<()>>,
}

/// The `legacy.export` handler.
pub struct LegacyExportCommandHandler {
    env: Arc<LegacyExportEnv>,
    log: Logger,
    state: Arc<Mutex<State>>,
}

impl std::fmt::Debug for LegacyExportCommandHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LegacyExportCommandHandler")
            .finish_non_exhaustive()
    }
}

fn lock(m: &Mutex<State>) -> MutexGuard<'_, State> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Store {
    PdaDb,
    FromSql,
    /// `vietqr_only`: no store at all.
    None,
}

impl Store {
    fn name(self) -> &'static str {
        match self {
            Store::PdaDb => sources::PDA_DB,
            Store::FromSql => sources::FROM_SQL,
            Store::None => "registry",
        }
    }
}

/// What the pre-check settled.
struct Prepared {
    sql: Option<Arc<dyn LegacySqlSource>>,
    store: Store,
    maintenance: LegacyBundleMaintenance,
    app_version: Option<String>,
    vietqr: VietQrRead,
    /// The `app_offline.htm` of `take_offline`.
    offline: Option<Placement>,
}

/// The reason string stored for a refused command.
fn reject_reason(e: &ExportError) -> String {
    let reason = match e.code.as_str() {
        codes::LEGACY_SITE_LIVE => "legacy_site_live",
        codes::LEGACY_SITE_UNRESOLVED => "legacy_site_unresolved",
        codes::LEGACY_SITE_OFFLINE_FAILED => "legacy_site_offline_failed",
        codes::LEGACY_STORE_MISSING => "legacy_store_missing",
        codes::LEGACY_VIETQR_NOT_SET => "legacy_vietqr_not_set",
        codes::LEGACY_VIETQR_UNREADABLE => "legacy_vietqr_unreadable",
        codes::BAD_ARGUMENTS => "legacy_bad_arguments",
        c if c.starts_with("SQL_") => "legacy_sql_unreachable",
        _ => "legacy_precheck_failed",
    };
    format!("{reason}: {}", e.message)
}

impl LegacyExportCommandHandler {
    /// A handler over `env`; `log` is the root logger.
    pub fn new(env: LegacyExportEnv, log: &Logger) -> LegacyExportCommandHandler {
        LegacyExportCommandHandler {
            env: Arc::new(env),
            log: log.for_category(CATEGORY),
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    fn now(&self) -> DateTimeOffset {
        now(&self.env.clock)
    }

    fn rejected(&self, c: &SyncCommand, reason: &str) -> SyncCommandResult {
        SyncCommandResult {
            command_id: c.command_id,
            status: CommandStatus::Rejected,
            reason: Some(reason.to_owned()),
            result: None,
            at_utc: self.now(),
            exec_ms: None,
            hops: Default::default(),
        }
    }

    fn accepted(&self, c: &SyncCommand, run_id: &str) -> SyncCommandResult {
        let mut m = Map::new();
        m.insert("accepted", true);
        m.insert("runId", run_id);
        SyncCommandResult {
            command_id: c.command_id,
            status: CommandStatus::Applied,
            reason: None,
            result: Some(Json::Object(m)),
            at_utc: self.now(),
            exec_ms: None,
            hops: Default::default(),
        }
    }

    /// Pre-checks and accepts `command`, then runs it in the background: applied with
    /// `{"accepted":true,"runId":...}`, or rejected with the reason.
    pub fn execute(&self, command: &SyncCommand, _ctx: &Ctx) -> CommandResult<SyncCommandResult> {
        if command.contract_version > site_command_contract::VERSION {
            return Ok(self.rejected(command, "contract_version_unsupported"));
        }
        let payload = match LegacyExportPayload::from_element(&command.payload) {
            None => {
                return Ok(self.rejected(
                    command,
                    "invalid_payload: payload is not a legacy.export object",
                ));
            }
            Some(p) => p,
        };
        if let Some(problem) = payload.problem() {
            let rid = Uuid::new_v4().to_string();
            RunEvents::new((self.env.events)(&rid), &rid).failed(&ExportError::refused(
                stages::PRECHECK,
                "INVALID_PAYLOAD",
                "the command payload is invalid",
            ));
            return Ok(self.rejected(command, &format!("invalid_payload: {problem}")));
        }

        let run_id = Uuid::new_v4().to_string();
        {
            let mut st = lock(&self.state);
            if let Some((_, known)) = st.accepted.iter().find(|(id, _)| *id == command.command_id) {
                return Ok(self.accepted(command, known));
            }
            if st.running.is_some() {
                drop(st);
                RunEvents::new((self.env.events)(&run_id), &run_id).failed(&ExportError::refused(
                    stages::PRECHECK,
                    "ALREADY_RUNNING",
                    "another legacy export is running",
                ));
                return Ok(self.rejected(command, "legacy_export_already_running"));
            }
            // The slot is taken before the pre-check: a second command cannot slip in while
            // SQL Server is being asked.
            st.running = Some(run_id.clone());
        }
        let events = RunEvents::new((self.env.events)(&run_id), &run_id);
        let prepared = match self.precheck(&payload, &events) {
            Ok(p) => p,
            Err(mut e) => {
                e.stage = stages::PRECHECK;
                lock(&self.state).running = None;
                // A refusal is told to the cloud like any failed stage.
                events.failed(&e);
                self.log.warn(
                    "legacy.export {Id} refused ({Code}, stage {Stage})",
                    &[
                        Value::from(command.command_id),
                        Value::from(e.code.as_str()),
                        Value::from(e.stage),
                    ],
                );
                return Ok(self.rejected(command, &reject_reason(&e)));
            }
        };

        let env = self.env.clone();
        let state = self.state.clone();
        let log = self.log.clone();
        let rid = run_id.clone();
        let spawned = std::thread::Builder::new()
            .name("legacy-export".to_owned())
            .spawn(move || {
                let _slot = Slot { state: &state };
                run(&env, &log, &payload, &prepared, &rid);
            });
        let mut st = lock(&self.state);
        match spawned {
            Ok(handle) => {
                st.thread = Some(handle);
                st.accepted.push_back((command.command_id, run_id.clone()));
                while st.accepted.len() > REMEMBERED {
                    st.accepted.pop_front();
                }
                drop(st);
                Ok(self.accepted(command, &run_id))
            }
            Err(e) => {
                st.running = None;
                drop(st);
                self.log.error(
                    "legacy.export {Id} could not start its thread ({Kind})",
                    &[
                        Value::from(command.command_id),
                        Value::from(format!("{:?}", e.kind())),
                    ],
                );
                Ok(self.rejected(command, "legacy_export_not_started"))
            }
        }
    }

    /// Waits for the run in flight (tests, and a clean stop).
    pub fn wait_idle(&self) {
        let handle = lock(&self.state).thread.take();
        if let Some(h) = handle
            && h.join().is_err()
        {
            self.log
                .error("A legacy.export run stopped abnormally", &[]);
        }
    }

    /// The run id of the run in flight.
    pub fn running(&self) -> Option<String> {
        lock(&self.state).running.clone()
    }

    fn precheck(&self, p: &LegacyExportPayload, events: &RunEvents) -> Result<Prepared> {
        let env = &self.env;
        offline::report_ignored(p, events, &self.log);
        if p.vietqr_only {
            // Nothing but the registry: no gate, no store, no SQL Server.
            let vietqr = env.vietqr.read();
            match &vietqr {
                VietQrRead::NotSet => {
                    return Err(ExportError::refused(
                        stages::PRECHECK,
                        codes::LEGACY_VIETQR_NOT_SET,
                        "the legacy registry holds no VietQR account",
                    ));
                }
                VietQrRead::Unreadable(why) => {
                    return Err(ExportError::refused(
                        stages::PRECHECK,
                        codes::LEGACY_VIETQR_UNREADABLE,
                        format!("the legacy VietQR account could not be read: {why}"),
                    ));
                }
                VietQrRead::Read(_) => {}
            }
            return Ok(Prepared {
                sql: None,
                store: Store::None,
                maintenance: LegacyBundleMaintenance {
                    mode: VIETQR_ONLY_MODE.to_owned(),
                    evidence: "VietQR registry values only; no store rows".to_owned(),
                },
                app_version: (env.app_version)(p.site_root.as_deref()),
                vietqr,
                offline: None,
            });
        }

        let sql = (env.sql)(p)?;
        // SQL Server must answer before the command is accepted: the RVC list is read here.
        sql.rvc_list()?;
        // take_offline: the agent puts the legacy site in maintenance itself.
        let placed = if p.take_offline && !p.allow_live {
            let root = match &p.site_root {
                Some(r) => r.clone(),
                None => offline::discover(env.site.as_ref())?,
            };
            let placed = offline::place(&root, env.site.as_ref())?;
            offline::report_placed(events, &placed);
            Some(placed)
        } else {
            None
        };
        match self.precheck_live(p, sql, placed.as_ref()) {
            Ok(prep) => Ok(Prepared {
                offline: placed,
                ..prep
            }),
            Err(mut e) => {
                // Refused before anything was committed: the site goes back online.
                if let Some(o) = &placed
                    && let Err(rm) = offline::finish(
                        o,
                        offline::CommitState::NotCommitted,
                        stages::PRECHECK,
                        events,
                        &self.log,
                    )
                {
                    events.failed(&rm);
                    e.message.push_str(&format!(" ({})", rm.message));
                }
                Err(e)
            }
        }
    }

    /// The maintenance and store gates of a store export.
    fn precheck_live(
        &self,
        p: &LegacyExportPayload,
        sql: Arc<dyn LegacySqlSource>,
        placed: Option<&Placement>,
    ) -> Result<Prepared> {
        let env = &self.env;
        let site_root = placed.map(|o| o.root.as_str()).or(p.site_root.as_deref());
        let mut maintenance = maintenance_gate(
            site_root,
            p.app_pool.as_deref(),
            p.allow_live,
            env.pools.as_ref(),
        )?;
        if let Some(o) = placed {
            maintenance.evidence = o.evidence(&maintenance.evidence);
        }
        let app_version = (env.app_version)(site_root);
        let store = match p.source.as_str() {
            sources::FROM_SQL => Store::FromSql,
            sources::PDA_DB => Store::PdaDb,
            _ if env.pda_db.is_file() => Store::PdaDb,
            _ => Store::FromSql,
        };
        match store {
            Store::FromSql => require_sql_store(&env.pda_db, app_version.as_deref())?,
            _ if !env.pda_db.is_file() => {
                let message = match sql.pda_tables() {
                    Ok(tables) => explain_missing_store(&env.pda_db, &sql.database(), &tables),
                    Err(e) => format!(
                        "pda.db not found at '{}'; SQL Server could not be checked for data of an older build ({}).",
                        env.pda_db.display(),
                        e.code
                    ),
                };
                return Err(ExportError::refused(
                    stages::PRECHECK,
                    codes::LEGACY_STORE_MISSING,
                    message,
                ));
            }
            _ => {}
        }
        Ok(Prepared {
            sql: Some(sql),
            store,
            maintenance,
            app_version,
            vietqr: env.vietqr.read(),
            offline: None,
        })
    }
}

/// Clears the single-run slot when the run ends, however it ends.
struct Slot<'a> {
    state: &'a Mutex<State>,
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        lock(self.state).running = None;
    }
}

fn now(clock: &SharedClock) -> DateTimeOffset {
    DateTimeOffset::from_unix_nanos(clock.now().unix_nanos(), 0)
        .unwrap_or(DateTimeOffset::UNIX_EPOCH)
}

// ---- the run

/// Removes the runs an earlier process left behind: a bundle holds guest and staff data.
fn clear_stale(work: &Path, log: &Logger) {
    let Ok(entries) = std::fs::read_dir(work) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let gone = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(e) = gone {
            log.warn(
                "A stale legacy export folder could not be deleted ({Kind})",
                &[Value::from(format!("{:?}", e.kind()))],
            );
        }
    }
}

fn run(
    env: &LegacyExportEnv,
    log: &Logger,
    p: &LegacyExportPayload,
    prep: &Prepared,
    run_id: &str,
) {
    let events = RunEvents::new((env.events)(run_id), run_id);
    events.started(
        &prep.maintenance.mode,
        prep.store.name(),
        p.commit,
        p.vietqr_only,
    );
    log.info(
        "legacy.export run {RunId} started (store {Store}, commit {Commit})",
        &[
            Value::from(run_id),
            Value::from(prep.store.name()),
            Value::from(p.commit),
        ],
    );

    clear_stale(&env.work_dir, log);
    let run_dir = env.work_dir.join(run_id);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_stages(env, p, prep, &run_dir, &events)
    }))
    .unwrap_or_else(|_| {
        Err(ExportError::new(
            stages::EXPORT,
            codes::LEGACY_EXPORT_FAILED,
            "the run stopped unexpectedly",
        ))
    });

    // Success or failure: the bundle and the zip never stay behind.
    let deleted = match std::fs::remove_dir_all(&run_dir) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => {
            log.error(
                "The legacy export folder of run {RunId} could not be deleted ({Kind}); delete it by hand",
                &[Value::from(run_id), Value::from(format!("{:?}", e.kind()))],
            );
            events.failed(&ExportError::refused(
                stages::CLEANUP,
                codes::LEGACY_EXPORT_FAILED,
                format!(
                    "the local bundle could not be deleted ({:?}); delete it by hand",
                    e.kind()
                ),
            ));
            false
        }
    };

    if let Some(o) = &prep.offline {
        // The site stays offline only once the cloud commit succeeded.
        let state = offline::CommitState::of(outcome.as_ref().map(|d| d.committed));
        if let Err(e) = offline::finish(o, state, stages::CLEANUP, &events, log) {
            events.failed(&e);
        }
    }

    match outcome {
        Ok(done) => {
            let mut detail = done.detail(deleted);
            detail.insert("manifest_sha256", done.manifest_sha.as_str());
            events.uploaded(detail);
            log.info(
                "legacy.export run {RunId} finished (committed {Committed})",
                &[Value::from(run_id), Value::from(done.committed)],
            );
        }
        Err(e) => {
            events.failed(&e);
            log.warn(
                "legacy.export run {RunId} failed at {Stage} ({Code})",
                &[
                    Value::from(run_id),
                    Value::from(e.stage),
                    Value::from(e.code.as_str()),
                ],
            );
        }
    }
}

struct Done {
    manifest_sha: String,
    zip_sha256: String,
    bytes: u64,
    rows: i64,
    committed: bool,
    outcome: UploadOutcome,
}

impl Done {
    fn detail(&self, local_deleted: bool) -> Map {
        let mut m = Map::new();
        m.insert("zip_sha256", self.zip_sha256.as_str());
        m.insert("bytes", i64::try_from(self.bytes).unwrap_or(i64::MAX));
        m.insert("rows", self.rows);
        m.insert("committed", self.committed);
        m.insert("local_deleted", local_deleted);
        let label = if self.outcome.vietqr_kept {
            "vietqr_account_kept"
        } else if self.committed {
            "committed"
        } else {
            "dry_run_only"
        };
        m.insert("outcome", label);
        m.insert("dry_run", Json::Object(self.outcome.dry_run.detail()));
        if let Some(c) = &self.outcome.commit {
            m.insert("commit", Json::Object(c.detail()));
        }
        if let Some(c) = &self.outcome.post_commit {
            m.insert("post_commit_dry_run", Json::Object(c.detail()));
        }
        m
    }
}

fn run_stages(
    env: &LegacyExportEnv,
    p: &LegacyExportPayload,
    prep: &Prepared,
    run_dir: &Path,
    events: &RunEvents,
) -> Result<Done> {
    let bundle_dir = run_dir.join("bundle");
    std::fs::create_dir_all(run_dir)
        .map_err(|e| ExportError::io(stages::EXPORT, "the work folder could not be created", &e))?;
    export_bundle(env, p, prep, &bundle_dir, events)?;

    let zip = BundleZip::create(&bundle_dir, run_dir, env.max_bytes)?;
    let ctx = Ctx::background().with_timeout(RUN_DEADLINE);
    let plan = UploadPlan {
        commit: p.commit,
        prefer_legacy: p.prefer_legacy,
        keep_tenant: p.keep_tenant,
        vietqr_only: p.vietqr_only,
    };
    let outcome = run_sequence(env.uploader.as_ref(), &zip, plan, events, &ctx)?;
    Ok(Done {
        manifest_sha: zip.bundle.manifest_sha256.clone(),
        zip_sha256: zip.sha256.clone(),
        bytes: zip.length,
        rows: zip.rows,
        committed: outcome.commit.is_some(),
        outcome,
    })
}

fn export_bundle(
    env: &LegacyExportEnv,
    p: &LegacyExportPayload,
    prep: &Prepared,
    out: &Path,
    events: &RunEvents,
) -> Result<()> {
    let mut notify = |f: &crate::contracts::legacy::LegacyBundleFile, bytes: u64| {
        let mut d = Map::new();
        d.insert("table", f.table.as_str());
        d.insert("rows", f.rows);
        d.insert("bytes", i64::try_from(bytes).unwrap_or(i64::MAX));
        d.insert("disposition", f.disposition.json_name());
        events.export(
            site_event_levels::INFO,
            "LEGACY_EXPORT_TABLE",
            &format!("{}: {} rows", f.table, f.rows),
            d,
        );
    };
    report_vietqr(events, &prep.vietqr);
    let created = now(&env.clock);

    if p.vietqr_only {
        let database = p.database.clone().unwrap_or_default();
        let m = bundle::export_vietqr_only(
            out,
            &prep.vietqr,
            prep.app_version.clone(),
            &database,
            &env.exporter,
            created,
            &mut notify,
        )?;
        written(events, &m, &prep.maintenance.mode, None);
        return Ok(());
    }

    let sql = prep.sql.clone().ok_or_else(|| {
        ExportError::new(
            stages::EXPORT,
            codes::LEGACY_EXPORT_FAILED,
            "no SQL source for the run",
        )
    })?;
    let store: Box<dyn LegacyStore> = match prep.store {
        Store::FromSql => Box::new(LegacySqlStoreSource::open(sql.clone(), env.zone.clone())?),
        _ => Box::new(LegacySqliteSource::open(&env.pda_db)?),
    };
    let parts = ExportParts {
        store: store.as_ref(),
        sql: sql.as_ref(),
        branding: env.branding.as_ref(),
        vietqr: &prep.vietqr,
        app_version: prep.app_version.clone(),
        maintenance: prep.maintenance.clone(),
        exporter: env.exporter.clone(),
        created_at: created,
    };
    let manifest = bundle::export(out, &parts, &mut notify)?;
    let leftovers = bundle::leftovers(&manifest, store.as_ref());
    if !leftovers.is_empty() {
        let mut d = Map::new();
        d.insert("tables", i64::try_from(leftovers.len()).unwrap_or(i64::MAX));
        d.insert("rows", leftovers.iter().map(|l| l.rows).sum::<i64>());
        let names: Vec<Json> = leftovers
            .iter()
            .take(LEFTOVER_NAMES)
            .map(|l| Json::from(format!("{}:{}", l.table, l.rows)))
            .collect();
        d.insert("names", Json::Array(names));
        events.export(
            site_event_levels::WARN,
            "LEGACY_EXPORT_SQL_LEFTOVERS",
            "SQL Server still holds rows nobody accounted for; they are not in the bundle",
            d,
        );
    }
    written(events, &manifest, &prep.maintenance.mode, store.kind());
    Ok(())
}

fn written(
    events: &RunEvents,
    m: &crate::contracts::legacy::LegacyBundleManifest,
    mode: &str,
    store: Option<String>,
) {
    let mut d = Map::new();
    d.insert("files", i64::try_from(m.files.len()).unwrap_or(i64::MAX));
    d.insert("rows", m.files.iter().map(|f| f.rows).sum::<i64>());
    d.insert(
        "left_out",
        i64::try_from(m.left_out.len()).unwrap_or(i64::MAX),
    );
    d.insert("mode", mode);
    d.insert("database", m.source.sql_database.as_str());
    d.insert(
        "app_version",
        m.source.app_version.clone().unwrap_or_default(),
    );
    d.insert(
        "build_standing",
        format!("{:?}", builds::standing(m.source.app_version.as_deref())).to_lowercase(),
    );
    d.insert(
        "store",
        store.unwrap_or_else(|| builds::PDA_DB_STORE.to_owned()),
    );
    events.export(
        site_event_levels::INFO,
        "LEGACY_EXPORT_WRITTEN",
        &format!("Bundle written: {} files", m.files.len()),
        d,
    );
}

/// What was found of the VietQR account: bank BIN and the masked account number only,
/// never the full number.
fn report_vietqr(events: &RunEvents, vietqr: &VietQrRead) {
    match vietqr {
        VietQrRead::Read(_) => {
            let mut d = Map::new();
            d.insert("bank_bin", vietqr.value("BankBin").unwrap_or_default());
            d.insert("account", vietqr.masked_account());
            events.export(
                site_event_levels::INFO,
                "LEGACY_EXPORT_VIETQR",
                &format!("VietQR account {} read", vietqr.masked_account()),
                d,
            );
        }
        VietQrRead::NotSet => {}
        VietQrRead::Unreadable(problem) => {
            let mut d = Map::new();
            d.insert("problem", problem.as_str());
            events.export(
                site_event_levels::WARN,
                codes::LEGACY_VIETQR_UNREADABLE,
                &format!("VietQR account not read: {problem}"),
                d,
            );
        }
    }
}
