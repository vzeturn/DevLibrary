//! Static agent settings, section `Agent` (port of `Config/AgentOptions.cs` and the
//! `PrintRelayOptions` it nests), with the configuration layering of the C# host
//! (`Hosting/Cli.cs`: `Host.CreateApplicationBuilder` plus the site file, environment and
//! command line) and `ConfigurationBinder` conversions.

use super::providers::{ConfigError, Configuration, Provider};
use std::path::{Path, PathBuf};

/// The configuration section the options bind from.
pub const SECTION: &str = "Agent";

/// The .NET application name: `PdaSyncAgent.settings.json` is loaded next to
/// `appsettings.json` by the .NET 10 host defaults.
pub const APPLICATION_NAME: &str = "PdaSyncAgent";

/// The print relay endpoint for the order hub (ADR 0098), section `Agent:PrintRelay`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrintRelayOptions {
    /// Join the order hub's print relay. Default on.
    pub enabled: bool,
    /// CIDR ranges the relay may print to. Empty (the default) allows every private LAN
    /// range: 10/8, 172.16/12, 192.168/16 and fc00::/7 (RT-1309, ADR 0010); entries narrow
    /// that and must themselves be private. Public and loopback addresses are never
    /// allowed. A null entry (JSON `null` in an array) is kept as `None` like the C# list
    /// keeps it, and makes the relay configuration invalid.
    pub printer_subnets: Vec<Option<String>>,
    /// TCP ports a job may target. Default `[9100]`; configured entries are appended
    /// (`ConfigurationBinder` adds to the initialised list).
    pub ports: Vec<i32>,
    /// Folder of the durable job journal; default `<DataDir>\print-queue`.
    pub queue_dir: Option<String>,
    /// Report the Windows print queues to the cloud. Default on.
    pub discover_queues: bool,
    /// Minutes between queue discovery runs (default 360).
    pub discovery_minutes: i32,
}

impl Default for PrintRelayOptions {
    fn default() -> PrintRelayOptions {
        PrintRelayOptions {
            enabled: true,
            printer_subnets: Vec::new(),
            ports: vec![9100],
            queue_dir: None,
            discover_queues: true,
            discovery_minutes: 360,
        }
    }
}

/// Capture switches, section `Agent:Capture` (RT-1318).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureFlags {
    /// `capture.change_tracking` (`Agent:Capture:ChangeTracking`, also read as
    /// `Agent:Capture:change_tracking`): turn SQL Server Change Tracking on for the
    /// captured tables in the grants check (ADR 0011 D2). Default off: while it is off the
    /// agent issues no Change Tracking statement at all.
    pub change_tracking: bool,
}

/// Static agent settings (`AgentOptions`); cloud-pushed values live in
/// [`RuntimeConfig`](super::RuntimeConfig).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentOptions {
    /// Cloud base URL. Normally written by `enroll`.
    pub cloud_url: Option<String>,
    /// State directory. Default [`default_data_dir`]; `None` only when configured as null.
    pub data_dir: Option<String>,
    /// Seconds between heartbeats (15).
    pub heartbeat_seconds: i32,
    /// Outbox size cap in bytes (2 GiB).
    pub outbox_cap_bytes: i64,
    /// Time zone of SMILE_POS's naive datetimes (Windows or IANA id); `None` = the machine's.
    pub site_time_zone: Option<String>,
    /// Batches uploaded per cycle (8).
    pub upload_batches_per_cycle: i32,
    /// Minutes a stream may wait for parent rows before escalating (15).
    pub parent_pending_escalate_minutes: i32,
    /// Blob queue size cap in bytes (512 MiB).
    pub blob_queue_cap_bytes: i64,
    /// Blobs uploaded per cycle (4).
    pub upload_blobs_per_cycle: i32,
    /// Outbox depth above which `UPLOAD_BACKLOG` is reported (500).
    pub upload_backlog_alert_batches: i32,
    /// Websocket failures before the command channel falls back to long-poll (3).
    pub web_socket_failures_before_poll: i32,
    /// Commands of different aggregates run side by side on at most this many workers, each
    /// with its own store connection; one aggregate's commands stay serial (8, RT-1318).
    pub command_workers: i32,
    /// Streams whose next batch is posted at the same time; one stream stays in seq order
    /// (4, RT-1318).
    pub upload_parallel_streams: i32,
    /// Self-update (ADR 0097). Default on.
    pub auto_update: bool,
    /// Minutes between self-update checks (45).
    pub auto_update_check_minutes: i32,
    /// The print relay endpoint.
    pub print_relay: PrintRelayOptions,
    /// Capture switches.
    pub capture: CaptureFlags,
}

impl Default for AgentOptions {
    fn default() -> AgentOptions {
        AgentOptions {
            cloud_url: None,
            data_dir: Some(default_data_dir().to_string_lossy().into_owned()),
            heartbeat_seconds: 15,
            outbox_cap_bytes: 2 * 1024 * 1024 * 1024,
            site_time_zone: None,
            upload_batches_per_cycle: 8,
            parent_pending_escalate_minutes: 15,
            blob_queue_cap_bytes: 512 * 1024 * 1024,
            upload_blobs_per_cycle: 4,
            upload_backlog_alert_batches: 500,
            web_socket_failures_before_poll: 3,
            command_workers: 8,
            upload_parallel_streams: 4,
            auto_update: true,
            auto_update_check_minutes: 45,
            print_relay: PrintRelayOptions::default(),
            capture: CaptureFlags::default(),
        }
    }
}

/// `Environment.GetFolderPath(SpecialFolder.CommonApplicationData)`: `C:\ProgramData` on
/// Windows (the `ProgramData` known folder), `/usr/share` elsewhere.
pub fn common_application_data() -> PathBuf {
    #[cfg(windows)]
    {
        use dl_winos::registry::{Hive, Key, View, expand};
        let from_registry = Key::open(
            Hive::LocalMachine,
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList",
            View::Default,
        )
        .and_then(|k| k.string("ProgramData"))
        .ok()
        .flatten()
        .and_then(|v| expand(&v).ok())
        .filter(|v| !v.is_empty());
        if let Some(v) = from_registry {
            return PathBuf::from(v);
        }
        match std::env::var_os("ProgramData") {
            Some(v) if !v.is_empty() => PathBuf::from(v),
            _ => PathBuf::from(r"C:\ProgramData"),
        }
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/usr/share")
    }
}

/// `%ProgramData%\MM\SyncAgent`.
pub fn default_data_dir() -> PathBuf {
    common_application_data().join("MM").join("SyncAgent")
}

/// Site overrides kept across upgrades: `%ProgramData%\MM\SyncAgent\appsettings.site.json`.
pub fn site_settings_file() -> PathBuf {
    default_data_dir().join("appsettings.site.json")
}

/// The folder of the running executable (`AppContext.BaseDirectory`), or the working
/// directory when it cannot be determined.
pub fn base_directory() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default()
}

/// The configuration of the service and the `enroll`/`grants`/`diag`/`status`/`run` verbs,
/// in the C# provider order (lowest priority first):
/// `DOTNET_`-prefixed environment, command line (host settings), `appsettings.json`,
/// `appsettings.{env}.json`, `PdaSyncAgent.settings.json`,
/// `PdaSyncAgent.settings.{env}.json` (all in `content_root`), environment, command line,
/// then the site file, environment and command line again. `{env}` is the host
/// `environment` key (`DOTNET_ENVIRONMENT`, `--environment`), default `Production`.
pub fn host_configuration<S: AsRef<str>>(
    args: &[S],
    content_root: &Path,
    site_file: &Path,
) -> Result<Configuration, ConfigError> {
    host_configuration_with(args, content_root, site_file, &|prefix| {
        Provider::environment(prefix)
    })
}

/// [`host_configuration`] with an injectable environment (tests).
pub fn host_configuration_with<S: AsRef<str>>(
    args: &[S],
    content_root: &Path,
    site_file: &Path,
    env: &dyn Fn(&str) -> Provider,
) -> Result<Configuration, ConfigError> {
    let mut c = Configuration::new();
    c.add(env("DOTNET_"));
    let cmd = Provider::command_line(args)?;
    c.add(cmd.clone());
    let env_name = c
        .get("environment")
        .filter(|s| !s.is_empty())
        .unwrap_or("Production")
        .to_owned();
    let app_name = c
        .get("applicationName")
        .filter(|s| !s.is_empty())
        .unwrap_or(APPLICATION_NAME)
        .to_owned();
    for name in [
        "appsettings.json".to_owned(),
        format!("appsettings.{env_name}.json"),
        format!("{app_name}.settings.json"),
        format!("{app_name}.settings.{env_name}.json"),
    ] {
        if let Some(p) = Provider::json_file(&content_root.join(name), true)? {
            c.add(p);
        }
    }
    c.add(env(""));
    c.add(cmd.clone());
    if let Some(p) = Provider::json_file(site_file, true)? {
        c.add(p);
    }
    c.add(env(""));
    c.add(cmd);
    Ok(c)
}

/// The configuration a legacy verb reads to find the configured data folder
/// (`Cli.LegacyRun`): `appsettings.json` in `base_dir`, then the environment.
pub fn legacy_tool_configuration(base_dir: &Path) -> Result<Configuration, ConfigError> {
    let mut c = Configuration::new();
    if let Some(p) = Provider::json_file(&base_dir.join("appsettings.json"), true)? {
        c.add(p);
    }
    c.add(Provider::environment(""));
    Ok(c)
}

impl AgentOptions {
    /// `AgentHost.LoadOptions`: the defaults with section `Agent` bound over them.
    pub fn load(configuration: &Configuration) -> Result<AgentOptions, ConfigError> {
        let mut o = AgentOptions::default();
        let b = Binder { c: configuration };
        let s = SECTION;
        b.string(&format!("{s}:CloudUrl"), &mut o.cloud_url);
        b.string(&format!("{s}:DataDir"), &mut o.data_dir);
        b.int(&format!("{s}:HeartbeatSeconds"), &mut o.heartbeat_seconds)?;
        b.long(&format!("{s}:OutboxCapBytes"), &mut o.outbox_cap_bytes)?;
        b.string(&format!("{s}:SiteTimeZone"), &mut o.site_time_zone);
        b.int(
            &format!("{s}:UploadBatchesPerCycle"),
            &mut o.upload_batches_per_cycle,
        )?;
        b.int(
            &format!("{s}:ParentPendingEscalateMinutes"),
            &mut o.parent_pending_escalate_minutes,
        )?;
        b.long(
            &format!("{s}:BlobQueueCapBytes"),
            &mut o.blob_queue_cap_bytes,
        )?;
        b.int(
            &format!("{s}:UploadBlobsPerCycle"),
            &mut o.upload_blobs_per_cycle,
        )?;
        b.int(
            &format!("{s}:UploadBacklogAlertBatches"),
            &mut o.upload_backlog_alert_batches,
        )?;
        b.int(
            &format!("{s}:WebSocketFailuresBeforePoll"),
            &mut o.web_socket_failures_before_poll,
        )?;
        b.int(&format!("{s}:CommandWorkers"), &mut o.command_workers)?;
        b.int(
            &format!("{s}:UploadParallelStreams"),
            &mut o.upload_parallel_streams,
        )?;
        b.bool(&format!("{s}:AutoUpdate"), &mut o.auto_update)?;
        b.int(
            &format!("{s}:AutoUpdateCheckMinutes"),
            &mut o.auto_update_check_minutes,
        )?;
        let p = format!("{s}:PrintRelay");
        let pr = &mut o.print_relay;
        b.bool(&format!("{p}:Enabled"), &mut pr.enabled)?;
        b.string_list(&format!("{p}:PrinterSubnets"), &mut pr.printer_subnets);
        b.int_list(&format!("{p}:Ports"), &mut pr.ports);
        b.string(&format!("{p}:QueueDir"), &mut pr.queue_dir);
        b.bool(&format!("{p}:DiscoverQueues"), &mut pr.discover_queues)?;
        b.int(&format!("{p}:DiscoveryMinutes"), &mut pr.discovery_minutes)?;
        let c = format!("{s}:Capture");
        b.bool(
            &format!("{c}:ChangeTracking"),
            &mut o.capture.change_tracking,
        )?;
        b.bool(
            &format!("{c}:change_tracking"),
            &mut o.capture.change_tracking,
        )?;
        Ok(o)
    }

    /// The data directory as a path (`Path.Combine` of a null `DataDir` throws in C#: an
    /// error here).
    pub fn data_dir_path(&self) -> Result<PathBuf, ConfigError> {
        self.data_dir
            .as_deref()
            .map(PathBuf::from)
            .ok_or_else(|| ConfigError::Convert {
                value: String::new(),
                path: format!("{SECTION}:DataDir"),
                type_name: "System.String",
            })
    }
}

/// What a property's configuration section holds.
enum Slot<'a> {
    /// Nothing (or only children for a scalar): the property keeps its value.
    Keep,
    /// Present with a null value and no children: `default(T)`.
    Null,
    /// A value to convert.
    Text(&'a str),
}

struct Binder<'a> {
    c: &'a Configuration,
}

impl<'a> Binder<'a> {
    fn slot(&self, path: &str) -> Slot<'a> {
        match self.c.lookup(path) {
            Some(Some(v)) => Slot::Text(v),
            Some(None) if !self.c.has_children(path) => Slot::Null,
            _ => Slot::Keep,
        }
    }

    fn string(&self, path: &str, target: &mut Option<String>) {
        match self.slot(path) {
            Slot::Keep => {}
            Slot::Null => *target = None,
            Slot::Text(v) => *target = Some(v.to_owned()),
        }
    }

    fn int(&self, path: &str, target: &mut i32) -> Result<(), ConfigError> {
        match self.slot(path) {
            Slot::Keep => {}
            Slot::Null => *target = 0,
            Slot::Text(v) => {
                *target = convert_i32(v).ok_or_else(|| convert_error(v, path, "System.Int32"))?
            }
        }
        Ok(())
    }

    fn long(&self, path: &str, target: &mut i64) -> Result<(), ConfigError> {
        match self.slot(path) {
            Slot::Keep => {}
            Slot::Null => *target = 0,
            Slot::Text(v) => {
                *target = convert_i64(v).ok_or_else(|| convert_error(v, path, "System.Int64"))?
            }
        }
        Ok(())
    }

    fn bool(&self, path: &str, target: &mut bool) -> Result<(), ConfigError> {
        match self.slot(path) {
            Slot::Keep => {}
            Slot::Null => *target = false,
            Slot::Text(v) => {
                *target = convert_bool(v).ok_or_else(|| convert_error(v, path, "System.Boolean"))?
            }
        }
        Ok(())
    }

    /// Collection items: converted and appended in key order; an item that fails to convert
    /// or has children is skipped (the binder swallows item errors); a null item is
    /// `default(T)`.
    fn items(&self, path: &str) -> Vec<Slot<'a>> {
        self.c
            .children(path)
            .into_iter()
            .map(|k| self.slot(&format!("{path}:{k}")))
            .collect()
    }

    fn string_list(&self, path: &str, target: &mut Vec<Option<String>>) {
        for item in self.items(path) {
            match item {
                Slot::Keep => {}
                Slot::Null => target.push(None),
                Slot::Text(v) => target.push(Some(v.to_owned())),
            }
        }
    }

    fn int_list(&self, path: &str, target: &mut Vec<i32>) {
        for item in self.items(path) {
            match item {
                Slot::Keep => {}
                Slot::Null => target.push(0),
                Slot::Text(v) => {
                    if let Some(n) = convert_i32(v) {
                        target.push(n);
                    }
                }
            }
        }
    }
}

fn convert_error(value: &str, path: &str, type_name: &'static str) -> ConfigError {
    ConfigError::Convert {
        value: value.to_owned(),
        path: path.to_owned(),
        type_name,
    }
}

/// `string.Trim()` then trailing NULs (number parsing accepts them).
fn trim_number(s: &str) -> &str {
    s.trim().trim_end_matches('\0')
}

/// `BaseNumberConverter`: trimmed text; `#`, `0x` or `&h` prefixes parse hexadecimal
/// (`Convert.ToInt32(s, 16)`: unsigned bits, no sign); otherwise `int.Parse` with
/// `NumberStyles.Integer` and the invariant culture.
fn convert_integer(text: &str, bits: u32) -> Option<i64> {
    let t = text.trim();
    let hex = if let Some(rest) = t.strip_prefix('#') {
        Some(rest)
    } else if t
        .get(..2)
        .is_some_and(|p| p.eq_ignore_ascii_case("0x") || p.eq_ignore_ascii_case("&h"))
    {
        Some(&t[2..])
    } else {
        None
    };
    if let Some(h) = hex {
        // Convert.ToInt32(s, 16) accepts one more 0x prefix.
        let h = match h.get(..2) {
            Some(p) if p.eq_ignore_ascii_case("0x") => &h[2..],
            _ => h,
        };
        if h.is_empty() || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let v = u64::from_str_radix(h.trim_start_matches('0'), 16)
            .ok()
            .or_else(|| h.bytes().all(|b| b == b'0').then_some(0))?;
        return if bits == 32 {
            u32::try_from(v).ok().map(|u| i64::from(u as i32))
        } else {
            Some(v as i64)
        };
    }
    let t = trim_number(t);
    let digits = t.strip_prefix(['-', '+']).unwrap_or(t);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v: i128 = t.parse().ok()?;
    let (min, max) = if bits == 32 {
        (i128::from(i32::MIN), i128::from(i32::MAX))
    } else {
        (i128::from(i64::MIN), i128::from(i64::MAX))
    };
    if v < min || v > max {
        return None;
    }
    i64::try_from(v).ok()
}

fn convert_i32(text: &str) -> Option<i32> {
    convert_integer(text, 32).and_then(|v| i32::try_from(v).ok())
}

fn convert_i64(text: &str) -> Option<i64> {
    convert_integer(text, 64)
}

/// `BooleanConverter`: `bool.Parse` of the trimmed text (also trailing/leading NULs).
fn convert_bool(text: &str) -> Option<bool> {
    let t = text
        .trim()
        .trim_matches(|c: char| c == '\0' || c.is_whitespace());
    if t.eq_ignore_ascii_case("true") {
        Some(true)
    } else if t.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(pairs: &[(&str, Option<&str>)]) -> Configuration {
        let mut c = Configuration::new();
        c.add(Provider::memory("m", pairs.iter().copied()));
        c
    }

    fn load(pairs: &[(&str, Option<&str>)]) -> Result<AgentOptions, ConfigError> {
        AgentOptions::load(&mem(pairs))
    }

    #[test]
    fn conversions_match_dotnet() {
        // Expected values: .NET 10 ConfigurationBinder (sa-core check, "CFG" lines).
        let o = load(&[
            ("Agent:HeartbeatSeconds", Some("0xFFFFFFFF")),
            ("Agent:OutboxCapBytes", Some("&h10")),
            ("Agent:UploadBatchesPerCycle", Some(" 0x10 ")),
            ("Agent:UploadBlobsPerCycle", Some(" +7 ")),
            ("Agent:AutoUpdate", Some(" TRUE ")),
            ("Agent:BlobQueueCapBytes", Some("0XFF")),
        ])
        .unwrap();
        assert_eq!(o.heartbeat_seconds, -1);
        assert_eq!(o.outbox_cap_bytes, 16);
        assert_eq!(o.upload_batches_per_cycle, 16);
        assert_eq!(o.upload_blobs_per_cycle, 7);
        assert!(o.auto_update);
        assert_eq!(o.blob_queue_cap_bytes, 255);
        assert!(
            !load(&[("Agent:AutoUpdate", Some("false\0\0"))])
                .unwrap()
                .auto_update
        );

        for (key, value, ty) in [
            ("Agent:HeartbeatSeconds", "abc", "System.Int32"),
            ("Agent:HeartbeatSeconds", "", "System.Int32"),
            ("Agent:HeartbeatSeconds", "99999999999", "System.Int32"),
            ("Agent:HeartbeatSeconds", "1,000", "System.Int32"),
            ("Agent:HeartbeatSeconds", "0x-1", "System.Int32"),
            ("Agent:HeartbeatSeconds", "\u{661}\u{662}", "System.Int32"),
            ("Agent:UploadBlobsPerCycle", "0x", "System.Int32"),
            ("Agent:OutboxCapBytes", "1.0e3", "System.Int64"),
            ("Agent:AutoUpdate", "", "System.Boolean"),
            ("Agent:AutoUpdate", "1", "System.Boolean"),
        ] {
            assert_eq!(
                load(&[(key, Some(value))]).unwrap_err(),
                ConfigError::Convert {
                    value: value.into(),
                    path: key.into(),
                    type_name: ty
                },
                "{key}={value:?}"
            );
        }
        assert_eq!(
            load(&[("Agent:HeartbeatSeconds", Some("abc"))])
                .unwrap_err()
                .to_string(),
            "Failed to convert configuration value 'abc' at 'Agent:HeartbeatSeconds' to type 'System.Int32'."
        );
    }

    #[test]
    fn change_tracking_is_off_unless_configured() {
        // RT-1318 brief: the flag capture.change_tracking defaults to false.
        assert!(!load(&[]).unwrap().capture.change_tracking);
        for key in [
            "Agent:Capture:ChangeTracking",
            "agent:capture:changetracking",
            "Agent:Capture:change_tracking",
        ] {
            assert!(
                load(&[(key, Some("true"))])
                    .unwrap()
                    .capture
                    .change_tracking,
                "{key}"
            );
        }
        assert!(
            !load(&[("Agent:Capture:ChangeTracking", Some("False"))])
                .unwrap()
                .capture
                .change_tracking
        );
    }

    #[test]
    fn nulls_and_collections_match_dotnet() {
        let o = load(&[
            ("Agent:HeartbeatSeconds", None),
            ("Agent:AutoUpdate", None),
            ("Agent:PrintRelay:Enabled", None),
            ("Agent:DataDir", None),
            ("Agent:CloudUrl", Some("")),
        ])
        .unwrap();
        assert_eq!(o.heartbeat_seconds, 0);
        assert!(!o.auto_update);
        assert!(!o.print_relay.enabled);
        assert_eq!(o.data_dir, None);
        assert_eq!(o.cloud_url.as_deref(), Some(""));

        let mut c = Configuration::new();
        c.add(
            Provider::json_text(
                "j",
                r#"{"Agent":{"HeartbeatSeconds":{"a":1},"PrintRelay":{"Ports":[1,"x",null,{},3],"PrinterSubnets":["a",null,{"b":1},["c"],"d"],"QueueDir":[]}}}"#,
            )
            .unwrap(),
        );
        let o = AgentOptions::load(&c).unwrap();
        assert_eq!(o.heartbeat_seconds, 15);
        assert_eq!(o.print_relay.ports, [9100, 1, 0, 0, 3]);
        assert_eq!(
            o.print_relay.printer_subnets,
            [Some("a".to_owned()), None, Some("d".to_owned())]
        );
        assert_eq!(o.print_relay.queue_dir.as_deref(), Some(""));
    }

    #[test]
    fn host_layering_matches_dotnet() {
        let dir =
            std::env::temp_dir().join(format!("pda-cfg-{}", dl_uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("appsettings.json"),
            "{ // comment\n \"Agent\": { \"HeartbeatSeconds\": 20, /* x */ \"AutoUpdate\": \"False\",\n \"PrintRelay\": { \"Ports\": [9101, \"x\", 9102], \"PrinterSubnets\": [\"10.0.0.0/8\"], \"Enabled\": false, },\n \"CloudUrl\": null, \"SiteTimeZone\": \"\", \"BlobQueueCapBytes\": 1000 }, }",
        )
        .unwrap();
        let site = dir.join("site.json");
        std::fs::write(
            &site,
            "{\"agent\": {\"heartbeatseconds\": \"25\", \"DataDir\": \"C:\\\\Site\", \"PrintRelay\": {\"Ports\": [1, 2]}}}",
        )
        .unwrap();
        std::fs::write(
            dir.join("PdaSyncAgent.settings.Staging.json"),
            "{\"Agent\":{\"ParentPendingEscalateMinutes\":9}}",
        )
        .unwrap();
        let env = |prefix: &str| {
            Provider::environment_from(
                prefix,
                [
                    ("Agent__UploadBatchesPerCycle", " 0x10 "),
                    ("agent__uploadblobspercycle", " +7 "),
                    ("DOTNET_ENVIRONMENT", "Staging"),
                    ("DOTNET_Agent__UploadBacklogAlertBatches", "77"),
                ],
            )
        };
        let args = [
            "enroll",
            "--Agent:DataDir",
            "D:\\x",
            "/Agent:WebSocketFailuresBeforePoll=7",
            "--Agent:CommandWorkers=3",
            "--agent:HEARTBEATSECONDS=30",
            "--Agent:AutoUpdateCheckMinutes",
        ];
        let c = host_configuration_with(&args, &dir, &site, &env).unwrap();
        let o = AgentOptions::load(&c).unwrap();
        // .NET 10 ("CFG layered"): {"CloudUrl":null,"DataDir":"D:\\x","HeartbeatSeconds":30,
        // "SiteTimeZone":"","UploadBatchesPerCycle":16,"BlobQueueCapBytes":1000,
        // "UploadBlobsPerCycle":7,"WebSocketFailuresBeforePoll":7,"AutoUpdate":false,
        // "PrintRelay":{"Enabled":false,"PrinterSubnets":["10.0.0.0/8"],"Ports":[9100,1,2,9102]}}
        assert_eq!(o.cloud_url, None);
        assert_eq!(o.data_dir.as_deref(), Some("D:\\x"));
        assert_eq!(o.heartbeat_seconds, 30);
        assert_eq!(o.site_time_zone.as_deref(), Some(""));
        assert_eq!(o.upload_batches_per_cycle, 16);
        assert_eq!(o.blob_queue_cap_bytes, 1000);
        assert_eq!(o.upload_blobs_per_cycle, 7);
        assert_eq!(o.web_socket_failures_before_poll, 7);
        assert_eq!(o.command_workers, 3);
        assert_eq!(o.upload_parallel_streams, 4);
        assert!(!o.auto_update);
        assert_eq!(o.auto_update_check_minutes, 45);
        assert!(!o.print_relay.enabled);
        assert_eq!(
            o.print_relay.printer_subnets,
            [Some("10.0.0.0/8".to_owned())]
        );
        assert_eq!(o.print_relay.ports, [9100, 1, 2, 9102]);
        assert_eq!(o.parent_pending_escalate_minutes, 9);
        assert_eq!(o.upload_backlog_alert_batches, 77);
        std::fs::write(&site, "{\"Agent\":{\"A\":1,\"a\":2}}").unwrap();
        let err = host_configuration_with(&args, &dir, &site, &env).unwrap_err();
        assert!(
            err.to_string()
                .contains("A duplicate key 'Agent:a' was found."),
            "{err}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn defaults_match_dotnet() {
        let o = AgentOptions::default();
        assert_eq!(o.heartbeat_seconds, 15);
        assert_eq!(o.outbox_cap_bytes, 2_147_483_648);
        assert_eq!(o.blob_queue_cap_bytes, 536_870_912);
        assert_eq!(o.print_relay.ports, [9100]);
        if cfg!(windows) {
            assert!(site_settings_file().ends_with(r"MM\SyncAgent\appsettings.site.json"));
        } else {
            assert_eq!(default_data_dir(), PathBuf::from("/usr/share/MM/SyncAgent"));
        }
    }
}
