//! The edition probe and the `WITH` options it allows.
//!
//! Go always sent `WITH COMPRESSION`, which SQL Server rejects on Express and Web, on
//! Workgroup, on SQL Server 2005, and on 2008 (10.0) outside Enterprise/Developer. The probe
//! reads `EngineEdition`, `Edition` and the version once per connection and
//! [`BackupOptions::for_server`] adds `COMPRESSION` only where the edition supports it.
//! `CHECKSUM` exists in every edition since SQL Server 2005, so it is always added there.

use dl_tds::Client;

use crate::Error;

/// What one connection found out about its server.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerInfo {
    /// `SERVERPROPERTY('EngineEdition')`: 2 Standard (incl. Web/Workgroup), 3 Enterprise
    /// (incl. Developer/Evaluation), 4 Express, 5+ Azure flavours.
    pub engine_edition: i32,
    /// `SERVERPROPERTY('Edition')`, e.g. `Standard Edition (64-bit)`.
    pub edition: String,
    /// Major version (9 = 2005, 10 = 2008/2008 R2, 11 = 2012, … 16 = 2022).
    pub major: u8,
    /// Minor version (50 for 2008 R2).
    pub minor: u8,
    /// The transport rung the login used (`as-configured`, `tls1.0`, `unencrypted`).
    pub transport: String,
}

/// The `WITH` options one BACKUP statement gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupOptions {
    /// `COMPRESSION` is added.
    pub compression: bool,
    /// `CHECKSUM` is added.
    pub checksum: bool,
    /// One line for the job steps: what was chosen and why.
    pub reason: String,
}

impl BackupOptions {
    /// The options this server supports.
    pub fn for_server(info: &ServerInfo) -> BackupOptions {
        let edition = info.edition.to_ascii_lowercase();
        let r2_or_later = (info.major, info.minor) >= (10, 50);
        let compression = info.major >= 10
            && match info.engine_edition {
                3 => true,
                2 => r2_or_later && !edition.contains("web") && !edition.contains("workgroup"),
                _ => false,
            };
        let checksum = info.major >= 9;
        let reason = format!(
            "{} {}.{} (engine edition {}): {}, {}",
            if info.edition.is_empty() {
                "SQL Server"
            } else {
                info.edition.as_str()
            },
            info.major,
            info.minor,
            info.engine_edition,
            if compression {
                "COMPRESSION"
            } else {
                "no COMPRESSION (not supported by this edition/version)"
            },
            if checksum { "CHECKSUM" } else { "no CHECKSUM" },
        );
        BackupOptions {
            compression,
            checksum,
            reason,
        }
    }

    /// The `WITH` clause, starting with `extra` (e.g. `DIFFERENTIAL`).
    pub fn with_clause(&self, extra: Option<&str>) -> String {
        let mut parts: Vec<&str> = Vec::new();
        parts.extend(extra);
        if self.compression {
            parts.push("COMPRESSION");
        }
        if self.checksum {
            parts.push("CHECKSUM");
        }
        parts.push("STATS = 10");
        format!("WITH {}", parts.join(", "))
    }
}

/// Reads the edition and version on a fresh connection.
pub fn probe(client: &mut Client, transport: &str) -> Result<ServerInfo, Error> {
    let r = client
        .query(
            "SELECT CAST(SERVERPROPERTY('EngineEdition') AS int) AS ee, \
             CAST(SERVERPROPERTY('Edition') AS nvarchar(128)) AS ed",
            &[],
        )
        .map_err(|e| Error::Sql(format!("read the server edition: {e}")))?;
    let row = r
        .rows()
        .first()
        .ok_or_else(|| Error::Sql("server edition: no row".into()))?;
    let (major, minor, _) = client.server_version();
    Ok(ServerInfo {
        engine_edition: row.try_get::<Option<i32>>("ee").flatten().unwrap_or(0),
        edition: row
            .try_get::<Option<String>>("ed")
            .flatten()
            .unwrap_or_default(),
        major,
        minor,
        transport: transport.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(ee: i32, edition: &str, major: u8, minor: u8) -> ServerInfo {
        ServerInfo {
            engine_edition: ee,
            edition: edition.into(),
            major,
            minor,
            transport: "as-configured".into(),
        }
    }

    #[test]
    fn compression_only_where_the_edition_supports_it() {
        let cases = [
            (info(3, "Enterprise Edition (64-bit)", 16, 0), true),
            (info(3, "Developer Edition (64-bit)", 10, 0), true),
            (info(2, "Standard Edition (64-bit)", 10, 50), true),
            (info(2, "Standard Edition (64-bit)", 15, 0), true),
            (info(2, "Standard Edition", 10, 0), false),
            (info(2, "Web Edition (64-bit)", 15, 0), false),
            (info(2, "Workgroup Edition", 10, 50), false),
            (info(4, "Express Edition (64-bit)", 16, 0), false),
            (info(3, "Enterprise Edition", 9, 0), false),
            (info(5, "SQL Azure", 12, 0), false),
        ];
        for (i, want) in cases {
            let o = BackupOptions::for_server(&i);
            assert_eq!(o.compression, want, "{i:?}");
            assert!(o.checksum);
            assert!(o.reason.contains(if want {
                "COMPRESSION"
            } else {
                "no COMPRESSION"
            }));
        }
    }

    #[test]
    fn with_clause_lists_the_chosen_options() {
        let full = BackupOptions::for_server(&info(3, "Enterprise", 16, 0));
        assert_eq!(
            full.with_clause(None),
            "WITH COMPRESSION, CHECKSUM, STATS = 10"
        );
        assert_eq!(
            full.with_clause(Some("DIFFERENTIAL")),
            "WITH DIFFERENTIAL, COMPRESSION, CHECKSUM, STATS = 10"
        );
        let express = BackupOptions::for_server(&info(4, "Express Edition", 16, 0));
        assert_eq!(express.with_clause(None), "WITH CHECKSUM, STATS = 10");
        let old = BackupOptions::for_server(&info(0, "", 8, 0));
        assert_eq!(old.with_clause(None), "WITH STATS = 10");
    }
}
