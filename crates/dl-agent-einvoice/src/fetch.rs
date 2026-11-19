//! One bill from SQL rows to a collected invoice (`FetchCollected`): shared by the
//! collector cycle and the read-only `collect_dry_run` command, so a dry run builds
//! exactly what a cycle would push.

use std::time::Duration;

use crate::config::Rules;
use crate::domain::CollectedInvoice;
use crate::fo::FoRows;
use crate::log::{self, V};
use crate::pos::PosError;
use crate::sql::{CollectKey, Sql};

/// Fetches `key` (FO bill or POS check) and builds its collected invoice under `rules`.
///
/// # Errors
/// A SQL failure or a bill the collector cannot build.
pub fn collected(
    sql: &Sql,
    rules: &Rules,
    key: &CollectKey,
    timeout: Duration,
) -> Result<CollectedInvoice, String> {
    if key.source == "POS" {
        let raw = sql.fetch_pos_raw(&key.id, timeout)?;
        return crate::collect_pos(&raw, rules);
    }
    let rows: FoRows = sql.fetch_fo_rows(&key.id, timeout)?;
    for d in &rows.detail {
        if !rows.lines.iter().any(|l| l.line_key == d.parent_line_key) {
            log::warn(
                "agent: orphaned detail item dropped (no parent line)",
                &[
                    ("source_ref", V::S(&key.source_ref())),
                    ("line_key", V::S(&d.parent_line_key)),
                    ("icode", V::S(&d.icode)),
                ],
            );
        }
    }
    let mut fetch = |check_no: &str| -> Result<_, PosError> {
        let raw = sql
            .fetch_pos_raw(check_no, timeout)
            .map_err(PosError::Invalid)?;
        let r = crate::enrich_check(&raw, rules);
        match &r {
            Err(PosError::NotPosCheck) => log::debug(
                "agent: fo pos-charge ref is not a POS check",
                &[("check_no", V::S(check_no))],
            ),
            Err(PosError::Invalid(e)) => log::warn(
                "agent: fo pos-charge enrich skipped (fetch failed)",
                &[("check_no", V::S(check_no)), ("err", V::S(e))],
            ),
            Ok(_) => {}
        }
        r
    };
    crate::fo::collect(&key.id, &rows, rules, &mut fetch)
}
