//! Azolo E-Invoice as a dl-agent module (`modules.einvoice`).
//!
//! A port of the MM-EINVOICE agent (Go `cmd/agent`, `internal/agentcollector`, through the
//! Rust port in `rust/agent`): it reads pending bills from the hotel's SMILE_FO / SMILE_POS
//! SQL Server databases, pushes them to the E-Invoice platform, writes issued invoice
//! numbers back, and relays the platform's VNPT portal calls from the hotel's Vietnamese
//! address (ADR 0016). Push items are byte-identical to the Go agent's, checked against
//! vectors generated from the Go code (`tests/golden.rs`, `tests/legacy_golden.rs`).
//!
//! Register it with [`einvoice_module`]; it runs only where the remote config sets
//! `modules.einvoice.enabled = true`.

pub mod agent_config;
pub mod config;
pub mod domain;
pub mod dryrun;
pub mod dsn;
pub mod enroll;
pub mod fetch;
pub mod fo;
pub mod gojson;
pub mod install;
pub mod legacy;
pub mod legacy_sql;
pub mod log;
pub mod module;
pub mod money;
pub mod outbox;
pub mod platform;
pub mod pos;
pub mod pos_names;
pub mod profile;
pub mod relay;
pub mod runner;
pub mod sql;
pub mod stats;
pub mod tcvn3;

pub use enroll::einvoice_enroll_tool;
pub use module::selftest::einvoice_selftest;
pub use module::{EinvoiceModule, einvoice_module};

use domain::{CollectedInvoice, PushItem};

/// The POS branch of `FetchCollected` after the rows are scanned: normalise, recalc, TCVN3.
pub fn collect_pos(
    raw: &pos::PosRawCheck,
    rules: &config::Rules,
) -> Result<CollectedInvoice, String> {
    let mut inv = pos::normalize(raw).map_err(|e| match e {
        // Go wraps errNotPOSCheck with the check number: "agent: pos <no>: not a POS check".
        pos::PosError::NotPosCheck => format!("agent: pos {}: {e}", raw.check_no),
        pos::PosError::Invalid(m) => m,
    })?;
    pos::recalc_line_tax(&mut inv, rules.recalc_tax_into_sc)?;
    tcvn3::convert_invoice(&mut inv);
    Ok(inv)
}

/// The check fetcher the FO enrich step uses: normalise and recalc (no TCVN3 here — the
/// whole FO invoice is converted once at the end).
pub fn enrich_check(
    raw: &pos::PosRawCheck,
    rules: &config::Rules,
) -> Result<CollectedInvoice, pos::PosError> {
    let mut inv = pos::normalize(raw)?;
    pos::recalc_line_tax(&mut inv, rules.recalc_tax_into_sc).map_err(pos::PosError::Invalid)?;
    Ok(inv)
}

/// `ToPushItemCollected`.
pub fn push_item(inv: CollectedInvoice) -> Result<PushItem, String> {
    domain::to_push_item(inv)
}
