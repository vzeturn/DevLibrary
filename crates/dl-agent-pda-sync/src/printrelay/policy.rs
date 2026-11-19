//! Port of `PrintRelay/PrinterTargetPolicy.cs`: the relay's safety gate (ADR 0098).

use crate::config::PrintRelayOptions;
use crate::util::ctx::Ctx;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;

/// A CIDR range (`System.Net.IPNetwork`): a base address with no bits set past the
/// prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IpNetwork {
    base: IpAddr,
    prefix: u8,
}

fn bits(ip: IpAddr) -> (u128, u8) {
    match ip {
        IpAddr::V4(a) => (u128::from(u32::from(a)), 32),
        IpAddr::V6(a) => (u128::from(a), 128),
    }
}

/// The leading `prefix` bits of a `width`-bit address set (`prefix <= width`).
fn mask(prefix: u8, width: u8) -> u128 {
    if prefix == 0 {
        return 0;
    }
    let full = if width >= 128 {
        u128::MAX
    } else {
        (1u128 << width) - 1
    };
    full & !((1u128 << (width - prefix)) - 1)
}

impl IpNetwork {
    /// `new IPNetwork(base, prefix)`; `None` when the prefix is too long for the family or
    /// the base has bits set past it.
    pub fn new(base: IpAddr, prefix: u8) -> Option<IpNetwork> {
        let (b, width) = bits(base);
        if prefix > width || b & !mask(prefix, width) != 0 {
            return None;
        }
        Some(IpNetwork { base, prefix })
    }

    /// `IPNetwork.TryParse`: `address/prefix`.
    pub fn parse(s: &str) -> Option<IpNetwork> {
        let (addr, prefix) = s.rsplit_once('/')?;
        if prefix.is_empty() || !prefix.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let prefix: u8 = prefix.parse().ok()?;
        IpNetwork::new(addr.parse().ok()?, prefix)
    }

    /// The base address.
    pub fn base(&self) -> IpAddr {
        self.base
    }

    /// The prefix length.
    pub fn prefix(&self) -> u8 {
        self.prefix
    }

    /// `IPNetwork.Contains`: same family and the same leading `prefix` bits.
    pub fn contains(&self, address: IpAddr) -> bool {
        let (b, w1) = bits(self.base);
        let (a, w2) = bits(address);
        w1 == w2 && (a & mask(self.prefix, w1)) == b
    }
}

impl std::fmt::Display for IpNetwork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.base, self.prefix)
    }
}

/// Why a job's target was not accepted; `retryable` only for a name lookup that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetRefusal {
    /// The reason.
    pub reason: String,
    /// Whether the cloud may push the job again.
    pub retryable: bool,
}

/// Resolves a host name to addresses (`Dns.GetHostAddressesAsync`); the error is the
/// failure message.
pub type Resolver = Arc<dyn Fn(&str, &Ctx) -> Result<Vec<IpAddr>, String> + Send + Sync>;

fn private_ranges() -> [IpNetwork; 4] {
    // Literal, valid networks.
    [
        IpNetwork {
            base: IpAddr::V4([10, 0, 0, 0].into()),
            prefix: 8,
        },
        IpNetwork {
            base: IpAddr::V4([172, 16, 0, 0].into()),
            prefix: 12,
        },
        IpNetwork {
            base: IpAddr::V4([192, 168, 0, 0].into()),
            prefix: 16,
        },
        IpNetwork {
            base: IpAddr::V6([0xfc00, 0, 0, 0, 0, 0, 0, 0].into()),
            prefix: 7,
        },
    ]
}

/// The system resolver (`getaddrinfo`).
pub fn system_resolver() -> Resolver {
    Arc::new(|host: &str, _ctx: &Ctx| {
        (host, 0u16)
            .to_socket_addrs()
            .map(|it| it.map(|a| a.ip()).collect())
            .map_err(|e| crate::printrelay::os_message(&e))
    })
}

/// A job may only reach a private-range address inside the allowed subnets, on an
/// allowed port (`PrinterTargetPolicy`). A host name is resolved once and the checked
/// address is what the printer dials. A refusal is permanent; only a failed lookup is
/// retryable.
pub struct PrinterTargetPolicy {
    subnets: Vec<IpNetwork>,
    ports: Vec<i32>,
    resolve: Resolver,
}

impl std::fmt::Debug for PrinterTargetPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrinterTargetPolicy")
            .field("subnets", &self.subnets)
            .field("ports", &self.ports)
            .finish_non_exhaustive()
    }
}

impl PrinterTargetPolicy {
    /// Builds the gate from the configured subnets or, when none is configured, from
    /// `auto_subnets`. `Err` with the reason when an entry is not a private CIDR, a port is
    /// out of range, or nothing is allowed at all: the relay then stays off.
    pub fn create(
        options: &PrintRelayOptions,
        auto_subnets: impl IntoIterator<Item = IpNetwork>,
        resolve: Option<Resolver>,
    ) -> Result<PrinterTargetPolicy, String> {
        let mut subnets: Vec<IpNetwork> = Vec::new();
        if !options.printer_subnets.is_empty() {
            for entry in &options.printer_subnets {
                match entry.as_deref().map(str::trim).and_then(IpNetwork::parse) {
                    Some(net) if is_private_network(&net) => subnets.push(net),
                    _ => {
                        return Err(format!(
                            "PrinterSubnets entry '{}' is not a private CIDR range",
                            entry.as_deref().unwrap_or("")
                        ));
                    }
                }
            }
        } else {
            for net in auto_subnets {
                if is_private_network(&net) && !subnets.contains(&net) {
                    subnets.push(net);
                }
            }
        }
        if subnets.is_empty() {
            return Err("no private printer subnet is configured or found on this machine".into());
        }
        if options.ports.is_empty() || options.ports.iter().any(|p| !(1..=65535).contains(p)) {
            return Err("Ports must list at least one TCP port between 1 and 65535".into());
        }
        let mut ports = Vec::new();
        for p in &options.ports {
            if !ports.contains(p) {
                ports.push(*p);
            }
        }
        Ok(PrinterTargetPolicy {
            subnets,
            ports,
            resolve: resolve.unwrap_or_else(system_resolver),
        })
    }

    /// The allowed subnets.
    pub fn subnets(&self) -> &[IpNetwork] {
        &self.subnets
    }

    /// The allowed ports.
    pub fn ports(&self) -> &[i32] {
        &self.ports
    }

    /// The checked endpoint to dial, or the refusal (`ResolveAsync`).
    pub fn resolve(&self, host: &str, port: i32, ctx: &Ctx) -> Result<SocketAddr, TargetRefusal> {
        if !self.ports.contains(&port) {
            return Err(TargetRefusal {
                reason: format!("port {port} is not an allowed printer port"),
                retryable: false,
            });
        }
        let literal = host.trim_matches(['[', ']']);
        let candidates = match literal.parse::<IpAddr>() {
            Ok(ip) => vec![ip],
            Err(_) => (self.resolve)(host, ctx).map_err(|e| TargetRefusal {
                reason: format!("printer host '{host}' could not be resolved: {e}"),
                retryable: true,
            })?,
        };
        // In range: the caller's port passed the allow-list (1..=65535).
        let port = u16::try_from(port).unwrap_or_default();
        for ip in candidates {
            let address = match ip {
                IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
                v4 => v4,
            };
            if self.is_allowed(address) {
                return Ok(SocketAddr::new(address, port));
            }
        }
        Err(TargetRefusal {
            reason: format!(
                "printer host '{host}' is not inside an allowed private printer subnet"
            ),
            retryable: false,
        })
    }

    /// A private address inside an allowed subnet.
    pub fn is_allowed(&self, address: IpAddr) -> bool {
        private_ranges().iter().any(|r| r.contains(address))
            && self.subnets.iter().any(|s| s.contains(address))
    }
}

/// Whether `net` lies inside one of 10/8, 172.16/12, 192.168/16, fc00::/7.
pub fn is_private_network(net: &IpNetwork) -> bool {
    private_ranges().iter().any(|r| {
        bits(r.base).1 == bits(net.base).1 && net.prefix >= r.prefix && r.contains(net.base)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(s: &str) -> IpNetwork {
        IpNetwork::parse(s).unwrap()
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn options(subnets: &[&str]) -> PrintRelayOptions {
        PrintRelayOptions {
            printer_subnets: subnets.iter().map(|s| Some((*s).to_owned())).collect(),
            ..PrintRelayOptions::default()
        }
    }

    fn policy(resolve: Option<Resolver>) -> PrinterTargetPolicy {
        PrinterTargetPolicy::create(&options(&["192.168.1.0/24"]), [], resolve).unwrap()
    }

    #[test]
    fn networks_parse_like_dotnet() {
        assert_eq!(net("10.0.0.0/8").to_string(), "10.0.0.0/8");
        assert!(IpNetwork::parse("10.0.0.1/8").is_none());
        assert!(IpNetwork::parse("10.0.0.0").is_none());
        assert!(IpNetwork::parse("10.0.0.0/33").is_none());
        assert!(net("0.0.0.0/0").contains(ip("8.8.8.8")));
        assert!(!net("0.0.0.0/0").contains(ip("::1")));
        assert!(net("fc00::/7").contains(ip("fd12::1")));
    }

    #[test]
    fn create_refuses_non_private_subnets() {
        for s in ["8.8.8.0/24", "0.0.0.0/0", "10.0.0.0/7", "not-a-cidr"] {
            let e = PrinterTargetPolicy::create(&options(&[s]), [], None).unwrap_err();
            assert!(e.contains(s), "{e}");
        }
    }

    #[test]
    fn create_refuses_when_nothing_is_allowed() {
        let e =
            PrinterTargetPolicy::create(&PrintRelayOptions::default(), [net("8.8.8.0/24")], None)
                .unwrap_err();
        assert!(e.contains("no private printer subnet"));
    }

    #[test]
    fn create_refuses_bad_ports() {
        let o = PrintRelayOptions {
            ports: vec![0],
            ..options(&["10.1.0.0/16"])
        };
        let e = PrinterTargetPolicy::create(&o, [], None).unwrap_err();
        assert!(e.contains("Ports"));
    }

    #[test]
    fn create_uses_auto_subnets_only_when_none_configured() {
        let auto =
            PrinterTargetPolicy::create(&PrintRelayOptions::default(), [net("10.9.0.0/16")], None)
                .unwrap();
        assert!(auto.is_allowed(ip("10.9.3.4")));
        let configured =
            PrinterTargetPolicy::create(&options(&["192.168.1.0/24"]), [net("10.9.0.0/16")], None)
                .unwrap();
        assert!(!configured.is_allowed(ip("10.9.3.4")));
        assert!(configured.is_allowed(ip("192.168.1.50")));
    }

    #[test]
    fn resolve_allows_a_private_target_on_an_allowed_port() {
        let t = policy(None)
            .resolve("192.168.1.50", 9100, &Ctx::background())
            .unwrap();
        assert_eq!(t, "192.168.1.50:9100".parse().unwrap());
    }

    #[test]
    fn resolve_refuses_permanently() {
        for (host, port) in [
            ("192.168.2.50", 9100),
            ("8.8.8.8", 9100),
            ("127.0.0.1", 9100),
            ("192.168.1.50", 22),
        ] {
            let r = policy(None)
                .resolve(host, port, &Ctx::background())
                .unwrap_err();
            assert!(!r.retryable, "{host}:{port}");
        }
    }

    #[test]
    fn resolve_dials_the_checked_address_of_a_host_name() {
        let p = policy(Some(Arc::new(|_: &str, _: &Ctx| {
            Ok(vec![ip("8.8.8.8"), ip("192.168.1.9")])
        })));
        let t = p
            .resolve("kitchen.local", 9100, &Ctx::background())
            .unwrap();
        assert_eq!(t.ip(), ip("192.168.1.9"));
    }

    #[test]
    fn resolve_refuses_a_host_name_that_resolves_outside() {
        let p = policy(Some(Arc::new(|_: &str, _: &Ctx| Ok(vec![ip("8.8.8.8")]))));
        let r = p
            .resolve("evil.example", 9100, &Ctx::background())
            .unwrap_err();
        assert!(!r.retryable);
    }

    #[test]
    fn resolve_a_failed_lookup_is_retryable() {
        let p = policy(Some(Arc::new(|_: &str, _: &Ctx| {
            Err("No such host is known.".to_owned())
        })));
        let r = p
            .resolve("kitchen.local", 9100, &Ctx::background())
            .unwrap_err();
        assert!(r.retryable);
        assert_eq!(
            r.reason,
            "printer host 'kitchen.local' could not be resolved: No such host is known."
        );
    }
}
