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

/// A job may only reach a private (LAN) address on an allowed port
/// (`PrinterTargetPolicy`). With no `PrinterSubnets` every private range is allowed
/// (owner decision 2026-11-26, RT-1309, ADR 0010): a printer on a routed store LAN is
/// printed to, and only a failed connection is an error. `PrinterSubnets` narrows that to
/// the listed ranges. Public, loopback and link-local addresses are always refused, so the
/// relay never becomes a TCP proxy to the internet or to the server itself. A host name is
/// resolved once and the checked address is what the printer dials. A refusal is
/// permanent; only a failed lookup is retryable.
pub struct PrinterTargetPolicy {
    subnets: Vec<IpNetwork>,
    configured: bool,
    ports: Vec<i32>,
    resolve: Resolver,
}

impl std::fmt::Debug for PrinterTargetPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrinterTargetPolicy")
            .field("subnets", &self.subnets)
            .field("configured", &self.configured)
            .field("ports", &self.ports)
            .finish_non_exhaustive()
    }
}

fn join<T: std::fmt::Display>(items: &[T]) -> String {
    items
        .iter()
        .map(T::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

impl PrinterTargetPolicy {
    /// Builds the gate from the configured `PrinterSubnets`, or every private range when
    /// none is configured. `Err` with the reason when an entry is not a private CIDR or a
    /// port is out of range: the relay then stays off.
    pub fn create(
        options: &PrintRelayOptions,
        resolve: Option<Resolver>,
    ) -> Result<PrinterTargetPolicy, String> {
        let mut subnets: Vec<IpNetwork> = Vec::new();
        for entry in &options.printer_subnets {
            match entry.as_deref().map(str::trim).and_then(IpNetwork::parse) {
                Some(net) if is_private_network(&net) => {
                    if !subnets.contains(&net) {
                        subnets.push(net);
                    }
                }
                _ => {
                    return Err(format!(
                        "PrinterSubnets entry '{}' is not a private CIDR range",
                        entry.as_deref().unwrap_or("")
                    ));
                }
            }
        }
        let configured = !subnets.is_empty();
        if !configured {
            subnets = private_ranges().to_vec();
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
            configured,
            ports,
            resolve: resolve.unwrap_or_else(system_resolver),
        })
    }

    /// The allowed subnets: the configured `PrinterSubnets`, else every private range.
    pub fn subnets(&self) -> &[IpNetwork] {
        &self.subnets
    }

    /// Whether `PrinterSubnets` narrows the private ranges.
    pub fn is_narrowed(&self) -> bool {
        self.configured
    }

    /// The allowed ports.
    pub fn ports(&self) -> &[i32] {
        &self.ports
    }

    /// Where the relay prints, for logs, site events and the selftest: "any private LAN
    /// address (10.0.0.0/8, …)" or "PrinterSubnets [192.168.1.0/24]", plus the ports.
    pub fn describe(&self) -> String {
        let scope = if self.configured {
            format!("PrinterSubnets [{}]", join(&self.subnets))
        } else {
            format!("any private LAN address ({})", join(&self.subnets))
        };
        format!("{scope}, ports [{}]", join(&self.ports))
    }

    /// The checked endpoint to dial, or the refusal (`ResolveAsync`).
    pub fn resolve(&self, host: &str, port: i32, ctx: &Ctx) -> Result<SocketAddr, TargetRefusal> {
        if !self.ports.contains(&port) {
            return Err(TargetRefusal {
                reason: format!(
                    "port {port} is not an allowed printer port (allowed: {})",
                    join(&self.ports)
                ),
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
        let mut any_private = false;
        let mut seen = Vec::new();
        for ip in candidates {
            let address = match ip {
                IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
                v4 => v4,
            };
            if self.is_allowed(address) {
                return Ok(SocketAddr::new(address, port));
            }
            any_private |= is_private_address(address);
            seen.push(address);
        }
        let shown = if seen.len() == 1 && seen[0].to_string() == literal {
            format!("printer host '{host}'")
        } else {
            format!("printer host '{host}' ({})", join(&seen))
        };
        let reason = if any_private {
            format!(
                "{shown} is outside PrinterSubnets [{}] (modules.pda-sync.PrintRelay in the hub config; leave PrinterSubnets empty to allow every private LAN printer)",
                join(&self.subnets)
            )
        } else {
            format!(
                "{shown} is not a private LAN address; the print relay only dials 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16 and fc00::/7, never public or loopback addresses"
            )
        };
        Err(TargetRefusal {
            reason,
            retryable: false,
        })
    }

    /// A private address inside an allowed subnet.
    pub fn is_allowed(&self, address: IpAddr) -> bool {
        is_private_address(address) && self.subnets.iter().any(|s| s.contains(address))
    }
}

fn is_private_address(address: IpAddr) -> bool {
    private_ranges().iter().any(|r| r.contains(address))
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
        PrinterTargetPolicy::create(&options(&["192.168.1.0/24"]), resolve).unwrap()
    }

    fn default_policy() -> PrinterTargetPolicy {
        PrinterTargetPolicy::create(&PrintRelayOptions::default(), None).unwrap()
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
            let e = PrinterTargetPolicy::create(&options(&[s]), None).unwrap_err();
            assert!(e.contains(s), "{e}");
        }
    }

    #[test]
    fn with_no_subnet_configured_every_private_lan_is_allowed() {
        // RT-1309: a printer on a routed store LAN that is not one of the server's own
        // adapter subnets.
        let p = default_policy();
        assert!(!p.is_narrowed());
        let t = p
            .resolve("10.244.204.245", 9100, &Ctx::background())
            .unwrap();
        assert_eq!(t, "10.244.204.245:9100".parse().unwrap());
        for a in ["10.0.0.1", "172.31.255.1", "192.168.77.8", "fd12::7"] {
            assert!(p.is_allowed(ip(a)), "{a}");
        }
        assert!(
            p.describe()
                .starts_with("any private LAN address (10.0.0.0/8")
        );
        assert!(p.describe().ends_with("ports [9100]"));
    }

    #[test]
    fn with_no_subnet_configured_public_and_local_addresses_are_still_refused() {
        let p = default_policy();
        for a in [
            "8.8.8.8",
            "127.0.0.1",
            "169.254.1.1",
            "::1",
            "172.32.0.1",
            "11.0.0.1",
        ] {
            assert!(!p.is_allowed(ip(a)), "{a}");
            let r = p.resolve(a, 9100, &Ctx::background()).unwrap_err();
            assert!(!r.retryable, "{a}");
            assert!(
                r.reason.contains("not a private LAN address"),
                "{}",
                r.reason
            );
        }
        let r = p
            .resolve("10.244.204.245", 22, &Ctx::background())
            .unwrap_err();
        assert_eq!(
            r.reason,
            "port 22 is not an allowed printer port (allowed: 9100)"
        );
    }

    #[test]
    fn create_refuses_bad_ports() {
        let o = PrintRelayOptions {
            ports: vec![0],
            ..options(&["10.1.0.0/16"])
        };
        let e = PrinterTargetPolicy::create(&o, None).unwrap_err();
        assert!(e.contains("Ports"));
    }

    #[test]
    fn configured_subnets_narrow_the_private_ranges() {
        let p = policy(None);
        assert!(p.is_narrowed());
        assert!(p.is_allowed(ip("192.168.1.50")));
        assert!(!p.is_allowed(ip("10.244.204.245")));
        let r = p
            .resolve("10.244.204.245", 9100, &Ctx::background())
            .unwrap_err();
        assert!(!r.retryable);
        assert!(
            r.reason.starts_with(
                "printer host '10.244.204.245' is outside PrinterSubnets [192.168.1.0/24]"
            ),
            "{}",
            r.reason
        );
        assert_eq!(
            p.describe(),
            "PrinterSubnets [192.168.1.0/24], ports [9100]"
        );
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
        let p = default_policy();
        let p = PrinterTargetPolicy {
            resolve: Arc::new(|_: &str, _: &Ctx| Ok(vec![ip("8.8.8.8")])),
            ..p
        };
        let r = p
            .resolve("evil.example", 9100, &Ctx::background())
            .unwrap_err();
        assert!(!r.retryable);
        assert!(
            r.reason
                .starts_with("printer host 'evil.example' (8.8.8.8) is not a private LAN address"),
            "{}",
            r.reason
        );
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
