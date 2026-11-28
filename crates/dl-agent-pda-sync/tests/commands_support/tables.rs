//! The fake's table verbs (port of the C# `FakeTableCommandUnit`), kept apart so the
//! fake store stays within its size budget.

use super::*;

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
