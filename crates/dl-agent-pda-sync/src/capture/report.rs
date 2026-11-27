//! Counters of one capture pass (`Strategies/CaptureReport.cs`).

use super::error::CaptureError;
use super::util::contains_ignore_case;

/// A source row that could not be mapped; it is reported (and retried next cycle), never
/// silently dropped. `reason` is the mapper's message and can quote row values: it stays
/// on the site. `rule` names the failed rule with schema terms only
/// (`missing_key:CloseTime`, `format:SubTotal`) and is what gets logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedRow {
    /// Vendor table.
    pub table: String,
    /// Mapper message (site only).
    pub reason: String,
    /// Failed rule (loggable).
    pub rule: String,
}

/// A table skipped this run because its read failed with a schema error.
#[derive(Debug, Clone, PartialEq)]
pub struct TableFault {
    /// Vendor table.
    pub table: String,
    /// The failure.
    pub error: CaptureError,
}

/// Counters of one capture pass. Infrastructure errors are returned, not recorded here.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CaptureReport {
    /// Batches sent.
    pub batches: i32,
    /// Rows sent.
    pub rows_sent: i32,
    /// Tombstones sent.
    pub tombstones_sent: i32,
    /// Blobs sent.
    pub blobs_sent: i32,
    /// Blobs skipped because no blob sink is configured.
    pub blobs_skipped_no_sink: i32,
    /// Echoes suppressed.
    pub echoes_suppressed: i32,
    /// Open checks deferred because they changed under the read.
    pub unstable_checks_deferred: i32,
    /// Duplicate keys seen.
    pub duplicate_keys: i32,
    /// Open-check rows read from the vendor database this pass (full read, or only the
    /// changed checks).
    pub open_rows_read: i32,
    /// The open-check pass read everything instead of only the changed checks.
    pub open_full_read: bool,
    /// Store-wide rows (NULL key defaulted, ADR 0106) not sent because an explicit row of
    /// the same key wins.
    pub null_keys_shadowed: i32,
    /// Planned tables that do not exist in this SMILE_POS build (distinct, in plan order).
    pub missing_tables: Vec<String>,
    /// The subset of `missing_tables` the catalog marks `optional_on_site`.
    pub optional_missing_tables: Vec<String>,
    /// The cycle deadline was hit.
    pub deadline_hit: bool,
    /// Rows that could not be mapped.
    pub rejected: Vec<RejectedRow>,
    /// Tables skipped after a schema error; the rest of the pass ran (AD1).
    pub table_faults: Vec<TableFault>,
    /// Notes.
    pub notes: Vec<String>,
}

impl CaptureReport {
    /// An empty report.
    pub fn new() -> CaptureReport {
        CaptureReport::default()
    }

    /// `TablesMissing`.
    pub fn tables_missing(&self) -> usize {
        self.missing_tables.len()
    }

    /// Records a planned table that the source does not have.
    pub fn table_missing(&mut self, table: &str, optional: bool) {
        if !contains_ignore_case(&self.missing_tables, table) {
            self.missing_tables.push(table.to_owned());
        }
        if optional && !contains_ignore_case(&self.optional_missing_tables, table) {
            self.optional_missing_tables.push(table.to_owned());
        }
    }

    /// Missing tables a healthy site is expected to have: the only ones worth a warning.
    pub fn unexpected_missing_tables(&self) -> Vec<String> {
        self.missing_tables
            .iter()
            .filter(|t| !contains_ignore_case(&self.optional_missing_tables, t))
            .cloned()
            .collect()
    }

    /// Adds `other` into this report.
    pub fn merge(&mut self, other: CaptureReport) {
        self.batches += other.batches;
        self.rows_sent += other.rows_sent;
        self.tombstones_sent += other.tombstones_sent;
        self.blobs_sent += other.blobs_sent;
        self.blobs_skipped_no_sink += other.blobs_skipped_no_sink;
        self.echoes_suppressed += other.echoes_suppressed;
        self.unstable_checks_deferred += other.unstable_checks_deferred;
        self.duplicate_keys += other.duplicate_keys;
        self.open_rows_read += other.open_rows_read;
        self.open_full_read |= other.open_full_read;
        self.null_keys_shadowed += other.null_keys_shadowed;
        for t in &other.missing_tables {
            self.table_missing(t, contains_ignore_case(&other.optional_missing_tables, t));
        }
        self.deadline_hit |= other.deadline_hit;
        self.rejected.extend(other.rejected);
        self.table_faults.extend(other.table_faults);
        self.notes.extend(other.notes);
    }
}
