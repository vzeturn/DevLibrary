//! Remote attach (PROTOCOL.md §4.2, RT-0261): the hub's `attach` command runs one of the
//! binary's module enrol tools (`pms-enroll`, `pda-enroll`, ...) on this machine, so a product
//! is switched on for an installed agent without anyone at the site.
//!
//! The command's arguments are `{module, tool, argv, env}`: the hub has already enabled
//! `modules.<module>` in the config; `env` carries the product's one-time code in the
//! variable the tool already reads (never argv, so it stays out of the process list). The
//! tool runs as a child process of the service (`<exe> <tool> --data <dir> <argv...>`) with
//! no console input, so a tool that would prompt fails instead of waiting. The answer is the
//! tool's exit status and output with every secret value masked. After a success the hub
//! restarts the module so it starts on its new credentials.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dl_json::Value;

use crate::cli::App;
use crate::sys_module::SelfTestHook;

/// Longest an enrol tool may run before it is stopped.
pub const TOOL_TIMEOUT: Duration = Duration::from_secs(600);
/// Prefix every variable the hub may set must carry (the tools read `DL_AGENT_*`).
pub const ENV_PREFIX: &str = "DL_AGENT_";
/// What replaces a secret value in the answer.
pub const MASK: &str = "***";
const MAX_ARGS: usize = 32;

/// One checked `attach` command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The module the hub enabled (for the log and the answer).
    pub module: String,
    /// The registered tool to run.
    pub tool: String,
    /// The tool's arguments after `--data <dir>`.
    pub argv: Vec<String>,
    /// Variables for the tool (secrets).
    pub env: BTreeMap<String, String>,
}

fn text(v: &Value, key: &str) -> Result<String, String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("attach: missing {key}"))
}

fn clean(s: &str) -> bool {
    !s.chars().any(|c| c == '\0' || c == '\r' || c == '\n')
}

/// Checks the command's arguments: a registered tool, plain one-line arguments that never
/// override `--data`, and `DL_AGENT_*` variables only.
pub fn plan(args: &Value, tools: &[&str]) -> Result<Plan, String> {
    let module = text(args, "module")?;
    let tool = text(args, "tool")?;
    if !tools.contains(&tool.as_str()) {
        return Err(format!("attach: this build has no tool {tool}"));
    }
    let argv: Vec<String> = match args.get("argv") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|a| {
                a.as_str()
                    .map(str::to_string)
                    .ok_or("attach: argv holds a non-string")
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("attach: argv is not a list".into()),
    };
    if argv.len() > MAX_ARGS {
        return Err("attach: too many arguments".into());
    }
    if let Some(bad) = argv
        .iter()
        .find(|a| !clean(a) || *a == "--data" || a.starts_with("--data="))
    {
        return Err(format!("attach: argument {bad:?} is not allowed"));
    }
    let mut env = BTreeMap::new();
    match args.get("env") {
        None | Some(Value::Null) => {}
        Some(Value::Object(map)) => {
            for (k, v) in map.iter() {
                let ok_name = k.starts_with(ENV_PREFIX)
                    && k.len() <= 64
                    && k.chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
                if !ok_name {
                    return Err(format!("attach: variable {k} is not allowed"));
                }
                let v = v
                    .as_str()
                    .filter(|s| !s.contains('\0'))
                    .ok_or_else(|| format!("attach: variable {k} is not text"))?;
                env.insert(k.clone(), v.to_string());
            }
        }
        Some(_) => return Err("attach: env is not an object".into()),
    }
    Ok(Plan {
        module,
        tool,
        argv,
        env,
    })
}

/// `text` with every value of `secrets` (4 characters or longer) replaced by [`MASK`].
pub fn mask(text: &str, secrets: &BTreeMap<String, String>) -> String {
    let mut out = text.to_string();
    for v in secrets.values().filter(|v| v.len() >= 4) {
        out = out.replace(v.as_str(), MASK);
    }
    out
}

fn drain(mut r: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Err(e) = r.read_to_end(&mut buf) {
            buf.extend_from_slice(format!("\n[output cut: {e}]").as_bytes());
        }
        buf
    })
}

/// Runs `exe args...` with `env` added, no input, for at most `timeout`; returns success and
/// the combined output (stdout then stderr) with the secrets masked.
pub fn run(
    exe: &Path,
    args: &[String],
    env: &BTreeMap<String, String>,
    timeout: Duration,
) -> (bool, String) {
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (false, format!("cannot start {}: {e}", exe.display())),
    };
    let out = child.stdout.take().map(drain);
    let err = child.stderr.take().map(drain);
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() >= timeout => {
                if let Err(e) = child.kill() {
                    dl_log::warn!("cannot stop the enrol tool"; error = %e);
                }
                if let Err(e) = child.wait() {
                    dl_log::warn!("cannot reap the enrol tool"; error = %e);
                }
                break Err(format!("stopped after {} s", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => break Err(format!("cannot wait for the tool: {e}")),
        }
    };
    let mut text = String::new();
    for h in [out, err].into_iter().flatten() {
        match h.join() {
            Ok(bytes) => text.push_str(&String::from_utf8_lossy(&bytes)),
            Err(_) => text.push_str("\n[output reader panicked]"),
        }
    }
    let (ok, head) = match status {
        Ok(s) if s.success() => (true, "exit 0".to_string()),
        Ok(s) => (false, format!("exit {}", s.code().unwrap_or(-1))),
        Err(why) => (false, why),
    };
    (ok, mask(&format!("{head}\n{}", text.trim_end()), env))
}

/// The hook the `sys` module runs for the `attach` command.
pub fn hook(app: &App, dir: &Path) -> SelfTestHook {
    let tools: Vec<&'static str> = app.tools.iter().map(|t| t.name).collect();
    let dir: PathBuf = dir.to_path_buf();
    SelfTestHook(Arc::new(move |args: &Value| {
        let plan = match plan(args, &tools) {
            Ok(p) => p,
            Err(why) => return (false, why),
        };
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => return (false, format!("attach: cannot locate the agent: {e}")),
        };
        let mut argv = vec![
            plan.tool.clone(),
            "--data".to_string(),
            dir.display().to_string(),
        ];
        argv.extend(plan.argv.iter().cloned());
        let (ok, out) = run(&exe, &argv, &plan.env, TOOL_TIMEOUT);
        dl_log::info!("attach finished"; module = %plan.module, tool = %plan.tool, ok = ok);
        (ok, format!("{} {}: {out}", plan.module, plan.tool))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dl_json::json;

    const TOOLS: &[&str] = &["pms-enroll", "pda-enroll"];

    #[test]
    fn plan_takes_a_registered_tool_with_argv_and_env() {
        let p = plan(
            &json!({"module": "pms-site", "tool": "pms-enroll",
                    "argv": ["--cloud-url", "https://pms.example"],
                    "env": {"DL_AGENT_PMS_CODE": "C0DE-1234"}}),
            TOOLS,
        )
        .unwrap();
        assert_eq!(p.tool, "pms-enroll");
        assert_eq!(p.argv, vec!["--cloud-url", "https://pms.example"]);
        assert_eq!(p.env.get("DL_AGENT_PMS_CODE").unwrap(), "C0DE-1234");
        let bare = plan(&json!({"module": "pms-site", "tool": "pms-enroll"}), TOOLS).unwrap();
        assert!(bare.argv.is_empty() && bare.env.is_empty());
    }

    #[test]
    fn plan_refuses_anything_else() {
        let bad = [
            json!({"tool": "pms-enroll"}),
            json!({"module": "x", "tool": "enroll"}),
            json!({"module": "x", "tool": "run"}),
            json!({"module": "x", "tool": "pms-enroll", "argv": ["--data", "C:\\x"]}),
            json!({"module": "x", "tool": "pms-enroll", "argv": ["--data=C:\\x"]}),
            json!({"module": "x", "tool": "pms-enroll", "argv": ["a\nb"]}),
            json!({"module": "x", "tool": "pms-enroll", "argv": [1]}),
            json!({"module": "x", "tool": "pms-enroll", "argv": "--x"}),
            json!({"module": "x", "tool": "pms-enroll", "env": {"PATH": "C:\\evil"}}),
            json!({"module": "x", "tool": "pms-enroll", "env": {"DL_AGENT_x": "1"}}),
            json!({"module": "x", "tool": "pms-enroll", "env": {"DL_AGENT_X": 1}}),
            json!({"module": "x", "tool": "pms-enroll", "env": []}),
        ];
        for args in bad {
            assert!(plan(&args, TOOLS).is_err(), "{args:?}");
        }
    }

    #[test]
    fn mask_hides_secret_values() {
        let env = BTreeMap::from([
            ("DL_AGENT_PMS_CODE".to_string(), "C0DE-1234".to_string()),
            ("DL_AGENT_SHORT".to_string(), "ab".to_string()),
        ]);
        assert_eq!(
            mask("code C0DE-1234 used, ab kept", &env),
            "code *** used, ab kept"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_passes_env_masks_output_and_reports_the_exit() {
        let env = BTreeMap::from([("DL_AGENT_T".to_string(), "S3CRET-VALUE".to_string())]);
        let args = vec![
            "-c".to_string(),
            "echo got $DL_AGENT_T; echo oops >&2; exit 3".to_string(),
        ];
        let (ok, out) = run(Path::new("/bin/sh"), &args, &env, Duration::from_secs(10));
        assert!(!ok);
        assert_eq!(out, "exit 3\ngot ***\noops");
        let (ok, out) = run(
            Path::new("/bin/sh"),
            &["-c".to_string(), "read x || echo no input".to_string()],
            &BTreeMap::new(),
            Duration::from_secs(10),
        );
        assert!(ok);
        assert_eq!(out, "exit 0\nno input");
    }

    #[cfg(unix)]
    #[test]
    fn run_stops_a_tool_that_hangs() {
        let started = Instant::now();
        let (ok, out) = run(
            Path::new("/bin/sh"),
            &["-c".to_string(), "exec sleep 30".to_string()],
            &BTreeMap::new(),
            Duration::from_millis(300),
        );
        assert!(!ok);
        assert!(out.starts_with("stopped after"), "{out}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn run_reports_a_missing_executable() {
        let (ok, out) = run(
            Path::new("/no/such/dl-agent"),
            &[],
            &BTreeMap::new(),
            Duration::from_secs(1),
        );
        assert!(!ok);
        assert!(out.contains("cannot start"));
    }
}
