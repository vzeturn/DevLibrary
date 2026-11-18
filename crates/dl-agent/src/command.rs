//! Remote commands from the hub (PROTOCOL.md §4).

use std::sync::{Arc, Mutex, PoisonError};

use dl_json::{Value, json};

/// Kinds the runtime handles itself; any other kind goes to the named module.
pub const BUILT_IN: &[&str] = &[
    "restart",
    "restart_module",
    "update_now",
    "set_log_level",
    "collect_diagnostics",
    "reload_config",
    "revoke",
];

/// Largest command output sent back (the hub caps at 64 000 characters).
pub const MAX_OUTPUT: usize = 60_000;

/// One command.
#[derive(Clone, Debug, PartialEq)]
pub struct Command {
    /// Hub id; every command gets exactly one result.
    pub id: String,
    /// What to do.
    pub kind: String,
    /// Target module, for module commands and `restart_module`.
    pub module: Option<String>,
    /// Arguments.
    pub args: Value,
    /// Unix seconds after which the command is answered `expired`.
    pub expires_at: u64,
}

impl Command {
    /// Parses one wire command; `None` when it has no id or kind (cannot be answered).
    pub fn from_json(v: &Value) -> Option<Command> {
        Some(Command {
            id: v.get("id").and_then(Value::as_str)?.to_string(),
            kind: v.get("kind").and_then(Value::as_str)?.to_string(),
            module: v.get("module").and_then(Value::as_str).map(str::to_string),
            args: v.get("args").cloned().unwrap_or(Value::Null),
            expires_at: v
                .get("expires_at")
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX),
        })
    }

    /// Whether the runtime handles this kind.
    pub fn is_built_in(&self) -> bool {
        BUILT_IN.contains(&self.kind.as_str())
    }
}

/// The answer to one command.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandResult {
    /// Command id.
    pub id: String,
    /// Success.
    pub ok: bool,
    /// Output or error, truncated to [`MAX_OUTPUT`].
    pub output: String,
    /// Unix seconds.
    pub finished_at: u64,
}

impl CommandResult {
    /// A result, with the output truncated on a character boundary.
    pub fn new(id: &str, ok: bool, output: impl Into<String>, finished_at: u64) -> CommandResult {
        let mut output = output.into();
        if output.len() > MAX_OUTPUT {
            let mut cut = MAX_OUTPUT;
            while !output.is_char_boundary(cut) {
                cut -= 1;
            }
            output.truncate(cut);
            output.push('…');
        }
        CommandResult {
            id: id.to_string(),
            ok,
            output,
            finished_at,
        }
    }

    /// Wire body for `POST /v1/commands/{id}/result`.
    pub fn body(&self) -> Value {
        json!({"ok": self.ok, "output": self.output, "finished_at": self.finished_at})
    }
}

/// Results waiting to be sent, shared by the runtime and the modules.
#[derive(Clone, Debug, Default)]
pub struct Outbox(Arc<Mutex<Vec<CommandResult>>>);

impl Outbox {
    /// Queues a result.
    pub fn push(&self, r: CommandResult) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(r);
    }

    /// Takes every queued result.
    pub fn take(&self) -> Vec<CommandResult> {
        core::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Puts results back after a failed upload, ahead of newer ones.
    pub fn requeue(&self, mut rs: Vec<CommandResult>) {
        let mut q = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        rs.append(&mut q);
        *q = rs;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_classifies() {
        let c = Command::from_json(
            &json!({"id": "c1", "kind": "restart_module", "module": "einvoice", "expires_at": 9}),
        )
        .unwrap();
        assert!(c.is_built_in());
        assert_eq!(c.module.as_deref(), Some("einvoice"));
        let m = Command::from_json(
            &json!({"id": "c2", "kind": "einvoice.resync", "module": "einvoice"}),
        )
        .unwrap();
        assert!(!m.is_built_in());
        assert_eq!(m.expires_at, u64::MAX);
        assert!(Command::from_json(&json!({"kind": "restart"})).is_none());
    }

    #[test]
    fn output_is_truncated_on_char_boundary() {
        let r = CommandResult::new("c", true, "ệ".repeat(MAX_OUTPUT), 1);
        assert!(r.output.len() <= MAX_OUTPUT + '…'.len_utf8());
        assert!(r.output.ends_with('…'));
    }

    #[test]
    fn outbox_requeue_keeps_order() {
        let o = Outbox::default();
        o.push(CommandResult::new("a", true, "", 1));
        let first = o.take();
        o.push(CommandResult::new("b", true, "", 2));
        o.requeue(first);
        let ids: Vec<String> = o.take().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, ["a", "b"]);
    }
}
