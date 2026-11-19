//! Port of `PrintQueueDiscoveryTests.cs`, plus the registry reader over a fake registry.

use dl_agent_pda_sync::contracts::wire::{
    PrintQueueInfo, PrintQueueReport, print_queue_kinds, site_event_levels,
};
use dl_agent_pda_sync::printrelay::discovery::{
    QueueReadError, QueueReportError, RegValue, Registry, adapter_networks, classify, host_networks,
};
use dl_agent_pda_sync::printrelay::{
    IpNetwork, PrintQueueCloud, PrintQueueDiscovery, PrintQueueSource, RawPrinterEntry,
    SpoolerQueues, WindowsPrintQueueSource,
};
use dl_agent_pda_sync::reporting::{Exception, Logger, SiteEvents};
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_json::{Map, Value as Json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct FakeQueueSource {
    queues: Mutex<Vec<RawPrinterEntry>>,
    fail: Mutex<Option<QueueReadError>>,
}

impl PrintQueueSource for FakeQueueSource {
    fn read(&self) -> Result<Vec<RawPrinterEntry>, QueueReadError> {
        if let Some(e) = self.fail.lock().unwrap().clone() {
            return Err(e);
        }
        Ok(self.queues.lock().unwrap().clone())
    }
}

#[derive(Default)]
struct FakeQueueCloud {
    reports: Mutex<Vec<PrintQueueReport>>,
    filled: Mutex<Vec<String>>,
    fail: Mutex<Option<String>>,
}

impl PrintQueueCloud for FakeQueueCloud {
    fn report(
        &self,
        report: &PrintQueueReport,
        _ctx: &Ctx,
    ) -> Result<Vec<String>, QueueReportError> {
        self.reports.lock().unwrap().push(report.clone());
        if let Some(m) = self.fail.lock().unwrap().clone() {
            return Err(QueueReportError::Failed(Box::new(Exception::new(
                "System.InvalidOperationException",
                m,
            ))));
        }
        Ok(self.filled.lock().unwrap().clone())
    }
}

/// (level, code, message, detail).
type Reported = (String, String, String, Option<Map>);

#[derive(Default)]
struct RecordingEvents {
    reported: Mutex<Vec<Reported>>,
}

impl SiteEvents for RecordingEvents {
    fn report(&self, level: &str, code: &str, message: &str, detail: Option<Map>) {
        self.reported
            .lock()
            .unwrap()
            .push((level.into(), code.into(), message.into(), detail));
    }
}

impl RecordingEvents {
    fn codes(&self) -> Vec<String> {
        self.reported
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.1.clone())
            .collect()
    }

    fn list(&self, i: usize, key: &str) -> Vec<String> {
        let r = self.reported.lock().unwrap();
        let d = r[i].3.as_ref().unwrap();
        match d.get(key) {
            Some(Json::Array(a)) => a
                .iter()
                .map(|v| match v {
                    Json::String(s) => s.clone(),
                    other => panic!("{other:?}"),
                })
                .collect(),
            other => panic!("{other:?}"),
        }
    }
}

struct Env {
    source: Arc<FakeQueueSource>,
    cloud: Arc<FakeQueueCloud>,
    events: Arc<RecordingEvents>,
    spooler_queues: Arc<SpoolerQueues>,
    agent_id: dl_uuid::Uuid,
}

impl Env {
    fn new() -> Env {
        Env {
            source: Arc::default(),
            cloud: Arc::default(),
            events: Arc::default(),
            spooler_queues: Arc::new(SpoolerQueues::new()),
            agent_id: dl_uuid::Uuid::new_v4(),
        }
    }

    fn discovery(&self, spooler: bool) -> PrintQueueDiscovery {
        PrintQueueDiscovery::new(
            self.source.clone(),
            self.cloud.clone(),
            self.events.clone(),
            self.agent_id,
            &Logger::none(),
            spooler.then(|| self.spooler_queues.clone()),
        )
    }

    fn set(&self, queues: Vec<RawPrinterEntry>) {
        *self.source.queues.lock().unwrap() = queues;
    }
}

fn entry(
    name: &str,
    port: &str,
    host: Option<&str>,
    tcp_port: Option<i32>,
    protocol: Option<i32>,
) -> RawPrinterEntry {
    RawPrinterEntry {
        name: name.into(),
        port_name: port.into(),
        tcp_host: host.map(Into::into),
        tcp_port,
        tcp_protocol: protocol,
    }
}

fn plain(name: &str, port: &str) -> RawPrinterEntry {
    entry(name, port, None, None, None)
}

fn info(name: &str, kind: &str, port: &str, host: Option<&str>) -> PrintQueueInfo {
    PrintQueueInfo {
        name: name.into(),
        kind: kind.into(),
        port_name: port.into(),
        host: host.map(Into::into),
        port: host.map(|_| 9100),
    }
}

#[test]
fn classify_sorts_queues_by_port() {
    let cases = [
        (
            "IP_192.168.1.50",
            Some("192.168.1.50"),
            Some(9100),
            Some(1),
            print_queue_kinds::TCP,
        ),
        (
            "IP_192.168.1.50",
            Some("192.168.1.50"),
            Some(9100),
            None,
            print_queue_kinds::TCP,
        ),
        (
            "IP_192.168.1.50",
            Some("192.168.1.50"),
            Some(515),
            Some(2),
            print_queue_kinds::OTHER,
        ),
        ("USB001", None, None, None, print_queue_kinds::USB),
        ("WSD-1234", None, None, None, print_queue_kinds::WSD),
        ("LPT1:", None, None, None, print_queue_kinds::OTHER),
    ];
    for (port, host, tcp_port, protocol, kind) in cases {
        assert_eq!(
            classify(&entry("Bar", port, host, tcp_port, protocol)).kind,
            kind,
            "{port}"
        );
    }
}

#[test]
fn host_networks_allows_only_private_literal_hosts() {
    let nets = host_networks(&[
        info("a", print_queue_kinds::TCP, "IP_a", Some("10.0.5.7")),
        info("b", print_queue_kinds::TCP, "IP_b", Some("8.8.8.8")),
        info("c", print_queue_kinds::TCP, "IP_c", Some("printer.local")),
        info("d", print_queue_kinds::USB, "USB001", None),
    ]);
    assert_eq!(nets, [IpNetwork::parse("10.0.5.7/32").unwrap()]);
}

#[test]
fn run_reports_every_queue_and_allows_the_spooler_ones() {
    let env = Env::new();
    env.set(vec![
        entry(
            "Kitchen",
            "IP_192.168.1.50",
            Some("192.168.1.50"),
            Some(9100),
            Some(1),
        ),
        plain("Cashier", "USB001"),
        plain("Fax", "LPT1:"),
    ]);
    *env.cloud.filled.lock().unwrap() = vec!["Kitchen -> 192.168.1.50:9100".into()];

    let queues = env.discovery(true).run(&Ctx::background()).unwrap();

    assert_eq!(queues.len(), 3);
    let reports = env.cloud.reports.lock().unwrap().clone();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].agent_id, env.agent_id.to_string());
    assert_eq!(reports[0].spooler_printing, Some(true));
    assert_eq!(
        env.events.codes(),
        [
            "PRINT_QUEUES_DISCOVERED",
            "PRINT_QUEUES_SPOOLER_READY",
            "PRINT_QUEUES_UNSUPPORTED",
            "PRINT_QUEUES_AUTOFILLED"
        ]
    );
    assert_eq!(env.events.list(1, "queues"), ["Cashier"]);
    assert_eq!(
        env.events.reported.lock().unwrap()[2].0,
        site_event_levels::WARN
    );
    assert_eq!(env.events.list(2, "queues"), ["Fax"]);
    assert_eq!(
        env.events.reported.lock().unwrap()[0].2,
        "Found 3 print queue(s) on this server: 1 on a TCP/IP port, 1 on USB or WSD, 1 on another port"
    );
    assert_eq!(env.events.list(0, "tcp"), ["Kitchen = 192.168.1.50:9100"]);
    assert_eq!(env.events.list(0, "spooler"), ["Cashier (usb, USB001)"]);
    assert_eq!(env.events.list(0, "other"), ["Fax (LPT1:)"]);
    assert_eq!(
        env.spooler_queues.try_resolve("spooler:cashier").as_deref(),
        Some("Cashier")
    );
    assert_eq!(env.spooler_queues.try_resolve("spooler:Fax"), None);
}

#[test]
fn run_without_spooler_printing_warns_about_usb_queues_and_does_not_claim_the_capability() {
    let env = Env::new();
    env.set(vec![plain("Cashier", "USB001")]);

    env.discovery(false).run(&Ctx::background()).unwrap();

    let reports = env.cloud.reports.lock().unwrap().clone();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].spooler_printing, None);
    assert_eq!(
        env.events.codes(),
        ["PRINT_QUEUES_DISCOVERED", "PRINT_QUEUES_UNSUPPORTED"]
    );
}

#[test]
fn run_refreshes_the_allow_list_on_every_read() {
    let env = Env::new();
    env.set(vec![plain("Cashier", "USB001")]);
    let d = env.discovery(true);
    d.run(&Ctx::background()).unwrap();
    env.set(vec![plain("Cashier", "USB001"), plain("Bar", "WSD-77")]);
    d.run(&Ctx::background()).unwrap();

    assert_eq!(env.spooler_queues.count(), 2);
    assert!(env.spooler_queues.try_resolve("spooler:Bar").is_some());
}

#[test]
fn run_does_not_repeat_an_unchanged_report() {
    let env = Env::new();
    env.set(vec![entry(
        "Kitchen",
        "IP_1",
        Some("192.168.1.50"),
        Some(9100),
        Some(1),
    )]);
    let d = env.discovery(true);
    d.run(&Ctx::background()).unwrap();
    d.run(&Ctx::background()).unwrap();
    assert_eq!(env.cloud.reports.lock().unwrap().len(), 1);
}

#[test]
fn run_retries_a_report_the_cloud_did_not_take() {
    let env = Env::new();
    env.set(vec![entry(
        "Kitchen",
        "IP_1",
        Some("192.168.1.50"),
        Some(9100),
        Some(1),
    )]);
    *env.cloud.fail.lock().unwrap() = Some("HTTP 503".into());
    let d = env.discovery(true);
    d.run(&Ctx::background()).unwrap();
    *env.cloud.fail.lock().unwrap() = None;
    d.run(&Ctx::background()).unwrap();

    assert_eq!(env.cloud.reports.lock().unwrap().len(), 2);
    assert!(
        env.events
            .codes()
            .contains(&"PRINT_QUEUES_REPORT_FAILED".to_owned())
    );
    let r = env.events.reported.lock().unwrap();
    let failed = r
        .iter()
        .find(|e| e.1 == "PRINT_QUEUES_REPORT_FAILED")
        .unwrap();
    assert_eq!(
        failed.2,
        "The print queue list could not be sent to the cloud: HTTP 503"
    );
}

#[test]
fn run_reports_a_queue_list_that_cannot_be_read() {
    let env = Env::new();
    *env.source.fail.lock().unwrap() = Some(QueueReadError {
        type_name: "UnauthorizedAccessException",
        message: "denied".into(),
    });

    let queues = env.discovery(true).run(&Ctx::background()).unwrap();

    assert!(queues.is_empty());
    assert!(env.cloud.reports.lock().unwrap().is_empty());
    assert_eq!(env.events.codes(), ["PRINT_QUEUES_DISCOVERY_FAILED"]);
    assert_eq!(
        env.events.reported.lock().unwrap()[0].2,
        "The print queues of this server could not be read (UnauthorizedAccessException); set printer addresses by hand"
    );
}

/// A registry of `path -> (subkeys, values)`.
/// (subkeys, values) of one key.
type FakeKey = (Vec<String>, Vec<(String, RegValue)>);

#[derive(Default)]
struct FakeRegistry {
    keys: HashMap<String, FakeKey>,
}

impl FakeRegistry {
    fn key(&mut self, path: &str, values: Vec<(&str, RegValue)>) {
        let (parent, name) = path.rsplit_once('\\').unwrap();
        if let Some(p) = self.keys.get_mut(parent) {
            p.0.push(name.into());
        }
        self.keys.insert(
            path.into(),
            (
                Vec::new(),
                values.into_iter().map(|(n, v)| (n.into(), v)).collect(),
            ),
        );
    }
}

impl Registry for FakeRegistry {
    fn subkeys(&self, path: &str) -> Result<Option<Vec<String>>, QueueReadError> {
        Ok(self.keys.get(path).map(|k| k.0.clone()))
    }

    fn value(&self, path: &str, name: &str) -> Result<Option<RegValue>, QueueReadError> {
        Ok(self
            .keys
            .get(path)
            .and_then(|k| k.1.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone())))
    }
}

#[test]
fn the_registry_reader_reads_the_spooler_store_like_the_csharp_source() {
    use dl_agent_pda_sync::printrelay::discovery::{PRINTERS_KEY, TCP_PORTS_KEY};
    let s = |v: &str| RegValue::String(v.into());
    let mut r = FakeRegistry::default();
    r.key(PRINTERS_KEY, vec![]);
    r.key(TCP_PORTS_KEY, vec![]);
    r.key(
        &format!(r"{PRINTERS_KEY}\Kitchen"),
        vec![("Port", s("IP_10.0.0.9, IP_2"))],
    );
    r.key(&format!(r"{PRINTERS_KEY}\Bar"), vec![("Port", s("IP_bar"))]);
    r.key(
        &format!(r"{PRINTERS_KEY}\Cashier"),
        vec![("Port", s("USB001"))],
    );
    r.key(
        &format!(r"{PRINTERS_KEY}\Odd"),
        vec![("Port", RegValue::Dword(3))],
    );
    r.key(
        &format!(r"{TCP_PORTS_KEY}\IP_10.0.0.9"),
        vec![
            ("IPAddress", s(" 10.0.0.9 ")),
            ("PortNumber", RegValue::Dword(9100)),
            ("Protocol", RegValue::Dword(1)),
        ],
    );
    r.key(
        &format!(r"{TCP_PORTS_KEY}\IP_bar"),
        vec![
            ("IPAddress", s("  ")),
            ("HostName", s("bar.local")),
            ("PortNumber", s("9100")),
        ],
    );
    let queues = WindowsPrintQueueSource::new(Arc::new(r))
        .read_registry()
        .unwrap();
    assert_eq!(
        queues,
        [
            entry(
                "Kitchen",
                "IP_10.0.0.9, IP_2",
                Some("10.0.0.9"),
                Some(9100),
                Some(1)
            ),
            entry("Bar", "IP_bar", Some("bar.local"), None, None),
            plain("Cashier", "USB001"),
            plain("Odd", ""),
        ]
    );
    assert_eq!(classify(&queues[0]).kind, print_queue_kinds::TCP);
    assert_eq!(classify(&queues[1]).kind, print_queue_kinds::OTHER);
}

#[test]
fn adapter_networks_are_the_private_subnets_of_the_tcpip_interfaces() {
    const IFACES: &str = r"SYSTEM\CurrentControlSet\Services\Tcpip\Parameters\Interfaces";
    let s = |v: &str| RegValue::String(v.into());
    let m = |v: &[&str]| RegValue::MultiString(v.iter().map(|x| (*x).to_owned()).collect());
    let mut r = FakeRegistry::default();
    r.key(IFACES, vec![]);
    r.key(
        &format!(r"{IFACES}\{{a}}"),
        vec![
            ("EnableDHCP", RegValue::Dword(1)),
            ("DhcpIPAddress", s("192.168.1.23")),
            ("DhcpSubnetMask", s("255.255.255.0")),
        ],
    );
    r.key(
        &format!(r"{IFACES}\{{b}}"),
        vec![
            ("EnableDHCP", RegValue::Dword(0)),
            ("IPAddress", m(&["10.1.2.3", "8.8.8.8"])),
            ("SubnetMask", m(&["255.255.0.0", "255.255.255.0"])),
        ],
    );
    r.key(
        &format!(r"{IFACES}\{{c}}"),
        vec![
            ("EnableDHCP", RegValue::Dword(1)),
            ("DhcpIPAddress", s("0.0.0.0")),
            ("DhcpSubnetMask", s("0.0.0.0")),
        ],
    );
    assert_eq!(
        adapter_networks(&r),
        [
            IpNetwork::parse("192.168.1.0/24").unwrap(),
            IpNetwork::parse("10.1.0.0/16").unwrap()
        ]
    );
}
