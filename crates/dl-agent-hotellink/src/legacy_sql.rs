//! The SQL login the old SMILE-HL agent left on this machine
//! (`%ProgramData%\SMILE-HL-Agent\config.json`, secrets DPAPI LocalMachine, in place or
//! renamed by the bench), for the installer to reuse (RT-0303).

use dl_agent::legacy_sql::{Candidate, files, from_connection_string, machine_dirs};

/// The SQL connection of every readable `config.json`, newest first. Read only: a plaintext
/// secret is not sealed back (that is the module's job when it runs).
pub fn hotellink_legacy_sql() -> Vec<Candidate> {
    let mut out = Vec::new();
    for file in files(&machine_dirs(&["SMILE-HL-Agent"], &[]), "config.json") {
        match crate::config::read(&file) {
            Ok(Some(cfg)) => {
                let source = format!("the old HotelLink agent ({})", file.display());
                out.extend(from_connection_string(&cfg.sql.conn_string(), &source));
            }
            Ok(None) => {}
            Err(e) => eprintln!("Warning: {e}"),
        }
    }
    out
}
