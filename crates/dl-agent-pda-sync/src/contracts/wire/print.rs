//! Print queue report (`Wire/PrintQueues.cs`, ADR 0098/0105).

use crate::contracts::json::{self, WireJson};
use crate::util::stj::{Obj, ReadError, Writer};

/// How a Windows print queue on the store server reaches its printer (ADR 0098).
pub mod print_queue_kinds {
    /// A Standard TCP/IP port: printed to `host:port` over raw TCP.
    pub const TCP: &str = "tcp";
    /// A USB port: printed RAW through the Windows spooler (ADR 0105).
    pub const USB: &str = "usb";
    /// A WSD port: Windows spooler printing.
    pub const WSD: &str = "wsd";
    /// Any other port monitor.
    pub const OTHER: &str = "other";

    /// True when the relay can print without the spooler.
    pub fn is_raw_tcp(kind: &str) -> bool {
        kind == TCP
    }

    /// True for the queues printed RAW through the spooler: USB and WSD only.
    pub fn is_spooler(kind: &str) -> bool {
        kind == USB || kind == WSD
    }
}

/// The `spooler:<queue name>` printer address form (ADR 0105).
pub mod print_spooler_address {
    /// Prefix.
    pub const PREFIX: &str = "spooler:";

    /// `spooler:` + the trimmed queue name.
    pub fn format(queue: &str) -> String {
        format!("{PREFIX}{}", queue.trim())
    }

    /// Whether the address is a spooler address (prefix case-insensitive, leading spaces
    /// ignored).
    pub fn is_spooler(address: Option<&str>) -> bool {
        address.is_some_and(|a| {
            let t = a.trim_start();
            t.len() >= PREFIX.len()
                && t.is_char_boundary(PREFIX.len())
                && t[..PREFIX.len()].eq_ignore_ascii_case(PREFIX)
        })
    }

    /// The queue name of a spooler address (trimmed, never empty).
    pub fn try_parse(address: Option<&str>) -> Option<String> {
        let a = address?;
        if !is_spooler(Some(a)) {
            return None;
        }
        let q = a.trim_start()[PREFIX.len()..].trim();
        (!q.is_empty()).then(|| q.to_owned())
    }
}

/// Limits the cloud enforces on a report (`PrintQueueJsonContext` constants).
pub mod print_queue_limits {
    /// Most queues per report.
    pub const MAX_QUEUES: usize = 64;
    /// Longest queue name.
    pub const MAX_NAME_LENGTH: usize = 200;
}

/// One local print queue on the store server.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PrintQueueInfo {
    /// Queue name.
    pub name: String,
    /// One of [`print_queue_kinds`].
    pub kind: String,
    /// Windows port name.
    pub port_name: String,
    /// TCP host (TCP queues printing RAW).
    pub host: Option<String>,
    /// TCP port.
    pub port: Option<i32>,
}

impl WireJson for PrintQueueInfo {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("name").string(&self.name);
        w.key("kind").string(&self.kind);
        w.key("port_name").string(&self.port_name);
        json::opt_str(w, "host", &self.host);
        json::opt_int(w, "port", self.port.map(i64::from));
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(PrintQueueInfo {
            name: o.string("name")?,
            kind: o.string("kind")?,
            port_name: o.string("port_name")?,
            host: o.opt_string("host")?,
            port: o.opt_int("port")?,
        })
    }
}

/// Body of `POST /sync/v1/print/queues` (`PrintQueueJsonContext`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PrintQueueReport {
    /// Agent id.
    pub agent_id: String,
    /// Queues.
    pub queues: Vec<PrintQueueInfo>,
    /// True from an agent that prints RAW through the spooler (ADR 0105).
    pub spooler_printing: Option<bool>,
}

impl WireJson for PrintQueueReport {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("agent_id").string(&self.agent_id);
        w.key("queues");
        json::write_items(w, &self.queues);
        json::opt_bool(w, "spooler_printing", self.spooler_printing);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(PrintQueueReport {
            agent_id: o.string("agent_id")?,
            queues: o.list("queues", json::item)?,
            spooler_printing: o.opt_bool("spooler_printing")?,
        })
    }
}

/// A legacy host printer the cloud switched to `tcp` because its name matched a queue.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PrintQueueAutoFill {
    /// Printer id.
    pub printer_id: i32,
    /// RVC.
    pub rvc_no: i32,
    /// Printer name.
    pub name: String,
    /// New address.
    pub address: String,
    /// Matched queue.
    pub queue: String,
}

impl WireJson for PrintQueueAutoFill {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("printer_id").int(self.printer_id.into());
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("name").string(&self.name);
        w.key("address").string(&self.address);
        w.key("queue").string(&self.queue);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(PrintQueueAutoFill {
            printer_id: o.int("printer_id")?,
            rvc_no: o.int("rvc_no")?,
            name: o.string("name")?,
            address: o.string("address")?,
            queue: o.string("queue")?,
        })
    }
}

/// The `data` of the answer to [`PrintQueueReport`] (the API envelope is camelCase: read
/// it with `json::read_with(v, stj::WEB)`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PrintQueueReportResult {
    /// Printers switched.
    pub filled: Vec<PrintQueueAutoFill>,
}

impl WireJson for PrintQueueReportResult {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("filled");
        json::write_items(w, &self.filled);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(PrintQueueReportResult {
            filled: o.list("filled", json::item)?,
        })
    }
}
