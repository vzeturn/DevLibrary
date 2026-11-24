//! The `modules.membership` section of the dl-agent remote config.
//!
//! ```json
//! {"modules": {"membership": {
//!     "enabled": true,
//!     "program_data_dir": "C:\\ProgramData\\MM.Membership.Agent",
//!     "read_only": true
//! }}}
//! ```
//!
//! `read_only` (default `true`) keeps the M1 behaviour: SMILE is only read and pulled jobs
//! are never acked. With `read_only: false` (and `pull_jobs: true`) the agent runs the hub's
//! jobs against SMILE and acks them, like the .NET service. Only one writer may run per
//! hotel: stop the .NET `MM.Membership.Agent` service before turning writes on.
//!
//! ```text
//! write keys: write_lock_timeout_ms (10000), default_payment_code ("1", "" = not written),
//! default_breakfast_code ("BFF", "" = not written), extra_bed_write_mode (SMILE_SPECIAL)
//! ```
//!
//! `enabled` is read by the supervisor (the module is opt-in). The other keys mirror the
//! .NET `appsettings.json` sections (`Worker`, `CheckoutScan`, `AvailabilityScan`,
//! `FolioObservationScan`, `Smile`, `Earn`) with the same defaults; absent or wrongly typed
//! keys take the default. The secrets (hub token, SMILE connection string) are never here:
//! they stay in `config.dpapi`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use dl_json::Value;

use crate::config;
use crate::jobs::beds::{MODE_FOLD_LEGACY, MODE_SMILE_SPECIAL};

/// The .NET `FullCapabilities`: what the agent tells the hub it understands.
pub const FULL_CAPABILITIES: [&str; 7] = [
    "STRUCTURED_ERRORS",
    "DB_IDENTITY",
    "WRITE_GATE",
    "LINK_FOLIO",
    "ATOMIC_MULTI_ROOM",
    "EXPECTED_FOLIOS",
    "EXTRA_BED_SPECIAL_V1",
];

/// The hub caps one folio-observation batch at 500 (a larger one is rejected with 400).
pub const MAX_OBSERVATION_BATCH: usize = 500;

/// The module's settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Where `config.dpapi` and the cursor files live.
    pub program_data_dir: PathBuf,
    /// `true`: SMILE is only read and pulled jobs are never acked (M1). `false`: jobs run.
    pub read_only: bool,
    /// Pull jobs to count them (they are leased and left to expire, never acked).
    pub pull_jobs: bool,
    /// `Worker:MaxJobsPerPull`.
    pub max_jobs_per_pull: u32,
    /// `Worker:HeartbeatSeconds`.
    pub heartbeat: Duration,
    /// The capabilities sent in the heartbeat.
    pub capabilities: Vec<String>,
    /// The smallest gap between two reports with the same code.
    pub report_interval: Duration,
    /// `FolioObservationScan`.
    pub folio: FolioSettings,
    /// `AvailabilityScan`.
    pub availability: AvailabilitySettings,
    /// The room inventory scan.
    pub rooms: RoomSettings,
    /// `Smile`, write side.
    pub write: WriteSettings,
    /// `Smile` and `Earn` (read side only).
    pub smile: SmileSettings,
    /// `CheckoutScan`.
    pub checkout: crate::checkout::CheckoutSettings,
}

/// `FolioObservationScan`.
#[derive(Debug, Clone, PartialEq)]
pub struct FolioSettings {
    /// `Enabled`.
    pub enabled: bool,
    /// `IntervalSeconds`.
    pub interval: Duration,
    /// `MaxPerScan`, capped at [`MAX_OBSERVATION_BATCH`].
    pub max_per_scan: usize,
    /// `OverlapMinutes`.
    pub overlap: Duration,
    /// `ColdStartLookbackDays`.
    pub cold_start_lookback_days: i64,
}

/// `AvailabilityScan`.
#[derive(Debug, Clone, PartialEq)]
pub struct AvailabilitySettings {
    /// `ProbeIntervalSeconds`.
    pub probe_interval: Duration,
    /// `ProbeIdleMaxSeconds` (RT-0756): the probe interval doubles after each probe that saw
    /// no change, up to this ceiling, and drops back to `probe_interval` on a change or a
    /// skipped (lock-busy) tick. Clamped to `probe_interval`..=60 s; equal to it turns the
    /// adaptive pace off.
    pub probe_idle_max: Duration,
    /// `HeartbeatMinutes`, clamped to 1–5.
    pub heartbeat: Duration,
    /// `HorizonDays`.
    pub horizon_days: i64,
    /// `FullPushEveryScans`.
    pub full_push_every: u32,
    /// `MaxFolios`.
    pub max_folios: i64,
}

impl AvailabilitySettings {
    /// `ResolvedProbeInterval`: at least 1 s.
    pub fn resolved_probe_interval(&self) -> Duration {
        self.probe_interval.max(Duration::from_secs(1))
    }

    /// `ResolvedProbeIdleMax`: `probe_idle_max` clamped to `resolved_probe_interval()..=60 s`.
    pub fn resolved_probe_idle_max(&self) -> Duration {
        let base = self.resolved_probe_interval();
        self.probe_idle_max
            .clamp(base, Duration::from_secs(60).max(base))
    }

    /// `ProbeDelayAfterIdle`: the next probe after `idle_probes` probes in a row saw no change
    /// (`interval × 2^idle`, the exponent capped at 6, then at the idle ceiling).
    pub fn probe_delay_after_idle(&self, idle_probes: u32) -> Duration {
        self.resolved_probe_interval()
            .saturating_mul(1 << idle_probes.min(6))
            .min(self.resolved_probe_idle_max())
    }
}

/// The room inventory scan.
#[derive(Debug, Clone, PartialEq)]
pub struct RoomSettings {
    /// How often rooms and the room-type catalogue are read.
    pub interval: Duration,
    /// Full push every this many scans (deltas in between).
    pub full_push_every: u32,
}

/// `Smile` and `Earn`, read side.
#[derive(Debug, Clone, PartialEq)]
pub struct SmileSettings {
    /// `Smile:ExtraBedSpecialCode` (the hub's choice from sync-config wins).
    pub extra_bed_special_code: String,
    /// `CheckoutScan:GuestPhoneColumn`, a column of `dbo.AdditionalName`.
    pub guest_phone_column: String,
    /// `Earn:ChargeCodes`: an emergency override of the derived earn pairs.
    pub earn_charge_codes: Vec<String>,
    /// `Earn:MemberDiscountCode` (empty: disabled).
    pub member_discount_code: String,
}

/// `SmileWriteOptions`.
#[derive(Debug, Clone, PartialEq)]
pub struct WriteSettings {
    /// `Smile:WriteLockTimeoutMs` (non-positive: the default 10000).
    pub write_lock_timeout_ms: i32,
    /// `Smile:DefaultPaymentCode` (`None`: `Folio.PaymentCode` is not written).
    pub default_payment_code: Option<String>,
    /// `Smile:DefaultBreakfastCode` (`None`: neither `BreakfastCode` nor `IncludeBF`).
    pub default_breakfast_code: Option<String>,
    /// `Smile:ExtraBedWriteMode`: `FOLD_LEGACY` or `SMILE_SPECIAL`.
    pub extra_bed_mode: String,
    /// `ExtraBedModeProblem`: set when the configured mode was not understood.
    pub extra_bed_mode_problem: Option<String>,
    /// `Smile:SmileHotelId`: the SMILE `HotelID` whose extra-bed terms are read (`None`:
    /// the only one SMILE has).
    pub smile_hotel_id: Option<String>,
}

/// `ExtraBedMode`'s setter: an unknown value falls back to `FOLD_LEGACY` with a problem.
fn extra_bed_mode(v: &Value) -> (String, Option<String>) {
    let raw = v
        .get("extra_bed_write_mode")
        .and_then(Value::as_str)
        .unwrap_or(MODE_SMILE_SPECIAL);
    let mode = raw.trim().to_uppercase();
    if mode == MODE_FOLD_LEGACY || mode == MODE_SMILE_SPECIAL {
        return (mode, None);
    }
    let problem = (!raw.trim().is_empty()).then(|| {
        format!(
            "Smile:ExtraBedWriteMode = '{}' is not valid (only {MODE_FOLD_LEGACY} | {MODE_SMILE_SPECIAL}); the agent uses {MODE_FOLD_LEGACY}.",
            raw.trim()
        )
    });
    (MODE_FOLD_LEGACY.to_owned(), problem)
}

/// A code key: absent takes `default`; present but blank means "not written".
fn code_or(v: &Value, key: &str, default: &str) -> Option<String> {
    match v.get(key) {
        None | Some(Value::Null) => Some(default.to_owned()),
        Some(x) => x
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
    }
}

fn str_or(v: &Value, key: &str, default: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default)
        .to_owned()
}

fn uint(v: &Value, key: &str, default: u64) -> u64 {
    v.get(key)
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

fn flag(v: &Value, key: &str, default: bool) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn strings(v: &Value, key: &str) -> Option<Vec<String>> {
    v.get(key).and_then(Value::as_array).map(|a| {
        a.iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    })
}

impl Settings {
    /// Reads the section. `data_dir` is the module's own directory, used off Windows when
    /// `program_data_dir` is not set.
    pub fn from_config(v: &Value, data_dir: &Path) -> Settings {
        let program_data_dir = v
            .get("program_data_dir")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map_or_else(|| config::default_dir(data_dir), PathBuf::from);
        let secs = |key, d| Duration::from_secs(uint(v, key, d));
        let (bed_mode, bed_problem) = extra_bed_mode(v);
        let read_only = flag(v, "read_only", true);
        Settings {
            program_data_dir,
            read_only,
            checkout: crate::checkout::CheckoutSettings::from_config(v, read_only),
            pull_jobs: flag(v, "pull_jobs", false),
            max_jobs_per_pull: u32::try_from(uint(v, "max_jobs_per_pull", 20)).unwrap_or(20),
            heartbeat: secs("heartbeat_seconds", 30),
            capabilities: strings(v, "capabilities")
                .unwrap_or_else(|| FULL_CAPABILITIES.iter().map(|s| (*s).to_owned()).collect()),
            report_interval: Duration::from_secs(60 * uint(v, "report_interval_minutes", 15)),
            folio: FolioSettings {
                enabled: flag(v, "folio_scan_enabled", true),
                interval: secs("folio_interval_seconds", 60),
                max_per_scan: usize::try_from(uint(v, "folio_max_per_scan", 500))
                    .unwrap_or(MAX_OBSERVATION_BATCH)
                    .min(MAX_OBSERVATION_BATCH),
                overlap: Duration::from_secs(60 * uint(v, "folio_overlap_minutes", 15)),
                cold_start_lookback_days: i64::try_from(uint(
                    v,
                    "folio_cold_start_lookback_days",
                    7,
                ))
                .unwrap_or(7),
            },
            availability: AvailabilitySettings {
                probe_interval: secs("availability_probe_seconds", 5),
                probe_idle_max: secs("availability_probe_idle_max_seconds", 30),
                heartbeat: Duration::from_secs(
                    60 * uint(v, "availability_heartbeat_minutes", 5).clamp(1, 5),
                ),
                horizon_days: i64::try_from(uint(v, "availability_horizon_days", 365))
                    .unwrap_or(365),
                full_push_every: u32::try_from(uint(v, "availability_full_push_every", 24))
                    .unwrap_or(24),
                max_folios: i64::try_from(uint(v, "availability_max_folios", 20_000))
                    .unwrap_or(20_000),
            },
            rooms: RoomSettings {
                interval: secs("rooms_interval_seconds", 300),
                full_push_every: u32::try_from(uint(v, "rooms_full_push_every", 12)).unwrap_or(12),
            },
            smile: SmileSettings {
                extra_bed_special_code: str_or(v, "extra_bed_special_code", "EB"),
                guest_phone_column: str_or(v, "guest_phone_column", "MobileNum"),
                earn_charge_codes: strings(v, "earn_charge_codes").unwrap_or_default(),
                member_discount_code: str_or(v, "member_discount_code", ""),
            },
            write: WriteSettings {
                write_lock_timeout_ms: v
                    .get("write_lock_timeout_ms")
                    .and_then(Value::as_i64)
                    .filter(|n| *n > 0)
                    .and_then(|n| i32::try_from(n).ok())
                    .unwrap_or(10_000),
                default_payment_code: code_or(v, "default_payment_code", "1"),
                default_breakfast_code: code_or(v, "default_breakfast_code", "BFF"),
                extra_bed_mode: bed_mode,
                extra_bed_mode_problem: bed_problem,
                smile_hotel_id: v
                    .get("smile_hotel_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use dl_json::json;

    use super::*;

    #[test]
    fn defaults_follow_the_dotnet_appsettings() {
        let s = Settings::from_config(&Value::Null, Path::new("/data"));
        assert!(s.read_only && !s.pull_jobs);
        assert_eq!(s.heartbeat, Duration::from_secs(30));
        assert_eq!(s.capabilities.len(), FULL_CAPABILITIES.len());
        assert_eq!(s.folio.max_per_scan, 500);
        assert_eq!(s.folio.overlap, Duration::from_secs(900));
        assert_eq!(s.availability.heartbeat, Duration::from_secs(300));
        assert_eq!(s.smile.extra_bed_special_code, "EB");
        assert_eq!(s.smile.guest_phone_column, "MobileNum");
        if !cfg!(windows) {
            assert_eq!(s.program_data_dir, PathBuf::from("/data"));
        }
    }

    #[test]
    fn overrides_are_read_and_clamped() {
        let s = Settings::from_config(
            &json!({
                "program_data_dir": "/pd", "read_only": false, "pull_jobs": true,
                "folio_max_per_scan": 9000, "availability_heartbeat_minutes": 60,
                "capabilities": ["DB_IDENTITY"], "earn_charge_codes": [" RM ", ""],
                "member_discount_code": "MD"
            }),
            Path::new("/data"),
        );
        assert_eq!(s.program_data_dir, PathBuf::from("/pd"));
        assert!(!s.read_only && s.pull_jobs);
        assert_eq!(s.folio.max_per_scan, MAX_OBSERVATION_BATCH);
        assert_eq!(s.availability.heartbeat, Duration::from_secs(300));
        assert_eq!(s.capabilities, vec!["DB_IDENTITY".to_owned()]);
        assert_eq!(s.smile.earn_charge_codes, vec!["RM".to_owned()]);
        assert_eq!(s.smile.member_discount_code, "MD");
    }

    #[test]
    fn probe_backs_off_when_smile_is_idle_and_stays_under_the_ceiling() {
        let d = Settings::from_config(&Value::Null, Path::new("/data")).availability;
        assert_eq!(d.probe_interval, Duration::from_secs(5));
        assert_eq!(d.probe_idle_max, Duration::from_secs(30));
        let delays: Vec<u64> = (0..8)
            .map(|i| d.probe_delay_after_idle(i).as_secs())
            .collect();
        assert_eq!(delays, vec![5, 10, 20, 30, 30, 30, 30, 30]);
        // A ceiling under the interval turns the adaptive pace off; over 60 s is cut to 60.
        let off = Settings::from_config(
            &json!({"availability_probe_idle_max_seconds": 2}),
            Path::new("/d"),
        )
        .availability;
        assert_eq!(off.probe_delay_after_idle(5), Duration::from_secs(5));
        let high = Settings::from_config(
            &json!({"availability_probe_idle_max_seconds": 600}),
            Path::new("/d"),
        )
        .availability;
        assert_eq!(high.probe_delay_after_idle(6), Duration::from_secs(60));
        assert_eq!(
            high.probe_delay_after_idle(u32::MAX),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn write_settings_follow_smile_write_options() {
        let d = Settings::from_config(&Value::Null, Path::new("/data")).write;
        assert_eq!(d.write_lock_timeout_ms, 10_000);
        assert_eq!(d.default_payment_code.as_deref(), Some("1"));
        assert_eq!(d.default_breakfast_code.as_deref(), Some("BFF"));
        assert_eq!(d.extra_bed_mode, MODE_SMILE_SPECIAL);
        assert_eq!(d.extra_bed_mode_problem, None);
        let w = Settings::from_config(
            &json!({"write_lock_timeout_ms": -5, "default_payment_code": " ",
                    "default_breakfast_code": " BF ", "extra_bed_write_mode": " fold_legacy "}),
            Path::new("/data"),
        )
        .write;
        assert_eq!(w.write_lock_timeout_ms, 10_000);
        assert_eq!(w.default_payment_code, None);
        assert_eq!(w.default_breakfast_code.as_deref(), Some("BF"));
        assert_eq!(w.extra_bed_mode, MODE_FOLD_LEGACY);
        let bad =
            Settings::from_config(&json!({"extra_bed_write_mode": "BOTH"}), Path::new("/d")).write;
        assert_eq!(bad.extra_bed_mode, MODE_FOLD_LEGACY);
        assert!(bad.extra_bed_mode_problem.unwrap().contains("'BOTH'"));
    }
}
