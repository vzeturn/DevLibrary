//! The service host (`AgentHost.cs`, `AgentServices.cs`, `Cli.RunServiceAsync`): loops
//! start in order, stop on the signal, and the runtime hooks plug in where C# registers
//! the services.

use dl_agent_pda_sync::config::{AgentIdentity, AgentPaths, AgentStatus, Configuration, codes};
use dl_agent_pda_sync::hosting::host::{
    AgentHost, AgentServices, CapturePlugin, NoCapture, RuntimeHooks, RuntimeParts, build_host,
    run_service,
};
use dl_agent_pda_sync::hosting::services::{FnService, LoopService, ServiceError};
use dl_agent_pda_sync::hosting::verbs;
use dl_agent_pda_sync::hosting::{EXIT_FAILED, HostContext, run_with};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::security::FileSecretStore;
use dl_agent_pda_sync::transport::RetryPolicy;
use dl_agent_pda_sync::util::console::MemoryLines;
use dl_agent_pda_sync::util::ctx::Ctx;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

struct Dir(PathBuf);

impl Dir {
    fn new() -> Dir {
        let d = std::env::temp_dir()
            .join("pda-sync-agent-tests")
            .join(dl_uuid::Uuid::new_v4().simple().to_string());
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn host_context(dir: &Dir) -> HostContext {
    let paths = AgentPaths::new(dir.0.join("data"));
    paths.ensure_created().unwrap();
    let options = dl_agent_pda_sync::config::AgentOptions {
        data_dir: Some(paths.data_dir().to_string_lossy().into_owned()),
        ..Default::default()
    };
    HostContext {
        configuration: Configuration::new(),
        options,
        paths,
    }
}

fn identity() -> AgentIdentity {
    AgentIdentity {
        agent_id: dl_uuid::Uuid::new_v4(),
        tenant_id: "tenant-1".into(),
        // Nothing listens on port 9: the heartbeat fails and restarts with backoff.
        cloud_url: "http://127.0.0.1:9/".into(),
        machine_id: "m".into(),
        sql_instance: None,
        sql_database: None,
    }
}

fn ticking(name: &'static str, ticks: Arc<AtomicU32>) -> Box<dyn LoopService> {
    Box::new(FnService::new(name, move |ctx: &Ctx| {
        while !ctx.is_done() {
            ticks.fetch_add(1, Ordering::SeqCst);
            ctx.sleep(Duration::from_millis(5));
        }
        Ok::<(), ServiceError>(())
    }))
}

#[test]
fn loops_run_until_the_stop_signal_and_stop_in_reverse_order() {
    let (a, b) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)));
    let host = AgentHost::new(
        vec![ticking("A", a.clone()), ticking("B", b.clone())],
        RetryPolicy::default(),
        Logger::none(),
    );
    assert_eq!(host.service_names(), ["A", "B"]);
    let stop = dl_winsvc::StopSignal::new();
    let s = stop.clone();
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        s.stop();
    });
    let outcome = host.run(&stop).unwrap();
    stopper.join().unwrap();
    assert_eq!(outcome.stopped, ["B", "A"]);
    assert!(outcome.abandoned.is_empty());
    assert!(a.load(Ordering::SeqCst) > 0 && b.load(Ordering::SeqCst) > 0);
}

#[test]
fn a_loop_that_ignores_the_stop_is_abandoned_at_the_shutdown_timeout() {
    let stubborn: Box<dyn LoopService> = Box::new(FnService::new("Stubborn", |_: &Ctx| {
        std::thread::sleep(Duration::from_millis(600));
        Ok::<(), ServiceError>(())
    }));
    let host = AgentHost::new(vec![stubborn], RetryPolicy::default(), Logger::none())
        .with_shutdown_timeout(Duration::from_millis(50));
    let stop = dl_winsvc::StopSignal::new();
    let s = stop.clone();
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        s.stop();
    });
    let outcome = host.run(&stop).unwrap();
    stopper.join().unwrap();
    assert_eq!(outcome.abandoned, ["Stubborn"]);
}

fn fake_capture(
    _: &AgentServices,
) -> Result<CapturePlugin, dl_agent_pda_sync::hosting::cli::Unhandled> {
    Ok(CapturePlugin {
        service: ticking("Capture", Arc::new(AtomicU32::new(0))),
        status: Arc::new(NoCapture),
        nudge: None,
        upload_wake: None,
    })
}

fn commands(
    _: &AgentServices,
    _: &RuntimeParts,
) -> Result<Vec<Box<dyn LoopService>>, dl_agent_pda_sync::hosting::cli::Unhandled> {
    Ok(vec![ticking("Commands", Arc::new(AtomicU32::new(0)))])
}

fn print_relay(
    _: &AgentServices,
    _: &RuntimeParts,
) -> Result<Vec<Box<dyn LoopService>>, dl_agent_pda_sync::hosting::cli::Unhandled> {
    Ok(vec![ticking("PrintRelay", Arc::new(AtomicU32::new(0)))])
}

#[test]
fn the_runtime_is_wired_in_the_csharp_registration_order_and_stops_cleanly() {
    let d = Dir::new();
    let host = host_context(&d);
    let secrets = Arc::new(FileSecretStore::new(host.paths.secrets_dir()));
    let services = AgentServices::build(
        &host,
        identity(),
        "agent-test".into(),
        Logger::none(),
        secrets,
    )
    .unwrap();
    let hooks = RuntimeHooks {
        capture: Some(fake_capture),
        before_capture: vec![commands],
        after_capture: vec![print_relay],
    };
    let agent = build_host(&services, &hooks, None).unwrap();
    assert_eq!(
        agent.service_names(),
        [
            "Uploader",
            "BlobUploader",
            "Heartbeat",
            "Commands",
            "Capture",
            "PrintRelay"
        ]
    );
    assert!(host.paths.outbox_db().is_file() && host.paths.blob_queue_db().is_file());

    let stop = dl_winsvc::StopSignal::new();
    let s = stop.clone();
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        s.stop();
    });
    let outcome = agent
        .with_shutdown_timeout(Duration::from_secs(20))
        .run(&stop)
        .unwrap();
    stopper.join().unwrap();
    assert!(outcome.abandoned.is_empty(), "{outcome:?}");
    assert_eq!(outcome.stopped.len(), 6);
    // The heartbeat ran: with no agent token stored, the token provider asks for a new
    // enrollment, which the health writer records.
    let st = AgentStatus::try_read(&host.paths).unwrap().unwrap();
    assert_eq!(st.state, "reenroll_required");
    assert_eq!(st.error_code.as_deref(), Some(codes::REENROLL_REQUIRED));
}

#[test]
fn run_on_a_machine_that_is_not_enrolled_writes_the_status_and_exits_1() {
    let d = Dir::new();
    let host = host_context(&d);
    let err = MemoryLines::shared();
    assert_eq!(
        run_service(&host, &*err, &RuntimeHooks::default()),
        EXIT_FAILED
    );
    assert!(err.text().starts_with("[NOT_ENROLLED] This machine is not enrolled. Run: dl-agent pda-enroll --code <code> --url <url>"));
    let st = AgentStatus::try_read(&host.paths).unwrap().unwrap();
    assert_eq!(st.state, "not_enrolled");
    assert_eq!(st.error_code.as_deref(), Some(codes::NOT_ENROLLED));

    // The same through the command line and this build's verb table.
    let data = d.0.join("cli").to_string_lossy().into_owned();
    let (out, err) = (MemoryLines::shared(), MemoryLines::shared());
    let args: Vec<String> = ["run", "--data-dir", &data]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    assert_eq!(run_with(&args, &*out, &*err, &verbs::table()), EXIT_FAILED);
    assert!(err.text().contains("[NOT_ENROLLED]"));
}
