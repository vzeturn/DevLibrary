//! The SQL logins the old MM-EInvoice agent left on this machine (`%ProgramData%\MM-EInvoice\
//! config.enc`, in place or renamed by the bench), for the installer to reuse (RT-0303).

use dl_agent::legacy_sql::{Candidate, copies, from_connection_string, machine_dirs};

use crate::agent_config::AgentConfig;

/// The FO and POS logins of every readable `config.enc`, newest first.
pub fn einvoice_legacy_sql() -> Vec<Candidate> {
    let mut out = Vec::new();
    for dir in machine_dirs(&["MM-EInvoice"], &[])
        .iter()
        .flat_map(|d| copies(d))
    {
        let file = dir.join("config.enc");
        match AgentConfig::load(&dir) {
            Ok(Some(cfg)) => {
                let source = format!("the old E-Invoice agent ({})", file.display());
                out.extend(
                    [cfg.sql_dsn.as_str(), cfg.pos_sql_dsn.as_str()]
                        .into_iter()
                        .filter(|d| !d.trim().is_empty())
                        .filter_map(|d| from_connection_string(d, &source)),
                );
            }
            Ok(None) => {}
            Err(e) => eprintln!("Warning: {} is not readable: {e}", file.display()),
        }
    }
    out
}
