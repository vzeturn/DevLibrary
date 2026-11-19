//! The payload of `GET /api/agent/config`, field for field the hub's response
//! (`hub/src/routes/agent.ts`) and the Go agent's `hubclient` types, plus the
//! `credentialError` flags the hub sets on a value it could not re-seal.

use dl_json::{Map, Value};

use crate::Error;

/// One SQL Server instance the agent backs up.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServerConfig {
    /// Hub id.
    pub id: String,
    /// `host` or `host\instance`.
    pub host: String,
    /// TCP port; 0 means the default.
    pub port: u16,
    /// The SQL password sealed under the agent key (`""` for Windows authentication).
    pub encrypted_password: String,
    /// The hub could not re-seal the password.
    pub credential_error: bool,
    /// `SQL` or `Windows`.
    pub auth_type: String,
    /// The databases registered on this server.
    pub databases: Vec<DatabaseConfig>,
}

/// One database on a server.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DatabaseConfig {
    /// Hub id.
    pub id: String,
    /// The database name as SQL Server knows it.
    pub database_name: String,
}

/// One backup schedule with its retention policy and destination.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScheduleConfig {
    /// Hub id.
    pub id: String,
    /// The database it backs up.
    pub database_id: String,
    /// Five-field cron expression.
    pub cron_expression: String,
    /// IANA zone (or a fixed offset) the cron expression is read in.
    pub time_zone_id: String,
    /// `full`, `differential`/`incremental` or `log`/`transactionLog`.
    pub backup_type: String,
    /// Keep backups for this many days (0 = no age limit).
    pub retain_days: i64,
    /// Keep this many backups (0 = no count limit).
    pub retain_count: i64,
    /// Where the file goes.
    pub cloud: CloudDestinationConfig,
}

/// The destination embedded in a schedule.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CloudDestinationConfig {
    /// `AWSS3`, `AzureBlob`, `LocalFolder`, `GoogleDrive`, `OneDrive`, …
    pub provider: String,
    /// The destination's JSON credentials sealed under the agent key.
    pub encrypted_tokens: String,
    /// S3-compatible endpoint override.
    pub endpoint: Option<String>,
    /// The hub could not re-seal the tokens.
    pub credential_error: bool,
}

/// A restore an operator asked for.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RestoreRequestConfig {
    /// Hub id.
    pub id: String,
    /// The hub's backup history id (the one the agent minted when it ran the backup).
    pub backup_history_id: String,
    /// The server to restore onto.
    pub target_server_host: String,
    /// The database name to restore as.
    pub target_database: String,
    /// The target server's password sealed under the agent key.
    pub target_password_encrypted: String,
    /// The hub could not re-seal the password.
    pub credential_error: bool,
}

/// A one-off command such as `TestConnection`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CommandConfig {
    /// Hub id.
    pub id: String,
    /// `TestConnection` or `DiscoverDatabases`.
    pub kind: String,
    /// The server it concerns.
    pub server_id: String,
    /// The server's host.
    pub host: String,
    /// The server's port.
    pub port: u16,
    /// The server's password sealed under the agent key.
    pub encrypted_password: String,
    /// The hub could not re-seal the password.
    pub credential_error: bool,
    /// `SQL` or `Windows`.
    pub auth_type: String,
}

/// The whole `GET /api/agent/config` answer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfigResponse {
    /// Servers with their databases.
    pub servers: Vec<ServerConfig>,
    /// Schedules of those databases.
    pub schedules: Vec<ScheduleConfig>,
    /// Pending restores.
    pub restore_requests: Vec<RestoreRequestConfig>,
    /// Pending commands.
    pub commands: Vec<CommandConfig>,
}

fn obj(v: &Value) -> &Map {
    static EMPTY: std::sync::OnceLock<Map> = std::sync::OnceLock::new();
    v.as_object().unwrap_or_else(|| EMPTY.get_or_init(Map::new))
}

/// A string field; numbers are accepted too, since ids are opaque to the agent.
fn text(m: &Map, k: &str) -> String {
    match m.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn int(m: &Map, k: &str) -> i64 {
    m.get(k).and_then(Value::as_i64).unwrap_or(0)
}

fn port(m: &Map, k: &str) -> u16 {
    u16::try_from(int(m, k)).unwrap_or(0)
}

fn flag(m: &Map, k: &str) -> bool {
    m.get(k).and_then(Value::as_bool).unwrap_or(false)
}

fn list<'a>(m: &'a Map, k: &str) -> impl Iterator<Item = &'a Value> {
    m.get(k)
        .and_then(Value::as_array)
        .map(|a| a.iter())
        .into_iter()
        .flatten()
}

impl ConfigResponse {
    /// Reads the hub's JSON; a payload that is not an object is a hub error.
    pub fn from_json(v: &Value) -> Result<ConfigResponse, Error> {
        let m = v
            .as_object()
            .ok_or_else(|| Error::Hub("config: response is not a JSON object".into()))?;
        let servers = list(m, "servers")
            .map(|s| {
                let s = obj(s);
                ServerConfig {
                    id: text(s, "id"),
                    host: text(s, "host"),
                    port: port(s, "port"),
                    encrypted_password: text(s, "encryptedPassword"),
                    credential_error: flag(s, "credentialError"),
                    auth_type: text(s, "authType"),
                    databases: list(s, "databases")
                        .map(|d| DatabaseConfig {
                            id: text(obj(d), "id"),
                            database_name: text(obj(d), "databaseName"),
                        })
                        .collect(),
                }
            })
            .collect();
        let schedules = list(m, "schedules")
            .map(|s| {
                let s = obj(s);
                let c = obj(s.get("cloudDestination").unwrap_or(&Value::Null));
                ScheduleConfig {
                    id: text(s, "id"),
                    database_id: text(s, "databaseId"),
                    cron_expression: text(s, "cronExpression"),
                    time_zone_id: text(s, "timeZoneId"),
                    backup_type: text(s, "backupType"),
                    retain_days: int(s, "retainDays"),
                    retain_count: int(s, "retainCount"),
                    cloud: CloudDestinationConfig {
                        provider: text(c, "provider"),
                        encrypted_tokens: text(c, "encryptedTokens"),
                        endpoint: c.get("endpoint").and_then(Value::as_str).map(str::to_owned),
                        credential_error: flag(c, "credentialError"),
                    },
                }
            })
            .collect();
        let restore_requests = list(m, "restoreRequests")
            .map(|r| {
                let r = obj(r);
                RestoreRequestConfig {
                    id: text(r, "id"),
                    backup_history_id: text(r, "backupHistoryId"),
                    target_server_host: text(r, "targetServerHost"),
                    target_database: text(r, "targetDatabase"),
                    target_password_encrypted: text(r, "targetPasswordEncrypted"),
                    credential_error: flag(r, "credentialError"),
                }
            })
            .collect();
        let commands = list(m, "commands")
            .map(|c| {
                let c = obj(c);
                CommandConfig {
                    id: text(c, "id"),
                    kind: text(c, "type"),
                    server_id: text(c, "serverId"),
                    host: text(c, "host"),
                    port: port(c, "port"),
                    encrypted_password: text(c, "encryptedPassword"),
                    credential_error: flag(c, "credentialError"),
                    auth_type: text(c, "authType"),
                }
            })
            .collect();
        Ok(ConfigResponse {
            servers,
            schedules,
            restore_requests,
            commands,
        })
    }

    /// The server that owns database `database_id`, and the database.
    pub fn server_for_database(
        &self,
        database_id: &str,
    ) -> Option<(&ServerConfig, &DatabaseConfig)> {
        self.servers.iter().find_map(|s| {
            s.databases
                .iter()
                .find(|d| d.id == database_id)
                .map(|d| (s, d))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dl_json::json;

    #[test]
    fn reads_the_hub_payload_with_credential_errors() {
        let v = json!({
            "servers": [{"id": "s1", "host": "SQL01\\SMILE", "port": 1433,
                "encryptedPassword": "", "credentialError": true, "authType": "SQL",
                "databases": [{"id": "d1", "databaseName": "SMILE_FO"}]}],
            "schedules": [{"id": "sc1", "databaseId": "d1", "cronExpression": "0 2 * * *",
                "timeZoneId": "Asia/Ho_Chi_Minh", "backupType": "full", "retainDays": 7,
                "retainCount": 0, "cloudDestination": {"provider": "AWSS3",
                "encryptedTokens": "a.b.c", "endpoint": null}}],
            "restoreRequests": [{"id": "r1", "backupHistoryId": "h1",
                "targetServerHost": "SQL01", "targetDatabase": "X",
                "targetPasswordEncrypted": "a.b.c"}],
            "commands": [{"id": "c1", "type": "TestConnection", "serverId": "s1",
                "host": "SQL01", "port": 1433, "encryptedPassword": "", "authType": "Windows"}]
        });
        let c = ConfigResponse::from_json(&v).unwrap();
        assert!(c.servers[0].credential_error);
        assert_eq!(c.servers[0].databases[0].database_name, "SMILE_FO");
        assert_eq!(c.schedules[0].cloud.endpoint, None);
        assert!(!c.schedules[0].cloud.credential_error);
        assert_eq!(c.restore_requests[0].backup_history_id, "h1");
        assert_eq!(c.commands[0].kind, "TestConnection");
        let (s, d) = c.server_for_database("d1").unwrap();
        assert_eq!((s.id.as_str(), d.id.as_str()), ("s1", "d1"));
        assert!(c.server_for_database("nope").is_none());
    }

    #[test]
    fn empty_object_is_an_empty_config_and_garbage_is_an_error() {
        assert_eq!(
            ConfigResponse::from_json(&json!({})).unwrap(),
            ConfigResponse::default()
        );
        assert!(ConfigResponse::from_json(&json!([1])).is_err());
    }
}
