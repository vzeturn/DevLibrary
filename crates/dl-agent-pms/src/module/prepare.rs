//! Before a cycle: find the PMS config the way `SmilePmsAgent` does and decide whether
//! the site is enrolled (or can enroll), without touching the cloud.

use std::path::PathBuf;
use std::time::SystemTime;

use crate::config::{self, Config};
use crate::keystore::IDENTITY_FILE;

use super::settings::Settings;

/// What the module can do now.
#[derive(Debug)]
pub enum Prepared {
    /// Enrolled, or holding an enrollment code: run a cycle.
    Ready(Box<Config>),
    /// Nothing to run yet; `detail` is the degraded status.
    Waiting {
        /// What is missing.
        detail: String,
        /// The config file exists but cannot be used (reported once per detail).
        invalid: bool,
    },
}

impl Prepared {
    /// The waiting detail (`None` when ready).
    pub fn detail(&self) -> Option<&str> {
        match self {
            Prepared::Ready(_) => None,
            Prepared::Waiting { detail, .. } => Some(detail),
        }
    }
}

/// [`prepare_with`] over the process environment.
pub fn prepare(s: &Settings) -> Prepared {
    prepare_with(s, &|k| std::env::var(k).unwrap_or_default())
}

/// Loads the config (`config_path`, `$PMS_AGENT_CONFIG` or the port's default), applies
/// the `state_dir` override and checks for `identity.json` or an enrollment code.
pub fn prepare_with(s: &Settings, getenv: &dyn Fn(&str) -> String) -> Prepared {
    let path = config::resolve_path(s.config_path.as_deref(), getenv);
    let mut cfg = match config::load(Some(&path), getenv) {
        Ok(c) => c,
        Err(_) if !path.exists() => {
            return Prepared::Waiting {
                detail: format!("not enrolled: no config file at {}", path.display()),
                invalid: false,
            };
        }
        // Config errors name keys and rules, never values.
        Err(e) => {
            return Prepared::Waiting {
                detail: format!("config invalid: {e}"),
                invalid: true,
            };
        }
    };
    if let Some(d) = &s.state_dir {
        cfg.state_dir.clone_from(d);
    }
    if !cfg.state_dir.join(IDENTITY_FILE).exists() && cfg.enrollment_code.is_empty() {
        return Prepared::Waiting {
            detail: format!(
                "not enrolled: no {IDENTITY_FILE} in {} and no enrollment_code in {}",
                cfg.state_dir.display(),
                path.display()
            ),
            invalid: false,
        };
    }
    Prepared::Ready(Box::new(cfg))
}

/// What changes when someone re-enrolls or edits the config: the modification times of
/// the config file and of `identity.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint(Option<SystemTime>, Option<SystemTime>);

fn mtime(p: Option<PathBuf>) -> Option<SystemTime> {
    p.and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
}

/// The fingerprint of `cfg`'s files.
pub fn fingerprint(cfg: &Config) -> Fingerprint {
    Fingerprint(
        mtime(cfg.path.clone()),
        mtime(Some(cfg.state_dir.join(IDENTITY_FILE))),
    )
}

#[cfg(test)]
mod tests;
