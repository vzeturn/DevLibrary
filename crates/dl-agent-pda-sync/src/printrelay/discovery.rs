//! Port of `PrintRelay/PrintQueueDiscovery.cs` and `PrintQueueReporter.cs`: the Windows
//! print queues, classified and reported to the cloud (self-setup, ADR 0098/0105).

use super::Cancelled;
use super::policy::{IpNetwork, is_private_network};
use super::spooler::SpoolerQueues;
use crate::contracts::WireJson;
use crate::contracts::wire::{
    PrintQueueInfo, PrintQueueReport, print_queue_kinds, print_queue_limits, site_event_levels,
};
use crate::reporting::{Exception, Logger, SiteEvents};
use crate::transport::{self, SyncHttpClient, TokenProvider};
use crate::util::ctx::Ctx;
use dl_json::{Map, Value as Json};
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};

/// One print queue as Windows stores it; the TCP fields only for a Standard TCP/IP port
/// (`RawPrinterEntry`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RawPrinterEntry {
    /// Queue name.
    pub name: String,
    /// The `Port` value as stored (may list several ports).
    pub port_name: String,
    /// `IPAddress`, else `HostName`, of the TCP port.
    pub tcp_host: Option<String>,
    /// `PortNumber`.
    pub tcp_port: Option<i32>,
    /// `Protocol` (1 = RAW).
    pub tcp_protocol: Option<i32>,
}

/// Why the local queues could not be read (the C# `IOException`,
/// `UnauthorizedAccessException`, `SecurityException`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueReadError {
    /// The .NET exception type name (`ex.GetType().Name`).
    pub type_name: &'static str,
    /// The message.
    pub message: String,
}

impl std::fmt::Display for QueueReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for QueueReadError {}

/// Reads the local print queues (`IPrintQueueSource`); empty off Windows.
pub trait PrintQueueSource: Send + Sync {
    /// The queues.
    fn read(&self) -> Result<Vec<RawPrinterEntry>, QueueReadError>;
}

/// `PrintQueueClassifier.RawProtocol`.
pub const RAW_PROTOCOL: i32 = 1;

/// `PrintQueueClassifier.Classify`: a Standard TCP/IP port printing RAW is `tcp`; USB and
/// WSD print through the spooler; every other port is `other`.
pub fn classify(q: &RawPrinterEntry) -> PrintQueueInfo {
    let info = |kind: &str| PrintQueueInfo {
        name: q.name.clone(),
        kind: kind.to_owned(),
        port_name: q.port_name.clone(),
        host: None,
        port: None,
    };
    if let (Some(host), Some(port)) = (q.tcp_host.as_deref(), q.tcp_port)
        && !host.is_empty()
        && (1..=65535).contains(&port)
        && q.tcp_protocol.is_none_or(|p| p == RAW_PROTOCOL)
    {
        return PrintQueueInfo {
            host: Some(host.to_owned()),
            port: Some(port),
            ..info(print_queue_kinds::TCP)
        };
    }
    let port = q.port_name.trim();
    let starts = |p: &str| {
        port.len() >= p.len() && port.as_bytes()[..p.len()].eq_ignore_ascii_case(p.as_bytes())
    };
    if starts("USB") {
        info(print_queue_kinds::USB)
    } else if starts("WSD") {
        info(print_queue_kinds::WSD)
    } else {
        info(print_queue_kinds::OTHER)
    }
}

/// `HostNetworks`: the /32 (or /128) of every TCP queue host that is a private IP
/// literal.
pub fn host_networks(queues: &[PrintQueueInfo]) -> Vec<IpNetwork> {
    queues
        .iter()
        .filter(|q| print_queue_kinds::is_raw_tcp(&q.kind))
        .filter_map(|q| q.host.as_deref()?.parse::<IpAddr>().ok())
        .filter_map(|ip| IpNetwork::new(ip, if ip.is_ipv4() { 32 } else { 128 }))
        .filter(is_private_network)
        .collect()
}

/// A registry value as the queue reader needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegValue {
    /// `REG_SZ` / `REG_EXPAND_SZ` (unexpanded).
    String(String),
    /// `REG_MULTI_SZ`.
    MultiString(Vec<String>),
    /// `REG_DWORD`.
    Dword(u32),
    /// Anything else.
    Other,
}

/// Read access to `HKEY_LOCAL_MACHINE` (`Registry.LocalMachine`), so the queue reader
/// can run over a fake in tests.
pub trait Registry: Send + Sync {
    /// The subkey names of `path`, or `None` when the key does not exist.
    fn subkeys(&self, path: &str) -> Result<Option<Vec<String>>, QueueReadError>;
    /// A value of `path`, or `None` when the key or the value does not exist.
    fn value(&self, path: &str, name: &str) -> Result<Option<RegValue>, QueueReadError>;
}

/// The machine's registry over `dl_winos::registry` (64-bit view of a 64-bit process).
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsRegistry;

fn reg_error(e: &dl_winos::Error) -> QueueReadError {
    if e.code() == Some(dl_winos::codes::ACCESS_DENIED) {
        QueueReadError {
            type_name: "SecurityException",
            message: "Requested registry access is not allowed.".to_owned(),
        }
    } else {
        QueueReadError {
            type_name: "IOException",
            message: e.to_string(),
        }
    }
}

impl WindowsRegistry {
    fn open(path: &str) -> Result<Option<dl_winos::registry::Key>, QueueReadError> {
        use dl_winos::registry::{Hive, Key, View};
        match Key::open(Hive::LocalMachine, path, View::Default) {
            Ok(k) => Ok(Some(k)),
            Err(e) if e.code() == Some(dl_winos::codes::FILE_NOT_FOUND) => Ok(None),
            Err(e) => Err(reg_error(&e)),
        }
    }
}

impl Registry for WindowsRegistry {
    fn subkeys(&self, path: &str) -> Result<Option<Vec<String>>, QueueReadError> {
        match Self::open(path)? {
            Some(k) => k.subkeys().map(Some).map_err(|e| reg_error(&e)),
            None => Ok(None),
        }
    }

    fn value(&self, path: &str, name: &str) -> Result<Option<RegValue>, QueueReadError> {
        use dl_winos::registry::Value;
        let Some(k) = Self::open(path)? else {
            return Ok(None);
        };
        Ok(k.value(name).map_err(|e| reg_error(&e))?.map(|v| match v {
            Value::String(s) | Value::ExpandString(s) => RegValue::String(s),
            Value::MultiString(l) => RegValue::MultiString(l),
            Value::Dword(d) => RegValue::Dword(d),
            _ => RegValue::Other,
        }))
    }
}

/// `SYSTEM\CurrentControlSet\Control\Print\Printers`.
pub const PRINTERS_KEY: &str = r"SYSTEM\CurrentControlSet\Control\Print\Printers";
/// The Standard TCP/IP port monitor's ports.
pub const TCP_PORTS_KEY: &str =
    r"SYSTEM\CurrentControlSet\Control\Print\Monitors\Standard TCP/IP Port\Ports";

/// Print queues from the registry, the spooler's own store, so no WMI or spooler call can
/// hang the agent (`WindowsPrintQueueSource`).
pub struct WindowsPrintQueueSource {
    registry: Arc<dyn Registry>,
}

impl std::fmt::Debug for WindowsPrintQueueSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowsPrintQueueSource")
            .finish_non_exhaustive()
    }
}

impl Default for WindowsPrintQueueSource {
    fn default() -> Self {
        WindowsPrintQueueSource::new(Arc::new(WindowsRegistry))
    }
}

fn as_string(v: Option<RegValue>) -> Option<String> {
    match v {
        Some(RegValue::String(s)) => Some(s),
        _ => None,
    }
}

fn as_int(v: Option<RegValue>) -> Option<i32> {
    match v {
        Some(RegValue::Dword(d)) => Some(i32::from_ne_bytes(d.to_ne_bytes())),
        _ => None,
    }
}

impl WindowsPrintQueueSource {
    /// A source over `registry`.
    pub fn new(registry: Arc<dyn Registry>) -> WindowsPrintQueueSource {
        WindowsPrintQueueSource { registry }
    }

    /// Reads the queues from the registry (any OS; [`PrintQueueSource::read`] is empty off
    /// Windows).
    pub fn read_registry(&self) -> Result<Vec<RawPrinterEntry>, QueueReadError> {
        let r = &self.registry;
        let mut queues = Vec::new();
        let Some(names) = r.subkeys(PRINTERS_KEY)? else {
            return Ok(queues);
        };
        let tcp_ports = r.subkeys(TCP_PORTS_KEY)?.is_some();
        for name in names {
            let printer = format!(r"{PRINTERS_KEY}\{name}");
            let port = as_string(r.value(&printer, "Port")?).unwrap_or_default();
            // A queue may list several ports ("IP_1,IP_2"); the first is where it prints.
            let first = port
                .split(',')
                .map(str::trim)
                .find(|p| !p.is_empty())
                .unwrap_or("")
                .to_owned();
            let tcp = format!(r"{TCP_PORTS_KEY}\{first}");
            if first.is_empty() || !tcp_ports || r.subkeys(&tcp)?.is_none() {
                queues.push(RawPrinterEntry {
                    name,
                    port_name: port,
                    ..RawPrinterEntry::default()
                });
                continue;
            }
            let mut host = as_string(r.value(&tcp, "IPAddress")?);
            if host.as_deref().is_none_or(|h| h.trim().is_empty()) {
                host = as_string(r.value(&tcp, "HostName")?);
            }
            queues.push(RawPrinterEntry {
                name,
                port_name: port,
                tcp_host: host.map(|h| h.trim().to_owned()),
                tcp_port: as_int(r.value(&tcp, "PortNumber")?),
                tcp_protocol: as_int(r.value(&tcp, "Protocol")?),
            });
        }
        Ok(queues)
    }
}

impl PrintQueueSource for WindowsPrintQueueSource {
    fn read(&self) -> Result<Vec<RawPrinterEntry>, QueueReadError> {
        if cfg!(windows) {
            self.read_registry()
        } else {
            Ok(Vec::new())
        }
    }
}

/// Why a queue report failed: the agent stopping, or anything else (logged and retried
/// on the next run).
#[derive(Debug)]
pub enum QueueReportError {
    /// The context ended (`OperationCanceledException`).
    Cancelled,
    /// The report was not taken, with the .NET exception it stands for.
    Failed(Box<Exception>),
}

/// Sends the queue report to the cloud; returns the printers it auto-filled
/// (`IPrintQueueCloud`).
pub trait PrintQueueCloud: Send + Sync {
    /// `POST /sync/v1/print/queues`.
    fn report(&self, report: &PrintQueueReport, ctx: &Ctx)
    -> Result<Vec<String>, QueueReportError>;
}

/// `CloudPrintQueueReporter.Route`.
pub const ROUTE: &str = "/sync/v1/print/queues";

/// `POST /sync/v1/print/queues`, signed with the agent key, like the heartbeat
/// (`CloudPrintQueueReporter`).
pub struct CloudPrintQueueReporter {
    http: Arc<SyncHttpClient>,
    tokens: Arc<TokenProvider>,
}

impl std::fmt::Debug for CloudPrintQueueReporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudPrintQueueReporter")
            .finish_non_exhaustive()
    }
}

fn transport_error(e: transport::Error) -> QueueReportError {
    match e {
        transport::Error::Cancelled => QueueReportError::Cancelled,
        other => QueueReportError::Failed(Box::new(other.to_exception())),
    }
}

impl CloudPrintQueueReporter {
    /// A reporter over the runtime's signed client and token provider.
    pub fn new(http: Arc<SyncHttpClient>, tokens: Arc<TokenProvider>) -> CloudPrintQueueReporter {
        CloudPrintQueueReporter { http, tokens }
    }
}

impl PrintQueueCloud for CloudPrintQueueReporter {
    fn report(
        &self,
        report: &PrintQueueReport,
        ctx: &Ctx,
    ) -> Result<Vec<String>, QueueReportError> {
        let bearer = self.tokens.get_access_token(ctx).map_err(transport_error)?;
        let resp = self
            .http
            .post_json(ctx, ROUTE, &report.to_json(), Some(&bearer))
            .map_err(transport_error)?;
        if resp.status == 401 {
            self.tokens.invalidate();
        }
        if !resp.is_success() {
            return Err(QueueReportError::Failed(Box::new(Exception::new(
                "System.InvalidOperationException",
                format!("print queue report refused with HTTP {}", resp.status),
            ))));
        }
        let mut filled = Vec::new();
        let data = resp.data().map_err(transport_error)?;
        if let Some(d @ Json::Object(_)) = &data
            && let Some(Json::Array(list)) =
                transport::data_member(d, "filled").map_err(transport_error)?
        {
            for f in list {
                let text = |name: &str| -> Result<String, QueueReportError> {
                    Ok(
                        match transport::data_member(f, name).map_err(transport_error)? {
                            Some(v) => transport::element::string(v)
                                .map_err(transport_error)?
                                .unwrap_or_default(),
                            None => String::new(),
                        },
                    )
                };
                filled.push(format!("{} -> {}", text("name")?, text("address")?));
            }
        }
        Ok(filled)
    }
}

/// `PrintQueueDiscovery.MaxListed`.
pub const MAX_LISTED: usize = 30;
/// The logger category (`ILogger<PrintQueueDiscovery>`).
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.PrintRelay.PrintQueueDiscovery";

/// Self-setup (`PrintQueueDiscovery`, ADR 0098/0105): reads the local queues, refreshes
/// the USB/WSD queues the relay may print to through the spooler (only after a successful
/// read), reports them as site events and to the cloud, which auto-fills matching legacy
/// printers.
pub struct PrintQueueDiscovery {
    source: Arc<dyn PrintQueueSource>,
    cloud: Arc<dyn PrintQueueCloud>,
    events: Arc<dyn SiteEvents>,
    agent_id: dl_uuid::Uuid,
    log: Logger,
    spooler_queues: Option<Arc<SpoolerQueues>>,
    last_reported: Mutex<Option<String>>,
}

impl std::fmt::Debug for PrintQueueDiscovery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrintQueueDiscovery")
            .field("agent_id", &self.agent_id)
            .finish_non_exhaustive()
    }
}

fn strings(items: impl Iterator<Item = String>) -> Json {
    Json::Array(items.take(MAX_LISTED).map(Json::String).collect())
}

fn detail(pairs: Vec<(&str, Json)>) -> Option<Map> {
    let mut m = Map::new();
    for (k, v) in pairs {
        m.insert(k, v);
    }
    Some(m)
}

impl PrintQueueDiscovery {
    /// A discovery for `agent_id`; `spooler_queues` is `None` when spooler printing is
    /// not available.
    pub fn new(
        source: Arc<dyn PrintQueueSource>,
        cloud: Arc<dyn PrintQueueCloud>,
        events: Arc<dyn SiteEvents>,
        agent_id: dl_uuid::Uuid,
        log: &Logger,
        spooler_queues: Option<Arc<SpoolerQueues>>,
    ) -> PrintQueueDiscovery {
        PrintQueueDiscovery {
            source,
            cloud,
            events,
            agent_id,
            log: log.for_category(CATEGORY),
            spooler_queues,
            last_reported: Mutex::new(None),
        }
    }

    /// One discovery run (`RunAsync`): the classified queues, or [`Cancelled`] when `ctx`
    /// ended while reporting.
    pub fn run(&self, ctx: &Ctx) -> Result<Vec<PrintQueueInfo>, Cancelled> {
        let queues: Vec<PrintQueueInfo> = match self.source.read() {
            Ok(q) => q
                .iter()
                .map(classify)
                .take(print_queue_limits::MAX_QUEUES)
                .collect(),
            Err(ex) => {
                self.log.warn_ex(
                    Exception::new(
                        format!("System.{}", qualified(ex.type_name)),
                        ex.message.clone(),
                    ),
                    "Local print queues could not be read",
                    &[],
                );
                self.events.report(
                    site_event_levels::WARN,
                    "PRINT_QUEUES_DISCOVERY_FAILED",
                    &format!(
                        "The print queues of this server could not be read ({}); set printer addresses by hand",
                        ex.type_name
                    ),
                    None,
                );
                return Ok(Vec::new());
            }
        };
        if let Some(s) = &self.spooler_queues {
            s.replace(&queues);
        }
        let fingerprint = queues
            .iter()
            .map(|q| {
                format!(
                    "{}|{}|{}|{}|{}",
                    q.name,
                    q.kind,
                    q.port_name,
                    q.host.as_deref().unwrap_or(""),
                    q.port.map(|p| p.to_string()).unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        if self
            .last_reported
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_deref()
            == Some(fingerprint.as_str())
        {
            return Ok(queues);
        }

        let tcp: Vec<&PrintQueueInfo> = queues
            .iter()
            .filter(|q| print_queue_kinds::is_raw_tcp(&q.kind))
            .collect();
        let spooler: Vec<&PrintQueueInfo> = queues
            .iter()
            .filter(|q| print_queue_kinds::is_spooler(&q.kind))
            .collect();
        let unsupported: Vec<&PrintQueueInfo> = queues
            .iter()
            .filter(|q| {
                !print_queue_kinds::is_raw_tcp(&q.kind) && !print_queue_kinds::is_spooler(&q.kind)
            })
            .collect();
        let opt = |v: &Option<String>| v.clone().unwrap_or_default();
        self.events.report(
            site_event_levels::INFO,
            "PRINT_QUEUES_DISCOVERED",
            &format!(
                "Found {} print queue(s) on this server: {} on a TCP/IP port, {} on USB or WSD, {} on another port",
                queues.len(),
                tcp.len(),
                spooler.len(),
                unsupported.len()
            ),
            detail(vec![
                (
                    "tcp",
                    strings(tcp.iter().map(|q| {
                        format!(
                            "{} = {}:{}",
                            q.name,
                            opt(&q.host),
                            q.port.map(|p| p.to_string()).unwrap_or_default()
                        )
                    })),
                ),
                (
                    "spooler",
                    strings(
                        spooler
                            .iter()
                            .map(|q| format!("{} ({}, {})", q.name, q.kind, q.port_name)),
                    ),
                ),
                (
                    "other",
                    strings(unsupported.iter().map(|q| format!("{} ({})", q.name, q.port_name))),
                ),
            ]),
        );
        if !spooler.is_empty() && self.spooler_queues.is_some() {
            self.events.report(
                site_event_levels::INFO,
                "PRINT_QUEUES_SPOOLER_READY",
                &format!(
                    "{} USB/WSD print queue(s) can be printed through the Windows spooler (RAW)",
                    spooler.len()
                ),
                detail(vec![(
                    "queues",
                    strings(spooler.iter().map(|q| q.name.clone())),
                )]),
            );
        }
        let cannot_print: Vec<&PrintQueueInfo> = if self.spooler_queues.is_none() {
            spooler.iter().chain(unsupported.iter()).copied().collect()
        } else {
            unsupported
        };
        if !cannot_print.is_empty() {
            self.events.report(
                site_event_levels::WARN,
                "PRINT_QUEUES_UNSUPPORTED",
                &format!(
                    "{} print queue(s) are on a port the relay does not print to (not TCP/IP RAW, USB or WSD); printers on them print through the browser until they get an IP address",
                    cannot_print.len()
                ),
                detail(vec![("queues", strings(cannot_print.iter().map(|q| q.name.clone())))]),
            );
        }

        let report = PrintQueueReport {
            agent_id: self.agent_id.to_string(),
            queues: queues.clone(),
            spooler_printing: self.spooler_queues.as_ref().map(|_| true),
        };
        match self.cloud.report(&report, ctx) {
            Ok(filled) => {
                if !filled.is_empty() {
                    self.events.report(
                        site_event_levels::INFO,
                        "PRINT_QUEUES_AUTOFILLED",
                        &format!(
                            "The cloud set the address of {} imported printer(s) from this server's print queues",
                            filled.len()
                        ),
                        detail(vec![("printers", strings(filled.into_iter()))]),
                    );
                }
                *self
                    .last_reported
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(fingerprint);
            }
            Err(QueueReportError::Cancelled) => return Err(Cancelled),
            Err(QueueReportError::Failed(ex)) => {
                let message = ex.message.clone();
                self.log.warn_ex(
                    *ex,
                    "Print queue report could not be sent; it is retried on the next discovery run",
                    &[],
                );
                self.events.report(
                    site_event_levels::WARN,
                    "PRINT_QUEUES_REPORT_FAILED",
                    &format!("The print queue list could not be sent to the cloud: {message}"),
                    None,
                );
            }
        }
        Ok(queues)
    }
}

/// The namespace-qualified name of a queue read exception type.
fn qualified(type_name: &str) -> String {
    match type_name {
        "SecurityException" => "Security.SecurityException".to_owned(),
        "IOException" => "IO.IOException".to_owned(),
        other => other.to_owned(),
    }
}

/// The private IPv4 subnets of this machine's network adapters, from the TCP/IP
/// interface settings in the registry (`AdapterNetworks`). Windows only; empty elsewhere.
///
/// The C# code asks `NetworkInterface` for adapters that are up; the registry holds the
/// configured (static or DHCP) address of every adapter whether it is up or not.
pub fn adapter_networks(registry: &dyn Registry) -> Vec<IpNetwork> {
    const IFACES: &str = r"SYSTEM\CurrentControlSet\Services\Tcpip\Parameters\Interfaces";
    let mut out = Vec::new();
    let Ok(Some(ids)) = registry.subkeys(IFACES) else {
        return out;
    };
    let list = |v: Option<RegValue>| -> Vec<String> {
        match v {
            Some(RegValue::String(s)) => vec![s],
            Some(RegValue::MultiString(l)) => l,
            _ => Vec::new(),
        }
    };
    for id in ids {
        let key = format!(r"{IFACES}\{id}");
        let get = |n: &str| registry.value(&key, n).ok().flatten();
        let dhcp = matches!(get("EnableDHCP"), Some(RegValue::Dword(1)));
        let (addrs, masks) = if dhcp {
            (list(get("DhcpIPAddress")), list(get("DhcpSubnetMask")))
        } else {
            (list(get("IPAddress")), list(get("SubnetMask")))
        };
        for (a, m) in addrs.iter().zip(masks.iter()) {
            let (Ok(std::net::IpAddr::V4(a)), Ok(std::net::IpAddr::V4(m))) =
                (a.trim().parse::<IpAddr>(), m.trim().parse::<IpAddr>())
            else {
                continue;
            };
            let prefix = u32::from(m).leading_ones();
            if prefix == 0 || prefix > 32 || a.is_unspecified() {
                continue;
            }
            let base = std::net::Ipv4Addr::from(u32::from(a) & u32::from(m));
            if let Some(net) = IpNetwork::new(IpAddr::V4(base), u8::try_from(prefix).unwrap_or(32))
                && is_private_network(&net)
                && !out.contains(&net)
            {
                out.push(net);
            }
        }
    }
    out
}
