//! `dl-agent arrival-enroll`: enrolls this site the way Install.ps1 and
//! `MM.Arrival.Agent enroll` do, without the .NET agent.
//!
//! The enrollment token (shown once by the platform console) comes from
//! `DL_AGENT_ARRIVAL_TOKEN`, else the first line of stdin; the PMS connection string from
//! `DL_AGENT_ARRIVAL_PMS`, else the next line of stdin, else the one already saved. Neither
//! is ever taken from the command line (it would show in the process list) or printed.
//!
//! Steps: probe the PMS (`test-pms`, unless `--no-probe`; a TLS failure without an `Encrypt`
//! key retries with `Encrypt=False`, as Install.ps1 does), write `appsettings.json` when it is
//! missing and `appsettings.Production.json` (`CloudBaseUrl`, `PmsConnectionString` and the
//! absolute `IdentityFile` / `IdempotencyFile` under `data`), save the identity (DPAPI on
//! Windows) and create or keep the tenant master key, printing only its fingerprint. The
//! settings file and `data` are closed to everyone but SYSTEM and Administrators (0600 /
//! 0700 elsewhere). A site that already has an identity is left alone unless `--force`.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use dl_agent::Error;
use dl_agent::tool::{Tool, ToolContext};
use dl_cli::{Arg, Command, Matches};
use dl_json::{Map, Value};

use crate::agent::cloud;
use crate::agent::layout::{IDENTITY_IN_INSTALL, Layout};
use crate::agent::secrets::{Identity, TenantKeyStore};
use crate::agent::settings::{BASE_FILE, PRODUCTION_FILE, Settings};
use crate::agent::tds;
use crate::module;

/// The registered sub-command name.
pub const NAME: &str = "arrival-enroll";
/// The environment variable holding the enrollment token.
pub const TOKEN_ENV: &str = "DL_AGENT_ARRIVAL_TOKEN";
/// The environment variable holding the PMS connection string.
pub const PMS_ENV: &str = "DL_AGENT_ARRIVAL_PMS";

/// The `appsettings.json` the .NET agent ships with (written when the install has none).
const BASE_SETTINGS: &str = include_str!("../../assets/appsettings.json");
/// The .NET placeholder for an unset connection string.
const PLACEHOLDER: &str = "REPLACE_VIA_ENV";

/// The tool to register with `App::tool`.
pub fn arrival_enroll_tool() -> Tool {
    Tool {
        name: NAME,
        grammar,
        run,
    }
}

fn grammar(cmd: Command) -> Command {
    cmd.about(
        "Enrolls this site with Azolo Check-In for the arrival module (token from DL_AGENT_ARRIVAL_TOKEN or stdin, PMS connection string from DL_AGENT_ARRIVAL_PMS or stdin)",
    )
    .arg(
        Arg::option("cloud-url")
            .required()
            .value_name("URL")
            .help("Check-In cloud origin (https)"),
    )
    .arg(
        Arg::option("install-dir")
            .value_name("DIR")
            .help("The agent folder (default: the MM-Arrival Agent folder; must match modules.arrival.install_dir)"),
    )
    .arg(Arg::flag("force").help("Enroll again although an identity exists"))
    .arg(Arg::flag("no-probe").help("Do not test the PMS connection first"))
}

/// What the command line asked for.
#[derive(Debug, Clone, Default)]
pub struct Args {
    /// `--cloud-url`.
    pub cloud_url: String,
    /// `--install-dir`.
    pub install_dir: Option<PathBuf>,
    /// `--force`.
    pub force: bool,
    /// `--no-probe`.
    pub no_probe: bool,
}

impl Args {
    fn from_matches(m: &Matches) -> Args {
        Args {
            cloud_url: m.value("cloud-url").unwrap_or_default().trim().to_owned(),
            install_dir: m.value("install-dir").map(PathBuf::from),
            force: m.flag("force"),
            no_probe: m.flag("no-probe"),
        }
    }
}

/// Where the tool reads secrets and writes its lines.
pub struct Io<'a> {
    /// Lines printed for the operator.
    pub out: &'a mut dyn Write,
    /// The token and connection string when the environment has none.
    pub input: &'a mut dyn BufRead,
    /// `true` when `input` is a console (prompts are printed on stderr).
    pub prompt: bool,
    /// The environment.
    pub getenv: &'a dyn Fn(&str) -> String,
}

impl std::fmt::Debug for Io<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Io")
            .field("prompt", &self.prompt)
            .finish_non_exhaustive()
    }
}

fn run(ctx: &ToolContext<'_>, m: &Matches) -> Result<(), Error> {
    let args = Args::from_matches(m);
    let stdin = std::io::stdin();
    let prompt = stdin.is_terminal();
    let mut input = stdin.lock();
    let mut out = std::io::stdout();
    let getenv = |k: &str| std::env::var(k).unwrap_or_default();
    let fallback = ctx.data_dir.join("modules").join(module::NAME);
    let mut io = Io {
        out: &mut out,
        input: &mut input,
        prompt,
        getenv: &getenv,
    };
    run_args(&args, &fallback, &mut io).map_err(Error::Config)
}

/// The tool body with its environment injected; `fallback` is the install directory off
/// Windows when `--install-dir` is not given.
///
/// # Errors
/// A message for the operator (never a secret).
pub fn run_args(args: &Args, fallback: &Path, io: &mut Io<'_>) -> Result<(), String> {
    let fail = |e: &dyn std::fmt::Display| format!("{NAME}: {e}");
    let base = cloud::check_base_url(&args.cloud_url).map_err(|e| fail(&e))?;
    let layout = Layout::detect(args.install_dir.as_deref(), fallback);
    let install = layout.install_dir().to_path_buf();
    let saved = Settings::load(&install).map_err(|e| fail(&e))?;
    let data = install.join("data");
    let identity_path = install.join(IDENTITY_IN_INSTALL);
    let existing = [layout.resolve(&saved.identity_file), identity_path.clone()];
    if let Some(found) = existing.iter().find(|p| p.exists())
        && !args.force
    {
        return say(
            io,
            &format!(
                "{NAME}: already enrolled ({} exists); nothing changed. Use --force to enroll again.",
                found.display()
            ),
        );
    }

    let token = secret(io, TOKEN_ENV, "Enrollment token")?;
    if token.is_empty() {
        return Err(fail(&format!(
            "no enrollment token: set {TOKEN_ENV} or pass it on stdin"
        )));
    }
    let identity = Identity::from_token(&token).map_err(|e| fail(&e))?;
    let mut pms = secret(io, PMS_ENV, "PMS connection string")?;
    if pms.is_empty() && !saved.pms_connection_string.starts_with(PLACEHOLDER) {
        pms = saved.pms_connection_string.trim().to_string();
    }
    if pms.is_empty() {
        return Err(fail(&format!(
            "no PMS connection string: set {PMS_ENV} or pass it on stdin"
        )));
    }
    if !args.no_probe {
        pms = probe(io, pms)?;
    }

    restricted_dir(&data).map_err(|e| fail(&e))?;
    let base_file = install.join(BASE_FILE);
    if !base_file.exists() {
        write_file(&base_file, BASE_SETTINGS.as_bytes(), false).map_err(|e| fail(&e))?;
    }
    let mut agent = Map::new();
    agent.insert("CloudBaseUrl", base.to_string().trim_end_matches('/'));
    agent.insert("PmsConnectionString", pms.as_str());
    agent.insert("IdentityFile", path_text(&identity_path));
    agent.insert(
        "IdempotencyFile",
        path_text(&data.join("agent-idempotency.jsonl")),
    );
    let mut root = Map::new();
    root.insert("Agent", Value::Object(agent));
    let mut text = dl_json::to_string_pretty(&Value::Object(root));
    text.push('\n');
    write_file(&install.join(PRODUCTION_FILE), text.as_bytes(), true).map_err(|e| fail(&e))?;
    say(
        io,
        &format!("{NAME}: wrote {}", install.join(PRODUCTION_FILE).display()),
    )?;

    // The settings now name this file; an identity at an older path is left as it was.
    identity.save(&identity_path).map_err(|e| fail(&e))?;
    restrict(&identity_path).map_err(|e| fail(&e))?;
    say(
        io,
        &format!(
            "{NAME}: enrolled agent {} for tenant {}; identity saved to {}",
            identity.agent_id,
            identity.tenant_id,
            identity_path.display()
        ),
    )?;

    let settings = Settings::load(&install).map_err(|e| fail(&e))?;
    let key_file = layout.resolve(&settings.tenant_key_file);
    let keys = TenantKeyStore::new(&settings.tenant_key, key_file.clone());
    let fingerprint = keys.vault().map_err(|e| fail(&e))?.fingerprint();
    say(
        io,
        &format!(
            "{NAME}: tenant master key fingerprint {fingerprint} ({}). Keep a copy of that file offline: without the key the cloud data cannot be read.",
            key_file.display()
        ),
    )?;
    say(
        io,
        &format!(
            "{NAME}: set modules.{}.enabled = true (install_dir {}) for the module to link up.",
            module::NAME,
            install.display()
        ),
    )
}

fn say(io: &mut Io<'_>, line: &str) -> Result<(), String> {
    writeln!(io.out, "{line}").map_err(|e| format!("{NAME}: write output: {e}"))
}

/// The environment's value of `env`, else one line of input.
fn secret(io: &mut Io<'_>, env: &str, what: &str) -> Result<String, String> {
    let from_env = (io.getenv)(env).trim().to_string();
    if !from_env.is_empty() {
        return Ok(from_env);
    }
    if io.prompt {
        eprint!("{what}: ");
    }
    let mut line = String::new();
    io.input
        .read_line(&mut line)
        .map_err(|e| format!("{NAME}: read {what} from stdin: {e}"))?;
    Ok(line.trim().to_string())
}

/// `test-pms` and the `Encrypt=False` step-down of Install.ps1; the connection string to keep.
fn probe(io: &mut Io<'_>, pms: String) -> Result<String, String> {
    let cannot = |e: &dl_tds::Error| format!("{NAME}: cannot connect to the PMS database: {e}");
    match tds::probe(&pms) {
        Ok(p) => {
            say(io, &p.line())?;
            Ok(pms)
        }
        Err(e) if e.is_tls() && !tds::has_key(&pms, "encrypt") => {
            say(
                io,
                &format!("{NAME}: TLS to SQL Server failed ({e}); retrying with Encrypt=False"),
            )?;
            let fallback = format!("{};Encrypt=False", pms.trim_end_matches(';'));
            let p = tds::probe(&fallback).map_err(|e| {
                if dl_agent::sql::transport::should_step_down(&e) {
                    format!(
                        "{}. The SQL Server and this machine share no TLS version: install the TLS 1.2 update on SQL Server 2008-2012 (KB3135244).",
                        cannot(&e)
                    )
                } else {
                    cannot(&e)
                }
            })?;
            say(io, &p.line())?;
            Ok(fallback)
        }
        Err(e) => Err(cannot(&e)),
    }
}

fn path_text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Writes through a temporary file and a rename; `secret` restricts it first.
fn write_file(path: &Path, bytes: &[u8], secret: bool) -> Result<(), String> {
    let e = |what: &str, x: &dyn std::fmt::Display| format!("{what} {}: {x}", path.display());
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|x| e("create the folder of", &x))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    if secret {
        crate::agent::secrets::write_secret(&tmp, bytes, "settings").map_err(|x| e("write", &x))?;
        restrict(&tmp).map_err(|x| e("restrict", &x))?;
    } else {
        std::fs::write(&tmp, bytes).map_err(|x| e("write", &x))?;
    }
    std::fs::rename(&tmp, path).map_err(|x| e("replace", &x))
}

/// Creates `dir` closed to everyone but the service (and administrators).
fn restricted_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("restrict {}: {e}", dir.display()))?;
    }
    restrict(dir)
}

/// The Windows ACL of Install.ps1 (SYSTEM and Administrators only); a no-op elsewhere,
/// where files are written 0600.
fn restrict(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        dl_winsvc::acl::restrict(path).map_err(|e| format!("restrict {}: {e}", path.display()))?;
    }
    #[cfg(not(windows))]
    let _ = path;
    Ok(())
}
