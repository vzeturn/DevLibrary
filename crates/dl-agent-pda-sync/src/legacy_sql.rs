//! The SQL login the old PDAOrder SyncAgent left on this machine (`%ProgramData%\MM\SyncAgent`:
//! `agent.json` names the instance, `secrets\pda-sync-sql-password.bin` holds the `pda_sync`
//! password under DPAPI LocalMachine), for the installer to reuse (RT-0303).

use dl_agent::cli_install_sql::Secret;
use dl_agent::legacy_sql::{Candidate, copies, machine_dirs};
use dl_agent::sql::secret::AuthMode;

use crate::config::identity::AgentIdentity;
use crate::config::paths::AgentPaths;
use crate::config::sql::LOGIN;
use crate::security::{FileSecretStore, SecretStore, names::SQL_PASSWORD};

/// The `pda_sync` login of every readable install, newest first.
pub fn pda_legacy_sql() -> Vec<Candidate> {
    let mut out = Vec::new();
    for dir in machine_dirs(&["MM\\SyncAgent"], &[])
        .iter()
        .flat_map(|d| copies(d))
    {
        let paths = AgentPaths::new(&dir);
        let instance = match AgentIdentity::try_load(&paths) {
            Ok(Some(id)) => id.sql_instance.unwrap_or_default(),
            Ok(None) => continue,
            Err(e) => {
                eprintln!(
                    "Warning: {} is not readable: {e}",
                    paths.identity_file().display()
                );
                continue;
            }
        };
        if instance.trim().is_empty() {
            continue;
        }
        let store = FileSecretStore::dpapi(paths.secrets_dir());
        let password = match store.load(SQL_PASSWORD) {
            Ok(Some(bytes)) => match String::from_utf8(bytes) {
                Ok(p) => Secret::new(p),
                Err(_) => {
                    eprintln!("Warning: the old SyncAgent's SQL password is not UTF-8");
                    continue;
                }
            },
            Ok(None) => continue,
            Err(e) => {
                eprintln!(
                    "Warning: the old SyncAgent's SQL password in {}: {e}",
                    dir.display()
                );
                continue;
            }
        };
        out.push(Candidate {
            source: format!("the old PDAOrder SyncAgent ({})", dir.display()),
            auth: AuthMode::Sql,
            server: instance,
            user: LOGIN.to_string(),
            password,
        });
    }
    out
}
