//! `dl-agent pms-enroll`: writes `cloud_url` and a one-time `enrollment_code` into the
//! SmilePmsAgent config file (every other key kept, keys sorted, replaced atomically,
//! restricted like the port's state). The `pms-site` module redeems the code on its next
//! look (within 30 s, or at once on `pms.reconnect`) and then removes it from the file.
//!
//! A site that already has `identity.json` is enrolled: nothing changes unless `--force`,
//! which deletes `identity.json` so the new code is used (the Go agent's re-enroll step).
//! The code is never printed.

use std::io::Write;
use std::path::{Path, PathBuf};

use dl_agent::Error;
use dl_agent::tool::{Tool, ToolContext};
use dl_cli::{Arg, Command, Matches};
use dl_json::{Map, Value};

use crate::acl::{self, Guard};
use crate::config;
use crate::json;
use crate::keystore::IDENTITY_FILE;

#[cfg(test)]
mod tests;

/// The registered sub-command name.
pub const NAME: &str = "pms-enroll";

/// The tool to register with `App::tool`.
pub fn pms_enroll_tool() -> Tool {
    Tool {
        name: NAME,
        grammar,
        run,
    }
}

fn grammar(cmd: Command) -> Command {
    cmd.about("Writes the SMILE PMS cloud URL and an enrollment code for the pms-site module")
        .arg(
            Arg::option("cloud-url")
                .required()
                .value_name("URL")
                .help("SMILE PMS cloud base URL (https)"),
        )
        .arg(
            Arg::option("code")
                .required()
                .value_name("CODE")
                .help("One-time enrollment code from the PMS console"),
        )
        .arg(
            Arg::option("config").value_name("PATH").help(
                "The PMS agent config (default: PMS_AGENT_CONFIG, then the SmilePmsAgent one)",
            ),
        )
        .arg(
            Arg::option("state-dir")
                .value_name("DIR")
                .help("State directory to record in the config (default: the config's own)"),
        )
        .arg(Arg::flag("force").help("Enroll again although identity.json exists (deletes it)"))
}

/// What the command line asked for.
#[derive(Debug, Clone, Default)]
pub(crate) struct Args {
    pub cloud_url: String,
    pub code: String,
    pub config: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub force: bool,
}

impl Args {
    fn from_matches(m: &Matches) -> Args {
        let text = |k: &str| m.value(k).unwrap_or_default().trim().to_owned();
        Args {
            cloud_url: text("cloud-url"),
            code: text("code"),
            config: m.value("config").map(PathBuf::from),
            state_dir: m.value("state-dir").map(PathBuf::from),
            force: m.flag("force"),
        }
    }
}

fn run(_ctx: &ToolContext<'_>, m: &Matches) -> Result<(), Error> {
    let args = Args::from_matches(m);
    let mut out = std::io::stdout();
    let guard = acl::platform();
    run_args(&args, &mut out, guard.as_ref(), &|k| {
        std::env::var(k).unwrap_or_default()
    })
    .map_err(Error::Config)
}

/// The tool body with its environment injected.
pub(crate) fn run_args(
    args: &Args,
    out: &mut dyn Write,
    guard: &dyn Guard,
    getenv: &dyn Fn(&str) -> String,
) -> Result<(), String> {
    config::validate_cloud_url(&args.cloud_url)?;
    if args.code.is_empty() {
        return Err("--code is empty".into());
    }
    let path = config::resolve_path(args.config.as_deref(), getenv);
    let state_dir = state_dir_for(args, &path, getenv)?;
    let identity = state_dir.join(IDENTITY_FILE);
    if identity.exists() {
        if !args.force {
            return say(
                out,
                &format!(
                    "{NAME}: already enrolled ({} exists); nothing changed. Use --force to enroll again.",
                    identity.display()
                ),
            );
        }
        std::fs::remove_file(&identity)
            .map_err(|e| format!("{NAME}: remove {}: {e}", identity.display()))?;
        say(out, &format!("{NAME}: removed {}", identity.display()))?;
    }
    let mut obj = read_object(&path)?;
    obj.insert("cloud_url", args.cloud_url.as_str());
    obj.insert("enrollment_code", args.code.as_str());
    if let Some(d) = &args.state_dir {
        obj.insert("state_dir", d.to_string_lossy().into_owned());
    }
    write_config(&path, &obj, guard)?;
    acl::ensure_dir(guard, &state_dir).map_err(|e| format!("{NAME}: {e}"))?;
    say(
        out,
        &format!(
            "{NAME}: wrote cloud_url and enrollment_code to {}; the pms-site module enrolls \
             within 30 s (or send pms.reconnect).",
            path.display()
        ),
    )
}

fn say(out: &mut dyn Write, line: &str) -> Result<(), String> {
    writeln!(out, "{line}").map_err(|e| format!("{NAME}: write output: {e}"))
}

/// The state directory the module will use with the new config: `--state-dir`, else what
/// the existing file (and the environment) resolves to.
fn state_dir_for(
    args: &Args,
    path: &Path,
    getenv: &dyn Fn(&str) -> String,
) -> Result<PathBuf, String> {
    if let Some(d) = &args.state_dir {
        return Ok(d.clone());
    }
    let url = args.cloud_url.clone();
    let env = move |k: &str| {
        if k == config::ENV_CLOUD_URL {
            url.clone()
        } else {
            getenv(k)
        }
    };
    let cfg = config::load(Some(path), &env).map_err(|e| format!("{NAME}: {e}"))?;
    Ok(cfg.state_dir)
}

/// The config file's object, or an empty one when there is no file.
fn read_object(path: &Path) -> Result<Map, String> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(e) => return Err(format!("{NAME}: read {}: {e}", path.display())),
    };
    match json::parse(&raw) {
        Ok(Value::Object(o)) => Ok(o),
        Ok(_) => Err(format!("{NAME}: {} is not a JSON object", path.display())),
        Err(e) => Err(format!("{NAME}: {}: {e}", path.display())),
    }
}

/// Writes `obj` (keys sorted, like the port) through a restricted temporary file and a rename.
fn write_config(path: &Path, obj: &Map, guard: &dyn Guard) -> Result<(), String> {
    let e = |what: &str, x: &dyn std::fmt::Display| format!("{NAME}: {what}: {x}");
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|x| e(&format!("create {}", dir.display()), &x))?;
    }
    let mut entries: Vec<(&String, &Value)> = obj.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut sorted = Map::new();
    for (k, v) in entries {
        sorted.insert(k.clone(), v.clone());
    }
    let mut text = json::to_pretty(&Value::Object(sorted), "  ");
    text.push('\n');
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut f = private_file(&tmp).map_err(|x| e(&format!("create {}", tmp.display()), &x))?;
    f.write_all(text.as_bytes())
        .and_then(|()| f.sync_all())
        .map_err(|x| e(&format!("write {}", tmp.display()), &x))?;
    drop(f);
    acl::ensure_file(guard, &tmp).map_err(|x| e("restrict", &x))?;
    std::fs::rename(&tmp, path).map_err(|x| e(&format!("replace {}", path.display()), &x))
}

#[cfg(unix)]
fn private_file(p: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(p)
}

#[cfg(not(unix))]
fn private_file(p: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(p)
}
