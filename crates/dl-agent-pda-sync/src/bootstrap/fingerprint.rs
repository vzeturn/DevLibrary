//! Vendor schema fingerprint (port of `Bootstrap/SchemaFingerprint.cs`): the tables the
//! sync catalog classifies must be present in the candidate database.

use super::models::TableSet;
use crate::config::{AgentError, codes};
use crate::contracts::catalog::SyncCatalog;
use crate::reporting::format::render_value;
use crate::reporting::log::Value;

/// Tables every POS database has.
pub const CORE_TABLES: [&str; 6] = [
    "Invoice",
    "ItemDef",
    "Para",
    "GlobPara",
    "CashierDef",
    "TableList",
];

/// The default minimum score.
pub const DEFAULT_MINIMUM_SCORE: f64 = 0.85;

/// The fingerprint of the classified tables the PDA uses (`SchemaFingerprint`).
#[derive(Debug, Clone)]
pub struct SchemaFingerprint {
    expected: Vec<String>,
    minimum_score: f64,
}

/// `{score:P0}` as the agent's culture writes it (`85%`; en-US and vi-VN put no space
/// before the sign).
pub fn percent0(score: f64) -> String {
    render_value(&Value::Float(score), Some("P0")).replace(" %", "%")
}

impl SchemaFingerprint {
    /// The fingerprint of `catalog` with `minimum_score` (0.85 by default).
    pub fn new(catalog: &SyncCatalog, minimum_score: f64) -> SchemaFingerprint {
        SchemaFingerprint {
            expected: catalog
                .classified()
                .filter(|t| t.used_by_pda)
                .map(|t| t.table.clone())
                .collect(),
            minimum_score,
        }
    }

    /// The minimum score.
    pub fn minimum_score(&self) -> f64 {
        self.minimum_score
    }

    /// The share of the expected tables present; 0 when nothing is expected.
    pub fn score(&self, tables: &TableSet) -> f64 {
        if self.expected.is_empty() {
            return 0.0;
        }
        let found = self.expected.iter().filter(|t| tables.contains(t)).count();
        found as f64 / self.expected.len() as f64
    }

    /// Every core table present and the score at least the minimum.
    pub fn matches(&self, tables: &TableSet) -> bool {
        CORE_TABLES.iter().all(|t| tables.contains(t)) && self.score(tables) >= self.minimum_score
    }

    /// [`matches`](Self::matches), or `FINGERPRINT_MISMATCH` naming what is missing.
    pub fn require(&self, tables: &TableSet, database: &str) -> Result<(), AgentError> {
        let missing: Vec<&str> = CORE_TABLES
            .iter()
            .copied()
            .filter(|t| !tables.contains(t))
            .collect();
        let score = self.score(tables);
        if missing.is_empty() && score >= self.minimum_score {
            return Ok(());
        }
        Err(AgentError::new(
            codes::FINGERPRINT_MISMATCH,
            format!(
                "Database '{database}' does not look like an on-premise POS database (schema match {}, missing core tables: {}).",
                percent0(score),
                if missing.is_empty() {
                    "none".to_owned()
                } else {
                    missing.join(", ")
                }
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_tables_match_and_others_do_not() {
        let catalog = SyncCatalog::load().unwrap();
        let f = SchemaFingerprint::new(catalog, DEFAULT_MINIMUM_SCORE);
        let all: TableSet = catalog
            .classified()
            .map(|t| t.table.to_lowercase())
            .collect();
        assert!(f.matches(&all));
        assert!((f.score(&all) - 1.0).abs() < 1e-12);
        let other: TableSet = ["Customers", "Orders"].into_iter().collect();
        assert!(!f.matches(&other));
        let e = f.require(&other, "SHOP").unwrap_err();
        assert_eq!(e.code(), codes::FINGERPRINT_MISMATCH);
        assert_eq!(
            e.message(),
            "Database 'SHOP' does not look like an on-premise POS database (schema match 0%, missing core tables: Invoice, ItemDef, Para, GlobPara, CashierDef, TableList)."
        );
        assert_eq!(percent0(0.85), "85%");
        assert_eq!(percent0(1.0), "100%");
    }
}
