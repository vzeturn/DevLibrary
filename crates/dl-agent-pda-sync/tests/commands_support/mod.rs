//! In-memory store for the site command executor (port of the C# `FakeSiteCommandStore`,
//! `FakeOrderCommandUnit(.Combo)` and `FakeTableCommandUnit`): a command log plus the
//! vendor rows the commands touch. A unit works on a copy of the state that replaces the
//! committed state only when the work returns (a failure rolls back), mirroring the single
//! SQL transaction of `SqlSiteCommandStore`.

#![allow(dead_code)]

mod money;
mod orders;
mod txn;
mod txn_fo;

pub use orders::{Check, Line, OrderBook};
pub use txn::TxnState;
#[allow(unused_imports)]
pub use txn_fo::{CounterBook, FakeFolioRow, FolioBook};

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use dl_agent_pda_sync::capture::CaptureNudge;
use dl_agent_pda_sync::commands::{
    CheckMoneyFacts, CommandError, CommandWork, EmptyBillFacts, OrderCloseOutcome,
    OrderCommandUnit, OrderLineFacts, OrderPostOutcome, Result, SiteCommandStore, SiteCommandUnit,
    StoreColumn, TableCommandUnit, TxnCommandUnit, TxnCounterUnit, TxnRoomChargeUnit,
};
use dl_agent_pda_sync::config::{ManualClock, SharedClock, Timestamp};
use dl_agent_pda_sync::contracts::wire::{
    CheckStateHeader, OrderCheckCancelEmptyPayload, OrderCheckClosePayload, OrderCheckLine,
    OrderCheckMergePayload, OrderCheckOpenPayload, OrderCheckResult, OrderComboPostPayload,
    OrderCoversPayload, OrderItemPostPayload, OrderItemVoidPayload, OrderItemsMovePayload,
    OrderPoNumberPayload, OrderRoomChargePayload, OrderSendPayload, OrderTableChangePayload,
    OrderTableTransferPayload, OrderTenderAddPayload, OrderTenderRemovePayload,
    ReservationSavePayload, SyncCommandResult, kds_line_actions, order_command_reasons as reasons,
};
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_decimal::Decimal;
use dl_uuid::Uuid;

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The store's rows.
#[derive(Debug, Clone)]
pub struct State {
    pub open_checks: HashSet<(i32, i64)>,
    pub lines: HashMap<(i64, i64), Line>,
    pub reservations: HashMap<i32, (i32, String)>,
    pub next_reservation_id: i32,
    /// Check state fingerprint per open check; a check without an entry reads as not open.
    pub fingerprints: HashMap<i64, String>,
    pub orders: OrderBook,
    /// txn.snapshot rows (ADR 0159).
    pub txn: TxnState,
}

impl Default for State {
    fn default() -> Self {
        State {
            open_checks: HashSet::new(),
            lines: HashMap::new(),
            reservations: HashMap::new(),
            next_reservation_id: 100,
            fingerprints: HashMap::new(),
            orders: OrderBook::default(),
            txn: TxnState::default(),
        }
    }
}

/// What the work throws after its writes.
#[derive(Debug, Clone)]
pub enum Throw {
    /// `InvalidOperationException`.
    Invalid(String),
    /// `SqlException` with this number.
    Sql(i32, String),
}

/// Knobs of the fake.
#[derive(Debug, Default)]
pub struct Knobs {
    pub kds_accept_columns_missing: bool,
    pub throw_in_work: Option<Throw>,
    pub post_refusal: Option<String>,
    pub close_before_covers: bool,
    pub missing_columns: HashSet<String>,
    pub paid_checks: HashSet<i64>,
    pub discounted_checks: HashSet<i64>,
    /// ADR 0144: checks opened at or after the empty-bill cutoff; every other check is stale.
    pub fresh_checks: HashSet<i64>,
    pub move_audit_error: Option<i32>,
    /// txn.snapshot: the replace hits another bill's key.
    pub txn_key_conflict: bool,
}

/// The fake store.
#[derive(Default)]
pub struct FakeStore {
    pub committed: Mutex<State>,
    pub log: Mutex<HashMap<Uuid, SyncCommandResult>>,
    pub knobs: Mutex<Knobs>,
    /// Every order write the executor asked for, in order (survives a rollback).
    pub calls: Mutex<Vec<String>>,
    pub runs: AtomicUsize,
    pub work_runs: AtomicUsize,
    pub schema_probes: AtomicUsize,
}

impl FakeStore {
    pub fn new() -> Arc<FakeStore> {
        Arc::new(FakeStore::default())
    }

    pub fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.committed)
    }

    pub fn knobs(&self) -> MutexGuard<'_, Knobs> {
        lock(&self.knobs)
    }

    pub fn calls(&self) -> Vec<String> {
        lock(&self.calls).clone()
    }

    pub fn work_runs(&self) -> usize {
        self.work_runs.load(Ordering::SeqCst)
    }

    pub fn runs(&self) -> usize {
        self.runs.load(Ordering::SeqCst)
    }

    pub fn logged(&self, id: Uuid) -> bool {
        lock(&self.log).contains_key(&id)
    }

    fn call(&self, c: String) {
        lock(&self.calls).push(c);
    }
}

impl SiteCommandStore for FakeStore {
    fn find_recorded(&self, command_id: Uuid, _ctx: &Ctx) -> Result<Option<SyncCommandResult>> {
        Ok(lock(&self.log).get(&command_id).cloned())
    }

    fn run_once(
        &self,
        command_id: Uuid,
        work: CommandWork<'_>,
        ctx: &Ctx,
    ) -> Result<SyncCommandResult> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        if let Some(r) = lock(&self.log).get(&command_id) {
            return Ok(r.clone());
        }
        self.work_runs.fetch_add(1, Ordering::SeqCst);
        let mut draft = self.state().clone();
        let result = {
            let mut unit = Unit {
                s: &mut draft,
                owner: self,
            };
            work(&mut unit, ctx)?
        };
        // After the writes: nothing may survive.
        match self.knobs().throw_in_work.clone() {
            Some(Throw::Invalid(m)) => return Err(CommandError::Invalid(m)),
            Some(Throw::Sql(number, message)) => {
                return Err(CommandError::Sql(
                    dl_tds::Error::Server {
                        number,
                        state: 1,
                        class: 14,
                        message,
                        server: "fake".into(),
                        procedure: String::new(),
                        line: 1,
                    },
                    Vec::new(),
                ));
            }
            None => {}
        }
        *self.state() = draft;
        lock(&self.log).insert(command_id, result.clone());
        Ok(result)
    }
}

struct Unit<'a> {
    s: &'a mut State,
    owner: &'a FakeStore,
}

impl Unit<'_> {
    fn read(&self, rvc_no: i32, check_no: i64) -> Option<OrderCheckResult> {
        let c = self.s.orders.checks.get(&(rvc_no, check_no))?;
        let lines: Vec<OrderCheckLine> = self
            .s
            .orders
            .lines
            .iter()
            .filter(|(k, _)| k.0 == check_no)
            .map(|(_, l)| l.clone())
            .collect();
        let sub_total = lines.iter().fold(Decimal::ZERO, |a, l| a + l.amount);
        let header = CheckStateHeader {
            check_no,
            table_no: Some(c.table_no.clone()),
            guests: c.guests,
            sub_total,
            total_paid: Decimal::ZERO,
        };
        let fingerprint = OrderCheckResult::fingerprint(&header, &lines);
        Some(OrderCheckResult {
            rvc_no,
            check_no,
            table_no: Some(c.table_no.clone()),
            guests: c.guests,
            sub_total,
            total_paid: Decimal::ZERO,
            lines,
            fingerprint_after: fingerprint,
            trn_seq: None,
            sent_seqs: None,
        })
    }

    /// Keeps the guard's view of the check (open set + fingerprint) in step.
    fn touch(&mut self, rvc_no: i32, check_no: i64) {
        self.s.open_checks.insert((rvc_no, check_no));
        if let Some(r) = self.read(rvc_no, check_no) {
            self.s.fingerprints.insert(check_no, r.fingerprint_after);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn add_line(
        &mut self,
        check_no: i64,
        seq: i64,
        code: &str,
        name: &str,
        qty: Decimal,
        price: Decimal,
        seat: i32,
        cashier: &str,
        note: Option<String>,
    ) {
        self.s.orders.lines.insert(
            (check_no, seq),
            OrderCheckLine {
                line_seq: seq,
                transaction_code: Some(OrderCheckLine::ITEM_TRANSACTION_CODE.to_owned()),
                item_code: Some(code.to_owned()),
                item_name: Some(name.to_owned()),
                qty,
                price,
                amount: qty * price,
                seat_no: seat,
                cashier: Some(cashier.to_owned()),
                sent: false,
                note,
            },
        );
    }
}

fn qty_text(d: Decimal) -> String {
    d.normalize().to_string()
}

impl SiteCommandUnit for Unit<'_> {
    fn open_check_belongs_to_rvc(&mut self, rvc_no: i32, check_no: i64, _: &Ctx) -> Result<bool> {
        Ok(self.s.open_checks.contains(&(rvc_no, check_no)))
    }

    fn check_fingerprint(&mut self, check_no: i64, _: &Ctx) -> Result<Option<String>> {
        Ok(self.s.fingerprints.get(&check_no).cloned())
    }

    fn kds_accept_columns_exist(&mut self, _: &Ctx) -> Result<bool> {
        Ok(!self.owner.knobs().kds_accept_columns_missing)
    }

    fn apply_kds_line_state(
        &mut self,
        check_no: i64,
        line_seqs: &[i64],
        action: &str,
        cashier: &str,
        _: &Ctx,
    ) -> Result<i32> {
        let mut changed = 0;
        for seq in line_seqs {
            let Some(line) = self.s.lines.get(&(check_no, *seq)).cloned() else {
                continue;
            };
            let next = match action {
                kds_line_actions::ACCEPT if !line.accepted => Line {
                    accepted: true,
                    accepted_by: Some(cashier.to_owned()),
                    ..line
                },
                kds_line_actions::UNACCEPT if line.accepted => Line {
                    accepted: false,
                    accepted_by: None,
                    ..line
                },
                kds_line_actions::DONE if !line.done => Line { done: true, ..line },
                kds_line_actions::UNDONE if line.done => Line {
                    done: false,
                    ..line
                },
                _ => continue,
            };
            self.s.lines.insert((check_no, *seq), next);
            changed += 1;
        }
        Ok(changed)
    }

    fn insert_reservation(&mut self, r: &ReservationSavePayload, _: &Ctx) -> Result<i32> {
        let id = self.s.next_reservation_id;
        self.s.next_reservation_id += 1;
        self.s
            .reservations
            .insert(id, (r.rvc_no, r.guest_info.clone()));
        Ok(id)
    }

    fn delete_reservation(&mut self, rvc_no: i32, id: i32, _: &Ctx) -> Result<bool> {
        if self.s.reservations.get(&id).is_some_and(|r| r.0 == rvc_no) {
            self.s.reservations.remove(&id);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn orders(&mut self) -> &mut dyn OrderCommandUnit {
        self
    }

    fn tables(&mut self) -> &mut dyn TableCommandUnit {
        self
    }

    fn txn(&mut self) -> &mut dyn TxnCommandUnit {
        self
    }

    fn counters(&mut self) -> &mut dyn TxnCounterUnit {
        self
    }

    fn room_charge(&mut self) -> &mut dyn TxnRoomChargeUnit {
        self
    }
}

/// The package definitions the fake knows: combo code → (code, qty per combo, price).
fn combo(code: &str) -> Option<Vec<(&'static str, Decimal, Decimal)>> {
    (code == "10").then(|| {
        vec![
            ("CF01", Decimal::ONE, Decimal::from(20_000)),
            ("TEA", Decimal::from(2), Decimal::from(15_000)),
        ]
    })
}

impl OrderCommandUnit for Unit<'_> {
    fn check_status(&mut self, rvc_no: i32, check_no: i64, _: &Ctx) -> Result<Option<i32>> {
        Ok(self
            .s
            .orders
            .checks
            .get(&(rvc_no, check_no))
            .map(|c| c.status))
    }

    fn item_exists(&mut self, item_code: &str, _: &Ctx) -> Result<bool> {
        Ok(self.s.orders.items.contains(item_code))
    }

    fn open_check(&mut self, p: &OrderCheckOpenPayload, _: &Ctx) -> Result<i64> {
        self.owner.call(format!(
            "open {} {} {} {}",
            p.rvc_no, p.table_no, p.num_guest, p.cashier
        ));
        let check_no = self.s.orders.next_check_no;
        self.s.orders.next_check_no += 1;
        self.s.orders.checks.insert(
            (p.rvc_no, check_no),
            Check {
                table_no: p.table_no.clone(),
                guests: p.num_guest,
                status: 1,
                children: 0,
            },
        );
        self.touch(p.rvc_no, check_no);
        Ok(check_no)
    }

    fn find_line(
        &mut self,
        _rvc_no: i32,
        check_no: i64,
        trn_seq: i64,
        _: &Ctx,
    ) -> Result<Option<OrderLineFacts>> {
        Ok(self
            .s
            .orders
            .lines
            .get(&(check_no, trn_seq))
            .map(|l| OrderLineFacts {
                transaction_code: l.transaction_code.clone(),
                sent: l.sent,
            }))
    }

    fn post_item(&mut self, p: &OrderItemPostPayload, _: &Ctx) -> Result<OrderPostOutcome> {
        self.owner.call(format!(
            "post {} {} x{}",
            p.check_no,
            p.item_code,
            qty_text(p.qty)
        ));
        if let Some(reason) = self.owner.knobs().post_refusal.clone() {
            return Ok(OrderPostOutcome::refused(&reason));
        }
        let seq = self.s.orders.next_trn_seq;
        self.s.orders.next_trn_seq += 1;
        let name = match &p.open_name {
            Some(n) if n.chars().count() > 30 => n.chars().take(30).collect(),
            Some(n) => n.clone(),
            None => p.item_code.clone(),
        };
        self.add_line(
            p.check_no,
            seq,
            &p.item_code,
            &name,
            p.qty,
            p.price,
            p.seat_no,
            &p.cashier,
            p.note.clone(),
        );
        self.touch(p.rvc_no, p.check_no);
        Ok(OrderPostOutcome::posted(seq))
    }

    fn pending_send_seqs(&mut self, _rvc_no: i32, check_no: i64, _: &Ctx) -> Result<Vec<i64>> {
        Ok(self
            .s
            .orders
            .lines
            .iter()
            .filter(|(k, l)| k.0 == check_no && l.is_item() && !l.sent)
            .map(|(k, _)| k.1)
            .collect())
    }

    fn send(&mut self, p: &OrderSendPayload, _: &Ctx) -> Result<()> {
        self.owner.call(format!("send {}", p.check_no));
        for (k, l) in self.s.orders.lines.iter_mut() {
            if k.0 == p.check_no {
                l.sent = true;
            }
        }
        self.touch(p.rvc_no, p.check_no);
        Ok(())
    }

    fn void_unsent(&mut self, p: &OrderItemVoidPayload, _: &Ctx) -> Result<()> {
        self.owner
            .call(format!("void-unsent {} {}", p.check_no, p.trn_seq));
        self.s.orders.lines.remove(&(p.check_no, p.trn_seq));
        self.touch(p.rvc_no, p.check_no);
        Ok(())
    }

    fn void_sent(&mut self, p: &OrderItemVoidPayload, _: &Ctx) -> Result<()> {
        self.owner.call(format!(
            "void-sent {} {} {}",
            p.check_no,
            p.trn_seq,
            p.reason.as_deref().unwrap_or("")
        ));
        self.s.orders.lines.remove(&(p.check_no, p.trn_seq));
        self.touch(p.rvc_no, p.check_no);
        Ok(())
    }

    fn set_covers(&mut self, p: &OrderCoversPayload, _: &Ctx) -> Result<bool> {
        self.owner.call(format!(
            "covers {} {}/{} {}",
            p.check_no, p.num_guest, p.num_child, p.cashier
        ));
        let key = (p.rvc_no, p.check_no);
        if self.owner.knobs().close_before_covers
            && let Some(c) = self.s.orders.checks.get_mut(&key)
        {
            c.status = 2;
        }
        match self.s.orders.checks.get_mut(&key) {
            Some(c) if c.status == 1 => {
                c.guests = p.num_guest;
                c.children = p.num_child;
            }
            _ => return Ok(false),
        }
        self.touch(p.rvc_no, p.check_no);
        Ok(true)
    }

    fn post_combo(&mut self, p: &OrderComboPostPayload, _: &Ctx) -> Result<OrderPostOutcome> {
        self.owner.call(format!(
            "combo {} {} x{}",
            p.check_no,
            p.combo_code,
            qty_text(p.qty)
        ));
        if !self.s.orders.checks.contains_key(&(p.rvc_no, p.check_no)) {
            return Ok(OrderPostOutcome::refused(reasons::CHECK_NOT_FOUND));
        }
        let selected = p.selected_codes();
        let lines: Vec<(String, Decimal, Decimal)> = if let Some(components) = combo(&p.combo_code)
        {
            let lines: Vec<(String, Decimal, Decimal)> = components
                .into_iter()
                .filter(|c| selected.is_empty() || selected.iter().any(|s| s == c.0))
                .map(|(c, q, pr)| (c.to_owned(), q, pr))
                .collect();
            if !selected.is_empty() && lines.len() != selected.len() {
                return Ok(OrderPostOutcome::refused(reasons::ITEM_NOT_FOUND));
            }
            lines
        } else if self.s.orders.items.contains(&p.combo_code) {
            vec![(p.combo_code.clone(), Decimal::ONE, p.price)]
        } else {
            return Ok(OrderPostOutcome::refused(reasons::ITEM_NOT_FOUND));
        };
        let mut first = 0;
        for (code, qty, price) in lines {
            let seq = self.s.orders.next_trn_seq;
            self.s.orders.next_trn_seq += 1;
            if first == 0 {
                first = seq;
            }
            self.add_line(
                p.check_no,
                seq,
                &code,
                &code,
                qty * p.qty,
                price,
                p.seat_no,
                &p.cashier,
                p.note.clone(),
            );
        }
        self.touch(p.rvc_no, p.check_no);
        Ok(OrderPostOutcome::posted(first))
    }

    fn read_check(
        &mut self,
        rvc_no: i32,
        check_no: i64,
        _: &Ctx,
    ) -> Result<Option<OrderCheckResult>> {
        Ok(self.read(rvc_no, check_no))
    }

    fn add_tender(&mut self, p: &OrderTenderAddPayload, _: &Ctx) -> Result<OrderPostOutcome> {
        Ok(self.fake_add_tender(p))
    }

    fn add_room_charge(&mut self, p: &OrderRoomChargePayload, _: &Ctx) -> Result<OrderPostOutcome> {
        Ok(self.fake_add_room_charge(p))
    }

    fn remove_tender(&mut self, p: &OrderTenderRemovePayload, _: &Ctx) -> Result<Option<String>> {
        Ok(self.fake_remove_tender(p))
    }

    fn close_check(&mut self, p: &OrderCheckClosePayload, _: &Ctx) -> Result<OrderCloseOutcome> {
        Ok(self.fake_close_check(p))
    }
}

impl TableCommandUnit for Unit<'_> {
    fn missing_columns(&mut self, columns: &[StoreColumn], _: &Ctx) -> Result<Vec<String>> {
        self.owner.schema_probes.fetch_add(1, Ordering::SeqCst);
        let knobs = self.owner.knobs();
        Ok(columns
            .iter()
            .map(ToString::to_string)
            .filter(|c| knobs.missing_columns.contains(c))
            .collect())
    }

    fn lock_check(&mut self, rvc_no: i32, check_no: i64, _: &Ctx) -> Result<Option<i32>> {
        self.owner.call(format!("lock {check_no}"));
        Ok(self
            .s
            .orders
            .checks
            .get(&(rvc_no, check_no))
            .map(|c| c.status))
    }

    fn money_facts(&mut self, _rvc_no: i32, check_no: i64, _: &Ctx) -> Result<CheckMoneyFacts> {
        let k = self.owner.knobs();
        Ok(CheckMoneyFacts {
            has_payment: k.paid_checks.contains(&check_no),
            has_discount: k.discounted_checks.contains(&check_no),
        })
    }

    fn empty_bill_facts(
        &mut self,
        rvc_no: i32,
        check_no: i64,
        opened_before: &str,
        _: &Ctx,
    ) -> Result<Option<EmptyBillFacts>> {
        self.owner
            .call(format!("facts {check_no} before {opened_before}"));
        let fresh = self.owner.knobs().fresh_checks.contains(&check_no);
        let lines = self
            .s
            .orders
            .lines
            .keys()
            .filter(|k| k.0 == check_no)
            .count();
        Ok(self
            .s
            .orders
            .checks
            .get(&(rvc_no, check_no))
            .map(|c| EmptyBillFacts {
                table_no: c.table_no.trim().to_owned(),
                opened_before_cutoff: !fresh,
                line_count: i32::try_from(lines).unwrap_or(i32::MAX),
            }))
    }

    fn cancel_empty(&mut self, p: &OrderCheckCancelEmptyPayload, _: &Ctx) -> Result<()> {
        self.owner
            .call(format!("cancel-empty {} by {}", p.check_no, p.cashier));
        self.s.orders.checks.remove(&(p.rvc_no, p.check_no));
        self.s.open_checks.remove(&(p.rvc_no, p.check_no));
        self.s.fingerprints.remove(&p.check_no);
        Ok(())
    }

    fn set_po_number(&mut self, p: &OrderPoNumberPayload, _: &Ctx) -> Result<()> {
        // RT-1413: the header's PONumber only; nothing the fingerprint or the book models.
        let text = p.po_number.as_deref().unwrap_or_default();
        self.owner.call(format!("po {} {text}", p.check_no));
        Ok(())
    }

    fn change_table(&mut self, p: &OrderTableChangePayload, _: &Ctx) -> Result<()> {
        self.owner.call(format!(
            "table {} {} {}",
            p.check_no, p.table_no, p.num_guest
        ));
        if let Some(c) = self.s.orders.checks.get_mut(&(p.rvc_no, p.check_no)) {
            c.table_no = p.table_no.clone();
            c.guests = p.num_guest;
            c.children = 0;
        }
        self.touch(p.rvc_no, p.check_no);
        Ok(())
    }

    fn transfer(&mut self, p: &OrderTableTransferPayload, _: &Ctx) -> Result<()> {
        self.owner
            .call(format!("transfer {} {}", p.check_no, p.to_table_no.trim()));
        if let Some(c) = self.s.orders.checks.get_mut(&(p.rvc_no, p.check_no)) {
            c.table_no = p.to_table_no.trim().to_owned();
        }
        self.touch(p.rvc_no, p.check_no);
        Ok(())
    }

    fn merge(&mut self, p: &OrderCheckMergePayload, _: &Ctx) -> Result<()> {
        self.owner.call(format!(
            "merge {}->{}",
            p.source_check_no, p.target_check_no
        ));
        let keys: Vec<(i64, i64)> = self
            .s
            .orders
            .lines
            .keys()
            .filter(|k| k.0 == p.source_check_no)
            .copied()
            .collect();
        for k in keys {
            if let Some(mut l) = self.s.orders.lines.remove(&k) {
                l.line_seq = k.1;
                self.s.orders.lines.insert((p.target_check_no, k.1), l);
            }
        }
        self.s.orders.checks.remove(&(p.rvc_no, p.source_check_no));
        self.s.open_checks.remove(&(p.rvc_no, p.source_check_no));
        self.s.fingerprints.remove(&p.source_check_no);
        self.touch(p.rvc_no, p.target_check_no);
        Ok(())
    }

    fn lock_item_lines(&mut self, _rvc_no: i32, check_no: i64, _: &Ctx) -> Result<Vec<i64>> {
        self.owner.call(format!("lock-lines {check_no}"));
        Ok(self
            .s
            .orders
            .lines
            .iter()
            .filter(|(k, l)| k.0 == check_no && l.is_item())
            .map(|(k, _)| k.1)
            .collect())
    }

    fn move_items(
        &mut self,
        p: &OrderItemsMovePayload,
        trn_seqs: &[i64],
        _: &Ctx,
    ) -> Result<Option<i32>> {
        let list: Vec<String> = trn_seqs.iter().map(i64::to_string).collect();
        self.owner.call(format!(
            "move {}->{} {}",
            p.from_check_no,
            p.to_check_no,
            list.join(",")
        ));
        for seq in trn_seqs {
            let Some(line) = self.s.orders.lines.remove(&(p.from_check_no, *seq)) else {
                return Err(CommandError::Invalid(format!(
                    "Line {seq} left check {}.",
                    p.from_check_no
                )));
            };
            self.s.orders.lines.insert((p.to_check_no, *seq), line);
        }
        self.touch(p.rvc_no, p.from_check_no);
        self.touch(p.rvc_no, p.to_check_no);
        Ok(self.owner.knobs().move_audit_error)
    }
}

/// Counts capture nudges and records the checks a nudge asked to re-read (RT-1309).
#[derive(Default)]
pub struct CountingNudge(pub AtomicUsize, pub Mutex<Vec<String>>);

impl CountingNudge {
    pub fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }

    #[allow(dead_code)]
    pub fn checks(&self) -> Vec<String> {
        self.1.lock().unwrap().clone()
    }
}

impl CaptureNudge for CountingNudge {
    fn nudge_open_checks(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn nudge_open_check(&self, check_key: &str) {
        self.1.lock().unwrap().push(check_key.to_owned());
        self.nudge_open_checks();
    }
}

/// A manual clock at `unix_seconds`.
pub fn clock(unix_seconds: i64) -> SharedClock {
    Arc::new(ManualClock::new(Timestamp::from_unix_seconds(unix_seconds)))
}
