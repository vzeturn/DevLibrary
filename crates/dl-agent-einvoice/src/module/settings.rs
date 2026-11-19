//! The `modules.einvoice` section of the dl-agent remote config.
//!
//! ```json
//! {"modules": {"einvoice": {
//!     "enabled": true,
//!     "config_dir": "C:\\ProgramData\\MM-EInvoice",
//!     "poll_interval_seconds": 30,
//!     "sql_source": "sealed",
//!     "fo_database": "SMILE_FO",
//!     "pos_database": "SMILE_POS"
//! }}}
//! ```
//!
//! `enabled` is read by the supervisor (the module is opt-in). The sealed `config.enc`
//! (platform URL, API key, SQL connection strings) lives in `config_dir`.
//!
//! `sql_source` picks how SMILE_FO / SMILE_POS are reached: `sealed` (default) uses the
//! connection strings sealed in `config.enc` at enrolment; `site` uses the databases declared
//! in dl-agent's `sql.json` (`dl-agent sql-setup` / `sql-add`: Windows or SQL authentication,
//! pinned transport) named by `fo_database` and `pos_database` (`""` = the FO database holds
//! POS bills too; absent = `SMILE_POS` when the sealed config has a POS database).

use std::path::PathBuf;
use std::time::Duration;

use dl_agent::supervisor::ModuleContext;
use dl_json::Value;

use crate::agent_config::AgentConfig;
use crate::sql::Sql;

/// The module's settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Where `config.enc` lives.
    pub config_dir: PathBuf,
    /// Overrides the sealed config's poll interval when set.
    pub poll_interval: Option<Duration>,
    /// How the SMILE databases are reached.
    pub sql: SqlSource,
}

/// How the module reaches SMILE_FO / SMILE_POS.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SqlSource {
    /// The connection strings sealed in `config.enc` (default; every Go-era enrolment).
    #[default]
    Sealed,
    /// dl-agent's `sql.json` databases.
    Site {
        /// The SMILE_FO database name in `sql.json`.
        fo: String,
        /// The SMILE_POS database name; `Some("")` = FO holds POS bills; `None` = follow the
        /// sealed config (`SMILE_POS` when it has a POS database).
        pos: Option<String>,
    },
}

impl Settings {
    /// Reads the section; absent or wrongly typed keys take their defaults.
    pub fn from_config(v: &Value) -> Settings {
        let config_dir = v
            .get("config_dir")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map_or_else(default_config_dir, PathBuf::from);
        let poll_interval = v
            .get("poll_interval_seconds")
            .and_then(Value::as_u64)
            .filter(|s| *s > 0)
            .map(Duration::from_secs);
        let text = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .map(|s| s.trim().to_owned())
        };
        let sql = match text("sql_source").as_deref() {
            Some("site") => SqlSource::Site {
                fo: text("fo_database")
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "SMILE_FO".into()),
                pos: text("pos_database"),
            },
            _ => SqlSource::Sealed,
        };
        Settings {
            config_dir,
            poll_interval,
            sql,
        }
    }
}

impl Settings {
    /// The SMILE connections for `cfg`, through the source this section picks.
    ///
    /// # Errors
    /// A `site` database that `sql.json` does not declare.
    pub fn open_sql(&self, ctx: &ModuleContext, cfg: &AgentConfig) -> Result<Sql, String> {
        match &self.sql {
            SqlSource::Sealed => Ok(Sql::new(&cfg.sql_dsn, &cfg.pos_sql_dsn, cfg.sp_names())),
            SqlSource::Site { fo, pos } => {
                let pool = |name: &str| {
                    ctx.sql(name).map_err(|e| {
                        format!(
                            "sql_source is site but sql.json has no usable database {name}: {e}; \
                             declare it with dl-agent sql-setup, or set sql_source back to sealed"
                        )
                    })
                };
                let pos = match pos.as_deref() {
                    Some(name) => name.to_owned(),
                    None if cfg.pos_sql_dsn.is_empty() => String::new(),
                    None => "SMILE_POS".to_owned(),
                };
                let fo_pool = pool(fo)?;
                let pos_pool = if pos.is_empty() {
                    None
                } else {
                    Some((pos.as_str(), pool(&pos)?))
                };
                Ok(Sql::site((fo, fo_pool), pos_pool, cfg.sp_names()))
            }
        }
    }
}

/// The Go agent's `defaultConfigDir`: machine-wide, so enroll (admin) and the service
/// (LocalSystem) share it.
pub fn default_config_dir() -> PathBuf {
    if let Some(pd) = std::env::var_os("ProgramData").filter(|v| !v.is_empty()) {
        return PathBuf::from(pd).join("MM-EInvoice");
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("mm-einvoice-agent")
}

#[cfg(test)]
mod tests {
    use dl_json::json;

    use super::*;

    #[test]
    fn defaults_follow_the_go_agent() {
        let s = Settings::from_config(&Value::Null);
        assert_eq!(s.config_dir, default_config_dir());
        assert_eq!(s.poll_interval, None);
        let d = default_config_dir();
        assert!(
            d.ends_with("MM-EInvoice") || d.ends_with("mm-einvoice-agent"),
            "{}",
            d.display()
        );
    }

    #[test]
    fn overrides() {
        let s = Settings::from_config(&json!({
            "enabled": true,
            "config_dir": " /srv/einvoice ",
            "poll_interval_seconds": 45
        }));
        assert_eq!(s.config_dir, PathBuf::from("/srv/einvoice"));
        assert_eq!(s.poll_interval, Some(Duration::from_secs(45)));
    }

    #[test]
    fn bad_values_fall_back() {
        let s = Settings::from_config(&json!({
            "config_dir": "",
            "poll_interval_seconds": 0
        }));
        assert_eq!(s.config_dir, default_config_dir());
        assert_eq!(s.poll_interval, None);
        let s = Settings::from_config(&json!({"poll_interval_seconds": "30"}));
        assert_eq!(s.poll_interval, None);
    }

    #[test]
    fn sql_source_defaults_to_the_sealed_config() {
        assert_eq!(Settings::from_config(&Value::Null).sql, SqlSource::Sealed);
        let s = Settings::from_config(&json!({"sql_source": "other"}));
        assert_eq!(s.sql, SqlSource::Sealed);
    }

    #[test]
    fn site_sql_names_the_databases() {
        let s = Settings::from_config(&json!({"sql_source": "site"}));
        assert_eq!(
            s.sql,
            SqlSource::Site {
                fo: "SMILE_FO".into(),
                pos: None
            }
        );
        let s = Settings::from_config(&json!({
            "sql_source": " site ", "fo_database": "FO2", "pos_database": ""
        }));
        assert_eq!(
            s.sql,
            SqlSource::Site {
                fo: "FO2".into(),
                pos: Some(String::new())
            }
        );
    }
}
