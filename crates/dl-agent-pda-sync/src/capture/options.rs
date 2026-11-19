//! Capture settings (`Strategies/CaptureOptions.cs`).

use std::sync::Arc;
use std::time::Duration;

use super::governor::PollBudget;
use super::util::contains_ignore_case;
use crate::contracts::time::TimeZone;

/// The ledger tables that are logs (shorter snapshot window) by default.
pub const DEFAULT_LOG_TABLES: [&str; 7] = [
    "CheckOpenLog",
    "CheckActivities",
    "TrnActivities",
    "OrderLog",
    "PrintLog",
    "PDASendOrder",
    "PDABillPrint",
];

/// Capture settings.
#[derive(Debug, Clone)]
pub struct CaptureOptions {
    /// Agent id (batch envelope).
    pub agent_id: String,
    /// Time zone of the vendor's naive datetimes; "today" and day buckets are computed in it.
    pub site_time_zone: Arc<TimeZone>,
    /// Poll budget.
    pub budget: PollBudget,
    /// Largest rows+tombstones in one wire batch (2000).
    pub max_rows_per_batch: i32,
    /// Approximate payload cap of one batch (4 MiB, half of the 8 MB request cap).
    pub max_batch_bytes: i32,
    /// Keyset page size for steady-state reads (5000).
    pub page_rows: i32,
    /// Keyset chunk size of the initial snapshot (5000).
    pub snapshot_chunk_rows: i32,
    /// Rolling rescan of the last N calendar days of the closed ledger (3).
    pub rescan_days: i32,
    /// Rows re-read from the end of ledger tables that have no date column (2000).
    pub tail_rescan_rows: i32,
    /// Re-reads of an open check whose rows changed under a NOLOCK read before it is deferred (3).
    pub stable_read_retries: i32,
    /// Polls an open check must be missing before it is tombstoned (2).
    pub vanish_confirm_polls: i32,
    /// Snapshot window in months (24).
    pub snapshot_window_months: i32,
    /// Snapshot window of log tables in months (6).
    pub log_snapshot_window_months: i32,
    /// Also load history older than the window (low priority, only when nothing else is pending).
    pub include_backfill: bool,
    /// Ledger tables that are logs (matched ignoring case).
    pub log_tables: Vec<String>,
    /// Per-table date column used for day buckets (table matched ignoring case); replaces the built-in map.
    pub day_column_overrides: Vec<(String, String)>,
    /// Open checks poll interval (2 s).
    pub open_interval: Duration,
    /// Ledger tail interval (10 s).
    pub ledger_tail_interval: Duration,
    /// Ledger rescan interval (5 min).
    pub ledger_rescan_interval: Duration,
    /// Masters interval (120 s).
    pub master_interval: Duration,
}

impl CaptureOptions {
    /// Options with the C# defaults.
    pub fn new(agent_id: &str, site_time_zone: Arc<TimeZone>) -> CaptureOptions {
        CaptureOptions {
            agent_id: agent_id.to_owned(),
            site_time_zone,
            budget: PollBudget::default(),
            max_rows_per_batch: 2000,
            max_batch_bytes: 4 * 1024 * 1024,
            page_rows: 5000,
            snapshot_chunk_rows: 5000,
            rescan_days: 3,
            tail_rescan_rows: 2000,
            stable_read_retries: 3,
            vanish_confirm_polls: 2,
            snapshot_window_months: 24,
            log_snapshot_window_months: 6,
            include_backfill: true,
            log_tables: DEFAULT_LOG_TABLES.iter().map(|s| (*s).to_owned()).collect(),
            day_column_overrides: Vec::new(),
            open_interval: Duration::from_secs(2),
            ledger_tail_interval: Duration::from_secs(10),
            ledger_rescan_interval: Duration::from_secs(300),
            master_interval: Duration::from_secs(120),
        }
    }

    /// Whether `table` is a log table.
    pub fn is_log_table(&self, table: &str) -> bool {
        contains_ignore_case(&self.log_tables, table)
    }

    /// The configured day column of `table` (last setting wins, like a dictionary).
    pub fn day_column_override(&self, table: &str) -> Option<&str> {
        self.day_column_overrides
            .iter()
            .rev()
            .find(|(t, _)| crate::util::stj::eq_ignore_case(t, table))
            .map(|(_, c)| c.as_str())
    }
}
