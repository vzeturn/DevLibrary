//! Which catalog tables are captured and how (`Strategies/CapturePlan.cs`).

use super::error::{CaptureError, Result};
use super::options::CaptureOptions;
use super::util::distinct_ignore_case;
use crate::contracts::catalog::{CatalogTable, SyncCatalog, catalog_class};
use crate::util::stj::eq_ignore_case;

/// How a table is captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureStrategy {
    /// Open (temporary) checks: whole-check snapshot and hash diff every few seconds.
    OpenSnapshotDiff,
    /// Closed ledger: keyset watermark on the monotonic key plus rolling rescan of recent days.
    LedgerWatermarkRescan,
    /// Masters/config/small logs: periodic full keyset scan diffed against the shadow hashes.
    MasterScanDiff,
}

impl CaptureStrategy {
    /// The C# enum member name (`strategy.ToString()`).
    pub fn name(self) -> &'static str {
        match self {
            CaptureStrategy::OpenSnapshotDiff => "OpenSnapshotDiff",
            CaptureStrategy::LedgerWatermarkRescan => "LedgerWatermarkRescan",
            CaptureStrategy::MasterScanDiff => "MasterScanDiff",
        }
    }
}

/// A table the capture will read, with everything the strategies need resolved from the catalog.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanTable {
    /// The catalog entry.
    pub table: CatalogTable,
    /// Strategy.
    pub strategy: CaptureStrategy,
    /// Keyset order.
    pub order_columns: Vec<String>,
    /// Day bucket column (ledger).
    pub day_column: Option<String>,
    /// Check number column (open checks).
    pub check_column: Option<String>,
    /// Columns never read.
    pub exclude: Vec<String>,
    /// Columns read as bytes (TCVN3).
    pub bytes: Vec<String>,
}

impl PlanTable {
    /// The vendor table name.
    pub fn name(&self) -> &str {
        &self.table.table
    }
}

/// Why a catalog table is (not) captured; the audit trail for unclassified / keyless /
/// target-less tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanDecision {
    /// Vendor table.
    pub table: String,
    /// Catalog class.
    pub class: String,
    /// Strategy name, `None` when not captured.
    pub strategy: Option<String>,
    /// Reason.
    pub reason: String,
}

const DAY_COLUMNS: [(&str, &str); 20] = [
    ("Invoice", "CloseTime"),
    ("InvoiceCxl", "CloseTime"),
    ("Trn", "TrnTime"),
    ("VoidTrn", "TrnTime"),
    ("Payment", "PayTime"),
    ("CheckActivities", "sTime"),
    ("CheckOpenLog", "OpenTime"),
    ("OrderLog", "oTime"),
    ("TrnActivities", "tTime"),
    ("TaxTable", "TDate"),
    ("VATBillInfor", "INVDate"),
    ("InvoiceMerged", "PrintDate"),
    ("PDABillPrint", "OrderTime"),
    ("PDASendOrder", "SendTime"),
    ("PrintLog", "PrintTime"),
    ("ShiftMoney", "POSDate"),
    ("CashierShift", "OpenDate"),
    ("EmployeeShiftLog", "ClockInAt"),
    ("TableReservation", "FromDateTime"),
    ("TableReservationDtl", "PostTime"),
];

const CHECK_COLUMNS: [(&str, &str); 1] = [("TmpTaxTable", "TCheckNo")];

fn lookup(map: &[(&str, &'static str)], table: &str) -> Option<&'static str> {
    map.iter()
        .find(|(t, _)| eq_ignore_case(t, table))
        .map(|(_, c)| *c)
}

/// The capture plan.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CapturePlan {
    /// Open-check tables (`TmpInvoice` first).
    pub open: Vec<PlanTable>,
    /// Ledger tables (`Invoice` first).
    pub ledger: Vec<PlanTable>,
    /// Master tables.
    pub masters: Vec<PlanTable>,
    /// Decisions for every non-skip table.
    pub decisions: Vec<PlanDecision>,
}

impl CapturePlan {
    /// Open, then ledger, then masters.
    pub fn all(&self) -> impl Iterator<Item = &PlanTable> {
        self.open.iter().chain(&self.ledger).chain(&self.masters)
    }

    /// The planned table named `vendor_table` (ignoring case).
    pub fn find(&self, vendor_table: &str) -> Option<&PlanTable> {
        self.all().find(|t| eq_ignore_case(t.name(), vendor_table))
    }

    /// Builds the plan of `catalog`.
    pub fn build(catalog: &SyncCatalog, options: &CaptureOptions) -> Result<CapturePlan> {
        let mut open = Vec::new();
        let mut ledger = Vec::new();
        let mut masters = Vec::new();
        let mut decisions = Vec::new();

        for t in &catalog.tables {
            if let Some(reason) = rejection(t) {
                if t.class != catalog_class::SKIP {
                    decisions.push(PlanDecision {
                        table: t.table.clone(),
                        class: t.class.clone(),
                        strategy: None,
                        reason: reason.to_owned(),
                    });
                }
                continue;
            }
            let strategy = strategy_of(t)?;
            let planned = plan(catalog, t, strategy, options);
            decisions.push(PlanDecision {
                table: t.table.clone(),
                class: t.class.clone(),
                strategy: Some(strategy.name().to_owned()),
                reason: describe(&planned),
            });
            match strategy {
                CaptureStrategy::OpenSnapshotDiff => open.push(planned),
                CaptureStrategy::LedgerWatermarkRescan => ledger.push(planned),
                CaptureStrategy::MasterScanDiff => masters.push(planned),
            }
        }

        // TmpInvoice first, then by name ignoring case (stable).
        open.sort_by(|a, b| {
            let ra = i32::from(a.name() != "TmpInvoice");
            let rb = i32::from(b.name() != "TmpInvoice");
            ra.cmp(&rb)
                .then_with(|| super::util::cmp_ignore_case(a.name(), b.name()))
        });
        // closed_check_lines (Trn) references closed_checks (Invoice): the parent goes
        // first in every pass. Catalog order is kept otherwise (stable sort).
        ledger.sort_by_key(|p| i32::from(!eq_ignore_case(p.name(), "Invoice")));

        Ok(CapturePlan {
            open,
            ledger,
            masters,
            decisions,
        })
    }
}

/// The vendor tables the capture reads, in catalog order: every table the plan does not
/// reject (the tables SQL Server Change Tracking covers, RT-1318). Unlike
/// [`CapturePlan::build`] it needs no capture options and cannot fail.
pub fn captured_tables(catalog: &SyncCatalog) -> Vec<String> {
    catalog
        .tables
        .iter()
        .filter(|t| rejection(t).is_none())
        .map(|t| t.table.clone())
        .collect()
}

/// Fail closed: anything not explicitly classified, keyed and mappable is never read.
fn rejection(t: &CatalogTable) -> Option<&'static str> {
    if t.class == catalog_class::SKIP {
        return Some("skip");
    }
    if t.class == catalog_class::UNCLASSIFIED {
        return Some("unclassified: not captured until an owner classifies it (fail closed)");
    }
    if !t.has_pg_target() {
        return Some("classified but no pg_target: nothing to map onto, not captured");
    }
    if t.keys().is_empty() {
        return Some("no vendor key and no logical key: not captured");
    }
    None
}

fn strategy_of(t: &CatalogTable) -> Result<CaptureStrategy> {
    match t.capture.as_deref() {
        Some("snapshot-diff") => Ok(CaptureStrategy::OpenSnapshotDiff),
        Some("watermark+rescan") => Ok(CaptureStrategy::LedgerWatermarkRescan),
        Some("scan-diff") => Ok(CaptureStrategy::MasterScanDiff),
        other => Err(CaptureError::invalid_operation(format!(
            "Table {}: unknown capture strategy '{}'.",
            t.table,
            other.unwrap_or("")
        ))),
    }
}

/// Columns of `t` that must never be read: catalog exclusions plus never-sync columns.
pub fn exclude_of(catalog: &SyncCatalog, t: &CatalogTable) -> Vec<String> {
    distinct_ignore_case(
        t.exclude_columns
            .clone()
            .unwrap_or_default()
            .into_iter()
            .chain(catalog.never_sync_columns(&t.table)),
    )
}

fn plan(
    catalog: &SyncCatalog,
    t: &CatalogTable,
    strategy: CaptureStrategy,
    options: &CaptureOptions,
) -> PlanTable {
    let exclude = exclude_of(catalog, t);
    let bytes = t
        .transforms
        .as_ref()
        .and_then(|x| x.tcvn3_decode.clone())
        .unwrap_or_default();
    let order = match (&t.monotonic_key, strategy) {
        (Some(mono), CaptureStrategy::LedgerWatermarkRescan) => std::iter::once(mono.clone())
            .chain(
                t.keys()
                    .iter()
                    .filter(|k| !eq_ignore_case(k, mono))
                    .cloned(),
            )
            .collect(),
        _ => t.keys().to_vec(),
    };
    PlanTable {
        table: t.clone(),
        strategy,
        order_columns: order,
        day_column: if strategy == CaptureStrategy::LedgerWatermarkRescan {
            day_column_of(t, options)
        } else {
            None
        },
        check_column: if strategy == CaptureStrategy::OpenSnapshotDiff {
            Some(
                lookup(&CHECK_COLUMNS, &t.table)
                    .unwrap_or("CheckNo")
                    .to_owned(),
            )
        } else {
            None
        },
        exclude,
        bytes,
    }
}

fn day_column_of(t: &CatalogTable, options: &CaptureOptions) -> Option<String> {
    if let Some(configured) = options.day_column_override(&t.table) {
        return Some(configured.to_owned());
    }
    if let Some(known) = lookup(&DAY_COLUMNS, &t.table) {
        return Some(known.to_owned());
    }
    t.datetime_columns.as_ref()?.iter().find_map(|c| {
        let lower = c.to_lowercase();
        let ok =
            !lower.starts_with("lastchg") && (lower.contains("time") || lower.contains("date"));
        ok.then(|| c.clone())
    })
}

fn describe(p: &PlanTable) -> String {
    match p.strategy {
        CaptureStrategy::OpenSnapshotDiff => format!(
            "per-check snapshot-diff on {}",
            p.check_column.as_deref().unwrap_or("")
        ),
        CaptureStrategy::LedgerWatermarkRescan => match &p.day_column {
            None => format!(
                "keyset watermark on {}; no date column -> tail rescan of the last rows",
                p.order_columns.join("+")
            ),
            Some(day) => format!(
                "keyset watermark on {}; day rescan on {day}",
                p.order_columns.join("+")
            ),
        },
        CaptureStrategy::MasterScanDiff => {
            let mut s = format!("scan-diff on key {}", p.order_columns.join("+"));
            if p.table.keys_from_overlay {
                s.push_str(" (logical key)");
            }
            if p.table.duplicate_rows_tolerated {
                s.push_str(", duplicate rows tolerated (occurrence suffix)");
            }
            s
        }
    }
}

#[cfg(test)]
mod tests {
    //! `CapturePlanTests.cs` (the `CaptureContext.TargetOf` case is ported with the context).
    use super::*;
    use crate::contracts::time::TimeZone;
    use std::sync::Arc;

    fn catalog() -> &'static SyncCatalog {
        SyncCatalog::load().unwrap()
    }

    fn the_plan() -> CapturePlan {
        let opts = CaptureOptions::new(
            "agent-1",
            Arc::new(TimeZone::fixed("SE Asia Standard Time", 420)),
        );
        CapturePlan::build(catalog(), &opts).unwrap()
    }

    #[test]
    fn the_captured_tables_are_the_planned_tables() {
        let mut planned: Vec<String> = the_plan().all().map(|t| t.name().to_owned()).collect();
        let mut captured = captured_tables(catalog());
        planned.sort();
        captured.sort();
        assert_eq!(captured, planned);
        // ADR 0011: the open checks (TmpInvoice) and closed lines (Trn) are captured.
        assert!(captured.iter().any(|t| t == "TmpInvoice"));
        assert!(captured.iter().any(|t| t == "Trn"));
    }

    #[test]
    fn unclassified_tables_are_never_captured_and_are_listed_with_a_reason() {
        let plan = the_plan();
        let unclassified: Vec<&CatalogTable> = catalog()
            .tables
            .iter()
            .filter(|t| t.class == catalog_class::UNCLASSIFIED)
            .collect();
        assert!(!unclassified.is_empty());
        for t in unclassified {
            assert!(plan.find(&t.table).is_none(), "{}", t.table);
            assert!(plan.decisions.iter().any(|d| d.table == t.table
                && d.strategy.is_none()
                && d.reason.starts_with("unclassified")));
        }
    }

    #[test]
    fn the_closed_check_header_is_captured_before_its_lines() {
        let plan = the_plan();
        let names: Vec<&str> = plan.ledger.iter().map(PlanTable::name).collect();
        assert_eq!(names[0], "Invoice");
        assert!(names.iter().position(|n| *n == "Trn").unwrap() > 0);
    }

    #[test]
    fn skipped_tables_are_never_captured() {
        let plan = the_plan();
        for t in catalog()
            .tables
            .iter()
            .filter(|t| t.class == catalog_class::SKIP)
        {
            assert!(plan.find(&t.table).is_none(), "{}", t.table);
        }
    }

    #[test]
    fn every_captured_table_has_a_pg_target_and_a_key() {
        let plan = the_plan();
        assert!(plan.all().next().is_some());
        assert!(plan.all().all(|p| p.table.has_pg_target()
            && !p.table.keys().is_empty()
            && p.table.class != catalog_class::SKIP
            && p.table.class != catalog_class::UNCLASSIFIED));
    }

    #[test]
    fn classified_tables_without_a_target_or_key_are_recorded_as_not_captured() {
        let plan = the_plan();
        let rejected: Vec<&PlanDecision> = plan
            .decisions
            .iter()
            .filter(|d| d.strategy.is_none() && !d.reason.starts_with("unclassified"))
            .collect();
        assert!(!rejected.is_empty());
        assert!(
            rejected
                .iter()
                .all(|d| d.reason.contains("no pg_target") || d.reason.contains("no vendor key"))
        );
    }

    #[test]
    fn every_non_skip_table_has_exactly_one_decision() {
        let plan = the_plan();
        let mut expected: Vec<&str> = catalog()
            .tables
            .iter()
            .filter(|t| t.class != catalog_class::SKIP)
            .map(|t| t.table.as_str())
            .collect();
        let mut got: Vec<&str> = plan.decisions.iter().map(|d| d.table.as_str()).collect();
        expected.sort_unstable();
        got.sort_unstable();
        assert_eq!(got, expected);
    }

    #[test]
    fn strategy_follows_the_catalog_capture_mode() {
        let plan = the_plan();
        assert!(plan.open.iter().all(
            |p| p.table.capture.as_deref() == Some("snapshot-diff") && p.check_column.is_some()
        ));
        assert!(
            plan.ledger
                .iter()
                .all(|p| p.table.capture.as_deref() == Some("watermark+rescan"))
        );
        assert!(
            plan.masters
                .iter()
                .all(|p| p.table.capture.as_deref() == Some("scan-diff"))
        );
        assert_eq!(
            plan.open[0].name(),
            "TmpInvoice",
            "the header is written before its lines"
        );
    }

    #[test]
    fn ledger_order_starts_with_the_monotonic_key_and_never_uses_last_change_time() {
        let plan = the_plan();
        for p in &plan.ledger {
            if let Some(mono) = &p.table.monotonic_key {
                assert_eq!(&p.order_columns[0], mono, "{}", p.name());
            }
            assert!(!p.order_columns.iter().any(|c| c == "LastChgTime"));
            assert_ne!(p.day_column.as_deref(), Some("LastChgTime"));
        }
    }

    #[test]
    fn never_sync_columns_are_excluded_from_every_table() {
        let plan = the_plan();
        for p in plan.all() {
            for col in catalog().never_sync_columns(p.name()) {
                assert!(p.exclude.contains(&col), "{} {col}", p.name());
            }
        }
        assert!(
            plan.find("Invoice")
                .unwrap()
                .exclude
                .iter()
                .any(|c| c == "CardNumber")
        );
    }

    #[test]
    fn tables_with_logical_keys_and_tolerated_duplicates_are_described() {
        let plan = the_plan();
        assert!(
            plan.decisions
                .iter()
                .any(|d| d.table == "CashierRVC" && d.reason.contains("duplicate"))
        );
    }

    #[test]
    fn plan_summary_for_the_report() {
        let plan = the_plan();
        assert!(!plan.open.is_empty());
        assert!(!plan.ledger.is_empty());
        assert!(!plan.masters.is_empty());
    }
}
