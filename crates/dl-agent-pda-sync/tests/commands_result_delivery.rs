//! RT-1309: a command result is delivered durably. It is kept until the cloud acknowledged
//! it, posted again with backoff when the link or the cloud failed, flushed first on a
//! reconnect, and a redelivered command is answered again with the same stored result
//! without running twice.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use dl_agent_pda_sync::commands::keyed_pool;
use dl_agent_pda_sync::commands::result_outbox::{
    PostOutcome, ResultOutbox, ResultPoster, SOCKET_CONFIRM_WAIT, classify, parse_result_ack,
};
use dl_agent_pda_sync::commands::{
    ChannelError, CommandChannel, CommandChannelParts, CommandExecutor, Result as CommandResult,
    reconnect_delay,
};
use dl_agent_pda_sync::config::{
    AgentError, AgentOptions, AgentPaths, ManualClock, SharedClock, Timestamp, codes,
};
use dl_agent_pda_sync::contracts::time::DateTimeOffset;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, ResultHops, SyncCommand, SyncCommandResult,
};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::security::{SecretError, SecretStore, SharedSecretStore};
use dl_agent_pda_sync::transport::{RetryPolicy, SyncHttpClient, TokenProvider};
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_json::Value as Json;
use dl_uuid::Uuid;

const START: i64 = 1_794_729_600;

/// The stored result without its per-delivery stamps (RT-1318): every answer is stamped
/// when it was received and handled, a redelivery included.
fn unstamped(result: SyncCommandResult) -> SyncCommandResult {
    SyncCommandResult {
        hops: ResultHops::default(),
        ..result
    }
}

#[derive(Default)]
struct MemorySecrets(Mutex<HashMap<String, Vec<u8>>>);

impl SecretStore for MemorySecrets {
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>, SecretError> {
        Ok(self.0.lock().unwrap().get(name).cloned())
    }
    fn save(&self, name: &str, value: &[u8]) -> Result<(), SecretError> {
        self.0
            .lock()
            .unwrap()
            .insert(name.to_owned(), value.to_vec());
        Ok(())
    }
    fn delete(&self, name: &str) -> Result<(), SecretError> {
        self.0.lock().unwrap().remove(name);
        Ok(())
    }
}

struct CountingExecutor {
    runs: AtomicUsize,
    /// The simulated store round trip of each command.
    sql: Duration,
    /// Aggregates with a command running now: one aggregate never runs two at once.
    running: Mutex<HashSet<String>>,
}

impl CommandExecutor for CountingExecutor {
    fn execute(&self, c: &SyncCommand, _ctx: &Ctx) -> CommandResult<SyncCommandResult> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        if !self.sql.is_zero() {
            let first = self.running.lock().unwrap().insert(c.aggregate_key.clone());
            assert!(first, "two commands of {} ran at once", c.aggregate_key);
            std::thread::sleep(self.sql);
            self.running.lock().unwrap().remove(&c.aggregate_key);
        }
        Ok(SyncCommandResult {
            command_id: c.command_id,
            status: CommandStatus::Applied,
            reason: None,
            result: None,
            at_utc: DateTimeOffset::from_unix_seconds(START).unwrap(),
            exec_ms: None,
            hops: Default::default(),
        })
    }
}

/// Answers each post from a script (then acks) and records what was posted.
#[derive(Default)]
struct ScriptedPoster {
    script: Mutex<VecDeque<Result<PostOutcome, ()>>>,
    posted: Mutex<Vec<Uuid>>,
}

impl ScriptedPoster {
    fn with(script: Vec<Result<PostOutcome, ()>>) -> Arc<ScriptedPoster> {
        Arc::new(ScriptedPoster {
            script: Mutex::new(script.into()),
            posted: Mutex::default(),
        })
    }
    fn posted(&self) -> Vec<Uuid> {
        self.posted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl ResultPoster for ScriptedPoster {
    fn post(&self, r: &SyncCommandResult, _ctx: &Ctx) -> Result<PostOutcome, ChannelError> {
        self.posted.lock().unwrap().push(r.command_id);
        match self.script.lock().unwrap().pop_front() {
            Some(Err(())) => Err(ChannelError::Agent(AgentError::new(
                codes::CLOUD_UNREACHABLE,
                "connection reset",
            ))),
            Some(Ok(o)) => Ok(o),
            None => Ok(PostOutcome::Acked),
        }
    }
}

fn clock() -> SharedClock {
    Arc::new(ManualClock::new(Timestamp::from_unix_seconds(START)))
}

fn channel(exec: Arc<CountingExecutor>, poster: Arc<ScriptedPoster>) -> CommandChannel {
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
        AgentPaths::new(std::env::temp_dir().join("pda-rt1309-delivery")),
        clock(),
        &log,
    ));
    CommandChannel::with_poster(
        CommandChannelParts {
            http,
            tokens,
            executor: exec,
            retry: RetryPolicy {
                base_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(2),
                ..RetryPolicy::default()
            },
            options: AgentOptions::default(),
            clock: clock(),
            tenant_id: None,
        },
        &log,
        poster,
    )
}

fn command() -> SyncCommand {
    SyncCommand {
        command_id: Uuid::new_v4(),
        type_: "order.item_post".into(),
        aggregate_key: "order:3:x".into(),
        expected_hash: None,
        payload: Json::Null,
        created_by: "clerk-7".into(),
        expires_at_utc: None,
        contract_version: 1,
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
    }
}

fn exec() -> Arc<CountingExecutor> {
    slow_exec(Duration::ZERO)
}

fn slow_exec(sql: Duration) -> Arc<CountingExecutor> {
    Arc::new(CountingExecutor {
        runs: AtomicUsize::new(0),
        sql,
        running: Mutex::default(),
    })
}

// ---- (a) durable delivery

#[test]
fn a_result_stays_pending_through_failed_posts_until_the_cloud_acknowledges_it() {
    let poster = ScriptedPoster::with(vec![
        Err(()),
        Ok(PostOutcome::Retry(503)),
        Ok(PostOutcome::Acked),
    ]);
    let ch = channel(exec(), poster.clone());
    let cmd = command();
    let ctx = Ctx::background();

    ch.answer(&cmd, &ctx).unwrap();
    ch.flush_results(&ctx, false).unwrap();
    assert_eq!(
        ch.pending_results(),
        vec![cmd.command_id],
        "transport failure keeps it"
    );
    ch.flush_results(&ctx, true).unwrap();
    assert_eq!(
        ch.pending_results(),
        vec![cmd.command_id],
        "HTTP 503 keeps it"
    );
    ch.flush_results(&ctx, true).unwrap();
    assert!(ch.pending_results().is_empty(), "acknowledged: gone");
    assert_eq!(poster.posted().len(), 3);
}

#[test]
fn a_failed_post_is_not_retried_before_its_backoff_unless_forced_by_a_reconnect() {
    let poster = ScriptedPoster::with(vec![Err(())]);
    let ch = channel(exec(), poster.clone());
    let ctx = Ctx::background();
    ch.answer(&command(), &ctx).unwrap();
    ch.flush_results(&ctx, false).unwrap();
    assert_eq!(poster.posted().len(), 1);
    // The flush at link start is forced and posts at once, backoff or not.
    ch.flush_results(&ctx, true).unwrap();
    assert_eq!(poster.posted().len(), 2);
    assert!(ch.pending_results().is_empty());
}

#[test]
fn a_result_the_cloud_refuses_for_good_is_dropped_not_posted_forever() {
    let poster = ScriptedPoster::with(vec![Ok(PostOutcome::Refused(404))]);
    let ch = channel(exec(), poster.clone());
    let ctx = Ctx::background();
    ch.answer(&command(), &ctx).unwrap();
    ch.flush_results(&ctx, false).unwrap();
    assert!(ch.pending_results().is_empty());
    assert_eq!(classify(204), PostOutcome::Acked);
    assert_eq!(classify(404), PostOutcome::Refused(404));
    assert_eq!(classify(401), PostOutcome::Retry(401));
    assert_eq!(classify(429), PostOutcome::Retry(429));
    assert_eq!(classify(502), PostOutcome::Retry(502));
}

// ---- (b) redelivery

#[test]
fn a_redelivered_command_is_answered_again_with_the_same_result_and_runs_once() {
    // The first answer was written to a dead link and its post failed: still pending.
    let poster = ScriptedPoster::with(vec![Err(())]);
    let executor = exec();
    let ch = channel(executor.clone(), poster.clone());
    let cmd = command();
    let ctx = Ctx::background();

    let first = ch.answer(&cmd, &ctx).unwrap();
    ch.flush_results(&ctx, false).unwrap();
    // The cloud redelivers after its lease: answered again, never silently dropped.
    let again = ch.answer(&cmd, &ctx).unwrap();
    assert_eq!(unstamped(again), unstamped(first), "the same stored result");
    assert_eq!(executor.runs.load(Ordering::SeqCst), 1, "executed once");
    assert_eq!(
        ch.pending_results(),
        vec![cmd.command_id],
        "one entry, due at once"
    );
    ch.flush_results(&ctx, false).unwrap();
    assert!(ch.pending_results().is_empty());
    assert_eq!(poster.posted().len(), 2);
}

#[test]
fn a_redelivery_after_the_result_was_acknowledged_is_answered_again_too() {
    let poster = ScriptedPoster::with(vec![]);
    let executor = exec();
    let ch = channel(executor.clone(), poster.clone());
    let cmd = command();
    let ctx = Ctx::background();
    let first = ch.answer(&cmd, &ctx).unwrap();
    ch.flush_results(&ctx, false).unwrap();
    assert!(ch.pending_results().is_empty());
    let again = ch.answer(&cmd, &ctx).unwrap();
    ch.flush_results(&ctx, false).unwrap();
    assert_eq!(unstamped(again), unstamped(first));
    assert_eq!(executor.runs.load(Ordering::SeqCst), 1);
    assert_eq!(poster.posted(), vec![cmd.command_id, cmd.command_id]);
}

// ---- (c) reconnect

#[test]
fn a_link_that_was_up_and_dropped_reconnects_at_once_and_a_dead_one_backs_off() {
    let retry = RetryPolicy {
        base_delay: Duration::from_millis(500),
        jitter: Arc::new(|| 0.5),
        ..RetryPolicy::default()
    };
    assert_eq!(
        reconnect_delay(&retry, 1, Some(Duration::from_secs(61))),
        Duration::ZERO,
        "a stable link that dropped"
    );
    assert!(
        reconnect_delay(&retry, 1, Some(Duration::from_millis(200))) > Duration::ZERO,
        "a link that died at once backs off (no busy loop)"
    );
    assert!(
        reconnect_delay(&retry, 3, None) > reconnect_delay(&retry, 1, None),
        "failing to connect backs off more each time"
    );
}

#[test]
fn results_are_posted_beside_the_socket_loop_not_inline() {
    // RT-1309: a slow result post must not hold the next command; the loop runs on while
    // a thread beside it posts each result.
    struct SlowPoster(Mutex<Vec<Uuid>>);
    impl ResultPoster for SlowPoster {
        fn post(&self, r: &SyncCommandResult, _ctx: &Ctx) -> Result<PostOutcome, ChannelError> {
            std::thread::sleep(Duration::from_millis(300));
            self.0.lock().unwrap().push(r.command_id);
            Ok(PostOutcome::Acked)
        }
    }
    let poster = Arc::new(SlowPoster(Mutex::default()));
    let outbox = ResultOutbox::new(poster.clone(), RetryPolicy::default(), &Logger::none());
    let ctx = Ctx::background();
    let results: Vec<SyncCommandResult> = (0..3)
        .map(|_| exec().execute(&command(), &ctx).unwrap())
        .collect();

    let answered_in = outbox.post_beside(&ctx, || {
        let started = std::time::Instant::now();
        for r in &results {
            outbox.enqueue(r);
        }
        let answered = started.elapsed();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !outbox.is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        answered
    });

    assert!(answered_in < Duration::from_millis(50), "{answered_in:?}");
    assert!(outbox.is_empty(), "every result was posted beside the loop");
    assert_eq!(
        *poster.0.lock().unwrap(),
        results.iter().map(|r| r.command_id).collect::<Vec<_>>()
    );
}

// ---- (d) RT-1318: the socket is the fast path, HTTP the delayed fallback

fn result_of(cmd: &SyncCommand) -> SyncCommandResult {
    exec().execute(cmd, &Ctx::background()).unwrap()
}

#[test]
fn a_result_written_to_the_socket_is_not_posted_before_the_confirm_wait() {
    let poster = ScriptedPoster::with(vec![]);
    let outbox = ResultOutbox::new(poster.clone(), RetryPolicy::default(), &Logger::none());
    let ctx = Ctx::background();
    let cmd = command();
    outbox.enqueue_for_socket(&result_of(&cmd));
    outbox.flush(&ctx, false).unwrap();
    assert!(
        poster.posted().is_empty(),
        "no HTTP post while the socket may confirm"
    );
    assert_eq!(outbox.pending_ids(), vec![cmd.command_id]);
    // A dead link's forced flush posts it at once.
    outbox.flush(&ctx, true).unwrap();
    assert_eq!(poster.posted(), vec![cmd.command_id]);
    assert_eq!(SOCKET_CONFIRM_WAIT, Duration::from_secs(3));
}

#[test]
fn an_unconfirmed_socket_result_is_posted_after_the_wait() {
    let poster = ScriptedPoster::with(vec![]);
    let outbox = ResultOutbox::new(poster.clone(), RetryPolicy::default(), &Logger::none())
        .with_socket_wait(Duration::from_millis(30));
    let ctx = Ctx::background();
    let cmd = command();
    outbox.enqueue_for_socket(&result_of(&cmd));
    std::thread::sleep(Duration::from_millis(60));
    outbox.flush(&ctx, false).unwrap();
    assert_eq!(poster.posted(), vec![cmd.command_id]);
    assert!(outbox.is_empty());
}

#[test]
fn a_cloud_confirmation_frame_settles_the_result_without_a_post() {
    let poster = ScriptedPoster::with(vec![]);
    let outbox = ResultOutbox::new(poster.clone(), RetryPolicy::default(), &Logger::none());
    let cmd = command();
    outbox.enqueue_for_socket(&result_of(&cmd));
    let frame = format!(r#"{{"result_ack":"{}"}}"#, cmd.command_id);
    assert_eq!(parse_result_ack(&frame), Some(cmd.command_id));
    assert!(outbox.confirm(cmd.command_id));
    assert!(!outbox.confirm(cmd.command_id), "already settled");
    outbox.flush(&Ctx::background(), true).unwrap();
    assert!(poster.posted().is_empty());
    assert_eq!(parse_result_ack(r#"{"command_id":"x"}"#), None);
    assert_eq!(parse_result_ack(r#"{"result_ack":"not-a-uuid"}"#), None);
}

#[test]
fn a_late_ack_of_an_older_socket_answer_does_not_settle_the_newer_one() {
    let poster = ScriptedPoster::with(vec![]);
    let outbox = ResultOutbox::new(poster.clone(), RetryPolicy::default(), &Logger::none());
    let cmd = command();
    let mut unknown = result_of(&cmd);
    unknown.status = CommandStatus::Unknown;
    outbox.enqueue_for_socket(&unknown);
    // The redelivery's answer replaces it before the first frame's ack arrives.
    outbox.enqueue_for_socket(&result_of(&cmd));
    assert!(!outbox.confirm(cmd.command_id), "the old frame's ack");
    assert_eq!(outbox.pending_ids(), vec![cmd.command_id]);
    assert!(outbox.confirm(cmd.command_id), "the new frame's ack");
    assert!(outbox.is_empty());
    // An answer replaced by an HTTP-only one keeps the old frame's ack from settling it.
    outbox.enqueue_for_socket(&unknown);
    outbox.enqueue(&result_of(&cmd));
    assert!(!outbox.confirm(cmd.command_id));
    outbox.flush(&Ctx::background(), false).unwrap();
    assert_eq!(poster.posted(), vec![cmd.command_id], "posted over HTTP");
    assert!(outbox.is_empty());
}

// ---- (e) RT-1318 benchmark: commands of different checks run side by side

/// Throughput (commands/s) and p95 latency (cloud send to result back at the cloud) of 200
/// commands over 40 checks, offered at 200/s over a link of `LINK` each way, each command
/// taking `SQL` at the store, through the channel's `handle` on the keyed worker pool.
/// `workers = 1` is the serial socket loop before RT-1318.
fn simulate(workers: usize) -> (f64, Duration) {
    const COMMANDS: usize = 200;
    const CHECKS: usize = 40;
    const LINK: Duration = Duration::from_millis(30);
    const SQL: Duration = Duration::from_millis(30);
    const EVERY: Duration = Duration::from_millis(5);
    let exec = slow_exec(SQL);
    let ch = channel(exec.clone(), ScriptedPoster::with(vec![]));
    let ctx = Ctx::background();
    let done = Mutex::new(vec![None; COMMANDS]);
    let start = Instant::now();
    keyed_pool::run(
        workers,
        |(i, cmd): (usize, SyncCommand)| {
            let r = ch.handle(&cmd, &ctx).expect("not cancelled");
            assert_eq!(r.status, CommandStatus::Applied);
            done.lock().unwrap()[i] = Some(Instant::now());
        },
        |queue| {
            for i in 0..COMMANDS {
                let arrives = start + EVERY * u32::try_from(i).unwrap() + LINK;
                std::thread::sleep(arrives.saturating_duration_since(Instant::now()));
                let mut cmd = command();
                cmd.aggregate_key = format!("check:1:{}", i % CHECKS);
                queue.submit(&cmd.aggregate_key.clone(), (i, cmd));
            }
        },
    );
    let done = done.into_inner().unwrap();
    let mut latencies: Vec<Duration> = (0..COMMANDS)
        .map(|i| {
            let sent = start + EVERY * u32::try_from(i).unwrap();
            done[i].expect("every command answered") + LINK - sent
        })
        .collect();
    latencies.sort();
    let last = done.iter().flatten().max().copied().unwrap() + LINK;
    let throughput = COMMANDS as f64 / (last - start).as_secs_f64();
    assert_eq!(exec.runs.load(Ordering::SeqCst), COMMANDS);
    (throughput, latencies[COMMANDS * 95 / 100])
}

#[test]
#[ignore = "benchmark (~8 s): cargo test --test commands_result_delivery -- --ignored --nocapture"]
fn commands_of_different_checks_run_side_by_side() {
    let (serial_rate, serial_p95) = simulate(1);
    let (pool_rate, pool_p95) = simulate(AgentOptions::default().command_workers as usize);
    println!(
        "RT-1318 200 commands / 40 checks, 30 ms link, 30 ms SQL: \
         before {serial_rate:.0}/s p95 {} ms; after {pool_rate:.0}/s p95 {} ms",
        serial_p95.as_millis(),
        pool_p95.as_millis()
    );
    assert!(
        pool_rate > serial_rate * 3.0,
        "throughput {pool_rate:.0} vs {serial_rate:.0}"
    );
    assert!(
        pool_p95 * 5 < serial_p95,
        "p95 {pool_p95:?} vs {serial_p95:?}"
    );
}
