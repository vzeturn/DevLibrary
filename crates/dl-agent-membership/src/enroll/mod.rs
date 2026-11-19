//! Enrollment: the part of `EnrollCommand` an install needs. It spends the one-time hotel
//! code at `POST api/agent/enroll`, checks the hub address the answer names
//! (`EnrollHubUrlPolicy`) and writes `config.dpapi` the way the .NET agent does. The code
//! and the SMILE connection string come from the environment or stdin, never from argv,
//! and are never printed.

pub mod tool;

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dl_http::{ClientOptions, Request, Scheme, Url};

use crate::config::{AgentConfig, ConfigStore, FILE_NAME, Sealer};
use crate::hub::base_address;
use crate::module::AGENT_VERSION;
use crate::smile::TdsSmile;
use crate::wire::EnrollRequest;
use crate::wire::parse::{EnrollResponse, enroll_response};

/// Writes a progress line; a console that cannot be written to is logged, not fatal (the
/// enrollment itself still succeeds or fails on its own).
macro_rules! say {
    ($out:expr, $($arg:tt)*) => {{
        if let Err(e) = write!($out, $($arg)*).and_then(|()| $out.flush()) {
            crate::log::warn(
                "enroll output not written",
                &[("error", crate::log::V::S(&e.kind().to_string()))],
            );
        }
    }};
}

/// The environment variable with the one-time enrollment code.
pub const TOKEN_ENV: &str = "DL_AGENT_MEMBERSHIP_TOKEN";
/// The environment variable with the SMILE connection string.
pub const SMILE_ENV: &str = "DL_AGENT_MEMBERSHIP_SMILE";

const ENROLL_TIMEOUT: Duration = Duration::from_secs(60);

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollArgs {
    /// `--hub-url`: the hub origin (https; http only to loopback).
    pub hub_url: String,
    /// Where `config.dpapi` goes.
    pub config_dir: PathBuf,
    /// `--force`: enroll again over a readable `config.dpapi`.
    pub force: bool,
}

/// Whether `url` may carry the code or the token: https, or http to a loopback host.
fn secure(url: &Url) -> bool {
    match url.scheme() {
        Scheme::Https => true,
        Scheme::Http => is_loopback(url.host()),
    }
}

fn is_loopback(host: &str) -> bool {
    host == "localhost" || host == "::1" || host.starts_with("127.")
}

fn authority(url: &Url) -> String {
    let scheme = match url.scheme() {
        Scheme::Https => "https",
        Scheme::Http => "http",
    };
    format!("{scheme}://{}:{}", url.host(), url.port())
}

/// `EnrollHubUrlPolicy.Validate`: the address the hub returned must be absolute, https (or
/// http to loopback) and on the same origin (scheme, host, port) as the one called; the
/// path may differ.
pub fn check_hub_url(requested: &str, returned: &str) -> Result<(), String> {
    let returned = returned.trim();
    let r = match Url::parse(returned) {
        Ok(u) if !returned.is_empty() => u,
        _ => return Err("the hub returned an empty or relative hub address".into()),
    };
    if !secure(&r) {
        return Err(format!(
            "the hub returned an address without https ('{}')",
            authority(&r)
        ));
    }
    let q = Url::parse(requested.trim())
        .map_err(|_| "the --hub-url is not an absolute URL".to_owned())?;
    if q.scheme() == r.scheme() && q.host() == r.host() && q.port() == r.port() {
        Ok(())
    } else {
        Err(format!(
            "the hub returned another origin than the one called ('{}' is not '{}')",
            authority(&r),
            authority(&q)
        ))
    }
}

/// The machine name sent to the hub: `COMPUTERNAME`, `HOSTNAME`, `/etc/hostname`.
fn machine_name(env: &dyn Fn(&str) -> Option<String>) -> String {
    env("COMPUTERNAME")
        .or_else(|| env("HOSTNAME"))
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_owned())
        })
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "unknown-host".into())
}

/// The code from `DL_AGENT_MEMBERSHIP_TOKEN`, else the first line of stdin (BOM trimmed).
fn read_code(
    input: Option<&mut dyn BufRead>,
    out: &mut dyn Write,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<String, String> {
    if let Some(c) = env(TOKEN_ENV)
        .map(|c| c.trim().to_owned())
        .filter(|c| !c.is_empty())
    {
        return Ok(c);
    }
    let Some(input) = input else {
        return Err(format!(
            "no enrollment code: set {TOKEN_ENV} or pipe it on stdin"
        ));
    };
    say!(out, "Enrollment code: ");
    let mut line = String::new();
    input
        .read_line(&mut line)
        .map_err(|e| format!("read the enrollment code from stdin: {e}"))?;
    let code = line.trim_start_matches('\u{feff}').trim().to_owned();
    say!(out, "\n");
    if code.is_empty() {
        return Err(format!(
            "no enrollment code: set {TOKEN_ENV} or pipe it on stdin"
        ));
    }
    Ok(code)
}

/// `POST api/agent/enroll` (no auth).
fn post_enroll(hub_url: &str, req: &EnrollRequest) -> Result<EnrollResponse, String> {
    let url = base_address(hub_url)
        .and_then(|b| {
            b.join("api/agent/enroll")
                .map_err(|e| crate::hub::HubError::Transport(e.to_string()))
        })
        .map_err(|e| format!("hub address: {e}"))?;
    let request = Request::post(url, req.to_j().to_json().into_bytes())
        .with_header("Content-Type", "application/json; charset=utf-8")
        .map_err(|e| format!("enroll request: {e}"))?;
    let http = dl_http::Client::with_options(ClientOptions {
        timeout: Some(ENROLL_TIMEOUT),
        user_agent: Some(AGENT_VERSION.to_owned()),
        ..ClientOptions::default()
    });
    let resp = http
        .request(&request)
        .map_err(|e| format!("hub unreachable: {e}"))?;
    if !resp.status.is_success() {
        return Err(format!(
            "the hub refused the enrollment code (HTTP {}); ask for a new code",
            resp.status.as_u16()
        ));
    }
    if resp.body.trim_ascii().is_empty() {
        return Err("the hub answered the enrollment with an empty body".into());
    }
    enroll_response(&resp.body)
        .map_err(|e| format!("the hub's enrollment answer is unreadable: {e}"))
}

/// The tool body with its environment injected.
pub fn run_enroll(
    args: &EnrollArgs,
    sealer: Arc<dyn Sealer>,
    input: Option<&mut dyn BufRead>,
    out: &mut dyn Write,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<(), String> {
    let requested = Url::parse(args.hub_url.trim())
        .map_err(|_| format!("--hub-url {:?} is not an absolute URL", args.hub_url.trim()))?;
    if !secure(&requested) {
        return Err("--hub-url must use https (http only to this machine)".into());
    }
    let store = ConfigStore::new(args.config_dir.clone(), sealer);
    let previous = match store.load() {
        Ok(Some(cfg)) if !args.force => {
            say!(
                out,
                "already enrolled for hotel {} ({} kept); pass --force to enroll again\n",
                cfg.hotel_id,
                store.path().display()
            );
            return Ok(());
        }
        Ok(found) => found,
        Err(e) if !args.force => {
            return Err(format!(
                "{}: {e} (fix or remove it, or pass --force to enroll again)",
                store.path().display()
            ));
        }
        Err(e) => {
            say!(
                out,
                "existing {FILE_NAME} unreadable ({e}); enrolling over it\n"
            );
            None
        }
    };
    let code = read_code(input, out, env)?;
    let smile = env(SMILE_ENV)
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .or_else(|| previous.as_ref().map(|p| p.smile_connection_string.clone()))
        .unwrap_or_default();
    let target = TdsSmile::new(&smile).ok().map(|s| s.target());
    match &target {
        Some(t) => say!(out, "SMILE: {} / {}\n", t.data_source, t.database),
        None => say!(
            out,
            "no usable SMILE connection string (set {SMILE_ENV}); the module will report SMILE down\n"
        ),
    }
    let req = EnrollRequest {
        code,
        machine_name: machine_name(env),
        agent_version: AGENT_VERSION.to_owned(),
        smile_data_source: target.as_ref().map(|t| t.data_source.clone()),
        smile_database: target.as_ref().map(|t| t.database.clone()),
    };
    let resp = post_enroll(&args.hub_url, &req)?;
    check_hub_url(&args.hub_url, &resp.hub_base_url)
        .map_err(|e| format!("enrollment not saved: {e}"))?;
    let cfg = AgentConfig {
        hub_base_url: resp.hub_base_url.clone(),
        agent_token: resp.agent_token.clone(),
        hotel_id: resp.hotel_id,
        smile_connection_string: smile,
        origin_code: resp.origin_code.clone(),
    };
    let backup = store
        .save(&cfg)
        .map_err(|e| format!("{}: {e}", store.path().display()))?;
    if let Some(b) = backup {
        say!(out, "previous {FILE_NAME} kept as {}\n", b.display());
    }
    match resp.enrollment_id {
        Some(id) => say!(
            out,
            "enrolled for hotel {} (enrollment {id})\n",
            resp.hotel_id
        ),
        None => say!(out, "enrolled for hotel {}\n", resp.hotel_id),
    }
    if resp.machine_changed {
        say!(
            out,
            "warning: this hotel was enrolled on {} before; that agent's token no longer works\n",
            resp.previous_machine_name
                .as_deref()
                .unwrap_or("another machine")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_url_policy_wants_https_and_the_same_origin() {
        let ok = |a, b| check_hub_url(a, b).is_ok();
        assert!(ok("https://hub.example.vn", "https://hub.example.vn/api"));
        assert!(ok(
            "https://hub.example.vn/",
            "https://HUB.example.vn:443/x/"
        ));
        assert!(ok("http://127.0.0.1:5000", "http://127.0.0.1:5000/"));
        assert!(!ok("https://hub.example.vn", "http://hub.example.vn"));
        assert!(!ok("https://hub.example.vn", "https://evil.example.vn"));
        assert!(!ok("https://hub.example.vn", "https://hub.example.vn:8443"));
        assert!(!ok("https://hub.example.vn", ""));
        assert!(!ok("https://hub.example.vn", "/api"));
    }

    #[test]
    fn the_code_comes_from_the_environment_before_stdin() {
        let mut out = Vec::new();
        let env = |k: &str| (k == TOKEN_ENV).then(|| " CODE-1 ".to_owned());
        let mut stdin: &[u8] = b"OTHER\n";
        assert_eq!(
            read_code(Some(&mut stdin), &mut out, &env).unwrap(),
            "CODE-1"
        );
        let none = |_: &str| None;
        let mut stdin: &[u8] = "\u{feff}CODE-2\r\n".as_bytes();
        assert_eq!(
            read_code(Some(&mut stdin), &mut out, &none).unwrap(),
            "CODE-2"
        );
        let mut empty: &[u8] = b"";
        assert!(read_code(Some(&mut empty), &mut out, &none).is_err());
    }
}
