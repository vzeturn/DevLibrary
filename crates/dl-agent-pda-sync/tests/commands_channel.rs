//! The command channel's decisions and the order grants monitor (ports of the C#
//! `CommandChannelFailureCountTests` and `OrderGrantsTests`, plus the `HandleAsync`
//! rules of `CommandChannel.cs`).

mod commands_support;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use commands_support::{FakeStore, Throw, lock};
use dl_agent_pda_sync::bootstrap::{OrderGrantsProbe, probes::CHECKED};
use dl_agent_pda_sync::commands::channel::{PANICKED, runs_alone, web_socket_url};
use dl_agent_pda_sync::commands::grants::{
    RECHECK_INTERVAL, SERVICE_ATTEMPTS, SERVICE_GAP, next_wait,
};
use dl_agent_pda_sync::commands::{
    CommandChannel, CommandChannelParts, CommandError, CommandExecutor, GrantsRepair,
    OrderGrantsMonitor, RepairAttempt, RepairOutcome, Result as CommandResult, SiteCommandExecutor,
    next_failure_count,
};
use dl_agent_pda_sync::config::{
    AgentIdentity, AgentOptions, AgentPaths, ManualClock, SharedClock, Timestamp, codes,
};
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::time::DateTimeOffset;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, OrderCheckOpenPayload, SyncCommand, SyncCommandResult, order_aggregate,
    order_command_reasons, order_command_types, site_event_levels, table_command_types,
};
use dl_agent_pda_sync::reporting::{Logger, SiteEvents};
use dl_agent_pda_sync::security::{SecretError, SecretStore, SharedSecretStore, names};
use dl_agent_pda_sync::transport::{RetryPolicy, SyncHttpClient, TokenProvider};
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_json::Value as Json;
use dl_uuid::Uuid;

const START: i64 = 1_794_729_600;

// ---- CommandChannelFailureCountTests

#[test]
fn a_drop_after_the_link_was_up_starts_a_fresh_count() {
    assert_eq!(next_failure_count(2, true), 1);
    assert_eq!(next_failure_count(0, true), 1);
}

#[test]
fn failures_to_connect_add_up_toward_the_poll_fallback() {
    let mut failures = 0;
    for _ in 0..3 {
        failures = next_failure_count(failures, false);
    }
    assert_eq!(failures, 3);
}

#[test]
fn the_socket_url_keeps_the_cloud_authority_and_maps_the_scheme() {
    assert_eq!(
        web_socket_url("https://cloud.test/"),
        "wss://cloud.test/sync/v1/commands/ws"
    );
    assert_eq!(
        web_socket_url("http://localhost:5080/api"),
        "ws://localhost:5080/sync/v1/commands/ws"
    );
}

// ---- HandleAsync / DecideAsync

#[derive(Default)]
struct MemorySecrets(Mutex<HashMap<String, Vec<u8>>>);

impl SecretStore for MemorySecrets {
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>, SecretError> {
        Ok(lock(&self.0).get(name).cloned())
    }
    fn save(&self, name: &str, value: &[u8]) -> Result<(), SecretError> {
        lock(&self.0).insert(name.to_owned(), value.to_vec());
        Ok(())
    }
    fn delete(&self, name: &str) -> Result<(), SecretError> {
        lock(&self.0).remove(name);
        Ok(())
    }
}

/// An executor that answers from a script and counts its calls.
struct ScriptedExecutor {
    calls: AtomicUsize,
    recorded: Option<SyncCommandResult>,
    fail: bool,
}

impl CommandExecutor for ScriptedExecutor {
    fn execute(&self, command: &SyncCommand, _ctx: &Ctx) -> CommandResult<SyncCommandResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(CommandError::Invalid("connection reset".into()));
        }
        Ok(SyncCommandResult {
            command_id: command.command_id,
            status: CommandStatus::Applied,
            reason: None,
            result: None,
            at_utc: DateTimeOffset::from_unix_seconds(START).unwrap(),
            exec_ms: None,
            hops: Default::default(),
        })
    }

    fn find_recorded(&self, _id: Uuid, _ctx: &Ctx) -> CommandResult<Option<SyncCommandResult>> {
        Ok(self.recorded.clone())
    }
}

fn clock() -> SharedClock {
    Arc::new(ManualClock::new(Timestamp::from_unix_seconds(START)))
}

fn channel(executor: Arc<dyn CommandExecutor>, tenant: Option<&str>) -> CommandChannel {
    let log = Logger::none();
    let http = Arc::new(
        SyncHttpClient::new(
            "https://cloud.test",
            Duration::from_secs(5),
            None,
            RetryPolicy::default(),
            &log,
        )
        .unwrap(),
    );
    let secrets: SharedSecretStore = Arc::new(MemorySecrets::default());
    let tokens = Arc::new(TokenProvider::new(
        http.clone(),
        secrets,
        AgentPaths::new(std::env::temp_dir().join("pda-c1-channel")),
        clock(),
        &log,
    ));
    CommandChannel::new(
        CommandChannelParts {
            http,
            tokens,
            executor,
            retry: RetryPolicy::default(),
            options: AgentOptions::default(),
            clock: clock(),
            tenant_id: tenant.map(str::to_owned),
        },
        &log,
    )
}

fn scripted(recorded: Option<SyncCommandResult>, fail: bool) -> Arc<ScriptedExecutor> {
    Arc::new(ScriptedExecutor {
        calls: AtomicUsize::new(0),
        recorded,
        fail,
    })
}

fn command() -> SyncCommand {
    let p = OrderCheckOpenPayload {
        rvc_no: 3,
        table_no: "T1".into(),
        num_guest: 2,
        cashier: "clerk-7".into(),
    };
    SyncCommand {
        command_id: Uuid::new_v4(),
        type_: order_command_types::CHECK_OPEN.into(),
        aggregate_key: order_aggregate::open(3, Uuid::new_v4()),
        expected_hash: None,
        payload: p.to_element(),
        created_by: "clerk-7".into(),
        expires_at_utc: None,
        contract_version: 1,
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
    }
}

#[test]
fn a_terminal_result_is_remembered_and_a_redelivery_gets_it_back_without_running_again() {
    let exec = scripted(None, false);
    let ch = channel(exec.clone(), None);
    let cmd = command();
    let ctx = Ctx::background();

    let first = ch.handle(&cmd, &ctx).unwrap();
    let again = ch.handle(&cmd, &ctx).unwrap();

    assert_eq!(first.status, CommandStatus::Applied);
    assert!(
        first.exec_ms.is_some(),
        "each result carries how long the store took"
    );
    assert_eq!(again, first);
    assert_eq!(exec.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn a_newer_contract_and_another_tenant_are_refused_unexecuted() {
    let exec = scripted(None, false);
    let tenant = Uuid::new_v4();
    let ch = channel(exec.clone(), Some(&tenant.to_string()));
    let ctx = Ctx::background();

    let mut newer = command();
    newer.contract_version = 2;
    assert_eq!(
        ch.handle(&newer, &ctx).unwrap().reason.as_deref(),
        Some("contract_version_unsupported")
    );

    let mut foreign = command();
    foreign.tenant_id = Some(Uuid::new_v4());
    assert_eq!(
        ch.handle(&foreign, &ctx).unwrap().reason.as_deref(),
        Some("tenant_mismatch")
    );

    let mut own = command();
    own.tenant_id = Some(tenant);
    assert_eq!(
        ch.handle(&own, &ctx).unwrap().status,
        CommandStatus::Applied
    );
    assert_eq!(exec.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn an_expired_command_is_refused_unless_it_already_ran() {
    let ctx = Ctx::background();
    let mut cmd = command();
    cmd.expires_at_utc = Some(DateTimeOffset::from_unix_seconds(START - 1).unwrap());

    let none = scripted(None, false);
    let r = channel(none.clone(), None).handle(&cmd, &ctx).unwrap();
    assert_eq!(
        (r.status, r.reason.as_deref()),
        (CommandStatus::Rejected, Some("expired"))
    );
    assert_eq!(none.calls.load(Ordering::SeqCst), 0);

    let logged = SyncCommandResult {
        command_id: cmd.command_id,
        status: CommandStatus::Applied,
        reason: None,
        result: Some(Json::Null),
        at_utc: DateTimeOffset::from_unix_seconds(START - 5).unwrap(),
        exec_ms: None,
        hops: Default::default(),
    };
    let ran = scripted(Some(logged.clone()), false);
    assert_eq!(channel(ran, None).handle(&cmd, &ctx).unwrap(), logged);
}

#[test]
fn a_command_that_throws_is_an_unknown_outcome_that_is_not_remembered() {
    let exec = scripted(None, true);
    let ch = channel(exec.clone(), None);
    let cmd = command();
    let ctx = Ctx::background();

    let r = ch.handle(&cmd, &ctx).unwrap();
    assert_eq!(
        (r.status, r.reason.as_deref()),
        (CommandStatus::Unknown, Some("InvalidOperationException"))
    );
    ch.handle(&cmd, &ctx).unwrap();
    assert_eq!(
        exec.calls.load(Ordering::SeqCst),
        2,
        "the cloud retries an unknown outcome"
    );
}

/// Panics on its first run, then applies (RT-1318 review: a pool worker's command panics).
struct PanicOnceExecutor(AtomicUsize);

impl CommandExecutor for PanicOnceExecutor {
    fn execute(&self, command: &SyncCommand, _ctx: &Ctx) -> CommandResult<SyncCommandResult> {
        assert!(
            self.0.fetch_add(1, Ordering::SeqCst) > 0,
            "store driver bug"
        );
        Ok(SyncCommandResult {
            command_id: command.command_id,
            status: CommandStatus::Applied,
            reason: None,
            result: None,
            at_utc: DateTimeOffset::from_unix_seconds(START).unwrap(),
            exec_ms: None,
            hops: Default::default(),
        })
    }

    fn find_recorded(&self, _id: Uuid, _ctx: &Ctx) -> CommandResult<Option<SyncCommandResult>> {
        Ok(None)
    }
}

#[test]
fn a_command_that_panics_is_answered_unknown_and_runs_again_on_redelivery() {
    let exec = Arc::new(PanicOnceExecutor(AtomicUsize::new(0)));
    let ch = channel(exec.clone(), None);
    let cmd = command();
    let ctx = Ctx::background();

    let r = ch.handle_guarded(&cmd, &ctx).unwrap();
    assert_eq!(
        (r.status, r.reason.as_deref()),
        (CommandStatus::Unknown, Some(PANICKED))
    );
    let again = ch.handle_guarded(&cmd, &ctx).unwrap();
    assert_eq!(again.status, CommandStatus::Applied);
    assert_eq!(exec.0.load(Ordering::SeqCst), 2);
}

#[test]
fn commands_that_touch_several_checks_or_a_whole_rvc_run_alone() {
    let with = |type_: &str, key: &str| SyncCommand {
        type_: type_.into(),
        aggregate_key: key.into(),
        ..command()
    };
    assert!(runs_alone(&with(
        table_command_types::CHECK_MERGE,
        "check:3:12"
    )));
    assert!(runs_alone(&with(
        table_command_types::ITEMS_MOVE,
        "check:3:12"
    )));
    assert!(runs_alone(&with(
        table_command_types::TABLE_TRANSFER,
        "check:3:12"
    )));
    assert!(runs_alone(&with("site.anything", "rvc:3")));
    assert!(!runs_alone(&with(
        order_command_types::CHECK_OPEN,
        "check:3:12"
    )));
    assert!(!runs_alone(&command()));
}

// ---- OrderGrantsTests

/// Level, code, message, detail.
type Reported = (String, String, String, Option<dl_json::Map>);

#[derive(Default)]
struct RecordingEvents(Mutex<Vec<Reported>>);

impl SiteEvents for RecordingEvents {
    fn report(&self, level: &str, code: &str, message: &str, detail: Option<dl_json::Map>) {
        lock(&self.0).push((level.into(), code.into(), message.into(), detail));
    }
}

impl RecordingEvents {
    fn codes(&self) -> Vec<String> {
        lock(&self.0).iter().map(|e| e.1.clone()).collect()
    }
}

#[derive(Default)]
struct FakeProbe {
    missing: Mutex<Vec<String>>,
    calls: AtomicUsize,
}

impl OrderGrantsProbe for FakeProbe {
    fn missing(
        &self,
        _i: &str,
        _d: &str,
        _p: &str,
        _ctx: &Ctx,
    ) -> Result<Vec<String>, dl_agent_pda_sync::bootstrap::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(lock(&self.missing).clone())
    }
}

impl FakeProbe {
    fn set(&self, m: &[&str]) {
        *lock(&self.missing) = m.iter().map(|s| (*s).to_owned()).collect();
    }
}

struct FakeRepair {
    outcome: RepairOutcome,
    probe: Arc<FakeProbe>,
    calls: AtomicUsize,
}

impl FakeRepair {
    /// A repair whose identities end as `outcomes` (`(identity, outcome)`).
    fn new(probe: &Arc<FakeProbe>, outcomes: &[(&str, &str)]) -> Arc<FakeRepair> {
        Arc::new(FakeRepair {
            outcome: RepairOutcome {
                attempts: outcomes
                    .iter()
                    .map(|(identity, outcome)| RepairAttempt {
                        identity: (*identity).into(),
                        outcome: (*outcome).into(),
                    })
                    .collect(),
            },
            probe: probe.clone(),
            calls: AtomicUsize::new(0),
        })
    }
}

impl GrantsRepair for FakeRepair {
    fn try_repair(&self, _ctx: &Ctx) -> RepairOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.outcome.applied_by().is_some() {
            self.probe.set(&[]);
        }
        self.outcome.clone()
    }
}

struct Grants {
    events: Arc<RecordingEvents>,
    probe: Arc<FakeProbe>,
    clock: Arc<ManualClock>,
    secrets: SharedSecretStore,
}

impl Grants {
    fn new() -> Grants {
        let secrets: SharedSecretStore = Arc::new(MemorySecrets::default());
        secrets.save(names::SQL_PASSWORD, b"pw").unwrap();
        Grants {
            events: Arc::new(RecordingEvents::default()),
            probe: Arc::new(FakeProbe::default()),
            clock: Arc::new(ManualClock::new(Timestamp::from_unix_seconds(START))),
            secrets,
        }
    }

    fn monitor(&self, repair: Option<Arc<dyn GrantsRepair>>) -> OrderGrantsMonitor {
        let identity = AgentIdentity {
            agent_id: Uuid::new_v4(),
            tenant_id: "t1".into(),
            cloud_url: "https://cloud.test".into(),
            machine_id: "PC".into(),
            sql_instance: Some(r".\SQLEXPRESS".into()),
            sql_database: Some("SMILE_POS".into()),
        };
        OrderGrantsMonitor::new(
            self.events.clone(),
            self.probe.clone(),
            identity,
            self.secrets.clone(),
            self.clock.clone(),
            &Logger::none(),
            repair,
        )
    }

    fn past_the_gap(&self) {
        self.clock.advance(Duration::from_secs(61));
    }
}

#[test]
fn an_unreadable_command_with_an_id_is_rejected_terminally_instead_of_expiring() {
    let ch = channel(
        Arc::new(SiteCommandExecutor::new(
            FakeStore::new(),
            clock(),
            &Logger::none(),
        )),
        None,
    );
    let id = Uuid::new_v4();
    let frame = dl_json::parse(&format!(r#"{{"command_id":"{id}","type":42}}"#)).unwrap();

    let r = ch.unreadable(&frame).unwrap();

    assert_eq!((r.command_id, r.status), (id, CommandStatus::Rejected));
    assert!(
        r.reason
            .as_deref()
            .unwrap()
            .starts_with("invalid_payload: "),
        "{:?}",
        r.reason
    );
    assert!(
        ch.unreadable(&dl_json::parse(r#"{"type":"x"}"#).unwrap())
            .is_none()
    );
}

#[test]
fn missing_grants_send_one_event_with_the_instruction_per_episode() {
    let g = Grants::new();
    g.probe.set(&["EXECUTE dbo.PostITrn"]);
    let m = g.monitor(None);
    let ctx = Ctx::background();

    assert_eq!(m.check_grants(&ctx), Some(true));
    assert_eq!(m.check_grants(&ctx), Some(true));

    let events = lock(&g.events.0);
    assert_eq!(events.len(), 1);
    let (level, code, message, _) = &events[0];
    assert_eq!(
        (level.as_str(), code.as_str()),
        (site_event_levels::ERROR, codes::SQL_GRANTS_MISSING)
    );
    assert!(message.contains("dl-agent pda-grants"));
    assert!(
        !message.contains("irm "),
        "never the old agent's installer: {message}"
    );
    assert!(message.contains("EXECUTE dbo.PostITrn"));
    assert_eq!(
        g.probe.calls.load(Ordering::SeqCst),
        1,
        "a burst shares one probe"
    );
}

#[test]
fn present_grants_send_nothing_and_a_new_episode_reports_again() {
    let g = Grants::new();
    let m = g.monitor(None);
    let ctx = Ctx::background();
    assert_eq!(m.check_grants(&ctx), Some(false));
    assert!(g.events.codes().is_empty());

    g.probe.set(&["INSERT dbo.tmpInvoice"]);
    assert_eq!(m.check_grants(&ctx), Some(true));
    assert_eq!(g.events.codes().len(), 1);

    g.past_the_gap();
    g.probe.set(&[]);
    assert_eq!(m.check_grants(&ctx), Some(false));
    g.probe.set(&["INSERT dbo.tmpInvoice"]);
    assert_eq!(m.check_grants(&ctx), Some(true));
    assert_eq!(g.events.codes().len(), 2);
}

#[test]
fn an_order_command_denied_by_sql_is_rejected_terminally_and_checks_the_grants() {
    let g = Grants::new();
    g.probe.set(&["EXECUTE dbo.PostITrn"]);
    let store = FakeStore::new();
    store.knobs().throw_in_work = Some(Throw::Sql(
        229,
        "The EXECUTE permission was denied on the object 'PostITrn'".into(),
    ));
    let executor = SiteCommandExecutor::new(store.clone(), clock(), &Logger::none())
        .with_grants(Arc::new(g.monitor(None)));

    let r = executor.execute(&command(), &Ctx::background()).unwrap();

    assert_eq!(r.status, CommandStatus::Rejected);
    assert_eq!(
        r.reason.as_deref(),
        Some(order_command_reasons::SQL_GRANTS_MISSING)
    );
    assert_eq!(g.events.codes(), [codes::SQL_GRANTS_MISSING]);
    assert!(
        lock(&store.log).is_empty(),
        "nothing is logged, so a replay after `grants` applies normally"
    );
}

#[test]
fn the_service_applies_missing_grants_itself_as_the_smile_login_when_windows_is_refused() {
    let g = Grants::new();
    g.probe.set(&["UPDATE dbo.tmpInvoice(NGuest)"]);
    let repair = FakeRepair::new(
        &g.probe,
        &[("windows", codes::SQL_NO_ACCESS), ("sql:smile", "applied")],
    );

    assert_eq!(
        g.monitor(Some(repair.clone()))
            .check_grants(&Ctx::background()),
        Some(false)
    );

    assert_eq!(repair.calls.load(Ordering::SeqCst), 1);
    let events = lock(&g.events.0);
    assert_eq!(events.len(), 1);
    let (_, code, message, detail) = &events[0];
    assert_eq!(code, codes::SQL_GRANTS_REPAIRED);
    assert!(message.contains("as sql:smile"), "{message}");
    let detail = detail.as_ref().unwrap();
    assert_eq!(
        detail.get("identity").and_then(Json::as_str),
        Some("sql:smile")
    );
}

#[test]
fn a_refused_self_repair_is_retried_each_round_and_reported_once() {
    let g = Grants::new();
    g.probe.set(&["EXECUTE dbo.PostITrn"]);
    let repair = FakeRepair::new(
        &g.probe,
        &[
            ("windows", codes::SQL_NO_ACCESS),
            ("sql:smile", codes::SQL_LOGIN_FAILED),
        ],
    );
    let m = g.monitor(Some(repair.clone()));
    let ctx = Ctx::background();

    assert_eq!(m.check_grants(&ctx), Some(true));
    g.past_the_gap();
    assert_eq!(m.check_grants(&ctx), Some(true));

    assert_eq!(
        repair.calls.load(Ordering::SeqCst),
        2,
        "the repair is not once per process"
    );
    let events = lock(&g.events.0);
    assert_eq!(events.len(), 1, "one event per episode");
    let (_, code, message, detail) = &events[0];
    assert_eq!(code, codes::SQL_GRANTS_MISSING);
    assert!(message.contains("dl-agent pda-grants"), "{message}");
    assert!(message.contains("every 15 minutes"), "{message}");
    let detail = detail.as_ref().unwrap();
    assert_eq!(
        detail.get("self_repair").and_then(Json::as_str),
        Some("windows: SQL_NO_ACCESS; sql:smile: SQL_LOGIN_FAILED")
    );
    let identities = detail.get("identities").and_then(Json::as_array).unwrap();
    assert_eq!(identities.len(), 2);
}

#[test]
fn a_refused_order_command_never_runs_the_repair_and_wakes_the_service() {
    let g = Grants::new();
    g.probe.set(&["EXECUTE dbo.PostITrn"]);
    let repair = FakeRepair::new(&g.probe, &[("windows", "applied")]);
    let monitor = Arc::new(g.monitor(Some(repair.clone())));
    let store = FakeStore::new();
    store.knobs().throw_in_work = Some(Throw::Sql(
        229,
        "The EXECUTE permission was denied on the object 'PostITrn'".into(),
    ));
    let executor = SiteCommandExecutor::new(store.clone(), clock(), &Logger::none())
        .with_grants(monitor.clone());

    let r = executor.execute(&command(), &Ctx::background()).unwrap();

    assert_eq!(r.status, CommandStatus::Rejected);
    assert_eq!(
        r.reason.as_deref(),
        Some(order_command_reasons::SQL_GRANTS_MISSING)
    );
    assert_eq!(
        repair.calls.load(Ordering::SeqCst),
        0,
        "the command is answered before any repair"
    );
    assert!(
        g.events.codes().is_empty(),
        "SQL_GRANTS_MISSING waits until every identity failed"
    );
    assert!(monitor.take_wake(), "the service's loop repairs at once");
    assert!(!monitor.take_wake());
}

#[test]
fn the_service_rechecks_while_the_grants_are_missing_and_sleeps_when_they_are_present() {
    assert_eq!(next_wait(Some(true), 0), Some(RECHECK_INTERVAL));
    assert_eq!(next_wait(Some(false), 0), None);
    assert_eq!(next_wait(None, 1), Some(SERVICE_GAP));
    assert_eq!(next_wait(None, SERVICE_ATTEMPTS), Some(RECHECK_INTERVAL));
}

#[test]
fn the_probe_covers_the_guest_count_grant_of_ow1c() {
    assert!(CHECKED.contains(&("dbo.tmpInvoice", "UPDATE", Some("NGuest"))));
}

#[test]
fn an_answer_carries_when_the_agent_got_ran_and_finished_the_command() {
    use dl_agent_pda_sync::contracts::wire::ResultHops;
    use dl_agent_pda_sync::util::hop_clock;
    let ch = channel(scripted(None, false), None);
    let cmd = command();
    let ctx = Ctx::background();
    let before = hop_clock::now_micros();

    let first = ch.answer(&cmd, &ctx).unwrap();
    let h = first.hops;
    let (got, ran, done) = (
        h.t_received.unwrap(),
        h.t_started.unwrap(),
        h.t_finished.unwrap(),
    );
    assert!(before <= got && got <= ran && ran <= done, "{h:?}");
    assert!(first.to_json().contains(&format!("\"t_finished\":{done}")));

    // A redelivery gets the remembered result with this delivery's stamps.
    let again = ch.answer(&cmd, &ctx).unwrap();
    assert_eq!(again.status, first.status);
    assert!(again.hops.t_received.unwrap() >= done);
    assert_eq!(
        ch.handle(&cmd, &ctx).unwrap().hops,
        ResultHops::default(),
        "the remembered result itself holds no stamps"
    );
}
