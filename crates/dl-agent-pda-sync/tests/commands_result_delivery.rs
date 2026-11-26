//! RT-1309: a command result is delivered durably. It is kept until the cloud acknowledged
//! it, posted again with backoff when the link or the cloud failed, flushed first on a
//! reconnect, and a redelivered command is answered again with the same stored result
//! without running twice.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use dl_agent_pda_sync::commands::result_outbox::{
    PostOutcome, ResultOutbox, ResultPoster, classify,
};
use dl_agent_pda_sync::commands::{
    ChannelError, CommandChannel, CommandChannelParts, CommandExecutor, Result as CommandResult,
    reconnect_delay,
};
use dl_agent_pda_sync::config::{
    AgentError, AgentOptions, AgentPaths, ManualClock, SharedClock, Timestamp, codes,
};
use dl_agent_pda_sync::contracts::time::DateTimeOffset;
use dl_agent_pda_sync::contracts::wire::{CommandStatus, SyncCommand, SyncCommandResult};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::security::{SecretError, SecretStore, SharedSecretStore};
use dl_agent_pda_sync::transport::{RetryPolicy, SyncHttpClient, TokenProvider};
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_json::Value as Json;
use dl_uuid::Uuid;

const START: i64 = 1_794_729_600;

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
}

impl CommandExecutor for CountingExecutor {
    fn execute(&self, c: &SyncCommand, _ctx: &Ctx) -> CommandResult<SyncCommandResult> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(SyncCommandResult {
            command_id: c.command_id,
            status: CommandStatus::Applied,
            reason: None,
            result: None,
            at_utc: DateTimeOffset::from_unix_seconds(START).unwrap(),
            exec_ms: None,
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
    Arc::new(CountingExecutor {
        runs: AtomicUsize::new(0),
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
    assert_eq!(again, first, "the same stored result");
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
    assert_eq!(again, first);
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
        .map(|_| {
            CountingExecutor {
                runs: AtomicUsize::new(0),
            }
            .execute(&command(), &ctx)
            .unwrap()
        })
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
