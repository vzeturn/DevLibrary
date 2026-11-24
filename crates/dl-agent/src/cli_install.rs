//! `install`, `update` and `verify-self`: the agent's side of the one-line installer (README
//! "Installing on Windows") and the local update commands (PROTOCOL.md §6.5).
//!
//! `install` has three modes, chosen by [`mode`]:
//! - machine setup: `--hub` given (the one-line installer hands off this way) or, on Windows, a
//!   binary run from anywhere but the install folder. It copies itself to Program Files and
//!   does the SQL login, enrolment, setup, replaced agents, service, watchdog and link wait
//!   ([`crate::cli_install_win`]);
//! - `install stable|canary|x.y.z` from the installed binary: follows a channel (and updates
//!   now) or pins a version (installing it first when it is not the running one);
//! - plain `install` from the installed binary: (re)registers the service, as before.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dl_cli::{Arg, Command, Matches, ValueSource};

use crate::cli::{App, EXIT_CONFIG};
use crate::cli_install_sql::SqlOpts;
use crate::hub::{HttpTransport, HubClient, Transport};
use crate::identity::Identity;
use crate::manifest::{Manifest, TrustedKeys};
use crate::{Error, Version, current_target, now, optout};

/// A step failed (SQL login, enrolment, setup, update).
pub const EXIT_FAILED: i32 = 1;
/// The build does not match its signed manifest (`EX_DATAERR`).
pub const EXIT_VERIFY: i32 = 65;
/// The hub or the agent service cannot be reached (`EX_UNAVAILABLE`).
pub const EXIT_UNAVAILABLE: i32 = 69;
/// Not run as Administrator (`EX_NOPERM`).
pub const EXIT_NOT_ADMIN: i32 = 77;
/// How long `dl-agent update` waits for the service's answer.
const UPDATE_WAIT: Duration = Duration::from_secs(300);
/// How long it then waits for the service to come back on the new version.
const RESTART_WAIT: Duration = Duration::from_secs(120);
/// A heartbeat file older than this means the service is not running.
const BEAT_FRESH_SECONDS: u64 = 90;

/// What `install` was asked to land on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Follow the stable channel (`latest` is an alias, as in Claude Code's installer).
    Stable,
    /// Follow the canary channel.
    Canary,
    /// Hold this version.
    Version(Version),
    /// Drop the local choice and the pin: follow the channel the hub assigns.
    Auto,
}

impl Target {
    /// Parses `stable`, `latest`, `canary`, `auto` or `x.y.z`.
    pub fn parse(s: &str) -> Result<Target, Error> {
        match s.trim().to_ascii_lowercase().as_str() {
            "stable" | "latest" => Ok(Target::Stable),
            "canary" => Ok(Target::Canary),
            "auto" => Ok(Target::Auto),
            v => v.parse::<Version>().map(Target::Version).map_err(|_| {
                Error::Config(format!(
                    "{s:?} is not stable, canary, auto or a version x.y.z"
                ))
            }),
        }
    }

    /// The channel, for a channel target.
    pub fn channel(&self) -> Option<&'static str> {
        match self {
            Target::Stable => Some("stable"),
            Target::Canary => Some("canary"),
            Target::Version(_) | Target::Auto => None,
        }
    }
}

/// How `install` runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The whole machine setup ([`crate::cli_install_win::machine`]).
    Machine,
    /// Channel switch or version pin on an installed machine.
    Switch(Target),
    /// Register the service for this binary (the old `install`).
    Service,
}

/// Picks the mode: `--hub` or (Windows) a binary outside the install folder is a machine setup;
/// otherwise a target switches, and nothing registers the service.
pub fn mode(
    windows: bool,
    hub_given: bool,
    from_install_dir: bool,
    target: Option<Target>,
) -> Mode {
    if hub_given || (windows && !from_install_dir) {
        Mode::Machine
    } else if let Some(t) = target {
        Mode::Switch(t)
    } else {
        Mode::Service
    }
}

/// A failed step: what went wrong, the next step to take, the exit code.
#[derive(Debug)]
pub struct Failure {
    /// What went wrong.
    pub message: String,
    /// What to do about it.
    pub next: String,
    /// Process exit code.
    pub code: i32,
}

impl Failure {
    /// A failure with its next step.
    pub fn new(message: impl Into<String>, next: impl Into<String>, code: i32) -> Failure {
        Failure {
            message: message.into(),
            next: next.into(),
            code,
        }
    }

    /// Prints `FAILED: …` and `Next: …` on stderr; returns the exit code.
    pub fn print(&self) -> i32 {
        eprintln!("FAILED: {}", self.message);
        if !self.next.is_empty() {
            eprintln!("Next: {}", self.next);
        }
        self.code
    }
}

/// What the machine setup was asked to do.
#[derive(Clone, Debug, Default)]
pub struct MachineOpts {
    /// Hub URLs, first preferred (with the installer's fallbacks).
    pub hubs: Vec<String>,
    /// Enrolment code (`DL_AGENT_ENROLL_CODE`), asked when missing and needed.
    pub code: Option<String>,
    /// Channel to follow or version to pin.
    pub target: Option<Target>,
    /// The SQL login step.
    pub sql: SqlOpts,
    /// A module setup command run after enrolment (`arrival-enroll`, ...).
    pub setup: Option<String>,
    /// Service name patterns of older agents to take over.
    pub replace: Vec<String>,
    /// Replace the identity even when enrolled.
    pub reenroll: bool,
    /// Add the Windows Defender exclusion.
    pub defender: bool,
    /// Seconds to wait for the first heartbeat.
    pub link_timeout: u64,
    /// A person can answer prompts.
    pub interactive: bool,
}

impl MachineOpts {
    /// From the parsed `install` arguments.
    pub fn from_matches(m: &Matches, target: Option<Target>) -> Result<MachineOpts, Error> {
        let opt = |k: &str| {
            m.value(k)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(String::from)
        };
        let link_timeout = m
            .value("link-timeout")
            .unwrap_or("120")
            .parse()
            .map_err(|_| Error::Config("--link-timeout must be a number of seconds".into()))?;
        Ok(MachineOpts {
            hubs: split_hubs(m.value("hub").unwrap_or("")),
            code: opt("code"),
            target,
            sql: SqlOpts {
                name: opt("sql-name"),
                auth: opt("sql-auth"),
                server: opt("sql-server"),
                user: opt("sql-user"),
                prompt: m.flag("sql-prompt"),
                skip: m.flag("skip-sql"),
            },
            setup: opt("setup"),
            replace: m
                .values("replace")
                .iter()
                .filter(|p| !p.trim().is_empty())
                .cloned()
                .collect(),
            reenroll: m.flag("reenroll"),
            defender: !m.flag("no-defender"),
            link_timeout,
            interactive: std::io::stdin().is_terminal(),
        })
    }
}

/// `https://a, https://b/` → `["https://a", "https://b"]`.
pub fn split_hubs(csv: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for h in csv.split(',').map(|s| s.trim().trim_end_matches('/')) {
        if !h.is_empty() && !out.iter().any(|o| o == h) {
            out.push(h.to_string());
        }
    }
    out
}

/// The `install` grammar.
pub fn install_grammar(data: Arg) -> Command {
    Command::new("install")
        .about("Installs, repairs or switches the agent: with --hub the whole machine setup (the one-line installer), stable|canary|auto|x.y.z switches the channel or pins a version, nothing (re)registers the service")
        .arg(Arg::positional("target").help("stable or canary to follow, x.y.z to pin, auto for the hub's channel"))
        .arg(Arg::option("hub").help("Hub URL(s), comma separated, first preferred (machine setup)"))
        .arg(Arg::option("code").env("DL_AGENT_ENROLL_CODE").help("One-time enrolment code (asked when needed and missing)"))
        .arg(Arg::option("sql-auth").env("DL_AGENT_SQL_AUTH").choices(["windows", "sql"]).help("SQL login without asking: windows or sql (password from DL_AGENT_SQL_PASSWORD or a hidden prompt)"))
        .arg(Arg::option("sql-server").env("DL_AGENT_SQL_SERVER").help("SQL Server for --sql-auth (default: saved, local, .)"))
        .arg(Arg::option("sql-user").env("DL_AGENT_SQL_USER").help("SQL login name for --sql-auth sql"))
        .arg(Arg::flag("sql-prompt").help("Ask for the SQL login even with no SQL Server on this machine"))
        .arg(Arg::flag("skip-sql").help("Leave the SQL login as it is"))
        .arg(Arg::option("sql-name").help("Declare one database with the connection in DL_AGENT_SQL_CONNECTION instead"))
        .arg(Arg::option("setup").help("Module setup command to run after enrolment (its secrets in DL_AGENT_SETUP_ARGS)"))
        .arg(Arg::option("replace").multiple().help("Service name pattern (* and ?) of an older agent to stop and delete; its files stay"))
        .arg(Arg::flag("reenroll").help("Replace this machine's identity with a new enrolment"))
        .arg(Arg::flag("no-defender").help("Do not add the Windows Defender exclusion"))
        .arg(Arg::option("link-timeout").default("120").help("Seconds to wait for the first heartbeat"))
        .arg(data)
}

/// The `update` grammar.
pub fn update_grammar(data: Arg) -> Command {
    Command::new("update")
        .about("Checks the agent's channel and installs a newer signed build now (the running service does it)")
        .arg(Arg::option("wait").default("300").help("Seconds to wait for the service's answer"))
        .arg(data)
}

/// The `verify-self` grammar.
pub fn verify_grammar(data: Arg) -> Command {
    Command::new("verify-self")
        .about("Checks a build against its signed release manifest: Ed25519 signature (keys built into this binary), product, size and SHA-256")
        .arg(Arg::option("manifest").required().help("The release manifest file"))
        .arg(Arg::option("file").help("The build to check (default: this executable)"))
        .arg(Arg::option("target").help("Build target such as windows-x86_64 (default: this binary's)"))
        .arg(Arg::option("product").help("Release line (default: this binary's)"))
        .arg(data)
}

/// Runs `install`; returns the exit code.
pub fn main_install(app: &App, dir: &Path, m: &Matches) -> i32 {
    let target = match m.value("target").map(Target::parse).transpose() {
        Ok(t) => t,
        Err(e) => {
            let next = format!("{} install stable|canary|auto|x.y.z", app.service);
            return Failure::new(e.to_string(), next, EXIT_CONFIG).print();
        }
    };
    let hub_given = m.source("hub") == Some(ValueSource::CommandLine);
    let from_dir = running_from(&install_exe(&app.service, env_var));
    match mode(cfg!(windows), hub_given, from_dir, target.clone()) {
        Mode::Machine => match MachineOpts::from_matches(m, target) {
            Ok(o) => crate::cli_install_win::machine(app, dir, &o),
            Err(e) => {
                Failure::new(e.to_string(), "fix the option and run again", EXIT_CONFIG).print()
            }
        },
        Mode::Switch(t) => switch(app, dir, &t).unwrap_or_else(|f| f.print()),
        Mode::Service => match crate::cli::install(app, dir) {
            Ok(()) => {
                announce_selftest();
                0
            }
            Err(e) => Failure::new(
                e.to_string(),
                "run as Administrator; enrol first if it says so",
                EXIT_CONFIG,
            )
            .print(),
        },
    }
}

pub(crate) fn env_var(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

/// `%ProgramW6432%` (or `%ProgramFiles%`) `\DevLibrary\<service>\<service>.exe`: where the
/// one-line installer has always put the binary.
pub fn install_exe(service: &str, env: impl Fn(&str) -> Option<String>) -> PathBuf {
    let pf = env("ProgramW6432")
        .or_else(|| env("ProgramFiles"))
        .unwrap_or_else(|| r"C:\Program Files".into());
    PathBuf::from(pf)
        .join("DevLibrary")
        .join(service)
        .join(format!("{service}.exe"))
}

/// Whether this process runs `exe` (paths compared without case, either slash).
pub fn running_from(exe: &Path) -> bool {
    std::env::current_exe().is_ok_and(|me| same_path(&me, exe))
}

/// Windows path equality: case and slash direction ignored.
pub fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        p.display()
            .to_string()
            .replace('/', "\\")
            .to_ascii_lowercase()
    };
    norm(a) == norm(b)
}

/// `install stable|canary|auto|x.y.z` on an installed machine.
/// The new service runs the self-test on its first start (`selftest::post_install`).
fn announce_selftest() {
    println!("The agent runs a self-test on its first start and sends the result to the hub");
}

fn switch(app: &App, dir: &Path, t: &Target) -> Result<i32, Failure> {
    if !Identity::exists(dir) {
        return Err(Failure::new(
            format!("{} is not enrolled", dir.display()),
            "run the one-line installer first (irm <hub>/install.ps1 | iex)",
            EXIT_CONFIG,
        ));
    }
    let admin = || format!("run it as Administrator ({})", dir.display());
    match t {
        Target::Stable | Target::Canary => {
            let c = t.channel().unwrap_or("stable");
            let pin = optout::pin(dir);
            optout::set_channel(dir, Some(c))
                .map_err(|e| Failure::new(e.to_string(), admin(), EXIT_CONFIG))?;
            optout::set_pin(dir, None)
                .map_err(|e| Failure::new(e.to_string(), admin(), EXIT_CONFIG))?;
            match pin {
                Some(v) => println!("Following the {c} channel (pin {v} removed)"),
                None => println!("Following the {c} channel"),
            }
            update(app, dir, UPDATE_WAIT)
        }
        Target::Version(v) if *v == app.version => {
            optout::set_pin(dir, Some(v))
                .map_err(|e| Failure::new(e.to_string(), admin(), EXIT_CONFIG))?;
            println!(
                "Pinned at {v}: automatic updates hold until `{} install stable|canary|auto`",
                app.service
            );
            Ok(0)
        }
        Target::Version(v) => install_version(app, dir, v),
        Target::Auto => {
            optout::set_channel(dir, None)
                .map_err(|e| Failure::new(e.to_string(), admin(), EXIT_CONFIG))?;
            optout::set_pin(dir, None)
                .map_err(|e| Failure::new(e.to_string(), admin(), EXIT_CONFIG))?;
            println!("Following the channel the hub assigns");
            update(app, dir, UPDATE_WAIT)
        }
    }
}

/// Downloads `version` (signed manifest + artifact, both checked) and hands off to its own
/// machine setup, which installs it and pins it.
fn install_version(app: &App, dir: &Path, version: &Version) -> Result<i32, Failure> {
    if !cfg!(windows) {
        return Err(Failure::new(
            "installing another version is done on Windows",
            "use the hub's release controls on this platform",
            EXIT_CONFIG,
        ));
    }
    let id = Identity::load(dir).map_err(|e| {
        Failure::new(
            e.to_string(),
            "run the one-line installer again",
            EXIT_CONFIG,
        )
    })?;
    let hubs = id.hub_urls.clone();
    let transport = HttpTransport::new();
    let unreachable = |e: Error| {
        Failure::new(
            format!("hub unreachable or blocked by a proxy: {e}"),
            "check the network, then run again",
            EXIT_UNAVAILABLE,
        )
    };
    let text = fetch_manifest(&transport, &hubs, &app.product, version)
        .map_err(unreachable)?
        .ok_or_else(|| {
            Failure::new(
                format!(
                    "{} {version} is not published for this machine",
                    app.product
                ),
                "pick a version the hub lists",
                EXIT_CONFIG,
            )
        })?;
    let target = current_target();
    let m = Manifest::parse(&text)
        .map_err(|e| Failure::new(e.to_string(), "run again", EXIT_VERIFY))?;
    let artifact = m.artifact_for(&target).cloned().ok_or_else(|| {
        Failure::new(
            format!("{version} has no {target} build"),
            "pick another version",
            EXIT_CONFIG,
        )
    })?;
    println!(
        "Downloading {} {version} ({} bytes)",
        app.product, artifact.size
    );
    let bytes = HubClient::new(Box::new(HttpTransport::new()), id)
        .download(&artifact.url)
        .map_err(unreachable)?;
    verify_build(
        &text,
        &bytes,
        &target,
        &app.product,
        &crate::agent::release_keys(),
    )
    .map_err(|e| {
        Failure::new(
            format!("the download failed its check: {e}"),
            "possible tampering or a proxy rewriting downloads; run again from another network",
            EXIT_VERIFY,
        )
    })?;
    let tmp = std::env::temp_dir().join(format!(
        "{}-{version}-{}.exe",
        app.service,
        std::process::id()
    ));
    crate::store::write_bytes(&tmp, &bytes)
        .map_err(|e| Failure::new(e.to_string(), "free disk space in %TEMP%", EXIT_CONFIG))?;
    let v = version.to_string();
    let data = dir.display().to_string();
    let status = std::process::Command::new(&tmp)
        .args([
            "install",
            v.as_str(),
            "--hub",
            &hubs.join(","),
            "--skip-sql",
            "--data",
            &data,
        ])
        .status();
    if let Err(e) = std::fs::remove_file(&tmp) {
        eprintln!("Warning: cannot remove {}: {e}", tmp.display());
    }
    match status {
        Ok(s) => Ok(s.code().unwrap_or(EXIT_FAILED)),
        Err(e) => Err(Failure::new(
            format!("cannot run {}: {e}", tmp.display()),
            "the antivirus may block it; allow it and run again",
            EXIT_FAILED,
        )),
    }
}

/// `GET <hub>/install/manifest/<product>/<channel>/<version>` on each hub, stable first:
/// the signed manifest text, `None` when no channel has that version.
fn fetch_manifest(
    t: &dyn Transport,
    hubs: &[String],
    product: &str,
    version: &Version,
) -> Result<Option<String>, Error> {
    let mut last = None;
    for hub in hubs {
        for channel in ["stable", "canary"] {
            let url = format!(
                "{hub}/install/manifest/{}/{channel}/{version}",
                dl_http::encode_path_segment(product)
            );
            match t.send(
                "GET",
                &url,
                &[("Cache-Control", "no-cache".to_string())],
                b"",
            ) {
                Ok((200, body)) => {
                    let text = String::from_utf8(body)
                        .map_err(|_| Error::Manifest("manifest is not UTF-8".into()))?;
                    if !text.trim_start().starts_with('{') {
                        return Err(Error::Manifest(
                            "the answer is not a manifest (HTML from a proxy?)".into(),
                        ));
                    }
                    return Ok(Some(text));
                }
                Ok((404, _)) => {}
                Ok((code, _)) => {
                    last = Some(Error::Status {
                        code,
                        body: String::new(),
                    })
                }
                Err(e) => last = Some(e),
            }
        }
        if last.is_none() {
            return Ok(None);
        }
    }
    last.map_or(Ok(None), Err)
}

/// Checks `bytes` against the signed manifest `text`: signature by a trusted release key,
/// product, then the `target` artifact's size and SHA-256.
pub fn verify_build(
    text: &str,
    bytes: &[u8],
    target: &str,
    product: &str,
    keys: &TrustedKeys,
) -> Result<Manifest, Error> {
    let m = Manifest::parse(text)?;
    m.verify(keys)?;
    if m.product != product {
        return Err(Error::Manifest(format!(
            "manifest is for {}, not {product}",
            m.product
        )));
    }
    let a = m
        .artifact_for(target)
        .ok_or_else(|| Error::Manifest(format!("manifest has no {target} build")))?;
    crate::update::verify_artifact(bytes, a)?;
    Ok(m)
}

/// Runs `verify-self`: 0 verified, 65 not, 78 bad input.
pub fn main_verify(app: &App, m: &Matches) -> i32 {
    let file = match m.value("file") {
        Some(f) => PathBuf::from(f),
        None => match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                return Failure::new(
                    format!("cannot locate this executable: {e}"),
                    "pass --file",
                    EXIT_CONFIG,
                )
                .print();
            }
        },
    };
    let manifest = m.value("manifest").unwrap_or_default();
    let read = |p: &Path| {
        std::fs::read(p).map_err(|e| {
            Failure::new(
                format!("cannot read {}: {e}", p.display()),
                "check the path",
                EXIT_CONFIG,
            )
        })
    };
    let (text, bytes) = match (read(Path::new(manifest)), read(&file)) {
        (Ok(t), Ok(b)) => (t, b),
        (Err(f), _) | (_, Err(f)) => return f.print(),
    };
    let target = m.value("target").map_or_else(current_target, String::from);
    let product = m.value("product").unwrap_or(&app.product);
    let text = String::from_utf8_lossy(&text);
    match verify_build(&text, &bytes, &target, product, &crate::agent::release_keys()) {
        Ok(v) => {
            println!("verified {} {} ({}, {target}, release key {})", v.product, v.version, v.channel.as_str(), v.key_id);
            0
        }
        Err(e) => Failure::new(
            format!("{} does not match its signed release: {e}", file.display()),
            "possible tampering or a proxy rewriting downloads; do not install it, run again from another network",
            EXIT_VERIFY,
        )
        .print(),
    }
}

/// Runs `update`: asks the running service for an update check now (the signed `update_now`
/// path) and prints from → to.
pub fn main_update(app: &App, dir: &Path, m: &Matches) -> i32 {
    let wait = match m.value("wait").unwrap_or("300").parse::<u64>() {
        Ok(s) => Duration::from_secs(s),
        Err(_) => {
            return Failure::new("--wait must be a number of seconds", "", EXIT_CONFIG).print();
        }
    };
    update(app, dir, wait).unwrap_or_else(|f| f.print())
}

fn update(app: &App, dir: &Path, wait: Duration) -> Result<i32, Failure> {
    if let Some(p) = optout::pin(dir) {
        return Err(Failure::new(
            format!("pinned at {p}"),
            format!(
                "{} install stable|canary|auto follows a channel again",
                app.service
            ),
            EXIT_CONFIG,
        ));
    }
    let beat = fresh_beat(dir, now()).ok_or_else(|| {
        Failure::new(
            format!("the {} service is not running", app.service),
            format!(
                "start it (sc start {}), or run the one-line installer to repair it",
                app.service
            ),
            EXIT_UNAVAILABLE,
        )
    })?;
    let asked = now();
    optout::request_update(dir, asked).map_err(|e| {
        Failure::new(
            e.to_string(),
            format!("run it as Administrator ({})", dir.display()),
            EXIT_CONFIG,
        )
    })?;
    let channel = optout::channel(dir).unwrap_or_else(|| beat.channel.clone());
    println!(
        "Checking for updates ({} {}, channel {channel})",
        app.product, beat.version
    );
    let r = wait_result(dir, asked, wait)?;
    if !r.ok {
        return Err(Failure::new(
            format!("update failed: {}", r.output),
            "see logs\\agent.log; the hub may pause this release or hold this site",
            EXIT_FAILED,
        ));
    }
    if !r.installed {
        println!(
            "{} is up to date ({}, channel {}): {}",
            app.display, r.from, r.channel, r.output
        );
        return Ok(0);
    }
    println!("Installing {} -> {}", r.from, r.to);
    announce_selftest();
    if wait_version(dir, &r.to, RESTART_WAIT) {
        println!("Updated {} -> {}", r.from, r.to);
    } else {
        println!(
            "Installed {}; the service is still restarting (status: {} status)",
            r.to, app.service
        );
    }
    Ok(0)
}

fn fresh_beat(dir: &Path, now: u64) -> Option<crate::watchdog::Beat> {
    crate::watchdog::read_beat(dir).filter(|b| now.saturating_sub(b.at) <= BEAT_FRESH_SECONDS)
}

fn wait_result(dir: &Path, asked: u64, wait: Duration) -> Result<optout::UpdateResult, Failure> {
    let end = Instant::now() + wait;
    while Instant::now() < end {
        match optout::read_update_result(dir) {
            Ok(Some(r)) if r.at >= asked => return Ok(r),
            Ok(_) => {}
            Err(e) => return Err(Failure::new(e.to_string(), "run again", EXIT_FAILED)),
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(Failure::new(
        format!("no answer from the service in {} s", wait.as_secs()),
        "check that it runs (sc query) and see logs\\agent.log",
        EXIT_UNAVAILABLE,
    ))
}

fn wait_version(dir: &Path, version: &str, wait: Duration) -> bool {
    let end = Instant::now() + wait;
    while Instant::now() < end {
        if fresh_beat(dir, now()).is_some_and(|b| b.version == version) {
            return true;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    false
}

#[cfg(test)]
#[path = "cli_install_tests.rs"]
mod tests;
