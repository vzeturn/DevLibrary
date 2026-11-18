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
        s.decide(&m, &v("1.1.0"), "a1", "windows-x86_64"),
        Decision::Install(_, _)
    ));
    assert_eq!(
        s.decide(&m, &v("1.2.0"), "a1", "windows-x86_64"),
        Decision::UpToDate
    );
    assert_eq!(
        s.decide(&m, &v("1.3.0"), "a1", "windows-x86_64"),
        Decision::UpToDate
    );
    assert!(matches!(
        s.decide(&m, &v("1.1.0"), "a1", "linux-x86_64"),
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
        s.decide(&old, &v("1.0.0"), "a", "windows-x86_64"),
        Decision::Refused(_)
    ));
    // Another channel keeps its own freshness mark.
    let stable = manifest(Channel::Stable, "1.2.0", 10, b"x");
    assert!(matches!(
        s.decide(&stable, &v("1.0.0"), "a", "windows-x86_64"),
        Decision::Install(..)
    ));

    s.failed.push(v("1.3.0"));
    assert!(matches!(
        s.decide(&newer, &v("1.0.0"), "a", "windows-x86_64"),
        Decision::Refused(_)
    ));

    let mut min = manifest(Channel::Stable, "2.0.0", 30, b"x");
    min.min_version = Some(v("1.5.0"));
    assert!(matches!(
        s.decide(&min, &v("1.0.0"), "a", "windows-x86_64"),
        Decision::Refused(_)
    ));
}

#[test]
fn decide_respects_rollout_and_downgrade_flag() {
    let s = UpdateState::default();
    let mut m = manifest(Channel::Stable, "1.2.0", 10, b"x");
    m.rollout_percent = 0;
    assert_eq!(
        s.decide(&m, &v("1.0.0"), "a", "windows-x86_64"),
        Decision::NotInRollout
    );
    let mut down = manifest(Channel::Stable, "1.0.0", 10, b"x");
    down.allow_downgrade = true;
    assert!(matches!(
        s.decide(&down, &v("1.1.0"), "a", "windows-x86_64"),
        Decision::Install(..)
    ));
}

#[test]
fn state_round_trips_through_disk() {
    let d = TestDir::new("update-state");
    let mut s = UpdateState::default();
    s.accept(&manifest(Channel::Canary, "1.1.0", 42, b"x"));
    s.failed.push(v("1.0.5"));
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
