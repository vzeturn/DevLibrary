use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dl_ed25519::SigningKey;
use dl_encoding::base64::STANDARD as B64;
use dl_json::{Value, json};

use super::*;

const NOW: u64 = 1_763_500_000;

fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(NOW)
}

fn key() -> SigningKey {
    SigningKey::from_seed(&[7u8; 32])
}

fn trusted() -> TrustedKeys {
    TrustedKeys::new()
        .with_key("rel-test", &B64.encode(key().verifying_key().as_bytes()))
        .unwrap()
}

/// A command with `args` as given and, when `sign`, the signature of its signing bytes.
fn command(args: Value, expires_at: u64, sign: bool) -> Command {
    let mut cmd = Command {
        id: "c-1".into(),
        kind: KIND.into(),
        module: None,
        args,
        expires_at,
    };
    if sign {
        let sig = key().sign(&signing_bytes(&cmd).unwrap());
        let o = cmd.args.as_object_mut().unwrap();
        o.insert("key_id", "rel-test");
        o.insert("signature", B64.encode(sig));
    }
    cmd
}

#[derive(Default)]
struct Fake(Mutex<Vec<String>>);

impl ServiceControl for Fake {
    fn stop(&self, n: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(format!("stop {n}"));
        Ok(())
    }
    fn start(&self, n: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(format!("start {n}"));
        Ok(())
    }
    fn set_startup(&self, n: &str, s: Startup) -> Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .push(format!("startup {n} {}", startup_word(s)));
        Ok(())
    }
}

fn run(cmd: &Command) -> (bool, String, Vec<String>) {
    let fake = Fake::default();
    let (ok, out) = execute(cmd, &trusted(), &fake, Some("MM.Arrival.Agent"), now());
    let calls = fake.0.lock().unwrap().clone();
    (ok, out, calls)
}

#[test]
fn signed_stop_start_and_startup_type_do_exactly_what_was_asked() {
    let exp = NOW + 600;
    let cases = [
        (
            json!({"action": "stop", "service": "Spooler"}),
            "stop Spooler",
            "Spooler: stopped",
        ),
        (
            json!({"action": "start", "service": "Spooler"}),
            "start Spooler",
            "Spooler: running",
        ),
        (
            json!({"action": "set_startup", "service": "Spooler", "startup": "disabled"}),
            "startup Spooler disabled",
            "Spooler: startup disabled",
        ),
        (
            json!({"action": "set_startup", "service": "W32Time", "startup": "automatic"}),
            "startup W32Time automatic",
            "W32Time: startup automatic",
        ),
    ];
    for (args, call, out) in cases {
        let (ok, output, calls) = run(&command(args, exp, true));
        assert!(ok, "{output}");
        assert_eq!(output, out);
        assert_eq!(calls, vec![call.to_string()]);
    }
}

#[test]
fn unsigned_command_is_refused_and_touches_nothing() {
    let (ok, out, calls) = run(&command(
        json!({"action": "stop", "service": "Spooler"}),
        NOW + 60,
        false,
    ));
    assert!(!ok);
    assert!(out.contains("unsigned"), "{out}");
    assert!(calls.is_empty());
}

#[test]
fn tampered_unknown_key_and_malformed_signatures_are_refused() {
    let signed = command(
        json!({"action": "stop", "service": "Spooler"}),
        NOW + 60,
        true,
    );
    // Another service under the same signature.
    let mut other_service = signed.clone();
    other_service
        .args
        .as_object_mut()
        .unwrap()
        .insert("service", "Dnscache");
    // Another action.
    let mut other_action = signed.clone();
    other_action
        .args
        .as_object_mut()
        .unwrap()
        .insert("action", "start");
    // Another expiry (a replay with a longer life).
    let mut longer = signed.clone();
    longer.expires_at += 1;
    // Another command id.
    let mut other_id = signed.clone();
    other_id.id = "c-2".into();
    // Unknown key id, garbage and short signatures.
    let mut unknown_key = signed.clone();
    unknown_key
        .args
        .as_object_mut()
        .unwrap()
        .insert("key_id", "rel-other");
    let mut garbage = signed.clone();
    garbage
        .args
        .as_object_mut()
        .unwrap()
        .insert("signature", "!!!");
    let mut short = signed.clone();
    short
        .args
        .as_object_mut()
        .unwrap()
        .insert("signature", B64.encode([1u8; 10]));
    for (name, cmd) in [
        ("service", other_service),
        ("action", other_action),
        ("expiry", longer),
        ("id", other_id),
        ("key", unknown_key),
        ("garbage", garbage),
        ("short", short),
    ] {
        let (ok, out, calls) = run(&cmd);
        assert!(!ok, "{name}: {out}");
        assert!(out.starts_with("service: refused"), "{name}: {out}");
        assert!(calls.is_empty(), "{name}");
    }
}

#[test]
fn signed_by_a_key_the_agent_does_not_trust_is_refused() {
    let mut cmd = command(
        json!({"action": "stop", "service": "Spooler"}),
        NOW + 60,
        false,
    );
    let sig = SigningKey::from_seed(&[9u8; 32]).sign(&signing_bytes(&cmd).unwrap());
    let o = cmd.args.as_object_mut().unwrap();
    o.insert("key_id", "rel-test");
    o.insert("signature", B64.encode(sig));
    let (ok, out, calls) = run(&cmd);
    assert!(!ok && out.contains("bad signature"), "{out}");
    assert!(calls.is_empty());
}

#[test]
fn expired_unbounded_and_far_future_commands_are_refused_even_when_signed() {
    for (exp, why) in [
        (NOW, "expired"),
        (NOW - 5, "expired"),
        (u64::MAX, "no expiry"),
        (NOW + 25 * 3600, "too far"),
    ] {
        let (ok, out, calls) = run(&command(
            json!({"action": "stop", "service": "Spooler"}),
            exp,
            true,
        ));
        assert!(!ok && out.contains(why), "{exp}: {out}");
        assert!(calls.is_empty());
    }
}

#[test]
fn signed_but_invalid_arguments_and_the_agent_itself_are_refused() {
    let exp = NOW + 60;
    for args in [
        json!({"action": "restart", "service": "Spooler"}),
        json!({"action": "set_startup", "service": "Spooler"}),
        json!({"action": "set_startup", "service": "Spooler", "startup": "boot"}),
        json!({"action": "stop", "service": ""}),
        json!({"action": "stop", "service": "a\\b"}),
        json!({"action": "stop", "service": "Dl-Agent"}),
        json!({"action": "stop", "service": "mm.arrival.agent"}),
    ] {
        let (ok, out, calls) = run(&command(args.clone(), exp, true));
        assert!(!ok, "{args:?}: {out}");
        assert!(calls.is_empty());
    }
}

#[test]
fn a_failing_service_manager_is_reported_not_swallowed() {
    struct Broken;
    impl ServiceControl for Broken {
        fn stop(&self, _: &str) -> Result<(), String> {
            Err("access denied".into())
        }
        fn start(&self, _: &str) -> Result<(), String> {
            Err("access denied".into())
        }
        fn set_startup(&self, _: &str, _: Startup) -> Result<(), String> {
            Err("access denied".into())
        }
    }
    let cmd = command(
        json!({"action": "stop", "service": "Spooler"}),
        NOW + 60,
        true,
    );
    let (ok, out) = execute(&cmd, &trusted(), &Broken, None, now());
    assert!(!ok);
    assert!(
        out.contains("Spooler") && out.contains("access denied"),
        "{out}"
    );
}

#[test]
fn a_build_without_release_keys_refuses_every_command() {
    let cmd = command(
        json!({"action": "stop", "service": "Spooler"}),
        NOW + 60,
        true,
    );
    let fake = Fake::default();
    let (ok, out) = execute(&cmd, &TrustedKeys::new(), &fake, None, now());
    assert!(!ok && out.contains("unknown key"), "{out}");
    assert!(fake.0.lock().unwrap().is_empty());
}
