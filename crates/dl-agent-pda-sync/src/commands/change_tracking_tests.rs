//! Tests of the service's Change Tracking check over fakes. The flag rule (off: nothing is
//! probed or run), the event codes and the 15-minute retry while work is left are the
//! RT-1318 brief; SQL Server error 1222 is its lock timeout and 4060 a database the login
//! cannot open.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::*;
use crate::bootstrap::change_tracking::{CtStep, StepState};
use crate::commands::change_tracking_fakes::*;
use crate::commands::grants::shorter_wait;
use crate::config::codes;
use crate::contracts::wire::site_event_levels::WARN;

const FIFTEEN_MINUTES: Duration = Duration::from_secs(15 * 60);

fn identity() -> AgentIdentity {
    AgentIdentity {
        agent_id: dl_uuid::Uuid::new_v4(),
        tenant_id: "tenant".into(),
        cloud_url: "https://pda.example".into(),
        machine_id: "POS-1".into(),
        sql_instance: Some("POS\\SQLEXPRESS".into()),
        sql_database: Some("SMILE_POS".into()),
    }
}

struct Rig {
    monitor: ChangeTrackingMonitor,
    probe: Arc<FakeProbe>,
    exec: Arc<FakeExec>,
    events: Arc<FakeEvents>,
}

fn rig(enabled: bool, password: Option<&'static str>, probe: FakeProbe, exec: FakeExec) -> Rig {
    let probe = Arc::new(probe);
    let exec = Arc::new(exec);
    let events = Arc::new(FakeEvents::default());
    let monitor = ChangeTrackingMonitor::new(
        CtParts {
            enabled,
            tables: vec!["TmpInvoice".into(), "Trn".into(), "CashierRVC".into()],
            probe: probe.clone(),
            exec: exec.clone(),
            attempts: attempts(),
            events: events.clone(),
            identity: identity(),
            secrets: Arc::new(FakeSecrets(password)),
        },
        &Logger::none(),
    );
    Rig {
        monitor,
        probe,
        exec,
        events,
    }
}

fn check(r: &Rig) -> CtState {
    r.monitor.check(&Ctx::background())
}

#[test]
fn with_the_flag_off_nothing_is_probed_or_run() {
    let r = rig(false, Some("pw"), FakeProbe::new(|| Ok(fresh())), done());

    let state = check(&r);

    assert_eq!(state, CtState::Off);
    assert_eq!(state.recheck(), None);
    assert_eq!(r.probe.calls(), 0);
    assert!(ran(&r.exec).is_empty(), "flag off: no statement");
    assert!(r.events.lines().is_empty());
}

#[test]
fn the_real_check_loads_no_identity_with_the_flag_off() {
    let m = ChangeTrackingMonitor::real(
        false,
        vec!["Trn".into()],
        Arc::new(FakeEvents::default()),
        identity(),
        Arc::new(FakeSecrets(Some("pw"))),
        &Logger::none(),
    );

    assert!(m.parts.attempts.is_empty());
    assert_eq!(m.check(&Ctx::background()), CtState::Off);
}

#[test]
fn without_the_pda_sync_password_nothing_is_probed_and_it_is_checked_again() {
    let r = rig(true, None, FakeProbe::new(|| Ok(fresh())), done());

    let state = check(&r);

    assert_eq!(state, CtState::Unchecked);
    assert_eq!(state.recheck(), Some(FIFTEEN_MINUTES));
    assert_eq!(r.probe.calls(), 0);
    assert!(ran(&r.exec).is_empty());
}

#[test]
fn a_probe_failure_runs_nothing() {
    let r = rig(true, Some("pw"), FakeProbe::new(|| Err(sql(4060))), done());

    assert_eq!(check(&r), CtState::Unchecked);
    assert!(ran(&r.exec).is_empty());
    assert!(r.events.lines().is_empty());
}

#[test]
fn tracked_tables_need_no_round_and_no_event() {
    let r = rig(true, Some("pw"), FakeProbe::new(|| Ok(tracked())), done());

    let state = check(&r);

    assert_eq!(state, CtState::Ready);
    assert_eq!(state.recheck(), None);
    assert_eq!(r.probe.calls(), 1);
    assert!(ran(&r.exec).is_empty(), "nothing to enable: no connection");
    assert!(r.events.lines().is_empty());
}

#[test]
fn a_round_is_reported_once_while_its_outcome_stays_the_same() {
    let r = rig(true, Some("pw"), FakeProbe::new(|| Ok(fresh())), done());

    assert_eq!(
        check(&r),
        CtState::Ready,
        "a table without a key is not retried"
    );
    assert_eq!(check(&r), CtState::Ready);

    let events = r.events.lines();
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(
        events[0].starts_with(&format!("{WARN} {}: ", codes::CAPTURE_CT_PARTIAL)),
        "{events:?}"
    );
    assert!(events[0].contains("CashierRVC (no_pk)"), "{events:?}");
    assert!(!events[0].contains(PASSWORD));
}

#[test]
fn a_lock_timeout_is_pending_and_checked_again_in_fifteen_minutes() {
    let e = exec(
        |_| Ok(()),
        |_, step| match step {
            CtStep::EnableTable(t) if t == "TmpInvoice" => Err(sql(1222)),
            _ => Ok(StepState::Done),
        },
    );
    let r = rig(true, Some("pw"), FakeProbe::new(|| Ok(fresh())), e);

    let state = check(&r);

    assert_eq!(state, CtState::Pending);
    assert_eq!(state.recheck(), Some(FIFTEEN_MINUTES));
    let events = r.events.lines();
    assert_eq!(events.len(), 1);
    assert!(
        events[0].contains("lock_timeout (SQL error 1222)"),
        "{events:?}"
    );
}

#[test]
fn a_new_outcome_is_reported_again() {
    let locked = Arc::new(AtomicBool::new(true));
    let held = locked.clone();
    let e = exec(
        |_| Ok(()),
        move |_, step| match step {
            CtStep::EnableTable(t) if t == "TmpInvoice" && held.load(Ordering::SeqCst) => {
                Err(sql(1222))
            }
            _ => Ok(StepState::Done),
        },
    );
    let r = rig(true, Some("pw"), FakeProbe::new(|| Ok(fresh())), e);

    assert_eq!(check(&r), CtState::Pending);
    locked.store(false, Ordering::SeqCst);
    assert_eq!(check(&r), CtState::Ready);

    let events = r.events.lines();
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(!events[1].contains("lock_timeout"), "{events:?}");
}

#[test]
fn the_service_waits_for_the_shorter_of_the_two_checks() {
    let gap = Duration::from_secs(30);
    assert_eq!(shorter_wait(None, None), None, "both wait until woken");
    assert_eq!(
        shorter_wait(None, Some(FIFTEEN_MINUTES)),
        Some(FIFTEEN_MINUTES)
    );
    assert_eq!(
        shorter_wait(Some(FIFTEEN_MINUTES), None),
        Some(FIFTEEN_MINUTES)
    );
    assert_eq!(shorter_wait(Some(gap), Some(FIFTEEN_MINUTES)), Some(gap));
}
