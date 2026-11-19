//! Continuity with the agent a site already ran: a fresh enrolment (new key, new device,
//! procedures reinstalled) that keeps the old agent's SQL connection and collect window.
//!
//! The old `config.enc` is the MM-EInvoice Go agent's (same format, same folder) or an
//! earlier dl-agent enrolment. It is read from the config folder itself (`--force` over an
//! existing enrolment) or, when that has none, from the newest `<folder>.bench-old-<time>`
//! sibling, where the bench script moves the Go agent's folder before installing dl-agent.
//! Only the connection (server, instance or port, login, password, database names) and
//! `collect_from` are taken; the transport is negotiated again and the key, device id and
//! everything else are new. Nothing in the old folder is changed.

use std::path::{Path, PathBuf};

use dl_tds::Config;
use dl_time::OffsetDateTime;

use crate::agent_config::AgentConfig;
use crate::dsn::Dsn;

/// Where the bench script moves an older agent's folder (`benchLegacy.ts`).
const BENCH_OLD: &str = ".bench-old-";

/// What an earlier enrolment of this site knew.
#[derive(Debug, Clone, Default)]
pub struct Previous {
    /// The folder its `config.enc` was read from.
    pub dir: PathBuf,
    /// The FO connection, transport left to negotiation.
    pub fo: Option<Dsn>,
    /// The POS connection, transport left to negotiation.
    pub pos: Option<Dsn>,
    /// Bills dated from this were collected (or due).
    pub collect_from: Option<OffsetDateTime>,
}

/// The earlier enrolment, if one is readable: `Ok(None)` when there is none, `Err` with
/// the folder and reason when one exists but does not open (the caller says so and goes on
/// without it).
pub fn find(config_dir: &Path) -> Result<Option<Previous>, String> {
    let mut dirs = vec![config_dir.to_path_buf()];
    dirs.extend(bench_old(config_dir));
    for dir in dirs {
        match AgentConfig::load(&dir) {
            Ok(None) => {}
            Ok(Some(cfg)) => return Ok(Some(from_config(&cfg, dir))),
            Err(e) => return Err(format!("{}: {e}", dir.display())),
        }
    }
    Ok(None)
}

/// `<config_dir>.bench-old-*` siblings, newest first (the suffix is `yyyyMMddHHmmss`).
fn bench_old(config_dir: &Path) -> Vec<PathBuf> {
    let (Some(parent), Some(name)) = (config_dir.parent(), config_dir.file_name()) else {
        return Vec::new();
    };
    let prefix = format!("{}{BENCH_OLD}", name.to_string_lossy()).to_ascii_lowercase();
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut found: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_ascii_lowercase();
            n.starts_with(&prefix).then(|| (n, e.path()))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().map(|(_, p)| p).collect()
}

fn from_config(cfg: &AgentConfig, dir: PathBuf) -> Previous {
    Previous {
        dir,
        fo: connection(&cfg.sql_dsn),
        pos: connection(&cfg.pos_sql_dsn),
        collect_from: cfg.collect_from,
    }
}

/// The connection part of a sealed DSN; `None` when empty or unparsable.
pub(super) fn connection(dsn: &str) -> Option<Dsn> {
    if dsn.trim().is_empty() {
        return None;
    }
    let c = dsn.parse::<Config>().ok()?;
    Some(Dsn {
        host: c.host().to_owned(),
        port: c.port().map(|p| p.to_string()).unwrap_or_default(),
        instance: c.instance().unwrap_or_default().to_owned(),
        database: c.database().to_owned(),
        user: c.user().to_owned(),
        password: c.password().to_owned(),
        ..Dsn::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d = std::env::temp_dir().join(format!("dl-einvoice-prev-{tag}-{nanos}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn seal(dir: &Path, fo: &str, pos: &str) {
        let mut cfg = AgentConfig {
            config_dir: dir.to_path_buf(),
            ..AgentConfig::default()
        };
        cfg.sql_dsn = fo.into();
        cfg.pos_sql_dsn = pos.into();
        cfg.collect_from = Some(crate::enroll::vn_midnight(Some("2026-10-01")).unwrap());
        cfg.save().unwrap();
    }

    #[test]
    fn the_connection_without_its_transport() {
        let d = connection(
            "sqlserver://smile:p%40ss@10.0.0.5:1433?database=SMILE_FO&tlsmin=1.0&encrypt=disable",
        )
        .unwrap();
        assert_eq!(
            (d.host.as_str(), d.port.as_str(), d.instance.as_str()),
            ("10.0.0.5", "1433", "")
        );
        assert_eq!((d.user.as_str(), d.password.as_str()), ("smile", "p@ss"));
        assert_eq!(d.database, "SMILE_FO");
        assert!(d.transport().is_default(), "transport is negotiated again");
        let named = connection("sqlserver://sa:x@SRV/SQLEXPRESS?database=SMILE_POS").unwrap();
        assert_eq!(
            (named.host.as_str(), named.instance.as_str()),
            ("SRV", "SQLEXPRESS")
        );
        let windows = connection("sqlserver://SRV?database=SMILE_FO").unwrap();
        assert!(windows.user.is_empty(), "Windows authentication stays");
        assert!(connection("").is_none());
    }

    #[test]
    fn reads_the_folder_then_the_newest_bench_copy() {
        let root = scratch("find");
        let dir = root.join("MM-EInvoice");
        assert!(find(&dir).unwrap().is_none(), "nothing anywhere");

        let older = root.join("MM-EInvoice.bench-old-20261101090000");
        let newer = root.join("MM-EInvoice.bench-old-20261119090000");
        seal(&older, "sqlserver://a:1@OLD?database=SMILE_FO", "");
        seal(
            &newer,
            "sqlserver://a:1@NEW?database=SMILE_FO",
            "sqlserver://a:1@NEW?database=SMILE_POS",
        );
        let p = find(&dir).unwrap().expect("a bench copy");
        assert_eq!(p.dir, newer);
        assert_eq!(p.fo.unwrap().host, "NEW");
        assert_eq!(p.pos.unwrap().database, "SMILE_POS");
        assert!(p.collect_from.is_some());

        seal(&dir, "sqlserver://a:1@HERE?database=SMILE_FO", "");
        let p = find(&dir).unwrap().unwrap();
        assert_eq!(
            (p.dir, p.fo.unwrap().host),
            (dir.clone(), "HERE".to_owned())
        );
        assert!(p.pos.is_none());

        std::fs::write(dir.join(crate::agent_config::CONFIG_FILE), b"not sealed").unwrap();
        let e = find(&dir).unwrap_err();
        assert!(e.contains("MM-EInvoice"), "{e}");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
