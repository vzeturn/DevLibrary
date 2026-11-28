//! `pos.stock_balance` contract (`Wire/PosStockBalance.cs`, RT-1024, ADR 0165): the SMILE_BO
//! stock balance of some items in one cloud stock store, for the cloud's stock drift check.
//!
//! The Rust agent runs it since RT-1427 (`commands::pos_stock`): a SELECT-only read through the
//! `txn.stock_*` store map (`Pda_TxnStockMap`, bootstrap 170/175), delivered `Durable` with 3
//! attempts like the .NET agent.

use dl_decimal::Decimal;

use crate::contracts::json::{self, WireJson};
use crate::contracts::utf16_len;
use crate::util::stj::{Obj, ReadError, Writer};

/// Command type (`SiteCommandTypes.PosStockBalance`).
pub mod pos_stock_types {
    /// Read-only SMILE_BO balance of some items in one cloud stock store.
    pub const STOCK_BALANCE: &str = "pos.stock_balance";
}

/// Payload of [`pos_stock_types::STOCK_BALANCE`] (`PosStockBalancePayload`): the balance of
/// `item_codes` (SMILE_BO `ICode`) in cloud stock store `store_code` (the cloud RVC).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PosStockBalancePayload {
    /// Cloud stock store (RVC), mapped by the agent through `dbo.Pda_TxnStockMap`.
    pub store_code: i32,
    /// The items asked.
    pub item_codes: Vec<String>,
    /// Why the cloud asks.
    pub reason: Option<String>,
}

impl PosStockBalancePayload {
    /// Items per command (SQL Server takes at most 2100 parameters).
    pub const MAX_ITEMS: usize = 1000;
    /// Longest item code, in UTF-16 units.
    pub const MAX_ITEM_CODE: usize = 20;
    /// Longest reason, in UTF-16 units.
    pub const MAX_REASON: usize = 300;

    /// Aggregate key of this command: reads never order against a bill.
    pub fn aggregate_key(store_code: i32) -> String {
        format!("pos-stock:{store_code}")
    }

    /// The validation problem, `None` when valid (`PosStockBalancePayload.Problem`, same text).
    pub fn problem(&self) -> Option<String> {
        if !(1..=255).contains(&self.store_code) {
            return Some("store_code must be 1..255".into());
        }
        if !(1..=Self::MAX_ITEMS).contains(&self.item_codes.len()) {
            return Some(format!("item_codes must hold 1..{} codes", Self::MAX_ITEMS));
        }
        let mut seen: Vec<String> = Vec::with_capacity(self.item_codes.len());
        for code in &self.item_codes {
            let c = code.trim();
            if c.is_empty() || utf16_len(c) > Self::MAX_ITEM_CODE {
                return Some(format!(
                    "each item code must be 1..{} characters",
                    Self::MAX_ITEM_CODE
                ));
            }
            // OrdinalIgnoreCase on the .NET side; the same for the ASCII codes SMILE_BO holds.
            let key = c.to_uppercase();
            if seen.contains(&key) {
                return Some("an item code is repeated".into());
            }
            seen.push(key);
        }
        match &self.reason {
            Some(r) if utf16_len(r) > Self::MAX_REASON => Some(format!(
                "reason must be at most {} characters",
                Self::MAX_REASON
            )),
            _ => None,
        }
    }
}

impl WireJson for PosStockBalancePayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("store_code").int(self.store_code.into());
        w.key("item_codes");
        json::write_strings(w, &self.item_codes);
        json::opt_str(w, "reason", &self.reason);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(PosStockBalancePayload {
            store_code: o.int("store_code")?,
            item_codes: o.list("item_codes", json::string_item)?,
            reason: o.opt_string("reason")?,
        })
    }
}

/// One item of the result (`PosStockBalance`): the code as SMILE_BO stores it (trimmed) and
/// its posted balance in the BO store, `SUM(INVTrn.TrnQty)` in the item's stock unit.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PosStockBalance {
    /// SMILE_BO `ICode`.
    pub item_code: String,
    /// Posted balance.
    pub qty: Decimal,
}

impl WireJson for PosStockBalance {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("item_code").string(&self.item_code);
        w.key("qty").decimal(&self.qty);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(PosStockBalance {
            item_code: o.string("item_code")?,
            qty: o.decimal("qty")?,
        })
    }
}

/// Result of `pos.stock_balance` (`PosStockBalanceResult`): the BO database and store the cloud
/// store maps to and one row per asked item with posted lines (an item without a row is 0).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PosStockBalanceResult {
    /// Cloud stock store asked.
    pub store_code: i32,
    /// SMILE_BO database read.
    pub database: String,
    /// BO store (`RVC`) the cloud store maps to.
    pub store_rvc: i32,
    /// Balances by item code.
    pub items: Vec<PosStockBalance>,
}

impl WireJson for PosStockBalanceResult {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("store_code").int(self.store_code.into());
        w.key("database").string(&self.database);
        w.key("store_rvc").int(self.store_rvc.into());
        w.key("items");
        json::write_items(w, &self.items);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(PosStockBalanceResult {
            store_code: o.int("store_code")?,
            database: o.string("database")?,
            store_rvc: o.int("store_rvc")?,
            items: o.list("items", json::item)?,
        })
    }
}
