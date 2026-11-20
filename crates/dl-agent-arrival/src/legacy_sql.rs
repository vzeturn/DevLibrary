//! The SQL login the old MM.Arrival.Agent left on this machine
//! (`appsettings.Production.json` `Agent:PmsConnectionString`, in place, renamed or moved by
//! the bench), for the installer to reuse (RT-0303).

use dl_agent::legacy_sql::{Candidate, from_connection_string};

use crate::legacy::installs::discover;

/// The connection of every readable .NET agent install, newest first.
pub fn arrival_legacy_sql() -> Vec<Candidate> {
    let getenv = |k: &str| std::env::var(k).unwrap_or_default();
    let installs = match discover(&getenv, None) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("Warning: the old Check-In agent's folders: {e}");
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for i in installs {
        match i.settings() {
            Ok(s) if !s.pms_connection_string.trim().is_empty() => {
                let source = format!("the old Check-In agent ({})", i.dir.display());
                out.extend(from_connection_string(&s.pms_connection_string, &source));
            }
            Ok(_) => {}
            Err(e) => eprintln!("Warning: the old Check-In agent's settings: {e}"),
        }
    }
    out
}
