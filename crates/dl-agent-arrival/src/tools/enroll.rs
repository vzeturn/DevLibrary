//! `dl-agent arrival-enroll`: enrolls this site with Azolo Check-In, the fresh enrol that
//! follows the removal of the .NET agent (no in-place takeover of its identity).
//!
//! The enrollment token (shown once by the platform console) comes from
//! `DL_AGENT_ARRIVAL_TOKEN`, else the first line of stdin; the PMS connection string from
//! `DL_AGENT_ARRIVAL_PMS`, else the next line of stdin, else the one already saved, else the
//! one the removed .NET agent saved (its sign-in mode preselected; `DL_AGENT_ARRIVAL_SQL_AUTH`
//! = `windows` | `sql` with `DL_AGENT_ARRIVAL_SQL_USER` and `DL_AGENT_SQL_PASSWORD` chooses
//! another). None of them is ever taken from the command line (it would show in the process
//! list) or printed.
//!
//! Steps: probe the PMS (`test-pms`, unless `--no-probe`; a TLS failure without an `Encrypt`
//! key retries with `Encrypt=False`, as Install.ps1 does); decide the tenant master key so
//! the old agent's legal vault stays readable ([`crate::legacy::tenant_key`]: kept, or only
//! that key file imported, or refuse); write `appsettings.json` when it is missing and
//! `appsettings.Production.json` (`CloudBaseUrl`, `PmsConnectionString`, the absolute
//! `IdentityFile` / `IdempotencyFile` under `data`, and `TenantKeyFile` / `VaultDirectory`
//! when the key was imported or the vault stays elsewhere; other keys already there are
//! kept); save the identity (DPAPI on Windows) and record the enrol in `data/dl-enroll.json`,
//! printing only the key fingerprint. The settings file and `data` are closed to everyone but
//! SYSTEM and Administrators (0600 / 0700 elsewhere). A site that already has an identity is
//! left alone unless `--force`.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use dl_agent::Error;
use dl_agent::tool::{Tool, ToolContext};
use dl_cli::{Arg, Command, Matches};
use dl_json::{Map, Value};
use dl_time::OffsetDateTime;

use crate::agent::cloud;
use crate::agent::layout::{IDENTITY_IN_INSTALL, Layout};
use crate::agent::secrets::{Identity, TenantKeyStore};
use crate::agent::settings::{BASE_FILE, PRODUCTION_FILE, Settings};
use crate::agent::tds;
use crate::legacy::installs::{self, LegacyInstall};
use crate::legacy::sqlauth::{self, SqlAuth};
use crate::legacy::tenant_key::{self, KeyPlan, KeySource, PlanInput};
use crate::module;
use crate::tools::files::{self, AgentKeys};

/// The registered sub-command name.
pub const NAME: &str = "arrival-enroll";
/// The environment variable holding the enrollment token.
pub const TOKEN_ENV: &str = "DL_AGENT_ARRIVAL_TOKEN";
/// The environment variable holding the PMS connection string.
pub const PMS_ENV: &str = "DL_AGENT_ARRIVAL_PMS";
/// The environment variable choosing the SQL sign-in mode (`windows` | `sql`).
pub const SQL_AUTH_ENV: &str = "DL_AGENT_ARRIVAL_SQL_AUTH";
/// The environment variable naming the SQL login (with `sql`).
pub const SQL_USER_ENV: &str = "DL_AGENT_ARRIVAL_SQL_USER";
/// The environment variable holding the SQL login's password (with `sql`).
pub const SQL_PASSWORD_ENV: &str = "DL_AGENT_SQL_PASSWORD";
/// The environment variable holding a tenant key the operator recovered (base64, e.g. from
/// the Check-In key escrow).
pub const TENANT_KEY_ENV: &str = "DL_AGENT_ARRIVAL_TENANT_KEY";

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
    .arg(
        Arg::option("legacy-dir")
            .value_name("DIR")
            .help("The removed .NET agent folder to take the tenant key and PMS connection from (default: found under Program Files and the bench records)"),
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
    /// `--legacy-dir`.
    pub legacy_dir: Option<PathBuf>,
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
            legacy_dir: m.value("legacy-dir").map(PathBuf::from),
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
    let layout = Layout::detect_with(args.install_dir.as_deref(), fallback, io.getenv);
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
    let legacy = legacy_installs(args, io)?;

    let token = secret(io, TOKEN_ENV, "Enrollment token")?;
    if token.is_empty() {
        return Err(fail(&format!(
            "no enrollment token: set {TOKEN_ENV} or pass it on stdin"
        )));
    }
    let identity = Identity::from_token(&token).map_err(|e| fail(&e))?;
    let mut pms = connection(io, &saved, &legacy)?;
    if !args.no_probe {
        pms = probe(io, pms)?;
    }

    // Decide the tenant key before anything is written: a refusal leaves the site as it was.
    let supplied = (io.getenv)(TENANT_KEY_ENV);
    let supplied = supplied.trim();
    let key_plan = tenant_key::plan(&PlanInput {
        layout: &layout,
        target: &saved,
        legacy: &legacy,
        system32: layout.legacy_cwd(),
        supplied: (!supplied.is_empty()).then_some(supplied),
    })
    .map_err(|e| fail(&e))?;
    for note in &key_plan.notes {
        say(io, &format!("{NAME}: {note}"))?;
    }

    files::restricted_dir(&data).map_err(|e| fail(&e))?;
    let base_file = install.join(BASE_FILE);
    if !base_file.exists() {
        files::write_file(&base_file, BASE_SETTINGS.as_bytes(), false).map_err(|e| fail(&e))?;
    }
    // The key file first: settings naming a missing key file leave the link without a key.
    tenant_key::import(&key_plan).map_err(|e| fail(&e))?;
    if let Some(file) = &key_plan.tenant_key_file {
        files::restrict(file).map_err(|e| fail(&e))?;
    }
    let production = install.join(PRODUCTION_FILE);
    let text = files::production_settings(
        files::read_optional(&production)
            .map_err(|e| fail(&e))?
            .as_deref(),
        &AgentKeys {
            cloud_base_url: base.to_string().trim_end_matches('/').to_string(),
            pms_connection_string: pms.clone(),
            identity_file: identity_path.clone(),
            idempotency_file: data.join("agent-idempotency.jsonl"),
            tenant_key_file: key_plan.tenant_key_file.clone(),
            vault_directory: key_plan.vault_directory.clone(),
        },
    )
    .map_err(|e| fail(&e))?;
    files::write_file(&production, text.as_bytes(), true).map_err(|e| fail(&e))?;
    say(io, &format!("{NAME}: wrote {}", production.display()))?;

    // The key before the identity: a refusal here leaves the site without an enrolment.
    let settings = Settings::load(&install).map_err(|e| fail(&e))?;
    let key_file = layout.resolve(&settings.tenant_key_file);
    let vault_dir = layout.resolve(&settings.vault_directory);
    let fingerprint =
        install_key(&settings, &key_plan, &key_file, &vault_dir).map_err(|e| fail(&e))?;

    // The settings now name this file; an identity at an older path is left as it was.
    identity.save(&identity_path).map_err(|e| fail(&e))?;
    files::restrict(&identity_path).map_err(|e| fail(&e))?;
    say(
        io,
        &format!(
            "{NAME}: enrolled agent {} for tenant {}; identity saved to {}",
            identity.agent_id,
            identity.tenant_id,
            identity_path.display()
        ),
    )?;

    write_record(
        &install,
        &identity,
        &key_plan,
        &fingerprint,
        &key_file,
        &vault_dir,
        &legacy,
        &pms,
    )
    .map_err(|e| fail(&e))?;
    say(
        io,
        &format!(
            "{NAME}: tenant master key fingerprint {fingerprint} ({}, {}). Keep a copy of that file offline: without the key the cloud data cannot be read.",
            key_plan.source.as_str(),
            key_file.display()
        ),
    )?;
    let sealed: usize = key_plan.vault.iter().map(|s| s.files).sum();
    if sealed > 0 {
        say(
            io,
            &format!(
                "{NAME}: legal vault {} keeps {sealed} sealed files, all under this key",
                vault_dir.display()
            ),
        )?;
    }
    say(
        io,
        &format!(
            "{NAME}: set modules.{}.enabled = true (install_dir {}) for the module to link up.",
            module::NAME,
            install.display()
        ),
    )
}

/// The key the settings now reach: made only for a `created` plan with an empty vault,
/// otherwise the existing one, which must be the planned one. Answers its fingerprint.
fn install_key(
    settings: &Settings,
    plan: &KeyPlan,
    key_file: &Path,
    vault_dir: &Path,
) -> Result<String, String> {
    let keys = TenantKeyStore::new(&settings.tenant_key, key_file.to_path_buf());
    if plan.source == KeySource::Created {
        if plan.vault.iter().any(|s| s.files > 0 || s.malformed > 0) {
            return Err(
                "the plan found sealed vault files, so it cannot make a new key; stop and report this"
                    .into(),
            );
        }
        keys.create(vault_dir).map_err(|e| e.to_string())?;
    }
    let fingerprint = keys.vault().map_err(|e| e.to_string())?.fingerprint();
    match &plan.fingerprint {
        Some(expected) if *expected != fingerprint => Err(format!(
            "the install holds tenant key {fingerprint} but {expected} was planned; stop the module and report this"
        )),
        _ => Ok(fingerprint),
    }
}

/// The old .NET installs with their settings (an unreadable one stops the enrol: it might
/// hold the tenant key).
fn legacy_installs(args: &Args, io: &mut Io<'_>) -> Result<Vec<(LegacyInstall, Settings)>, String> {
    let found = installs::discover(io.getenv, args.legacy_dir.as_deref())
        .map_err(|e| format!("{NAME}: {e}"))?;
    let mut out = Vec::with_capacity(found.len());
    for install in found {
        let settings = install.settings().map_err(|e| {
            format!("{NAME}: cannot read the old agent settings ({e}); name the folder to follow with --legacy-dir")
        })?;
        say(
            io,
            &format!(
                "{NAME}: old agent install {} ({})",
                install.dir.display(),
                install.found.as_str()
            ),
        )?;
        out.push((install, settings));
    }
    Ok(out)
}

/// The PMS connection string: the environment, stdin, the install's own settings, else the
/// newest old install's; then the sign-in mode the operator chose (`DL_AGENT_ARRIVAL_SQL_AUTH`).
fn connection(
    io: &mut Io<'_>,
    saved: &Settings,
    legacy: &[(LegacyInstall, Settings)],
) -> Result<String, String> {
    let usable = |s: &str| !s.trim().is_empty() && !s.trim().starts_with(PLACEHOLDER);
    let mut pms = secret(io, PMS_ENV, "PMS connection string")?;
    let mut from = "given".to_string();
    if pms.is_empty() && usable(&saved.pms_connection_string) {
        pms = saved.pms_connection_string.trim().to_string();
        from = "saved".into();
    }
    if pms.is_empty()
        && let Some((install, s)) = legacy
            .iter()
            .find(|(_, s)| usable(&s.pms_connection_string))
    {
        pms = s.pms_connection_string.trim().to_string();
        from = format!("old agent {}", install.dir.display());
    }
    if pms.is_empty() {
        return Err(format!(
            "{NAME}: no PMS connection string: set {PMS_ENV} or pass it on stdin"
        ));
    }
    let preselected = sqlauth::detect(&pms);
    let chosen = (io.getenv)(SQL_AUTH_ENV);
    let auth = match chosen.trim() {
        "" => preselected,
        text => SqlAuth::parse(text)
            .ok_or_else(|| format!("{NAME}: {SQL_AUTH_ENV} must be windows or sql, not {text}"))?,
    };
    if auth != preselected || !chosen.trim().is_empty() {
        let user = (io.getenv)(SQL_USER_ENV);
        let password = (io.getenv)(SQL_PASSWORD_ENV);
        let user = Some(user.trim()).filter(|u| !u.is_empty());
        let password = Some(password.as_str()).filter(|p| !p.is_empty());
        pms = sqlauth::with_auth(&pms, auth, user, password).map_err(|e| format!("{NAME}: {e}"))?;
    }
    let login = sqlauth::user(&pms)
        .map(|u| format!(" as {u}"))
        .unwrap_or_default();
    say(
        io,
        &format!(
            "{NAME}: PMS connection from {from}; sql_auth={auth}{login} (preselected {preselected})"
        ),
    )?;
    Ok(pms)
}

/// `data/dl-enroll.json`: what this enrol did, for the self-test and the hub (no secrets).
#[allow(clippy::too_many_arguments)]
fn write_record(
    install: &Path,
    identity: &Identity,
    plan: &KeyPlan,
    fingerprint: &str,
    key_file: &Path,
    vault_dir: &Path,
    legacy: &[(LegacyInstall, Settings)],
    pms: &str,
) -> Result<(), String> {
    let mut key = Map::new();
    key.insert("source", plan.source.as_str());
    key.insert("fingerprint", fingerprint);
    key.insert("file", files::path_text(key_file));
    if let Some(from) = &plan.from {
        key.insert("from", from.to_string());
    }
    let mut vault = Map::new();
    vault.insert("directory", files::path_text(vault_dir));
    vault.insert(
        "sealed_files",
        plan.vault.iter().map(|s| s.files).sum::<usize>() as u64,
    );
    let old: Vec<Value> = legacy
        .iter()
        .map(|(i, _)| {
            let mut m = Map::new();
            m.insert("dir", files::path_text(&i.dir));
            m.insert("found", i.found.as_str());
            Value::Object(m)
        })
        .collect();
    let mut root = Map::new();
    root.insert("enrolled_at", OffsetDateTime::now_utc().to_rfc3339());
    root.insert("agent_id", identity.agent_id.as_str());
    root.insert("tenant_id", identity.tenant_id.as_str());
    root.insert("identity_source", "fresh_enrol");
    root.insert("sql_auth", sqlauth::detect(pms).as_str());
    root.insert("tenant_key", Value::Object(key));
    root.insert("vault", Value::Object(vault));
    root.insert("legacy_installs", Value::Array(old));
    let mut text = dl_json::to_string_pretty(&Value::Object(root));
    text.push('\n');
    files::write_file(&install.join(files::ENROLL_RECORD), text.as_bytes(), true)
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
