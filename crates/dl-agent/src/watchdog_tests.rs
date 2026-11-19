use super::*;
use crate::report::{ProblemReport, Severity};
use crate::store::testdir::TestDir;

fn beat(at: u64, hub_at: u64) -> Beat {
    Beat {
        at,
        hub_at,
        version: "1.0.0".into(),
        pid: 4242,
        channel: "canary".into(),
        skew: -3,
        links: Vec::new(),
    }
}

/// [`super::decide`] for beats whose modules have no cloud link of their own.
fn decide(
    s: &mut State,
    now: u64,
    service: Service,
    beat: Option<&Beat>,
    pending: bool,
    hub: impl FnOnce() -> bool,
) -> Action {
    super::decide(s, now, service, beat, pending, hub, |_: &Link| {
        panic!("no module link to ask")
    })
}

fn link(ok_at: u64, since: u64) -> Link {
    Link {
        module: "arrival".into(),
        url: "https://checkin.test".into(),
        ok_at,
        since,
    }
}

const T: u64 = 1_000_000;

#[test]
fn beat_written_by_an_older_agent_still_reads() {
    let d = TestDir::new("wd-old-beat");
    let old = dl_json::json!({"at": T, "hub_at": T - 5, "version": "1.0.0", "pid": 4242});
    store::write_json(&d.0.join(BEAT_FILE), &old).unwrap();
    let b = read_beat(&d.0).unwrap();
    assert_eq!((b.channel.as_str(), b.skew, b.hub_at), ("", 0, T - 5));
}

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
fn stale_module_link_restarts_when_its_cloud_answers_even_with_the_hub_up() {
    let hub = || panic!("the hub link is fresh: not asked");
    let with = |l: Link| Beat {
        links: vec![l],
        ..beat(T, T - 30)
    };
    let silent = with(link(T - STALE_SECONDS - 60, T - 3 * STALE_SECONDS));
    let mut s = State::default();
    assert_eq!(
        super::decide(
            &mut s,
            T,
            Service::Running,
            Some(&silent),
            false,
            hub,
            |_| false
        ),
        Action::Nothing("healthy".into()),
        "its cloud does not answer: a restart would not help"
    );
    let mut asked = Vec::new();
    let action = super::decide(
        &mut s,
        T,
        Service::Running,
        Some(&silent),
        false,
        hub,
        |l| {
            asked.push(l.url.clone());
            true
        },
    );
    assert_eq!(
        action,
        Action::Restart(
            "module arrival cloud link silent for 21 min while https://checkin.test answers".into()
        )
    );
    assert_eq!(asked, ["https://checkin.test"]);
    assert_eq!(s.not_before, T + MIN_BACKOFF_SECONDS);

    // Fresh, or never worked but declared only 5 min ago: healthy, the cloud is not asked.
    let mut s = State::default();
    for l in [link(T - 60, T - 3600), link(0, T - 300)] {
        assert_eq!(
            super::decide(
                &mut s,
                T,
                Service::Running,
                Some(&with(l)),
                false,
                hub,
                |_| { panic!("fresh link: not asked") }
            ),
            Action::Nothing("healthy".into())
        );
    }
    // Declared 25 min ago and never worked: stale.
    assert!(matches!(
        super::decide(&mut s, T, Service::Running, Some(&with(link(0, T - 1500))), false, hub, |_| true),
        Action::Restart(r) if r.contains("silent for 25 min")
    ));
}

#[test]
fn beat_carries_module_links_and_only_their_origin() {
    let d = TestDir::new("wd-links");
    let b = Beat {
        links: vec![link(T - 5, T - 900)],
        ..beat(T, T - 5)
    };
    write_beat(&d.0, &b).unwrap();
    assert_eq!(read_beat(&d.0), Some(b));
    assert_eq!(
        origin("https://user:pw@CheckIn.test:8443/api/poll?x=1"),
        "https://CheckIn.test:8443"
    );
    assert_eq!(origin("HTTP://a.test"), "http://a.test");
    assert_eq!(origin("a.test/x"), "https://a.test");
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
