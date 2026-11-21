//! `dl-agent pms-device-enroll`: makes the `pms-device` module startable. It writes
//! `Agent:ApiBaseUrl`, `Agent:AgentId` and `Agent:TokenFile` into the DeviceAgent
//! `appsettings.json` (every other key kept) and puts the agent token in the token file
//! (replaced atomically, private to the service).
//!
//! The PMS API authenticates a device agent with `Authorization: DeviceAgent <token>` and
//! stores only the token's SHA-256, so the token must live on this machine. The tool prints
//! that hash (`sha256=<hex>`) for the PMS operator to register as
//! `{"agentId","workstation","sha256"}`; the token itself is never printed. It comes from
//! `DL_AGENT_PMS_DEVICE_TOKEN`, else the token file already there, else it is generated.

use std::io::Write;
use std::path::{Path, PathBuf};

use dl_agent::Error;
use dl_agent::tool::{Tool, ToolContext};
use dl_cli::{Arg, Command, Matches};
use dl_json::{Map, Value};

use crate::agent::{TokenFile, TokenFileError};
use crate::config::{self, AgentOptions};
use crate::module::settings::default_appsettings_dir;

#[cfg(test)]
mod tests;

/// The registered sub-command name.
pub const NAME: &str = "pms-device-enroll";
/// Where the token may come from (kept off the command line and the process list).
pub const TOKEN_ENV: &str = "DL_AGENT_PMS_DEVICE_TOKEN";
/// The settings file the module reads.
const SETTINGS_FILE: &str = "appsettings.json";
/// The file name of the token under the data directory when `--token-file` is not given.
const TOKEN_FILE_NAME: &str = "agent.token";

/// Restricts a file to the service (injected so tests need no ACL).
type Restrict<'a> = &'a dyn Fn(&Path) -> Result<(), String>;

/// The tool to register with `App::tool`.
pub fn pms_device_enroll_tool() -> Tool {
    Tool {
        name: NAME,
        grammar,
        run,
    }
}

fn grammar(cmd: Command) -> Command {
    cmd.about("Writes the PMS API URL, agent id and token file for the pms-device module")
        .arg(
            Arg::option("api-url")
                .required()
                .value_name("URL")
                .help("SMILE PMS API origin (https; http only for localhost)"),
        )
        .arg(
            Arg::option("agent-id")
                .value_name("ID")
                .help("Device agent id, 1-64 of A-Z a-z 0-9 _ - (default: the computer name)"),
        )
        .arg(
            Arg::option("appsettings-dir")
                .value_name("DIR")
                .help("Folder of the DeviceAgent appsettings.json (default: the module's own)"),
        )
        .arg(
            Arg::option("token-file")
                .value_name("PATH")
                .help("Token file (default: agent.token under Agent:DataDirectory)"),
        )
        .arg(Arg::flag("force").help("Replace a token that is already enrolled"))
}

/// What the command line asked for.
#[derive(Debug, Clone, Default)]
pub(crate) struct Args {
    pub api_url: String,
    pub agent_id: Option<String>,
    pub appsettings_dir: Option<PathBuf>,
    pub token_file: Option<PathBuf>,
    pub force: bool,
    /// The module's default appsettings folder, used when `appsettings_dir` is `None`.
    pub default_dir: PathBuf,
}

impl Args {
    fn from_matches(m: &Matches, default_dir: PathBuf) -> Args {
        let text = |k: &str| {
            m.value(k)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        Args {
            api_url: text("api-url").unwrap_or_default(),
            agent_id: text("agent-id"),
            appsettings_dir: text("appsettings-dir").map(PathBuf::from),
            token_file: text("token-file").map(PathBuf::from),
            force: m.flag("force"),
            default_dir,
        }
    }
}

fn run(ctx: &ToolContext<'_>, m: &Matches) -> Result<(), Error> {
    // The module's own folder is `<data>/modules/<name>`; it is the default off Windows.
    let module_dir = ctx.data_dir.join("modules").join(crate::NAME);
    let args = Args::from_matches(m, default_appsettings_dir(&module_dir));
    let mut out = std::io::stdout();
    run_args(&args, &mut out, &restrict_private, &|k| {
        std::env::var(k).unwrap_or_default()
    })
    .map_err(Error::Config)
}

/// The tool body with its environment and the file restriction injected.
pub(crate) fn run_args(
    args: &Args,
    out: &mut dyn Write,
    restrict: &dyn Fn(&Path) -> Result<(), String>,
    getenv: &dyn Fn(&str) -> String,
) -> Result<(), String> {
    if let Some(e) = config::api_base_url_error(&args.api_url) {
        return Err(format!("{NAME}: --api-url: {e}"));
    }
    let agent_id = match &args.agent_id {
        Some(id) => id.clone(),
        None => default_agent_id(getenv)?,
    };
    if agent_id.contains('\n') || !config::is_valid_agent_id(&agent_id) {
        return Err(format!(
            "{NAME}: --agent-id must be 1-64 characters of A-Z, a-z, 0-9, '_' or '-'"
        ));
    }

    let dir = args
        .appsettings_dir
        .clone()
        .unwrap_or_else(|| args.default_dir.clone());
    let settings_path = dir.join(SETTINGS_FILE);
    let mut root = read_settings(&settings_path)?;
    let token_path = match &args.token_file {
        Some(p) => p.clone(),
        None => PathBuf::from(data_directory(&root)).join(TOKEN_FILE_NAME),
    };

    let (token, written) = decide_token(args, &token_path, getenv)?;
    if written {
        write_private(&token_path, token.as_bytes(), restrict)?;
    }
    set_agent_keys(&mut root, &args.api_url, &agent_id, &token_path);
    write_settings(&settings_path, &root)?;

    let sha = dl_encoding::hex::encode(<dl_hash::Sha256 as dl_hash::Digest>::digest(
        token.as_bytes(),
    ));
    say(
        out,
        &format!(
            "{NAME}: agent id {agent_id}; token file {}; {}; sha256={sha}",
            token_path.display(),
            if written {
                "token written"
            } else {
                "token kept"
            }
        ),
    )?;
    say(
        out,
        &format!(
            "{NAME}: wrote Agent:ApiBaseUrl, Agent:AgentId and Agent:TokenFile to {}; register \
             {{\"agentId\":\"{agent_id}\",\"sha256\":\"{sha}\"}} in the PMS.",
            settings_path.display()
        ),
    )
}

fn say(out: &mut dyn Write, line: &str) -> Result<(), String> {
    writeln!(out, "{line}").map_err(|e| format!("{NAME}: write output: {e}"))
}

/// The computer name reduced to the agent id alphabet and 64 characters.
fn default_agent_id(getenv: &dyn Fn(&str) -> String) -> Result<String, String> {
    let name = ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .map(|k| getenv(k))
        .find(|v| !v.trim().is_empty())
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .unwrap_or_default();
    let id: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(64)
        .collect();
    if id.is_empty() {
        return Err(format!("{NAME}: no usable computer name; pass --agent-id"));
    }
    Ok(id)
}

/// `Agent:DataDirectory` of the existing file, else the options default.
fn data_directory(root: &Map) -> String {
    root.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("agent"))
        .and_then(|(_, v)| v.as_object())
        .and_then(|a| {
            a.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("DataDirectory"))
        })
        .and_then(|(_, v)| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| AgentOptions::default().data_directory)
}

/// The token to use and whether the file has to be (re)written.
fn decide_token(
    args: &Args,
    path: &Path,
    getenv: &dyn Fn(&str) -> String,
) -> Result<(String, bool), String> {
    let existing = match TokenFile::new(path.to_string_lossy().into_owned()).read() {
        Ok(t) => Some(t),
        Err(TokenFileError::NotFound { .. } | TokenFileError::TooShort { .. }) => None,
        Err(e) => return Err(format!("{NAME}: {e}")),
    };
    let supplied = getenv(TOKEN_ENV).trim().to_owned();
    if !supplied.is_empty() {
        if supplied.encode_utf16().count() < TokenFile::MIN_LENGTH {
            return Err(format!(
                "{NAME}: {TOKEN_ENV} is shorter than {} characters",
                TokenFile::MIN_LENGTH
            ));
        }
        return match existing {
            Some(e) if e == supplied => Ok((e, false)),
            Some(_) if !args.force => Err(format!(
                "{NAME}: already enrolled ({} holds another token); use --force to replace it",
                path.display()
            )),
            _ => Ok((supplied, true)),
        };
    }
    match existing {
        Some(e) if !args.force => Ok((e, false)),
        _ => Ok((generate_token()?, true)),
    }
}

/// 32 bytes from the OS RNG as 64 hex characters.
fn generate_token() -> Result<String, String> {
    let bytes = dl_rand::secure_bytes::<32>().map_err(|e| format!("{NAME}: random token: {e}"))?;
    Ok(dl_encoding::hex::encode(bytes))
}

/// The settings file's object, or an empty one when there is no file. A file that does not
/// parse is an error: it is never overwritten.
fn read_settings(path: &Path) -> Result<Map, String> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(e) => return Err(format!("{NAME}: read {}: {e}", path.display())),
    };
    let text = String::from_utf8(raw)
        .map_err(|e| format!("{NAME}: {} is not UTF-8: {e}", path.display()))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let opts = dl_json::ParseOptions {
        keep_raw_numbers: true,
        ..dl_json::ParseOptions::default()
    };
    match dl_json::parse_with(text, opts) {
        Ok(Value::Object(o)) => Ok(o),
        Ok(_) => Err(format!("{NAME}: {} is not a JSON object", path.display())),
        Err(e) => Err(format!(
            "{NAME}: {} is not plain JSON ({e}); not changed, edit Agent:* by hand",
            path.display()
        )),
    }
}

/// Sets the three keys under `Agent` (matching an existing key in any case).
fn set_agent_keys(root: &mut Map, url: &str, id: &str, token_file: &Path) {
    let key = |m: &Map, name: &str| {
        m.keys()
            .find(|k| k.eq_ignore_ascii_case(name))
            .cloned()
            .unwrap_or_else(|| name.to_owned())
    };
    let agent_key = key(root, "Agent");
    if !matches!(root.get(&agent_key), Some(Value::Object(_))) {
        root.insert(agent_key.clone(), Value::Object(Map::new()));
    }
    if let Some(Value::Object(agent)) = root.get_mut(&agent_key) {
        for (name, value) in [
            ("ApiBaseUrl", url.to_owned()),
            ("AgentId", id.to_owned()),
            ("TokenFile", token_file.to_string_lossy().into_owned()),
        ] {
            let k = key(agent, name);
            agent.insert(k, value);
        }
    }
}

fn write_settings(path: &Path, root: &Map) -> Result<(), String> {
    let mut text = dl_json::to_string_pretty(&Value::Object(root.clone()));
    text.push('\n');
    replace_file(path, text.as_bytes(), None)
}

fn write_private(
    path: &Path,
    bytes: &[u8],
    restrict: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<(), String> {
    replace_file(path, bytes, Some(restrict))
}

/// Writes `bytes` through a temporary file next to `path` and a rename; the temporary file is
/// restricted before the rename when `restrict` is given.
fn replace_file(path: &Path, bytes: &[u8], restrict: Option<Restrict<'_>>) -> Result<(), String> {
    let e = |what: &str, x: &dyn std::fmt::Display| format!("{NAME}: {what}: {x}");
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|x| e(&format!("create {}", dir.display()), &x))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut f = open_tmp(&tmp, restrict.is_some())
        .map_err(|x| e(&format!("create {}", tmp.display()), &x))?;
    f.write_all(bytes)
        .and_then(|()| f.sync_all())
        .map_err(|x| e(&format!("write {}", tmp.display()), &x))?;
    drop(f);
    if let Some(r) = restrict {
        r(&tmp).map_err(|x| e("restrict", &x))?;
    }
    std::fs::rename(&tmp, path).map_err(|x| e(&format!("replace {}", path.display()), &x))
}

#[cfg(unix)]
fn open_tmp(p: &Path, private: bool) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    if private {
        o.mode(0o600);
    }
    o.open(p)
}

#[cfg(not(unix))]
fn open_tmp(p: &Path, _private: bool) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(p)
}

/// SYSTEM and Administrators only (the service runs as LocalSystem).
#[cfg(windows)]
fn restrict_private(p: &Path) -> Result<(), String> {
    dl_winsvc::acl::restrict(p).map_err(|e| format!("{e} (run as Administrator or LocalSystem)"))
}

/// The file was created 0600 already.
#[cfg(not(windows))]
fn restrict_private(_: &Path) -> Result<(), String> {
    Ok(())
}
