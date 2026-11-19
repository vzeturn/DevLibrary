//! The full runtime of the service (`AgentHost.AddAgentRuntime`): every hosted loop of
//! this build, wired with the real site events and capture runner, against the fake
//! cloud and a POS database that cannot be reached; it starts in the C# registration
//! order and stops cleanly.

mod outbox_support;
mod support;

use dl_agent_pda_sync::capture::{CaptureError, SourceFactory};
use dl_agent_pda_sync::config::{AgentIdentity, AgentOptions, Configuration};
use dl_agent_pda_sync::hosting::HostContext;
use dl_agent_pda_sync::hosting::cli::Unhandled;
use dl_agent_pda_sync::hosting::host::{
    AgentServices, CapturePlugin, RuntimeHooks, build_host, capture_with, hooks,
};
use dl_agent_pda_sync::reporting::sql_error::SqlFailure;
use dl_agent_pda_sync::reporting::{SiteEventLogBridge, SiteEventStore, SqliteSiteEventStore};
use dl_agent_pda_sync::security::{AgentKey, names};
use outbox_support::FakeCloud;
use std::sync::Arc;
use std::time::{Duration, Instant};
use support::TestEnv;

/// A POS reader whose server never answers (SQL error 53).
fn unreachable_sql(s: &AgentServices) -> Result<CapturePlugin, Unhandled> {
    let source: SourceFactory = Arc::new(|| {
        Err(CaptureError::sql(SqlFailure::single(
            53,
            20,
            "A network-related or instance-specific error occurred while establishing a connection to SQL Server.",
        )))
    });
    capture_with(s, source)
}

#[test]
fn the_full_runtime_starts_in_registration_order_and_stops_cleanly() {
    let env = TestEnv::new();
    let cloud = FakeCloud::start();
    let (_, _) = AgentKey::load_or_create(env.secrets.as_ref()).unwrap();
    env.secrets
        .save(names::AGENT_TOKEN, cloud.agent_token.as_bytes())
        .unwrap();
    let identity = AgentIdentity {
        agent_id: cloud.agent_id,
        tenant_id: "tenant-1".into(),
        cloud_url: cloud.base.clone(),
        machine_id: "m".into(),
        sql_instance: None,
        sql_database: None,
    };
    let host = HostContext {
        configuration: Configuration::new(),
        options: AgentOptions {
            data_dir: Some(env.paths.data_dir().to_string_lossy().into_owned()),
            ..AgentOptions::default()
        },
        paths: env.paths.clone(),
    };
    let services = AgentServices::build(
        &host,
        identity,
        "agent-test".into(),
        env.log.clone(),
        env.secrets.clone(),
    )
    .unwrap();
    let real = hooks();
    let runtime = RuntimeHooks {
        capture: Some(unreachable_sql),
        ..real
    };
    let bridge = Arc::new(SiteEventLogBridge::new(services.clock.clone()));
    let agent = build_host(&services, &runtime, Some(&bridge)).unwrap();
    assert_eq!(
        agent.service_names(),
        [
            "SiteEvents",
            "Uploader",
            "BlobUploader",
            "Heartbeat",
            "Commands",
            "OrderGrantsService",
            "Capture",
            "PrintRelay",
            "StaffSync",
            "SelfUpdate",
        ]
    );
    assert!(env.paths.events_db().is_file());

    let stop = dl_winsvc::StopSignal::new();
    let s = stop.clone();
    let c = cloud.state.clone();
    let stopper = std::thread::spawn(move || {
        // Stop once the heartbeat reached the cloud (or after 15 s).
        let until = Instant::now() + Duration::from_secs(15);
        while c.lock().unwrap().heartbeats.is_empty() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(300));
        s.stop();
    });
    let started = Instant::now();
    let outcome = agent
        .with_shutdown_timeout(Duration::from_secs(20))
        .run(&stop)
        .unwrap();
    stopper.join().unwrap();
    assert!(outcome.abandoned.is_empty(), "{outcome:?}");
    assert_eq!(
        outcome.stopped,
        [
            "SelfUpdate",
            "StaffSync",
            "PrintRelay",
            "Capture",
            "OrderGrantsService",
            "Commands",
            "Heartbeat",
            "BlobUploader",
            "Uploader",
            "SiteEvents",
        ]
    );
    assert!(started.elapsed() < Duration::from_secs(25));

    let heartbeats = cloud.with(|c| c.heartbeats.clone());
    assert!(!heartbeats.is_empty(), "no heartbeat reached the cloud");
    assert_eq!(heartbeats[0].agent_id, cloud.agent_id.to_string());

    // The fake cloud has no events route (404): the events stay queued in events.db,
    // with the stop event last (the site events loop stops after every other loop).
    let queue = SqliteSiteEventStore::try_open(env.paths.events_db(), 100).unwrap();
    let codes: Vec<String> = queue
        .peek(100)
        .unwrap()
        .into_iter()
        .map(|e| e.code)
        .collect();
    assert!(codes.iter().any(|c| c == "AGENT_STARTED"), "{codes:?}");
    assert_eq!(codes.last().map(String::as_str), Some("AGENT_STOPPING"));
}

#[test]
fn an_enum_log_value_renders_without_quotes_like_a_serilog_enum_scalar() {
    use dl_agent_pda_sync::reporting::Value;
    use dl_agent_pda_sync::reporting::format::{as_text, render_value};
    let state = Value::enum_name("Acking");
    assert_eq!(render_value(&state, None), "Acking");
    assert_eq!(render_value(&Value::from("Acking"), None), "\"Acking\"");
    assert_eq!(
        render_value(&Value::Seq(vec![state.clone(), Value::from("x")]), None),
        "[Acking, \"x\"]"
    );
    assert_eq!(as_text(&state), "Acking");
    assert!(state.is_scalar());
}
