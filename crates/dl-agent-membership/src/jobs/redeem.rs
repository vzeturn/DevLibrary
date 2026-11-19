//! `JobDispatcher.HandleStackedRedeemAsync` (voucher stacking, spec 2026-10-15 §3) and the
//! discount-code guard both REDEEM paths share.
//!
//! The hub is the source of truth for the discount the guest agreed to: it prices the
//! whole stay (special-night rules included) while the SMILE folio only carries the
//! first-night rate (`ExistingFolio.RateAmount`). So each voucher of the `vouchers` array
//! posts exactly its `appliedAmount`, in `sequence` order, one line per code, idempotent
//! per code. A voucher without a positive amount sends the whole job to the desk before any
//! line is posted: posting part of the set would leave the desk collecting the wrong total.

use dl_decimal::Decimal;

use super::{Dispatcher, MAX_TRANSACTION_CODE_LENGTH, WriteError};
use crate::jobs::payload::{JobPayload, Voucher};
use crate::jobs::result::Ack;
use crate::log::{self, V};
use crate::wire::parse::Job;

impl Dispatcher {
    /// `MemberDiscountEnabled` / `MemberDiscountCodeProblem`: the desk reason when
    /// `Earn:MemberDiscountCode` cannot be posted (empty, or longer than SMILE stores).
    pub(super) fn discount_code_refusal(&self, job_id: &str) -> Option<Ack> {
        let code = &self.settings.member_discount_code;
        if code.is_empty() {
            return Some(Ack::manual(
                job_id,
                "Earn:MemberDiscountCode để trống (tắt ghi giảm giá voucher lên folio) — áp voucher tay tại quầy.",
            ));
        }
        let len = code.chars().count();
        (len > MAX_TRANSACTION_CODE_LENGTH).then(|| {
            Ack::manual(
                job_id,
                &format!(
                    "Earn:MemberDiscountCode '{code}' dài {len} ký tự, vượt giới hạn {MAX_TRANSACTION_CODE_LENGTH} của FolioTransaction.TransactionCode — SMILE sẽ cắt hoặc từ chối. Đặt mã ngắn hơn hoặc để trống để tắt ghi giảm giá voucher lên folio."
                ),
            )
        })
    }

    /// `HandleStackedRedeemAsync`: one discount line per voucher on the first live folio
    /// (never one per room: that would double the discount), for the hub's `appliedAmount`.
    pub(super) fn redeem_stacked(
        &self,
        job: &Job,
        p: &JobPayload,
        vouchers: &[Voucher],
    ) -> Result<Ack, WriteError> {
        let mut priced: Vec<(&Voucher, Decimal)> = Vec::with_capacity(vouchers.len());
        for v in vouchers {
            match v.applied_amount {
                Some(amount) if amount.is_positive() => priced.push((v, amount)),
                _ => {
                    return Ok(Ack::manual(
                        &job.job_id,
                        &format!(
                            "voucher {} thiếu số tiền Hub đã áp (appliedAmount) — áp tay tại quầy.",
                            v.code
                        ),
                    ));
                }
            }
        }
        let live = self.live_folios(&Self::external_key(job, p))?;
        let Some(first) = live.first() else {
            return Ok(Ack::manual(&job.job_id, "không có folio để áp voucher."));
        };
        if let Some(refusal) = self.discount_code_refusal(&job.job_id) {
            return Ok(refusal);
        }
        let code = &self.settings.member_discount_code;
        let booking = p.booking_id.to_string();
        let nums: Vec<Decimal> = live.iter().map(|f| f.folio_num).collect();
        for (v, amount) in priced {
            // RefNumber = voucher code is the only trace of an earlier post: look on every
            // live folio and skip just that voucher, so a job whose ack was lost half-way
            // resumes with the vouchers it had not posted yet.
            if self.pms.has_posted_ref(&nums, &v.code)? {
                log::info(
                    "REDEEM: voucher already posted by an earlier delivery; not posting again",
                    &[("booking", V::S(&booking)), ("voucher", V::S(&v.code))],
                );
                continue;
            }
            self.pms
                .post_discount(first.folio_num, amount, code, &v.code)?;
            log::info(
                "REDEEM: voucher posted",
                &[
                    ("booking", V::S(&booking)),
                    ("voucher", V::S(&v.code)),
                    ("amount", V::S(&amount.to_string())),
                ],
            );
        }
        Ok(Ack::ok(&job.job_id))
    }
}

#[cfg(test)]
#[path = "redeem_tests.rs"]
mod tests;
