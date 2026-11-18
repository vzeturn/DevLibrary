use super::*;
use crate::report::{ProblemReport, Severity};
use crate::store::testdir::TestDir;

fn beat(at: u64, hub_at: u64) -> Beat {
    Beat {
        at,
        hub_at,
        version: "1.0.0".into(),
        pid: 4242,
    }
}

const T: u64 = 1_000_000;

#[test]
fn beat_and_state_round_trip() {
    let d = TestDir::new("wd-files");
    assert_eq!(read_beat(&d.0), None);
    write_beat(&d.0, &beat(T, T - 5)).unwrap();
    assert_eq!(read_beat(&d.0), Some(beat(T, T - 5)));
    assert_eq!(State::load(&d.0), State::default());
    let s = State {
        pending_since: Some(T),
        backoff: 7200,
        not_before: T + 3600,
    };
    s.save(&d.0).unwrap();
    assert_eq!(State::load(&d.0), s);
}

#[test]
fn stopped_starts_and_missing_or_disabled_are_left_alone() {
    let mut s = State::default();
    let never = || panic!("hub not needed");
    assert_eq!(
        decide(&mut s, T, Service::Stopped, None, false, never),
        Action::Start
    );
    assert!(matches!(
        decide(&mut s, T, Service::Disabled, None, false, never),
        Action::Nothing(_)
    ));
    assert!(matches!(
        decide(&mut s, T, Service::Missing, None, false, never),
        Action::Nothing(_)
    ));
}

#[test]
fn stuck_pending_is_killed_after_ten_minutes() {
    let mut s = State::default();
    let b = beat(T, T);
    let never = || panic!("hub not needed");
    assert!(matches!(
        decide(&mut s, T, Service::Pending, Some(&b), false, never),
        Action::Nothing(_)
    ));
    assert_eq!(s.pending_since, Some(T));
    assert!(matches!(
        decide(
            &mut s,
            T + STUCK_SECONDS - 1,
            Service::Pending,
            Some(&b),
            false,
            never
        ),
        Action::Nothing(_)
    ));
    assert_eq!(
        decide(
            &mut s,
            T + STUCK_SECONDS,
            Service::Pending,
            Some(&b),
            false,
            never
        ),
        Action::Kill(4242)
    );
    assert_eq!(s.pending_since, None);
    // Running again clears the marker.
    decide(&mut s, T, Service::Pending, Some(&b), false, never);
    decide(
        &mut s,
        T + 60,
        Service::Running,
        Some(&beat(T + 60, T + 60)),
        false,
        never,
    );
    assert_eq!(s.pending_since, None);
}

#[test]
fn hung_loop_restarts_with_backoff_from_one_to_twelve_hours() {
    let mut s = State::default();
    let hung = beat(T, T);
    let mut now = T + STALE_SECONDS;
    let never = || panic!("a hung loop restarts without asking the hub");
    assert!(matches!(
        decide(&mut s, now, Service::Running, Some(&hung), false, never),
        Action::Restart(r) if r.contains("loop silent")
    ));
    assert_eq!(s.not_before, now + 3600);
    // Held back inside the backoff, then allowed with a doubled wait.
    assert!(matches!(
        decide(&mut s, now + 600, Service::Running, Some(&hung), false, never),
        Action::Nothing(r) if r.contains("next restart allowed")
    ));
    let mut waits = Vec::new();
    for _ in 0..6 {
        now = s.not_before;
        assert!(matches!(
            decide(&mut s, now, Service::Running, Some(&hung), false, never),
            Action::Restart(_)
        ));
        waits.push((s.not_before - now) / 3600);
    }
    assert_eq!(waits, [2, 4, 8, 12, 12, 12]);
    // A healthy run resets the backoff.
    let fresh = beat(now, now);
    assert_eq!(
        decide(&mut s, now, Service::Running, Some(&fresh), false, never),
        Action::Nothing("healthy".into())
    );
    assert_eq!(s.backoff, MIN_BACKOFF_SECONDS);
}

#[test]
fn stale_hub_link_restarts_only_when_the_hub_answers() {
    let link_down = beat(T + STALE_SECONDS, T);
    let now = T + STALE_SECONDS;
    let mut s = State::default();
    assert!(matches!(
        decide(
            &mut s,
            now,
            Service::Running,
            Some(&link_down),
            false,
            || false
        ),
        Action::Nothing(_)
    ));
    assert!(matches!(
        decide(&mut s, now, Service::Running, Some(&link_down), false, || true),
        Action::Restart(r) if r.contains("hub")
    ));
}

#[test]
fn probation_and_missing_beat_are_left_alone() {
    let mut s = State::default();
    let hung = beat(T, T);
    let never = || panic!("hub not needed");
    assert!(matches!(
        decide(&mut s, T + 2 * STALE_SECONDS, Service::Running, Some(&hung), true, never),
        Action::Nothing(r) if r.contains("probation")
    ));
    assert!(matches!(
        decide(&mut s, T, Service::Running, None, false, never),
        Action::Nothing(_)
    ));
}

#[test]
fn watchdog_reports_are_drained_into_the_agent_spool() {
    let d = TestDir::new("wd-reports");
    let main = Spool::in_memory();
    assert_eq!(drain_reports(&d.0, &main).unwrap(), 0);
    let wd = Spool::open(d.0.join(REPORTS_FILE));
    wd.record(ProblemReport::new(
        Severity::Warning,
        "watchdog",
        "watchdog_restarted",
        "agent loop silent for 25 min",
        T,
    ))
    .unwrap();
    assert_eq!(drain_reports(&d.0, &main).unwrap(), 1);
    assert_eq!(main.len(), 1);
    assert!(!d.0.join(REPORTS_FILE).exists());
}
