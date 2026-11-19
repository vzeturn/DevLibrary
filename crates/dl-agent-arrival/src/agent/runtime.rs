//! `CloudLinkWorker` and the hosted services around it: the long-poll loop, the two uplinks
//! (answers, change notices), the request lanes and the change watcher, on plain threads.
//!
//! Outbound only: the agent polls `api/agent/poll`, runs what the cloud asks for and posts the
//! answers to `api/agent/results` (whose reply may carry more work). On stop, requests not yet
//! started are dropped (the cloud asks again), the ones running get [`GRACE`] to finish and
//! post, then the journal is flushed. A thread still inside a blocking call after that is left
//! to finish it, as the pms modules do.

use std::panic::AssertUnwindSafe;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dl_agent::report::Severity;
use dl_json::Map;
use dl_time::Offset;
use dl_winsvc::StopSignal;

use crate::agent::clock::{Clock, backoff};
use crate::agent::cloud::{self, CloudLink, POLL_PATH, POLL_TIMEOUT, RESULTS_PATH, UPLOAD_TIMEOUT};
use crate::agent::executor::Executor;
use crate::agent::health::{self, CloudFailure, Health};
use crate::agent::installer::{self, InstallError};
use crate::agent::journal::Journal;
use crate::agent::layout::Layout;
use crate::agent::link_status::{self, LinkStatus, LinkStatusFile};
use crate::agent::outbox::{self, Batching, Outbox, Pacer};
use crate::agent::processor::{Lane, Processor};
use crate::agent::secrets::{Identity, TenantKeyStore};
use crate::agent::settings::{Settings, Watch};
use crate::agent::vault::LegalVault;
use crate::agent::watcher::{self, Watcher};
use crate::contracts::{
    AgentHello, AgentPollRequest, PmsChangeNotice, PmsRequest, PmsResponse, Status, catalog,
};
use crate::error::Error;

/// Time running requests get to finish and post after a stop.
pub const GRACE: Duration = Duration::from_secs(5);
/// Threads answering requests that do not need the PMS.
pub const FAST_LANE_THREADS: usize = 2;
/// Tries per uplink batch; after that it is dropped (the cloud stopped waiting long before).
pub const MAX_UPLOAD_ATTEMPTS: u32 = 5;
/// How often the link heartbeat file is rewritten while connected.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);
/// Longest `proceduresReport` in the hello.
pub const MAX_PROCEDURES_REPORT: usize = 1500;
/// How often the metrics are published and `on_tick` runs.
pub const TICK: Duration = Duration::from_secs(1);
/// The hello's `agentVersion` column holds 32 characters.
const MAX_VERSION: usize = 32;

/// Installs the PMS procedure bundle (`Ok(true)` when it ran).
pub type Installer = Box<dyn FnOnce() -> Result<bool, InstallError> + Send>;

/// `dl-agent/<version>`, the agent version the cloud shows for this site.
pub fn agent_version() -> String {
    format!("dl-agent/{}", dl_agent::agent_version())
        .chars()
        .take(MAX_VERSION)
        .collect()
}

/// What the link is built from besides the settings.
pub struct Wiring {
    /// Runs catalog operations.
    pub executor: Arc<dyn Executor>,
    /// Seals envelopes and stamps journals.
    pub clock: Arc<dyn Clock>,
    /// State, metrics and reports.
    pub health: Arc<Health>,
    /// The hotel's UTC offset (`{today}` of the watches).
    pub utc_offset: Offset,
    /// Run before the first poll (`AutoInstallProcedures`).
    pub installer: Option<Installer>,
}

impl std::fmt::Debug for Wiring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wiring").finish_non_exhaustive()
    }
}

/// A link ready to run.
pub struct Parts {
    cloud: CloudLink,
    processor: Processor,
    journal: Arc<Journal>,
    link_status: LinkStatusFile,
    watches: Vec<Watch>,
    pms_threads: usize,
    wiring: Wiring,
}

impl std::fmt::Debug for Parts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Parts")
            .field("cloud", &self.cloud)
            .finish_non_exhaustive()
    }
}

impl Parts {
    /// Opens what `settings` point at (paths resolved by `layout`) for `identity`.
    ///
    /// # Errors
    /// [`Error::Config`] for a bad cloud URL or identity secret, [`Error::Io`] when the
    /// journal cannot be read.
    pub fn assemble(
        settings: &Settings,
        layout: &Layout,
        identity: &Identity,
        wiring: Wiring,
    ) -> Result<Parts, Error> {
        let user_agent = format!("{} (arrival)", agent_version());
        let cloud = CloudLink::new(
            &settings.cloud_base_url,
            identity.cipher()?,
            Arc::clone(&wiring.clock),
            &user_agent,
        )?;
        let keys = Arc::new(TenantKeyStore::new(
            &settings.tenant_key,
            layout.resolve(&settings.tenant_key_file),
        ));
        let journal = Arc::new(Journal::open(
            &layout.resolve(&settings.idempotency_file),
            Arc::clone(&wiring.clock),
        )?);
        let vault = LegalVault::new(layout.resolve(&settings.vault_directory), Arc::clone(&keys));
        let processor = Processor::new(
            Arc::clone(&wiring.executor),
            usize::try_from(settings.snapshot_capacity)
                .unwrap_or(1)
                .max(1),
            Arc::clone(&journal),
            keys,
            vault,
            Arc::clone(&wiring.health),
            settings.lock_timeout_seconds,
        );
        Ok(Parts {
            cloud,
            processor,
            journal,
            link_status: LinkStatusFile::next_to(&layout.resolve(&settings.identity_file)),
            watches: settings.watches.clone(),
            pms_threads: usize::try_from(settings.max_concurrency)
                .unwrap_or(1)
                .max(1),
            wiring,
        })
    }

    /// Runs the link until `stop`, then shuts it down (see the module docs). `on_tick` runs
    /// every [`TICK`] on the calling thread (the module answers its commands there).
    ///
    /// # Errors
    /// [`Error::Io`] when a thread cannot be started; [`Error::Config`] when the embedded
    /// catalog does not load.
    pub fn run(self, stop: &StopSignal, on_tick: &dyn Fn()) -> Result<(), Error> {
        let cat = catalog()?;
        let runs = watcher::plan(cat, &self.watches);
        let Parts {
            cloud,
            processor,
            journal,
            link_status,
            pms_threads,
            wiring,
            ..
        } = self;
        let Wiring {
            executor,
            clock,
            health,
            utc_offset,
            installer,
        } = wiring;
        let notices = Arc::new(Outbox::new(Some(outbox::NOTICE_CAPACITY)));
        let link = Arc::new(Link {
            cloud,
            processor,
            executor: Arc::clone(&executor),
            health: Arc::clone(&health),
            clock: Arc::clone(&clock),
            journal,
            link_status,
            fast: Outbox::new(None),
            pms: Outbox::new(None),
            results: Outbox::new(None),
            notices: Arc::clone(&notices),
            stop: StopSignal::new(),
            uplink_stop: StopSignal::new(),
            deadline: OnceLock::new(),
            procedures_report: Mutex::new(None),
            reported_link: Mutex::new(None),
            last_beat: Mutex::new(None),
            version: agent_version(),
            catalog_version: cat.version,
        });
        let mut threads = Threads::default();
        let started = link.start(&mut threads, pms_threads, installer).map(|()| {
            let w = Arc::new(Watcher::new(
                executor, notices, health, clock, utc_offset, cat,
            ));
            threads.watches = w.spawn(runs, &link.stop);
        });
        if let Err(e) = started {
            link.shutdown(threads);
            return Err(e);
        }
        while !stop.wait_timeout(TICK) {
            link.health.publish_metrics();
            on_tick();
        }
        link.shutdown(threads);
        Ok(())
    }
}

#[derive(Default)]
struct Threads {
    lanes: Vec<JoinHandle<()>>,
    uplinks: Vec<JoinHandle<()>>,
    poll: Vec<JoinHandle<()>>,
    watches: Vec<JoinHandle<()>>,
}

struct Link {
    cloud: CloudLink,
    processor: Processor,
    executor: Arc<dyn Executor>,
    health: Arc<Health>,
    clock: Arc<dyn Clock>,
    journal: Arc<Journal>,
    link_status: LinkStatusFile,
    fast: Outbox<PmsRequest>,
    pms: Outbox<PmsRequest>,
    results: Outbox<PmsResponse>,
    notices: Arc<Outbox<PmsChangeNotice>>,
    /// Set when the module stops: no new polls, no new requests.
    stop: StopSignal,
    /// Set once the lanes are done: the uplinks post what is left and end.
    uplink_stop: StopSignal,
    deadline: OnceLock<Instant>,
    procedures_report: Mutex<Option<String>>,
    reported_link: Mutex<Option<String>>,
    last_beat: Mutex<Option<Instant>>,
    version: String,
    catalog_version: i32,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn spawn(name: String, f: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>, Error> {
    std::thread::Builder::new()
        .name(name)
        .spawn(f)
        .map_err(|e| Error::io("start a link thread", e))
}

fn wait_for(threads: &[JoinHandle<()>], deadline: Instant) {
    while threads.iter().any(|t| !t.is_finished()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
}

impl Link {
    fn start(
        self: &Arc<Self>,
        t: &mut Threads,
        pms_threads: usize,
        installer: Option<Installer>,
    ) -> Result<(), Error> {
        for i in 0..FAST_LANE_THREADS {
            let me = Arc::clone(self);
            t.lanes.push(spawn(format!("arrival-fast-{i}"), move || {
                me.lane(&me.fast)
            })?);
        }
        for i in 0..pms_threads {
            let me = Arc::clone(self);
            t.lanes
                .push(spawn(format!("arrival-pms-{i}"), move || me.lane(&me.pms))?);
        }
        let me = Arc::clone(self);
        t.uplinks.push(spawn("arrival-results".into(), move || {
            me.uplink(&me.results, outbox::RESULTS, "result(s)", |batch| {
                AgentPollRequest {
                    hello: None,
                    responses: batch,
                    notices: Vec::new(),
                }
            });
        })?);
        let me = Arc::clone(self);
        t.uplinks.push(spawn("arrival-notices".into(), move || {
            me.uplink(&me.notices, outbox::NOTICES, "change notice(s)", |batch| {
                AgentPollRequest {
                    hello: None,
                    responses: Vec::new(),
                    notices: batch,
                }
            });
        })?);
        let me = Arc::clone(self);
        t.poll.push(spawn("arrival-poll".into(), move || {
            me.poll_loop(installer)
        })?);
        Ok(())
    }

    fn shutdown(&self, threads: Threads) {
        let deadline = Instant::now() + GRACE;
        let _ = self.deadline.set(deadline);
        self.stop.stop();
        let dropped = self.fast.drain(usize::MAX).len() + self.pms.drain(usize::MAX).len();
        if dropped > 0 {
            dl_log::warn!("requests not started before the stop were dropped; the cloud asks again"; count = dropped);
        }
        wait_for(&threads.lanes, deadline);
        self.uplink_stop.stop();
        wait_for(&threads.uplinks, deadline);
        if let Err(e) = self.journal.flush() {
            dl_log::error!("could not flush the idempotency journal"; error = %e);
        }
        self.health.publish_metrics();
        let left = [
            &threads.lanes,
            &threads.uplinks,
            &threads.poll,
            &threads.watches,
        ]
        .into_iter()
        .flatten()
        .filter(|t| !t.is_finished())
        .count();
        if left > 0 {
            dl_log::warn!("link threads still inside a blocking call were left to finish it"; count = left);
        }
    }

    /// `PollLoopAsync`.
    fn poll_loop(&self, installer: Option<Installer>) {
        if let Some(install) = installer {
            self.install(install);
        }
        let mut hello_sent = false;
        let mut failures = 0u32;
        while !self.stop.is_stopped() {
            let body = AgentPollRequest {
                hello: (!hello_sent).then(|| self.hello()),
                responses: Vec::new(),
                notices: Vec::new(),
            };
            let delay = match self.cloud.exchange(POLL_PATH, &body, POLL_TIMEOUT) {
                Ok(reply) => {
                    hello_sent = true;
                    failures = 0;
                    self.report_link(None);
                    self.beat();
                    self.health.cloud_ok(self.clock.now().unix_seconds());
                    self.dispatch(reply.requests);
                    Duration::from_millis(u64::try_from(reply.next_poll_delay_ms).unwrap_or(0))
                }
                Err(Error::CatalogRejected) => {
                    hello_sent = false;
                    failures = failures.saturating_add(1);
                    let delay = backoff(failures);
                    dl_log::warn!(
                        "Cloud rejected the catalog; retrying.";
                        catalog = self.catalog_version, failures = failures, retry_ms = delay.as_millis() as u64
                    );
                    let problem = format!("the cloud rejected catalog v{}", self.catalog_version);
                    self.health.cloud_failed(CloudFailure::Transport, &problem);
                    delay
                }
                Err(e) => {
                    hello_sent = false;
                    failures = failures.saturating_add(1);
                    let delay = backoff(failures);
                    let problem = self.problem(&e);
                    if matches!(e, Error::Refused { .. }) {
                        dl_log::error!("Cloud refused the agent."; problem = %problem, retry_ms = delay.as_millis() as u64);
                    } else {
                        dl_log::warn!("Cloud poll failed"; error = %e, retry_ms = delay.as_millis() as u64);
                    }
                    self.report_link(Some(&problem));
                    self.health.cloud_failed(cloud::classify(&e), &problem);
                    delay
                }
            };
            if !delay.is_zero() && self.stop.wait_timeout(delay) {
                break;
            }
        }
    }

    fn problem(&self, e: &Error) -> String {
        match e {
            Error::Refused { clock } => self.cloud.refused_text(*clock),
            Error::ClockSkew { seconds } => format!(
                "Replies from the cloud are stamped {seconds} s away from this machine's clock. Correct the Windows clock."
            ),
            other => format!("Cannot reach the cloud: {other}"),
        }
    }

    fn hello(&self) -> AgentHello {
        let fingerprint = self.executor.schema_fingerprint().unwrap_or_default();
        AgentHello {
            agent_version: self.version.clone(),
            catalog_version: self.catalog_version,
            machine_name: dl_agent::hub::hostname(),
            pms_schema_fingerprint: Some(format!("{}.{fingerprint}", installer::bundle().hash)),
            procedures_report: lock(&self.procedures_report).clone(),
        }
    }

    fn install(&self, install: Installer) {
        match install() {
            Ok(_) => self.health.install_ok(),
            Err(e) => {
                // Keep running: the cloud sees failing operations and the hello shows the problem.
                dl_log::error!("PMS procedure bundle could not be installed."; error = %e);
                let report: String = format!("procedures: {e}")
                    .chars()
                    .take(MAX_PROCEDURES_REPORT)
                    .collect();
                *lock(&self.procedures_report) = Some(report);
                self.health.install_failed(&e.0);
            }
        }
    }

    /// Writes the link status when it changed.
    fn report_link(&self, problem: Option<&str>) {
        let key = problem.unwrap_or(link_status::CONNECTED).to_string();
        {
            let mut last = lock(&self.reported_link);
            if last.as_deref() == Some(key.as_str()) {
                return;
            }
            *last = Some(key);
        }
        self.link_status.write(&LinkStatus {
            state: if problem.is_some() {
                link_status::FAILED
            } else {
                link_status::CONNECTED
            },
            agent_id: self.cloud.agent_id().to_string(),
            kid: self.cloud.kid().to_string(),
            at_utc: self.clock.now(),
            problem: problem.map(str::to_string),
        });
    }

    fn beat(&self) {
        {
            let mut last = lock(&self.last_beat);
            if last.is_some_and(|at| at.elapsed() < HEARTBEAT_INTERVAL) {
                return;
            }
            *last = Some(Instant::now());
        }
        self.link_status.beat(&self.clock.now(), &self.version);
    }

    /// Queues the cloud's requests on their lanes.
    fn dispatch(&self, requests: Vec<PmsRequest>) {
        if self.stop.is_stopped() {
            if !requests.is_empty() {
                dl_log::warn!("requests arrived during the stop; the cloud asks again"; count = requests.len());
            }
            return;
        }
        for request in requests {
            match self.processor.route(&request) {
                Lane::Fast => self.fast.push(request),
                Lane::Pms => self.pms.push(request),
            };
        }
    }

    /// One worker of a lane: answers requests until the stop.
    fn lane(&self, queue: &Outbox<PmsRequest>) {
        while queue.wait(&self.stop) && !self.stop.is_stopped() {
            let Some(request) = queue.drain(1).pop() else {
                continue;
            };
            let in_flight = &self.health.metrics.in_flight;
            in_flight.fetch_add(1, Ordering::Relaxed);
            let response = std::panic::catch_unwind(AssertUnwindSafe(|| {
                self.processor.handle(&request)
            }))
            .unwrap_or_else(|_| {
                dl_log::error!("answering a request panicked"; operation = %request.operation);
                self.health.count(Status::Failed);
                PmsResponse::bare(
                    &request.request_id,
                    Status::Failed,
                    Some("Agent error".into()),
                )
            });
            self.results.push(response);
            in_flight.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// `UplinkLoopAsync`.
    fn uplink<T>(
        &self,
        queue: &Outbox<T>,
        batching: Batching,
        what: &str,
        wrap: impl Fn(Vec<T>) -> AgentPollRequest,
    ) {
        let mut pacer = Pacer::new(batching.window);
        while queue.wait(&self.uplink_stop) {
            let wait = pacer.wait_before_post(Instant::now());
            if !wait.is_zero() {
                std::thread::sleep(wait);
            }
            let batch = queue.drain(batching.max);
            if batch.is_empty() {
                continue;
            }
            let count = batch.len();
            self.upload(&wrap(batch), count, what);
            pacer.posted(Instant::now());
        }
    }

    /// `UploadAsync`: up to [`MAX_UPLOAD_ATTEMPTS`]; one last try after the stop.
    fn upload(&self, body: &AgentPollRequest, count: usize, what: &str) {
        for attempt in 1..=MAX_UPLOAD_ATTEMPTS {
            let timeout = match self.deadline.get() {
                Some(deadline) if self.stop.is_stopped() => deadline
                    .saturating_duration_since(Instant::now())
                    .clamp(Duration::from_millis(250), UPLOAD_TIMEOUT),
                _ => UPLOAD_TIMEOUT,
            };
            match self.cloud.exchange(RESULTS_PATH, body, timeout) {
                Ok(reply) => {
                    self.dispatch(reply.requests);
                    return;
                }
                Err(e) if attempt == MAX_UPLOAD_ATTEMPTS || self.stop.is_stopped() => {
                    dl_log::error!("Dropped uplink batch after failed attempts."; count = count, what = what, attempts = attempt, error = %e);
                    let mut ctx = Map::new();
                    ctx.insert("count", dl_json::Value::from(count as u64));
                    self.health.report(
                        Severity::Error,
                        health::CLOUD_FAILED,
                        &format!("Dropped {count} {what} after {attempt} failed attempts."),
                        ctx,
                    );
                    return;
                }
                Err(e) => {
                    dl_log::warn!("Sending uplink batch failed; retrying."; count = count, what = what, attempt = attempt, error = %e);
                    // A stop cuts the wait short; the next round is the last try.
                    self.stop.wait_timeout(backoff(attempt));
                }
            }
        }
    }
}
