use std::sync::Arc;

use dl_ed25519::SigningKey;
use dl_json::json;
use dl_winsvc::control::{Call, FakeControl};
use dl_winsvc::scm::{StartType, State};

use super::*;
use crate::store::testdir::TestDir;

const NOW: u64 = 1_800_000_000;
const AGENT: &str = "agt_1";
const OWN: &str = "DevLibraryAgent";
const PMS: &str = "SmilePmsSync";

fn key() -> SigningKey {
    SigningKey::from_seed(&[7; 32])
}

fn trusted() -> TrustedKeys {
    let pk = B64.encode(key().verifying_key().as_bytes());
    TrustedKeys::new().with_key("ops-1", &pk).unwrap()
}

fn fake() -> Arc<FakeControl> {
    Arc::new(
        FakeControl::new()
            .with_service(OWN, State::Running, StartType::Automatic)
            .with_service(PMS, State::Running, StartType::Automatic),
    )
}

fn ops(ctl: &Arc<FakeControl>) -> ServiceOps {
    ServiceOps::new(
        AGENT,
        trusted(),
        ServiceAllowlist::new(OWN, &[PMS]),
        NonceStore::in_memory(),
        Box::new(Arc::clone(ctl)),
    )
}

fn request(service: &str, action: ServiceAction, nonce: &str) -> ServiceRequest {
    let mut r = ServiceRequest {
        command_id: format!("cmd-{nonce}"),
        agent_id: AGENT.into(),
        service: service.into(),
        action,
        nonce: nonce.into(),
        issued_at: NOW - 10,
        expires_at: NOW + 300,
        key_id: String::new(),
        signature: [0; SIGNATURE_LEN],
    };
    r.sign("ops-1", &key());
    r
}

fn err(ops: &mut ServiceOps, cmd: &Command) -> &'static str {
    ops.handle(cmd, NOW).unwrap_err().code()
}

#[test]
fn stop_start_and_start_type_through_the_fake() {
    let ctl = fake();
    let mut o = ops(&ctl);
    let stop = request(PMS, ServiceAction::Stop, "nonce-0000000001");
    assert_eq!(
        o.handle(&stop.to_command(), NOW).unwrap(),
        "SmilePmsSync: stopped"
    );
    let manual = request(
        "smilepmssync",
        ServiceAction::SetStartType(StartType::Manual),
        "nonce-0000000002",
    );
    let r = o.answer(&manual.to_command(), NOW);
    assert!(r.ok, "{}", r.output);
    assert_eq!(r.output, "SmilePmsSync: start type manual");
    let start = request(PMS, ServiceAction::Start, "nonce-0000000003");
    assert_eq!(
        o.handle(&start.to_command(), NOW).unwrap(),
        "SmilePmsSync: running"
    );
    assert_eq!(
        ctl.calls(),
        [
            Call::Stop(PMS.into()),
            Call::SetStartType(PMS.into(), StartType::Manual),
            Call::Start(PMS.into()),
        ]
    );
}

#[test]
fn wire_round_trip_keeps_the_signature_valid() {
    let r = request(
        PMS,
        ServiceAction::SetStartType(StartType::Disabled),
        "nonce-0000000004",
    );
    let cmd = Command::from_json(&json!({
        "id": r.command_id, "kind": KIND, "args": r.to_command().args, "expires_at": r.expires_at,
    }))
    .unwrap();
    let back = ServiceRequest::from_command(&cmd).unwrap();
    assert_eq!(back, r);
    back.verify(&trusted()).unwrap();
}

#[test]
fn unsigned_and_forged_commands_are_refused() {
    let ctl = fake();
    let mut o = ops(&ctl);
    let mut cmd = request(PMS, ServiceAction::Stop, "nonce-0000000005").to_command();
    if let Some(a) = cmd.args.as_object_mut() {
        a.remove("signature");
    }
    assert_eq!(err(&mut o, &cmd), "unsigned");

    // Tampered: signed for stop, sent as start.
    let mut cmd = request(PMS, ServiceAction::Stop, "nonce-0000000006").to_command();
    if let Some(a) = cmd.args.as_object_mut() {
        a.insert("action", "start");
    }
    assert_eq!(err(&mut o, &cmd), "bad_signature");

    // Signature moved to another command id.
    let mut cmd = request(PMS, ServiceAction::Stop, "nonce-0000000007").to_command();
    cmd.id = "other".into();
    assert_eq!(err(&mut o, &cmd), "bad_signature");

    // Signed by a key the agent does not trust.
    let mut r = request(PMS, ServiceAction::Stop, "nonce-0000000008");
    r.sign("ops-1", &SigningKey::from_seed(&[9; 32]));
    assert_eq!(err(&mut o, &r.to_command()), "bad_signature");
    r.sign("ops-2", &key());
    assert_eq!(err(&mut o, &r.to_command()), "unknown_key");

    let mut none = ServiceOps::new(
        AGENT,
        TrustedKeys::new(),
        ServiceAllowlist::new(OWN, &[PMS]),
        NonceStore::in_memory(),
        Box::new(Arc::clone(&ctl)),
    );
    let ok = request(PMS, ServiceAction::Stop, "nonce-0000000009").to_command();
    assert_eq!(err(&mut none, &ok), "no_trusted_keys");
    assert!(ctl.calls().is_empty(), "nothing may run: {:?}", ctl.calls());
}

#[test]
fn expiry_clock_and_lifetime_are_enforced() {
    let ctl = fake();
    let mut o = ops(&ctl);
    let r = request(PMS, ServiceAction::Stop, "nonce-0000000010");
    let e = o.handle(&r.to_command(), r.expires_at + 1).unwrap_err();
    assert_eq!(e.code(), "expired");
    assert!(e.to_string().starts_with("expired: expired at"), "{e}");

    let mut early = request(PMS, ServiceAction::Stop, "nonce-0000000011");
    early.issued_at = NOW + MAX_CLOCK_SKEW + 1;
    early.expires_at = early.issued_at + 60;
    early.sign("ops-1", &key());
    assert_eq!(err(&mut o, &early.to_command()), "not_yet_valid");

    let mut long = request(PMS, ServiceAction::Stop, "nonce-0000000012");
    long.expires_at = long.issued_at + MAX_LIFETIME + 1;
    long.sign("ops-1", &key());
    assert_eq!(err(&mut o, &long.to_command()), "bad_lifetime");
    assert!(ctl.calls().is_empty());
}

#[test]
fn a_nonce_runs_once() {
    let ctl = fake();
    let mut o = ops(&ctl);
    let r = request(PMS, ServiceAction::Stop, "nonce-0000000013");
    o.handle(&r.to_command(), NOW).unwrap();
    let e = o.handle(&r.to_command(), NOW + 1).unwrap_err();
    assert_eq!(e, ServiceOpError::Replayed("nonce-0000000013".into()));
    assert_eq!(ctl.calls().len(), 1);
}

#[test]
fn a_failed_call_still_burns_the_nonce_and_reports_the_cause() {
    let ctl = fake();
    let mut o = ops(&ctl);
    ctl.fail_next(dl_winsvc::Error::Win32 {
        op: "ControlService",
        code: dl_winsvc::codes::ACCESS_DENIED,
    });
    let r = request(PMS, ServiceAction::Stop, "nonce-0000000014");
    let res = o.answer(&r.to_command(), NOW);
    assert!(!res.ok);
    assert!(
        res.output
            .starts_with("service_failed: ControlService failed"),
        "{}",
        res.output
    );
    assert_eq!(err(&mut o, &r.to_command()), "replayed");
}

#[test]
fn only_managed_services_and_no_self_lockout() {
    let ctl = fake();
    let mut o = ops(&ctl);
    let r = request("MSSQLSERVER", ServiceAction::Stop, "nonce-0000000015");
    let e = o.handle(&r.to_command(), NOW).unwrap_err();
    assert_eq!(e, ServiceOpError::UnknownService("MSSQLSERVER".into()));
    let own = request(OWN, ServiceAction::Stop, "nonce-0000000016");
    assert_eq!(err(&mut o, &own.to_command()), "self_lockout");
    let off = request(
        OWN,
        ServiceAction::SetStartType(StartType::Disabled),
        "nonce-0000000017",
    );
    assert_eq!(err(&mut o, &off.to_command()), "self_lockout");
    let auto = request(
        OWN,
        ServiceAction::SetStartType(StartType::Automatic),
        "nonce-0000000018",
    );
    o.handle(&auto.to_command(), NOW).unwrap();
    assert_eq!(
        ctl.calls(),
        [Call::SetStartType(OWN.into(), StartType::Automatic)]
    );
}

#[test]
fn wrong_agent_and_bad_shapes() {
    let ctl = fake();
    let mut o = ops(&ctl);
    let mut r = request(PMS, ServiceAction::Stop, "nonce-0000000019");
    r.agent_id = "agt_2".into();
    r.sign("ops-1", &key());
    assert_eq!(err(&mut o, &r.to_command()), "wrong_agent");

    let base = request(PMS, ServiceAction::Stop, "nonce-0000000020").to_command();
    let with = |k: &str, v: Value| {
        let mut c = base.clone();
        if let Some(a) = c.args.as_object_mut() {
            a.insert(k, v);
        }
        c
    };
    assert_eq!(err(&mut o, &with("action", "pause".into())), "bad_request");
    assert_eq!(
        err(&mut o, &with("start_type", "manual".into())),
        "bad_request"
    );
    assert_eq!(err(&mut o, &with("nonce", "short".into())), "bad_request");
    assert_eq!(err(&mut o, &with("service", "a\nb".into())), "bad_request");
    assert_eq!(
        err(&mut o, &with("signature", "!!".into())),
        "bad_signature"
    );
    let mut other = base.clone();
    other.kind = "restart".into();
    assert_eq!(err(&mut o, &other), "bad_request");
    assert!(ctl.calls().is_empty());
}

#[test]
fn nonce_store_survives_a_restart_and_forgets_expired() {
    let d = TestDir::new("svc-nonce");
    let p = d.0.join("service-nonces.json");
    let mut s = NonceStore::open(&p).unwrap();
    s.record("n-live", NOW + 100, NOW).unwrap();
    s.record("n-old", NOW + 1, NOW).unwrap();
    let mut again = NonceStore::open(&p).unwrap();
    assert_eq!(again.len(), 2);
    assert_eq!(
        again.record("n-live", NOW + 100, NOW + 5),
        Err(ServiceOpError::Replayed("n-live".into()))
    );
    again.record("n-new", NOW + 100, NOW + 5).unwrap();
    assert_eq!(again.len(), 2, "n-old expired and was dropped");

    std::fs::write(&p, "[1]").unwrap();
    assert_eq!(NonceStore::open(&p).unwrap_err().code(), "nonce_store");
}
