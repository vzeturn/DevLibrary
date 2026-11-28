//! The in-memory `txn.counters` and room-charge units (port of the C# `FakeSiteCommandStore`
//! Counters and RoomCharge parts): the `dbo.Para` counters, their backing-table high water
//! and per-RVC objects, and a Front Office folio book. Kept in the txn state so a rollback
//! restores them with the draft.

use std::collections::{HashMap, HashSet};

use dl_agent_pda_sync::commands::{
    FoActiveRow, FoFolio, FoPostLine, Result, TxnCounterUnit, TxnRoomChargeUnit,
};
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_decimal::Decimal;

use super::Unit;

/// The counters of the fake store, keyed by (RVC, name).
#[derive(Debug, Clone, Default)]
pub struct CounterBook {
    /// `dbo.Para` counters.
    pub stored: HashMap<(i32, String), i64>,
    /// The highest counter in the backing tables; a missing key reads as none.
    pub in_use: HashMap<(i32, String), i64>,
    /// Counters the store keeps in a per-RVC object instead of `dbo.Para`.
    pub objects: HashSet<(i32, String)>,
}

/// One `FolioTransaction` row: `code` is `'A'` (active) or `'V'` (voided or mirror).
#[derive(Debug, Clone, PartialEq)]
pub struct FakeFolioRow {
    pub id: i64,
    pub folio: i64,
    pub reference: String,
    pub amount: Decimal,
    pub code: char,
    pub trn_code: i32,
    pub trn_sub: i32,
    pub clerk: String,
    pub comment: String,
}

/// The Front Office of the fake: database, tender links, in-house folios, folio rows.
#[derive(Debug, Clone)]
pub struct FolioBook {
    /// The resolved database; `None` reads as "not usable".
    pub database: Option<String>,
    pub links: HashMap<(i32, i32), i32>,
    pub folios: Vec<FoFolio>,
    pub rows: Vec<FakeFolioRow>,
    pub next_id: i64,
    pub posts: usize,
}

impl Default for FolioBook {
    fn default() -> Self {
        FolioBook {
            database: Some("SMILE_FO".into()),
            links: HashMap::new(),
            folios: Vec::new(),
            rows: Vec::new(),
            next_id: 1000,
            posts: 0,
        }
    }
}

impl FolioBook {
    /// The active rows.
    pub fn active(&self) -> Vec<FakeFolioRow> {
        self.rows
            .iter()
            .filter(|r| r.code == 'A')
            .cloned()
            .collect()
    }
}

impl TxnCounterUnit for Unit<'_> {
    fn has_own_counter_object(&mut self, rvc_no: i32, name: &str, _ctx: &Ctx) -> Result<bool> {
        Ok(self
            .s
            .txn
            .counters
            .objects
            .contains(&(rvc_no, name.to_owned())))
    }

    fn lock(&mut self, rvc_no: i32, name: &str, _ctx: &Ctx) -> Result<i64> {
        self.owner.call(format!("counter:lock:{rvc_no}:{name}"));
        Ok(*self
            .s
            .txn
            .counters
            .stored
            .entry((rvc_no, name.to_owned()))
            .or_insert(0))
    }

    fn raise(&mut self, rvc_no: i32, name: &str, value: i64, _ctx: &Ctx) -> Result<()> {
        self.owner
            .call(format!("counter:raise:{rvc_no}:{name}:{value}"));
        let stored = self
            .s
            .txn
            .counters
            .stored
            .entry((rvc_no, name.to_owned()))
            .or_insert(0);
        if *stored < value {
            *stored = value;
        }
        Ok(())
    }

    fn in_use(&mut self, rvc_no: i32, name: &str, _ctx: &Ctx) -> Result<Option<i64>> {
        Ok(self
            .s
            .txn
            .counters
            .in_use
            .get(&(rvc_no, name.to_owned()))
            .copied())
    }
}

impl TxnRoomChargeUnit for Unit<'_> {
    fn resolve_database(&mut self, _rvc: i32, _ctx: &Ctx) -> Result<Option<String>> {
        Ok(self.s.txn.fo.database.clone())
    }

    fn link_code(
        &mut self,
        _rvc: i32,
        trn_code: i32,
        trn_sub: i32,
        _ctx: &Ctx,
    ) -> Result<Option<i32>> {
        Ok(self.s.txn.fo.links.get(&(trn_code, trn_sub)).copied())
    }

    fn in_house_folios(
        &mut self,
        _database: &str,
        room: &str,
        folio_num: i64,
        _ctx: &Ctx,
    ) -> Result<Vec<FoFolio>> {
        Ok(self
            .s
            .txn
            .fo
            .folios
            .iter()
            .filter(|f| {
                if folio_num > 0 {
                    f.folio_num == folio_num
                } else {
                    f.room_code.eq_ignore_ascii_case(room)
                }
            })
            .cloned()
            .collect())
    }

    fn lock_active(
        &mut self,
        _database: &str,
        folio_num: i64,
        ref_number: &str,
        _ctx: &Ctx,
    ) -> Result<Vec<FoActiveRow>> {
        Ok(self
            .s
            .txn
            .fo
            .rows
            .iter()
            .filter(|r| r.folio == folio_num && r.reference == ref_number && r.code == 'A')
            .map(|r| FoActiveRow {
                transaction_id: r.id,
                amount: r.amount,
            })
            .collect())
    }

    fn reverse(
        &mut self,
        _database: &str,
        transaction_ids: &[i64],
        comment: &str,
        _ctx: &Ctx,
    ) -> Result<()> {
        let fo = &mut self.s.txn.fo;
        for id in transaction_ids {
            let i = fo
                .rows
                .iter()
                .position(|r| r.id == *id)
                .expect("reversed row exists");
            fo.rows[i].code = 'V';
            let mirror = FakeFolioRow {
                id: fo.next_id,
                amount: -fo.rows[i].amount,
                code: 'V',
                comment: comment.to_owned(),
                ..fo.rows[i].clone()
            };
            fo.next_id += 1;
            fo.rows.push(mirror);
        }
        Ok(())
    }

    fn post(&mut self, _database: &str, line: &FoPostLine, _ctx: &Ctx) -> Result<()> {
        self.owner.call(format!("fo:post:{}", line.folio_num));
        let fo = &mut self.s.txn.fo;
        fo.posts += 1;
        fo.rows.push(FakeFolioRow {
            id: fo.next_id,
            folio: line.folio_num,
            reference: line.ref_number.clone(),
            amount: line.amount,
            code: 'A',
            trn_code: line.transaction_code,
            trn_sub: line.transaction_sub_code,
            clerk: line.clerk_id.clone(),
            comment: line.comment.clone(),
        });
        fo.next_id += 1;
        Ok(())
    }
}
