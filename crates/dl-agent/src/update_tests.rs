use super::*;
use crate::channel::Channel;
use crate::manifest::Manifest;
use crate::store::testdir::TestDir;

fn manifest(channel: Channel, version: &str, published_at: u64, bytes: &[u8]) -> Manifest {
    let text = format!(
        r#"{{"product":"dl-agent","channel":"{}","version":"{version}","published_at":{published_at},
        "rollout_percent":100,"min_version":null,"allow_downgrade":false,"notes":"",
        "artifacts":[{{"target":"windows-x86_64","url":"https://hub.test/a","sha256":"{}","size":{}}}],
        "key_id":"k","signature":"{}"}}"#,
        channel.as_str(),
        dl_encoding::hex::encode(Sha256::digest(bytes)),
        bytes.len(),
        dl_encoding::base64::STANDARD.encode([0u8; 64]),
    );
    Manifest::parse(&text).unwrap()
}

fn v(s: &str) -> Version {
    s.parse().unwrap()
}

#[test]
fn decide_installs_newer_and_skips_same_or_older() {
    let s = UpdateState::default();
    let m = manifest(Channel::Stable, "1.2.0", 10, b"abc");
    assert!(matches!(
        s.decide(&m, &v("1.1.0"), "a1", "windows-x86_64", 1_000),
        Decision::Install(_, _)
    ));
    assert_eq!(
        s.decide(&m, &v("1.2.0"), "a1", "windows-x86_64", 1_000),
        Decision::UpToDate
    );
    assert_eq!(
        s.decide(&m, &v("1.3.0"), "a1", "windows-x86_64", 1_000),
        Decision::UpToDate
    );
    assert!(matches!(
        s.decide(&m, &v("1.1.0"), "a1", "linux-x86_64", 1_000),
        Decision::Refused(_)
    ));
}

#[test]
fn decide_refuses_replayed_failed_and_below_minimum() {
    let mut s = UpdateState::default();
    let newer = manifest(Channel::Canary, "1.3.0", 20, b"x");
    s.accept(&newer);
    let old = manifest(Channel::Canary, "1.2.0", 10, b"x");
    assert!(matches!(
        s.decide(&old, &v("1.0.0"), "a", "windows-x86_64", 1_000),
        Decision::Refused(_)
    ));
    // Another channel keeps its own freshness mark.
    let stable = manifest(Channel::Stable, "1.2.0", 10, b"x");
    assert!(matches!(
        s.decide(&stable, &v("1.0.0"), "a", "windows-x86_64", 1_000),
        Decision::Install(..)
    ));

    let mut min = manifest(Channel::Stable, "2.0.0", 30, b"x");
    min.min_version = Some(v("1.5.0"));
    assert!(matches!(
        s.decide(&min, &v("1.0.0"), "a", "windows-x86_64", 1_000),
        Decision::Refused(_)
    ));
}

#[test]
fn failure_cap_waits_a_day_after_three_and_stops_after_nine() {
    let mut s = UpdateState::default();
    let m = manifest(Channel::Stable, "1.3.0", 10, b"x");
    let install = |s: &UpdateState, now| s.decide(&m, &v("1.0.0"), "a", "windows-x86_64", now);
    let day = FAILURE_WINDOW_SECONDS;
    for i in 0..3 {
        assert!(matches!(install(&s, 1_000 + i), Decision::Install(..)));
        s.record_failure(&v("1.3.0"), 1_000 + i);
    }
    assert!(matches!(install(&s, 1_010), Decision::Deferred(_)));
    // Another version is not held back by these failures.
    let other = manifest(Channel::Stable, "1.3.1", 11, b"x");
    assert!(matches!(
        s.decide(&other, &v("1.0.0"), "a", "windows-x86_64", 1_010),
        Decision::Install(..)
    ));
    // A day later it is tried again; nine failures stop it until retry_update clears them.
    assert!(matches!(install(&s, 1_002 + day), Decision::Install(..)));
    for i in 0..6 {
        s.record_failure(&v("1.3.0"), 2 * day + i);
    }
    assert_eq!(s.failures_of(&v("1.3.0"), 10 * day), (0, 9));
    match install(&s, 10 * day) {
        Decision::Refused(r) => assert!(r.contains("retry_update"), "{r}"),
        d => panic!("{d:?}"),
    }
    s.record_failure(&v("1.3.0"), 10 * day);
    assert_eq!(
        s.failures_of(&v("1.3.0"), 10 * day).1,
        MAX_FAILURES_LIFETIME
    );
    assert_eq!(s.failed_versions(), [v("1.3.0")]);
    s.failures.clear();
    assert!(matches!(install(&s, 10 * day), Decision::Install(..)));
}

#[test]
fn old_failed_list_loads_as_one_old_failure() {
    let d = TestDir::new("update-legacy");
    std::fs::write(
        d.0.join("update.json"),
        r#"{"last_published":{},"failed":["1.0.5"],"pending":null}"#,
    )
    .unwrap();
    let s = UpdateState::load(&d.0).unwrap();
    assert_eq!(s.failures, [(v("1.0.5"), 0)]);
    assert_eq!(s.failures_of(&v("1.0.5"), 1_000_000), (0, 1));
}

#[test]
fn window_contains_local_time_and_crosses_midnight() {
    let w =
        |from: &str, to: &str| Window::parse(&dl_json::json!({"from": from, "to": to})).unwrap();
    // 2026-11-19 00:00 UTC = 07:00 in Vietnam.
    let midnight_utc = 1_795_046_400;
    let at = |h: u64, m: u64| midnight_utc + (h * 60 + m) * 60 - 7 * 3600; // local h:m
    let night = w("22:00", "05:00");
    assert!(night.contains(at(23, 30)) && night.contains(at(4, 59)) && night.contains(at(22, 0)));
    assert!(!night.contains(at(5, 0)) && !night.contains(at(12, 0)));
    let day = w("09:00", "17:00");
    assert!(day.contains(at(9, 0)) && !day.contains(at(17, 0)) && !day.contains(at(8, 59)));
    assert!(
        w("03:00", "03:00").contains(at(15, 0)),
        "from == to is all day"
    );
    let utc =
        Window::parse(&dl_json::json!({"from": "00:00", "to": "01:00", "utc_offset_minutes": 0}))
            .unwrap();
    assert!(utc.contains(midnight_utc + 59 * 60) && !utc.contains(midnight_utc + 3600));
    for bad in [
        dl_json::json!({"from": "24:00", "to": "01:00"}),
        dl_json::json!({"from": "1:00"}),
        dl_json::json!({"from": "01:00", "to": "02:00", "utc_offset_minutes": 2000}),
    ] {
        assert!(Window::parse(&bad).is_err(), "{bad:?}");
    }
}

#[test]
fn policy_holds_unless_urgent_required_or_forced() {
    let midnight_utc = 1_795_046_400; // 07:00 local
    let noon = midnight_utc + 5 * 3600;
    let body = dl_json::json!({"update": {"window": {"from": "22:00", "to": "05:00"}}});
    let (p, err) = Policy::from_config(&body);
    assert!(err.is_none());
    let mut m = manifest(Channel::Stable, "1.3.0", 10, b"x");
    assert!(p.hold(&m, noon, false).unwrap().contains("22:00–05:00"));
    assert_eq!(p.hold(&m, noon, true), None, "update_now skips the window");
    m.urgent = true;
    assert_eq!(p.hold(&m, noon, false), None);

    let (off, _) = Policy::from_config(&dl_json::json!({"update": {"auto": false}}));
    let mut plain = manifest(Channel::Stable, "1.3.0", 10, b"x");
    assert!(off.hold(&plain, noon, false).is_some());
    plain.urgent = true;
    assert!(
        off.hold(&plain, noon, false).is_some(),
        "urgent alone does not override auto off"
    );
    plain.required = true;
    assert_eq!(off.hold(&plain, noon, false), None);

    assert_eq!(
        Policy::from_config(&dl_json::json!({})).0,
        Policy::default()
    );
    let (fallback, err) =
        Policy::from_config(&dl_json::json!({"update": {"auto": false, "window": {"from": "x"}}}));
    assert!(err.is_some() && !fallback.auto && fallback.window.is_none());
}

#[test]
fn decide_respects_rollout_and_downgrade_flag() {
    let s = UpdateState::default();
    let mut m = manifest(Channel::Stable, "1.2.0", 10, b"x");
    m.rollout_percent = 0;
    assert_eq!(
        s.decide(&m, &v("1.0.0"), "a", "windows-x86_64", 1_000),
        Decision::NotInRollout
    );
    let mut down = manifest(Channel::Stable, "1.0.0", 10, b"x");
    down.allow_downgrade = true;
    assert!(matches!(
        s.decide(&down, &v("1.1.0"), "a", "windows-x86_64", 1_000),
        Decision::Install(..)
    ));
}

#[test]
fn state_round_trips_through_disk() {
    let d = TestDir::new("update-state");
    let mut s = UpdateState::default();
    s.accept(&manifest(Channel::Canary, "1.1.0", 42, b"x"));
    s.record_failure(&v("1.0.5"), 77);
    s.pending = Some(Pending {
        version: v("1.1.0"),
        previous_version: v("1.0.0"),
        probation_until: 99,
        boots: 2,
    });
    s.save(&d.0).unwrap();
    assert_eq!(UpdateState::load(&d.0).unwrap(), s);
    assert_eq!(
        UpdateState::load(&d.0.join("none")).unwrap(),
        UpdateState::default()
    );
}

#[test]
fn install_verifies_then_swaps_and_rollback_restores() {
    let d = TestDir::new("installer");
    let exe = d.0.join("dl-agent.exe");
    std::fs::write(&exe, b"old").unwrap();
    let inst = Installer { exe: exe.clone() };
    let m = manifest(Channel::Stable, "1.2.0", 1, b"new!");
    let art = &m.artifacts[0];

    assert!(matches!(
        inst.install(b"bad!", art),
        Err(Error::Integrity(_))
    ));
    assert_eq!(std::fs::read(&exe).unwrap(), b"old");

    inst.install(b"new!", art).unwrap();
    assert_eq!(std::fs::read(&exe).unwrap(), b"new!");
    inst.rollback().unwrap();
    assert_eq!(std::fs::read(&exe).unwrap(), b"old");
    assert!(inst.rollback().is_err(), "nothing left to roll back to");
}

#[test]
fn probation_counts_boots_then_rolls_back() {
    let mut s = UpdateState {
        pending: Some(Pending {
            version: v("1.2.0"),
            previous_version: v("1.1.0"),
            probation_until: 100,
            boots: 0,
        }),
        ..UpdateState::default()
    };
    for _ in 0..MAX_PROBATION_BOOTS {
        assert_eq!(on_boot(&mut s, &v("1.2.0")), BootAction::Probation);
    }
    assert_eq!(on_boot(&mut s, &v("1.2.0")), BootAction::RollBack);
}

#[test]
fn commit_needs_time_health_and_hub() {
    let p = Pending {
        version: v("1.2.0"),
        previous_version: v("1.1.0"),
        probation_until: 100,
        boots: 1,
    };
    let mut s = UpdateState {
        pending: Some(p),
        ..UpdateState::default()
    };
    assert!(!try_commit(&mut s, 99, true, true));
    assert!(!try_commit(&mut s, 100, false, true));
    assert!(!try_commit(&mut s, 100, true, false));
    assert!(try_commit(&mut s, 100, true, true));
    assert!(s.pending.is_none());
}

#[test]
fn boot_on_unexpected_version_forgets_pending() {
    let p = Pending {
        version: v("1.2.0"),
        previous_version: v("1.1.0"),
        probation_until: 100,
        boots: 0,
    };
    let mut s = UpdateState {
        pending: Some(p),
        ..UpdateState::default()
    };
    assert_eq!(on_boot(&mut s, &v("1.1.0")), BootAction::Normal);
    assert!(s.pending.is_none());
}
