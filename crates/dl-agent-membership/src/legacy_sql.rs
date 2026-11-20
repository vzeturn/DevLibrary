//! The SQL login the old MM.Membership.Agent left on this machine
//! (`%ProgramData%\MM.Membership.Agent\config.dpapi`, DPAPI LocalMachine, in place or renamed
//! by the bench), for the installer to reuse (RT-0303).

use std::sync::Arc;

use dl_agent::legacy_sql::{Candidate, copies, from_connection_string, machine_dirs};

use crate::config::{ConfigStore, DIR_NAME, Dpapi};

/// The SMILE connection of every readable `config.dpapi`, newest first.
pub fn membership_legacy_sql() -> Vec<Candidate> {
    let mut out = Vec::new();
    for dir in machine_dirs(&[DIR_NAME], &[])
        .iter()
        .flat_map(|d| copies(d))
    {
        let store = ConfigStore::new(&dir, Arc::new(Dpapi));
        match store.load() {
            Ok(Some(cfg)) => {
                let source = format!("the old Membership agent ({})", store.path().display());
                out.extend(from_connection_string(
                    &cfg.smile_connection_string,
                    &source,
                ));
            }
            Ok(None) => {}
            Err(e) => eprintln!("Warning: {} is not readable: {e}", store.path().display()),
        }
    }
    out
}
