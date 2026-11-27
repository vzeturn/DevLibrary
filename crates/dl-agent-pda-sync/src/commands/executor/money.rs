//! The money verbs (`SiteCommandExecutor.Money.cs`, ADR 0174, RT-1297): tender add and
//! remove answer with the check read inside the command transaction like the order verbs;
//! the close answers with the closed bill, since the check has left the open checks. A
//! refusal writes nothing and is logged.

use super::SiteCommandExecutor;
use crate::commands::error::{CommandError, Result};
use crate::contracts::WireJson;
use crate::contracts::wire::{
    OrderCheckClosePayload, OrderRoomChargePayload, OrderTenderAddPayload,
    OrderTenderRemovePayload, SyncCommand, SyncCommandResult,
};
use crate::util::ctx::Ctx;

impl SiteCommandExecutor {
    pub(super) fn tender_add(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let p = match self.payload(c, "an order.tender_add", OrderTenderAddPayload::problem) {
            Ok(p) => p,
            Err(r) => return Ok(*r),
        };
        self.run_guarded(
            c,
            &mut |unit, t| {
                let posted = unit.orders().add_tender(&p, t)?;
                if let Some(reason) = &posted.reason {
                    return Ok(self.reject(c, reason));
                }
                self.applied_check(c, unit, p.rvc_no, p.check_no, t, Some(posted.trn_seq), None)
            },
            ctx,
        )
    }

    pub(super) fn room_charge(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let p = match self.payload(c, "an order.room_charge", OrderRoomChargePayload::problem) {
            Ok(p) => p,
            Err(r) => return Ok(*r),
        };
        self.run_guarded(
            c,
            &mut |unit, t| {
                let posted = unit.orders().add_room_charge(&p, t)?;
                if let Some(reason) = &posted.reason {
                    return Ok(self.reject(c, reason));
                }
                self.applied_check(c, unit, p.rvc_no, p.check_no, t, Some(posted.trn_seq), None)
            },
            ctx,
        )
    }

    pub(super) fn tender_remove(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let p = match self.payload(
            c,
            "an order.tender_remove",
            OrderTenderRemovePayload::problem,
        ) {
            Ok(p) => p,
            Err(r) => return Ok(*r),
        };
        self.run_guarded(
            c,
            &mut |unit, t| {
                if let Some(reason) = unit.orders().remove_tender(&p, t)? {
                    return Ok(self.reject(c, &reason));
                }
                self.applied_check(c, unit, p.rvc_no, p.check_no, t, None, None)
            },
            ctx,
        )
    }

    pub(super) fn check_close(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let p = match self.payload(c, "an order.check_close", OrderCheckClosePayload::problem) {
            Ok(p) => p,
            Err(r) => return Ok(*r),
        };
        self.run_guarded(
            c,
            &mut |unit, t| {
                let outcome = unit.orders().close_check(&p, t)?;
                if let Some(reason) = &outcome.reason {
                    return Ok(self.reject(c, reason));
                }
                let Some(closed) = outcome.closed else {
                    return Err(CommandError::invalid(
                        "The close outcome has neither a bill nor a reason.",
                    ));
                };
                Ok(self.applied(c, Some(closed.to_element())))
            },
            ctx,
        )
    }
}
