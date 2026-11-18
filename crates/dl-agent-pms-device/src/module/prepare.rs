//! Reads the agent settings the way `SmilePmsDeviceAgent` did (appsettings files, `DOTNET_*`
//! and `Agent__*` variables) plus the module's overrides, validates them and checks that
//! the token file can be read. Nothing here returns or logs the token.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::agent::TokenFile;
use crate::config::{self, AgentOptions, HostInputs, LoadedConfiguration};

use super::settings::Settings;

/// Settings the worker can start with.
#[derive(Debug, Clone)]
pub struct Ready {
    /// The bound and validated `Agent` section (`RunOnce` forced off).
    pub options: AgentOptions,
    /// `appsettings.{environment}.json` was read too.
    pub environment: String,
    /// The folder the appsettings files were read from.
    pub content_root: PathBuf,
}

impl Ready {
    /// The files whose change restarts the worker with fresh settings: both appsettings
    /// files and the token file (the worker caches the token after its first read).
    pub fn watched(&self) -> Vec<PathBuf> {
        vec![
            self.content_root.join("appsettings.json"),
            self.content_root
                .join(format!("appsettings.{}.json", self.environment)),
            PathBuf::from(&self.options.token_file),
        ]
    }
}

/// The outcome of [`prepare`].
#[derive(Debug, Clone)]
pub enum Prepared {
    /// Start the worker.
    Ready(Box<Ready>),
    /// Stay degraded with this detail (no secret in it) and look again later.
    Waiting(String),
}

impl Prepared {
    /// The waiting detail; `None` when ready.
    pub fn detail(&self) -> Option<&str> {
        match self {
            Prepared::Ready(_) => None,
            Prepared::Waiting(d) => Some(d),
        }
    }
}

/// Loads, merges, binds and validates the settings, then reads the token file once.
///
/// `appsettings_dir` plays the content root of the old service (its install folder) and
/// `env` the process environment (`DOTNET_ENVIRONMENT`, `Agent__TokenFile` …).
pub fn prepare(settings: &Settings, module_dir: &Path, env: &[(String, String)]) -> Prepared {
    let ready = match load(settings, module_dir, env) {
        Ok(r) => r,
        Err(detail) => return Prepared::Waiting(detail),
    };
    if let Err(e) = TokenFile::new(ready.options.token_file.clone()).read() {
        // The error names the path, never the content.
        return Prepared::Waiting(format!("token not readable: {e}"));
    }
    Prepared::Ready(Box::new(ready))
}

fn load(settings: &Settings, module_dir: &Path, env: &[(String, String)]) -> Result<Ready, String> {
    let dir = settings.appsettings_dir(module_dir);
    let inputs = HostInputs {
        args: Vec::new(),
        env: env.to_vec(),
        current_dir: dir.clone(),
        base_dir: dir,
        system_dir: PathBuf::new(),
    };
    let invalid = |e: config::DotnetError| format!("config invalid: {}", one_line(&e.to_string()));
    let LoadedConfiguration {
        mut configuration,
        environment,
        content_root,
    } = config::load(&inputs).map_err(invalid)?;
    configuration.merge(&settings.override_configuration().map_err(invalid)?);
    let mut options = config::bind_agent_options(&configuration).map_err(invalid)?;
    // A smoke-run flag would stop the module after one empty poll; dl-agent runs it for good.
    options.run_once = false;
    config::validate(&options).map_err(|e| format!("config invalid: {}", e.0.join("; ")))?;
    Ok(Ready {
        options,
        environment,
        content_root,
    })
}

/// Joins the lines of a .NET exception text (`---> inner`) into one.
fn one_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Size and modification time of each watched file (`None`: missing).
pub type Fingerprint = Vec<Option<(u64, SystemTime)>>;

/// The current [`Fingerprint`] of `files`.
pub fn fingerprint(files: &[PathBuf]) -> Fingerprint {
    files
        .iter()
        .map(|f| {
            let m = std::fs::metadata(f).ok()?;
            Some((m.len(), m.modified().unwrap_or(SystemTime::UNIX_EPOCH)))
        })
        .collect()
}

/// The process environment, converted lossily like the .NET host does.
pub fn process_env() -> Vec<(String, String)> {
    std::env::vars_os()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.to_string_lossy().into_owned(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests;
