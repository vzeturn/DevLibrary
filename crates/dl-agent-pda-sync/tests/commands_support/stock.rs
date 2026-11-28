//! The in-memory SMILE_BO stock unit (port of the C# `FakeTxnStockUnit`): the store map, the
//! ledger probes and the vouchers, counts, receipts and issues the stock verbs write. A unit
//! works on a copy of the state, so a refusal or a failure leaves nothing behind.

use std::collections::{HashMap, HashSet};

use dl_agent_pda_sync::commands::{
    Result, StockApplied, StockCodeCount, StockCountDifference, StockCountPosted,
    StockCountSession, StockHeldLine, StockIssueLine, StockIssuePosted, StockLedgerProfile,
    StockReceipt, StockReceiptLedger, StockReceiptPosted, StockReceiptPostedLine, StockResult,
    StockSettings, StockStoreMap, StockVoucher, TxnStockUnit,
};
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_decimal::Decimal;
use dl_uuid::Uuid;

use super::Unit;

/// The stock part of the fake store.
#[derive(Debug, Clone)]
pub struct StockState {
    /// `Pda_TxnStockMap` by cloud store code.
    pub maps: HashMap<i32, StockStoreMap>,
    /// Database names the validation refuses.
    pub bad_databases: HashSet<String>,
    /// BO `DefRVCList` with each RVC's `RVCType`.
    pub stores: HashMap<i32, i32>,
    /// The ledger objects exist.
    pub ledger_ready: bool,
    /// The SX profile of `INVTrn`.
    pub sx_codes: Vec<StockCodeCount>,
    /// `GlobPara`.
    pub settings: StockSettings,
    /// `ItemDef` codes.
    pub items: HashSet<String>,
    /// `Pda_TxnApplied`: version, hash, command id, store key.
    pub applied: HashMap<String, (i64, String, Uuid, Option<i64>)>,
    /// `Pda_TxnStockHeld` per aggregate.
    pub held: HashMap<String, Vec<StockHeldLine>>,
    /// Every depletion / issue voucher posted, in order.
    pub vouchers: Vec<StockVoucher>,
    /// The next BO key (`VSeq`, session, header).
    pub next_seq: i64,
    /// The count objects exist.
    pub count_ready: bool,
    /// The count target passes its guards.
    pub count_target_ok: bool,
    /// `Inventory.VEnd` per item, for the counts.
    pub balances: HashMap<String, Decimal>,
    /// Count sessions by key, and the sessions posted in order.
    pub counts: HashMap<i64, StockCountPosted>,
    /// Count sessions written, in order.
    pub sessions: Vec<StockCountSession>,
    /// The receiving ledger.
    pub receipt_ledger: StockReceiptLedger,
    /// The receipt target passes its guards.
    pub receipt_target_ok: bool,
    /// Receipts by header key, and the receipts written in order.
    pub receipts: HashMap<i64, StockReceiptPosted>,
    /// Receipts written, in order.
    pub receipts_written: Vec<StockReceipt>,
    /// Issue vouchers by `VSeq`.
    pub issues: HashMap<i64, StockIssuePosted>,
    /// The vendor post of an issue posts no mirror line.
    pub issue_post_empty: bool,
}

impl Default for StockState {
    fn default() -> Self {
        StockState {
            maps: HashMap::new(),
            bad_databases: HashSet::new(),
            stores: HashMap::new(),
            ledger_ready: true,
            sx_codes: Vec::new(),
            settings: StockSettings {
                cost_method: Some(0),
                inv_period: Some(202_611),
                gl_period: Some(202_611),
            },
            items: HashSet::new(),
            applied: HashMap::new(),
            held: HashMap::new(),
            vouchers: Vec::new(),
            next_seq: 500,
            count_ready: true,
            count_target_ok: true,
            balances: HashMap::new(),
            counts: HashMap::new(),
            sessions: Vec::new(),
            receipt_ledger: StockReceiptLedger {
                ready: true,
                voucher_type: Some("RCV".into()),
                sr_codes: Vec::new(),
            },
            receipt_target_ok: true,
            receipts: HashMap::new(),
            receipts_written: Vec::new(),
            issues: HashMap::new(),
            issue_post_empty: false,
        }
    }
}

impl StockState {
    fn seq(&mut self) -> i64 {
        self.next_seq += 1;
        self.next_seq
    }
}

impl TxnStockUnit for Unit<'_> {
    fn lock_applied(&mut self, aggregate: &str, _: &Ctx) -> Result<Option<StockApplied>> {
        self.owner.call(format!("stock-lock:{aggregate}"));
        Ok(self.s.stock.applied.get(aggregate).map(|a| StockApplied {
            version: a.0,
            store_key: a.3,
        }))
    }

    fn save_applied(
        &mut self,
        aggregate: &str,
        version: i64,
        hash: &str,
        command_id: Uuid,
        store_key: Option<i64>,
        _: &Ctx,
    ) -> Result<()> {
        self.owner.call(format!("stock-save:{aggregate}"));
        self.s.stock.applied.insert(
            aggregate.to_owned(),
            (version, hash.to_owned(), command_id, store_key),
        );
        Ok(())
    }

    fn map(&mut self, store_code: i32, _: &Ctx) -> Result<Option<StockStoreMap>> {
        Ok(self.s.stock.maps.get(&store_code).cloned())
    }

    fn resolve_database(&mut self, configured: Option<&str>, _: &Ctx) -> Result<Option<String>> {
        let name = configured
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or("SMILE_BO");
        Ok((!self.s.stock.bad_databases.contains(name)).then(|| name.to_owned()))
    }

    fn stores_exist(&mut self, _: &str, rvcs: &[i32], _: &Ctx) -> Result<bool> {
        Ok(rvcs.iter().all(|r| self.s.stock.stores.contains_key(r)))
    }

    fn ledger_profile(&mut self, _: &str, _: &Ctx) -> Result<StockLedgerProfile> {
        self.owner.call("stock-profile".into());
        Ok(StockLedgerProfile {
            ready: self.s.stock.ledger_ready,
            voucher_types: 2,
            sx_codes: self.s.stock.sx_codes.clone(),
        })
    }

    fn settings(&mut self, _: &str, _: &Ctx) -> Result<StockSettings> {
        Ok(self.s.stock.settings)
    }

    fn unknown_items(&mut self, _: &str, item_codes: &[String], _: &Ctx) -> Result<Vec<String>> {
        Ok(item_codes
            .iter()
            .filter(|c| !self.s.stock.items.contains(*c))
            .cloned()
            .collect())
    }

    fn held(&mut self, aggregate: &str, _: &Ctx) -> Result<Vec<StockHeldLine>> {
        Ok(self
            .s
            .stock
            .held
            .get(aggregate)
            .cloned()
            .unwrap_or_default())
    }

    fn save_held(&mut self, aggregate: &str, lines: &[StockHeldLine], _: &Ctx) -> Result<()> {
        self.s
            .stock
            .held
            .insert(aggregate.to_owned(), lines.to_vec());
        Ok(())
    }

    fn post_voucher(&mut self, voucher: &StockVoucher, _: &Ctx) -> StockResult<i64> {
        self.owner.call(format!("stock-post:{}", voucher.reference));
        let vseq = self.s.stock.seq();
        self.s.stock.vouchers.push(voucher.clone());
        if voucher.trn_sub == 50 && !self.s.stock.issue_post_empty {
            let lines = voucher
                .lines
                .iter()
                .map(|l| StockIssueLine {
                    item_code: l.item_code.clone(),
                    trn_qty: l.trn_qty,
                    rvc: voucher.store_rvc,
                    with_rvc: voucher.with_rvc,
                })
                .collect();
            self.s.stock.issues.insert(
                vseq,
                StockIssuePosted {
                    vseq,
                    voucher_no: format!("SX{vseq}"),
                    reference: Some(voucher.reference.clone()),
                    lines,
                },
            );
        }
        Ok(vseq)
    }

    fn count_ledger_ready(&mut self, _: &str, _: &Ctx) -> Result<bool> {
        Ok(self.s.stock.count_ready)
    }

    fn count_target_valid(&mut self, _: &str, _: i32, _: i32, _: &str, _: &Ctx) -> Result<bool> {
        Ok(self.s.stock.count_target_ok)
    }

    fn post_count(
        &mut self,
        session: &StockCountSession,
        _: &Ctx,
    ) -> StockResult<StockCountPosted> {
        let seq = self.s.stock.seq();
        let lines: Vec<StockCountDifference> = session
            .lines
            .iter()
            .map(|l| StockCountDifference {
                item_code: l.item_code.clone(),
                system_qty: self
                    .s
                    .stock
                    .balances
                    .get(&l.item_code)
                    .copied()
                    .unwrap_or(Decimal::ZERO),
                counted_qty: l.counted_qty,
            })
            .collect();
        let any = lines.iter().any(|l| l.trn_qty() != Decimal::ZERO);
        let posted = StockCountPosted {
            session_seq: seq,
            voucher_no: any.then(|| format!("CNT{seq}")),
            lines,
            store_rvc: Some(session.store_rvc),
            reference: Some(session.reference.clone()),
        };
        self.s.stock.sessions.push(session.clone());
        self.s.stock.counts.insert(seq, posted.clone());
        Ok(posted)
    }

    fn read_count(
        &mut self,
        _: &str,
        session_seq: i64,
        _: &Ctx,
    ) -> Result<Option<StockCountPosted>> {
        Ok(self.s.stock.counts.get(&session_seq).cloned())
    }

    fn receipt_ledger(&mut self, _: &str, _: &Ctx) -> Result<StockReceiptLedger> {
        Ok(self.s.stock.receipt_ledger.clone())
    }

    fn receipt_target_valid(
        &mut self,
        _: &str,
        _: i32,
        _: i64,
        _: &[String],
        _: &Ctx,
    ) -> Result<bool> {
        Ok(self.s.stock.receipt_target_ok)
    }

    fn post_receipt(&mut self, receipt: &StockReceipt, _: &Ctx) -> StockResult<StockReceiptPosted> {
        let seq = self.s.stock.seq();
        let posted = StockReceiptPosted {
            receipt_seq: seq,
            voucher_no: format!("PNK{seq}"),
            lines: receipt
                .lines
                .iter()
                .map(|l| StockReceiptPostedLine {
                    item_code: l.item_code.clone(),
                    qty: l.qty,
                    amount: l.qty * l.unit_price,
                })
                .collect(),
            store_rvc: Some(receipt.store_rvc),
            reference: Some(receipt.reference.clone()),
            vendor_code: Some(receipt.vendor_code),
        };
        self.s.stock.receipts_written.push(receipt.clone());
        self.s.stock.receipts.insert(seq, posted.clone());
        Ok(posted)
    }

    fn read_receipt(
        &mut self,
        _: &str,
        receipt_seq: i64,
        _: &Ctx,
    ) -> Result<Option<StockReceiptPosted>> {
        Ok(self.s.stock.receipts.get(&receipt_seq).cloned())
    }

    fn read_issue(&mut self, _: &str, vseq: i64, _: &Ctx) -> Result<Option<StockIssuePosted>> {
        Ok(self.s.stock.issues.get(&vseq).cloned())
    }

    fn store_type(&mut self, _: &str, rvc: i32, _: &Ctx) -> Result<Option<i32>> {
        Ok(self.s.stock.stores.get(&rvc).copied())
    }
}
