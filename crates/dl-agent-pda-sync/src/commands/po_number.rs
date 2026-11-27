//! `order.po_number` (`SiteCommandExecutor.PoNumber.cs`, RT-1413): the guest picked for a
//! check in the PDA, written to the store's `TmpInvoice.PONumber`. Guarded like the table
//! change: the column must exist in this build and the check must be open in the RVC;
//! nothing else of the check is written, so it carries no expected state and returns no
//! check.

use super::error::Result;
use super::executor::SiteCommandExecutor;
use crate::contracts::wire::{OrderPoNumberPayload, SyncCommand, SyncCommandResult};
use crate::util::ctx::Ctx;

impl SiteCommandExecutor {
    pub(super) fn po_number(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let p = match self.payload(c, "an order.po_number", OrderPoNumberPayload::problem) {
            Ok(p) => p,
            Err(r) => return Ok(*r),
        };
        self.run_guarded(
            c,
            &mut |unit, t| {
                if let Some(r) = self.schema_refusal(c, unit, t)? {
                    return Ok(r);
                }
                if let Some(r) = self.open_refusal(c, unit, p.rvc_no, p.check_no, t)? {
                    return Ok(r);
                }
                unit.tables().set_po_number(&p, t)?;
                Ok(self.applied(c, None))
            },
            ctx,
        )
    }
}
