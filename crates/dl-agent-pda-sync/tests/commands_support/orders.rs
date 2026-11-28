//! The SMILE_POS rows of the fake's order verbs: KDS lines, `tmpInvoice` headers and the
//! order book.

use std::collections::{BTreeMap, HashMap, HashSet};

use dl_agent_pda_sync::contracts::wire::OrderCheckLine;

/// A KDS line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Line {
    pub accepted: bool,
    pub accepted_by: Option<String>,
    pub done: bool,
}

/// A `tmpInvoice` header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub table_no: String,
    pub guests: i32,
    pub status: i32,
    pub children: i32,
}

/// The SMILE_POS rows the order verbs touch.
#[derive(Debug, Clone)]
pub struct OrderBook {
    pub checks: HashMap<(i32, i64), Check>,
    pub lines: BTreeMap<(i64, i64), OrderCheckLine>,
    pub items: HashSet<String>,
    pub next_check_no: i64,
    pub next_trn_seq: i64,
}

impl Default for OrderBook {
    fn default() -> Self {
        OrderBook {
            checks: HashMap::new(),
            lines: BTreeMap::new(),
            items: ["CF01", "TEA"].iter().map(|s| (*s).to_owned()).collect(),
            next_check_no: 7001,
            next_trn_seq: 1,
        }
    }
}
