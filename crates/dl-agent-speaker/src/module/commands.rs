//! Commands the hub sends to the `speaker` module (kind `<name>` or `speaker.<name>`).
//! Answers are JSON without secrets (no token).
//!
//! - `status`: settings, link state (joined/refused RVCs, last error), the cached sound
//!   mode per RVC, the last fetch failures and the counters;
//! - `test` (`rvc`, optional `kind` = `order` | `service_bill` | `service_charge` |
//!   `service_waiter`, optional `table`): plays what a guest event of that RVC would, to
//!   check the speaker on site.
//!
//! Unknown commands are answered `false`.

use std::sync::PoisonError;

use dl_agent::command::Command;
use dl_agent::supervisor::ModuleContext;
use dl_json::{Map, Value};

use super::Runtime;
use crate::decision::GuestEvent;

const KINDS: [&str; 4] = ["order", "service_bill", "service_charge", "service_waiter"];

/// Answers every queued command.
pub fn answer_all(ctx: &ModuleContext, rt: &Runtime) {
    while let Some(c) = ctx.next_command() {
        let (ok, out) = answer(rt, &c);
        ctx.complete(&c.id, ok, &out);
    }
}

/// The answer to one command: `(ok, output)`.
pub fn answer(rt: &Runtime, cmd: &Command) -> (bool, String) {
    let kind = cmd.kind.strip_prefix("speaker.").unwrap_or(&cmd.kind);
    match kind {
        "status" => (true, dl_json::to_string(&status(rt))),
        "test" => test(rt, &cmd.args),
        other => (false, format!("unknown speaker command {other:?}")),
    }
}

/// The `status` answer.
pub fn status(rt: &Runtime) -> Value {
    let mut o = Map::new();
    o.insert(
        "rvcList",
        Value::Array(rt.settings.rvcs.iter().map(|r| Value::from(*r)).collect()),
    );
    o.insert(
        "soundRefreshSeconds",
        Value::from(rt.settings.sound_refresh.as_secs()),
    );
    o.insert("enrolled", Value::from(rt.link.is_some()));
    if let Some(link) = &rt.link {
        let s = link.state();
        let mut h = Map::new();
        h.insert("connected", Value::from(s.connected));
        h.insert(
            "joined",
            Value::Array(s.joined.iter().map(|r| Value::from(*r)).collect()),
        );
        let mut refused = Map::new();
        for (rvc, m) in &s.refused {
            refused.insert(rvc.to_string(), m.as_str());
        }
        h.insert("refused", Value::Object(refused));
        h.insert("lastError", s.last_error.map_or(Value::Null, Value::from));
        h.insert("connects", Value::from(s.connects));
        o.insert("hub", Value::Object(h));
    }
    let mut modes = Map::new();
    for rvc in &rt.settings.rvcs {
        let m = rt.cache.get(*rvc);
        let mut e = Map::new();
        e.insert("tone", m.tone.map_or(Value::Null, Value::from));
        e.insert("voice", Value::from(m.voice));
        e.insert("lang", m.lang.as_str());
        modes.insert(rvc.to_string(), Value::Object(e));
    }
    o.insert("modes", Value::Object(modes));
    let mut failures = Map::new();
    for (rvc, e) in rt
        .fetch_failures
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
    {
        failures.insert(rvc.to_string(), e.as_str());
    }
    o.insert("modeFetchFailures", Value::Object(failures));
    if let Some(a) = &rt.announcer {
        let mut c = Map::new();
        for (k, v) in a.stats().metrics() {
            c.insert(k, Value::from(v));
        }
        o.insert("counters", Value::Object(c));
    }
    Value::Object(o)
}

fn test(rt: &Runtime, args: &Value) -> (bool, String) {
    let Some(announcer) = &rt.announcer else {
        return (false, super::NOT_ENROLLED.to_owned());
    };
    let Some(rvc) = args.get("rvc").and_then(Value::as_i64) else {
        return (false, "test needs args.rvc".to_owned());
    };
    let kind = match args.get("kind").and_then(Value::as_str) {
        None => "order",
        Some(k) => match KINDS.iter().find(|x| **x == k) {
            Some(k) => k,
            None => return (false, format!("unknown kind {k:?}; one of {KINDS:?}")),
        },
    };
    let table = args
        .get("table")
        .and_then(Value::as_str)
        .unwrap_or("1")
        .to_owned();
    announcer.announce(&GuestEvent { kind, rvc, table });
    (true, format!("queued a {kind} announcement for RVC {rvc}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::Cache;
    use crate::module::settings::Settings;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn rt() -> Runtime {
        Runtime {
            settings: Settings {
                rvcs: vec![1, 2],
                sound_refresh: Duration::from_secs(120),
            },
            cache: Arc::new(Cache::new()),
            announcer: None,
            link: None,
            fetch_failures: Mutex::new(vec![(2, "sound-mode fetch returned status 403".into())]),
        }
    }

    fn cmd(kind: &str, args: Value) -> Command {
        Command {
            id: "c1".into(),
            kind: kind.into(),
            module: Some("speaker".into()),
            args,
            expires_at: 0,
        }
    }

    #[test]
    fn status_before_enrolment_shows_settings_and_default_modes() {
        let (ok, out) = answer(&rt(), &cmd("speaker.status", Value::Null));
        assert!(ok);
        let v = dl_json::parse(&out).unwrap();
        assert_eq!(v.get("enrolled"), Some(&Value::from(false)));
        assert_eq!(
            v.pointer("/modes/1/tone").and_then(Value::as_str),
            Some("guest")
        );
        assert!(
            v.pointer("/modeFetchFailures/2")
                .and_then(Value::as_str)
                .unwrap()
                .contains("403")
        );
    }

    #[test]
    fn test_and_unknown_commands() {
        let (ok, out) = answer(&rt(), &cmd("test", dl_json::json!({"rvc": 1})));
        assert!(!ok && out.contains("pda-enroll"), "{out}");
        let (ok, out) = answer(&rt(), &cmd("speaker.reboot", Value::Null));
        assert!(!ok && out.contains("reboot"), "{out}");
    }
}
