//! Finds local SQL Server instances (port of `Bootstrap/LocalSqlDiscovery.cs`): the
//! registry (Windows), SQL Browser (UDP 1434), then the default instance.

use super::error::{Error, Result};
use super::models::SqlInstance;
use crate::reporting::log::{Exception, Logger};
use crate::util::ctx::Ctx;
use crate::util::stj::eq_ignore_case;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::Duration;

/// `ILogger<LocalSqlDiscovery>`.
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.Bootstrap.LocalSqlDiscovery";
/// The registry key that lists the installed instances.
pub const INSTANCE_NAMES_KEY: &str = r"SOFTWARE\Microsoft\Microsoft SQL Server\Instance Names\SQL";
/// How long SQL Browser gets to answer.
pub const BROWSER_TIMEOUT: Duration = Duration::from_secs(2);

/// Finds SQL Server instances (`ISqlInstanceDiscovery`).
pub trait SqlInstanceDiscovery: Send + Sync {
    /// The candidate instances, best first.
    fn discover(&self, ctx: &Ctx) -> Result<Vec<SqlInstance>>;
}

/// Registry, SQL Browser, then `.` (`LocalSqlDiscovery`).
#[derive(Debug, Clone)]
pub struct LocalSqlDiscovery {
    log: Logger,
}

fn instance_of(name: &str, source: &str) -> SqlInstance {
    if eq_ignore_case(name, "MSSQLSERVER") {
        SqlInstance::new(".", source)
    } else {
        SqlInstance::new(format!(".\\{name}"), source)
    }
}

/// `LocalSqlDiscovery.ParseBrowserReply`: the `InstanceName` of every `;;`-separated
/// record (`MSSQLSERVER` is `.`).
pub fn parse_browser_reply(payload: &str) -> Vec<SqlInstance> {
    payload
        .split(";;")
        .filter(|c| !c.is_empty())
        .filter_map(|chunk| {
            let parts: Vec<&str> = chunk.split(';').collect();
            let i = parts.iter().position(|p| *p == "InstanceName")?;
            parts.get(i + 1).copied()
        })
        .filter(|n| !n.is_empty())
        .map(|n| instance_of(n, "sql-browser"))
        .collect()
}

/// `Encoding.ASCII.GetString`: bytes above 0x7F become `?`.
fn ascii(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| if b < 0x80 { b as char } else { '?' })
        .collect()
}

impl LocalSqlDiscovery {
    /// A discovery logging to `log`.
    pub fn new(log: &Logger) -> LocalSqlDiscovery {
        LocalSqlDiscovery {
            log: log.for_category(CATEGORY),
        }
    }

    /// The instance names under [`INSTANCE_NAMES_KEY`]; none when the key is missing or
    /// off Windows.
    pub fn from_registry() -> Vec<SqlInstance> {
        if !cfg!(windows) {
            return Vec::new();
        }
        use dl_winos::registry::{Hive, Key, View};
        let Ok(key) = Key::open(Hive::LocalMachine, INSTANCE_NAMES_KEY, View::Default) else {
            return Vec::new();
        };
        key.values()
            .unwrap_or_default()
            .into_iter()
            .map(|(name, _)| instance_of(&name, "registry"))
            .collect()
    }

    fn browser_instances(&self, ctx: &Ctx) -> Result<Vec<SqlInstance>> {
        let timeout = ctx
            .remaining()
            .map_or(BROWSER_TIMEOUT, |r| r.min(BROWSER_TIMEOUT))
            .max(Duration::from_millis(1));
        let attempt = || -> std::io::Result<Vec<u8>> {
            let udp = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))?;
            udp.send_to(&[0x03], SocketAddrV4::new(Ipv4Addr::LOCALHOST, 1434))?;
            udp.set_read_timeout(Some(timeout))?;
            let mut buf = vec![0u8; 65_535];
            let (n, _) = udp.recv_from(&mut buf)?;
            buf.truncate(n);
            Ok(buf)
        };
        match attempt() {
            Ok(reply) => Ok(parse_browser_reply(&ascii(reply.get(3..).unwrap_or(&[])))),
            Err(_) if ctx.is_done() => Err(Error::cancelled()),
            Err(e) => {
                let ex = Exception::new("System.Net.Sockets.SocketException", e.to_string());
                self.log.write(
                    crate::reporting::log::Level::Debug,
                    Some(ex),
                    "SQL Browser not answering",
                    &[],
                );
                Ok(Vec::new())
            }
        }
    }
}

impl SqlInstanceDiscovery for LocalSqlDiscovery {
    fn discover(&self, ctx: &Ctx) -> Result<Vec<SqlInstance>> {
        let mut found = LocalSqlDiscovery::from_registry();
        found.extend(self.browser_instances(ctx)?);
        found.push(SqlInstance::new(".", "default"));
        let mut distinct: Vec<SqlInstance> = Vec::new();
        for i in found {
            if !distinct
                .iter()
                .any(|d| eq_ignore_case(&d.server_name, &i.server_name))
            {
                distinct.push(i);
            }
        }
        Ok(distinct)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_browser_reply_is_parsed_into_instances() {
        let reply = "ServerName;HOST;InstanceName;SQLEXPRESS;IsClustered;No;Version;16.0;tcp;1433;;ServerName;HOST;InstanceName;MSSQLSERVER;IsClustered;No;;";
        let names: Vec<String> = parse_browser_reply(reply)
            .into_iter()
            .map(|i| i.server_name)
            .collect();
        assert_eq!(names, [r".\SQLEXPRESS", "."]);
        assert!(parse_browser_reply("").is_empty());
        assert!(parse_browser_reply("InstanceName").is_empty());
        assert_eq!(ascii(&[b'a', 0xC3, b'b']), "a?b");
    }

    #[test]
    fn discovery_always_ends_with_the_default_instance_once() {
        let d = LocalSqlDiscovery::new(&Logger::none());
        let found = d.discover(&Ctx::background()).unwrap();
        assert_eq!(found.iter().filter(|i| i.server_name == ".").count(), 1);
    }
}
