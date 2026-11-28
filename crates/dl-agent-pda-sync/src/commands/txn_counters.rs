//! The store seam of `txn.counters` (ADR 0161, RT-0848; `ITxnCounterUnit.cs`); the verb runs
//! in [`crate::commands::SiteCommandExecutor`] (`executor/txn_counters.rs`).

use super::error::Result;
use crate::util::ctx::Ctx;

/// The store reads and writes of `txn.counters` (`ITxnCounterUnit`) inside the transaction
/// of [`super::SiteCommandStore::run_once`]: the `dbo.Para` per-RVC counters
/// `dbo.GetRVCSeqNumber` advances. Counter names come only from
/// [`txn_counter_names`](crate::contracts::wire::txn_counter_names);
/// every value is a parameter.
pub trait TxnCounterUnit {
    /// True when the store keeps `name` of `rvc_no` in an object of its own
    /// (`dbo.LastCheck<rvc>` / `dbo.LastTrn<rvc>`, RT-0654) instead of `dbo.Para`: such a
    /// counter is not raised.
    fn has_own_counter_object(&mut self, rvc_no: i32, name: &str, ctx: &Ctx) -> Result<bool>;

    /// The stored counter, its `dbo.Para` row update-locked until commit (inserted at `'0'`
    /// first when missing, as `GetRVCSeqNumber` seeds it).
    fn lock(&mut self, rvc_no: i32, name: &str, ctx: &Ctx) -> Result<i64>;

    /// Sets the stored counter to `value` where it is lower; never lowers it.
    fn raise(&mut self, rvc_no: i32, name: &str, value: i64, ctx: &Ctx) -> Result<()>;

    /// The highest per-RVC counter already present in the counter's backing tables; `None`
    /// when none.
    fn in_use(&mut self, rvc_no: i32, name: &str, ctx: &Ctx) -> Result<Option<i64>>;
}
