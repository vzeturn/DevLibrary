//! SQL Server Browser ([MC-SQLR]): resolves a named instance to its TCP port over UDP 1434.

use std::net::{ToSocketAddrs, UdpSocket};
use std::time::Duration;

use crate::Error;

/// CLNT_UCAST_INST request: 0x04, instance name, NUL.
pub(crate) fn request(instance: &str) -> Vec<u8> {
    let mut b = vec![0x04];
    b.extend_from_slice(instance.as_bytes());
    b.push(0);
    b
}

/// Parses an SVR_RESP (`0x05`, u16 length, `key;value;...;;` records) and returns the
/// `tcp` port of `instance` (case-insensitive), as go-mssqldb's `parseInstances` +
/// `ParseBrowserData`.
pub(crate) fn parse_response(msg: &[u8], instance: &str) -> Result<u16, Error> {
    if msg.len() <= 3 || msg[0] != 0x05 {
        return Err(Error::Browser("invalid response".into()));
    }
    let text = String::from_utf8_lossy(&msg[3..]);
    let tokens: Vec<&str> = text.split(';').collect();
    let mut i = 0;
    let mut record: Vec<(&str, &str)> = Vec::new();
    let wanted = instance.to_uppercase();
    let check = |record: &Vec<(&str, &str)>| -> Option<Result<u16, Error>> {
        let name = record.iter().find(|(k, _)| *k == "InstanceName")?.1;
        if name.to_uppercase() != wanted {
            return None;
        }
        let port = record.iter().find(|(k, _)| *k == "tcp").map(|(_, v)| *v);
        Some(match port {
            Some(p) => p.parse::<u16>().map_err(|_| {
                Error::Browser(format!("invalid tcp port '{p}' for instance '{instance}'"))
            }),
            None => Err(Error::Browser(format!(
                "instance '{instance}' has no TCP port (TCP disabled?)"
            ))),
        })
    };
    while i < tokens.len() {
        let name = tokens[i];
        if name.is_empty() {
            if record.is_empty() {
                break;
            }
            if let Some(r) = check(&record) {
                return r;
            }
            record.clear();
            i += 1;
            continue;
        }
        let value = tokens.get(i + 1).copied().unwrap_or("");
        record.push((name, value));
        i += 2;
    }
    if !record.is_empty()
        && let Some(r) = check(&record)
    {
        return r;
    }
    Err(Error::Browser(format!(
        "no instance matching '{instance}' returned"
    )))
}

/// Asks the SQL Server Browser on `host` for the TCP port of `instance`.
pub(crate) fn resolve(host: &str, instance: &str, timeout: Duration) -> Result<u16, Error> {
    let addrs: Vec<_> = (host, 1434u16).to_socket_addrs()?.collect();
    let mut last = Error::Browser(format!("cannot resolve host '{host}'"));
    for addr in addrs {
        let bind = if addr.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        };
        let sock = UdpSocket::bind(bind)?;
        sock.set_read_timeout(Some(timeout.max(Duration::from_millis(100))))?;
        if let Err(e) = sock.send_to(&request(instance), addr) {
            last = Error::Io(e);
            continue;
        }
        let mut buf = vec![0u8; 16 * 1024 - 1];
        match sock.recv_from(&mut buf) {
            Ok((n, _)) => return parse_response(&buf[..n], instance),
            Err(e) => last = Error::Browser(format!("no answer from {addr}: {e}")),
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp(s: &str) -> Vec<u8> {
        let mut b = vec![0x05];
        b.extend_from_slice(&(s.len() as u16).to_le_bytes());
        b.extend_from_slice(s.as_bytes());
        b
    }

    #[test]
    fn request_format() {
        assert_eq!(request("SQLEXPRESS"), b"\x04SQLEXPRESS\x00");
    }

    #[test]
    fn parses_instances() {
        let r = resp(
            "ServerName;HOTEL;InstanceName;MSSQLSERVER;IsClustered;No;Version;16.0.1000.6;tcp;1433;;\
             ServerName;HOTEL;InstanceName;SQLEXPRESS;IsClustered;No;Version;15.0.2000.5;tcp;51234;np;\\\\HOTEL\\pipe\\MSSQL$SQLEXPRESS\\sql\\query;;",
        );
        assert_eq!(parse_response(&r, "sqlexpress").unwrap(), 51234);
        assert_eq!(parse_response(&r, "MSSQLSERVER").unwrap(), 1433);
        assert!(parse_response(&r, "OTHER").is_err());
        let r = resp("ServerName;H;InstanceName;NOTCP;IsClustered;No;np;x;;");
        assert!(parse_response(&r, "NOTCP").is_err());
        let r = resp("ServerName;H;InstanceName;BAD;tcp;99999;;");
        assert!(parse_response(&r, "BAD").is_err());
        assert!(parse_response(&[5, 0], "X").is_err());
        assert!(parse_response(&[4, 0, 0, b'a'], "X").is_err());
        // Unterminated last record still counts.
        let r = resp("ServerName;H;InstanceName;A;tcp;1500");
        assert_eq!(parse_response(&r, "a").unwrap(), 1500);
    }
}
