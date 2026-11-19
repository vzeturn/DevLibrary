//! `JobDispatcher` and `SyncWorker.ProcessOneAsync`: one hub job to one ack.
//!
//! SMILE is reached only through [`PmsWriter`], so the dispatcher's decisions (gate,
//! duplicate-safe lookups, room diff, voucher and reward rules) are tested without a
//! server. A writer path that is not ported returns [`WriteError::NotPorted`]; the job is
//! then left unacknowledged (the hub's lease expires and it is delivered again), so a
//! half-ported write never reaches SMILE.

use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use dl_decimal::Decimal;
use dl_uuid::Uuid;

pub use super::plan::{
    BookingPlan, MAX_TRANSACTION_CODE_LENGTH, PmsWriter, ProfileRequest, WriteContext, WriteError,
    Written, percent_discount, reward_ref,
};
use super::plan::{ProfileFailure, classify, profile_unavailable, rule_string, summary};

use super::JobAction;
use super::beds::{BedDecision, MODE_FOLD_LEGACY};
use super::gate::{WriteGate, denied, is_gated};
use super::guest::{self, split_full_name};
use super::payload::{JobPayload, Room, parse_link_folio, parse_payload};
use super::preflight::{PreflightInputs, decide_extra_bed};
use super::result::{Ack, EXTRA_BED_LOCKED, FOLIO_NOT_FOUND, UNKNOWN};
use super::room_diff::{DiffKind, ExistingFolio, diff};
use super::travel_agent::TravelAgent;
use crate::log::{self, V};
use crate::smile::SmileError;
use crate::wire::parse::Job;

/// What the dispatcher needs from the agent settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchSettings {
    /// `AgentConfig.OriginCode`.
    pub origin_code: String,
    /// `Earn:MemberDiscountCode` (empty: voucher discounts are applied by hand).
    pub member_discount_code: String,
    /// `Smile:ExtraBedWriteMode`.
    pub extra_bed_mode: String,
}

/// `JobDispatcher`.
pub struct Dispatcher {
    pms: Arc<dyn PmsWriter>,
    gate: Option<Arc<WriteGate>>,
    settings: DispatchSettings,
}

impl fmt::Debug for Dispatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Dispatcher")
            .field("gated", &self.gate.is_some())
            .finish_non_exhaustive()
    }
}

/// The outcome of one job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Send this ack.
    Ack(Ack),
    /// Do not ack: the hub delivers the job again after its lease (unported path).
    Leave(String),
}

impl Dispatcher {
    /// A dispatcher writing through `pms`, asking `gate` (when given) before any write.
    pub fn new(
        pms: Arc<dyn PmsWriter>,
        gate: Option<Arc<WriteGate>>,
        settings: DispatchSettings,
    ) -> Dispatcher {
        Dispatcher {
            pms,
            gate,
            settings,
        }
    }

    /// `SyncWorker.ProcessOneAsync`: the outcome, with the delivery's lease on the ack.
    pub fn process(&self, job: &Job) -> Outcome {
        let outcome = match self.dispatch(job) {
            Ok(ack) => Outcome::Ack(ack),
            Err(WriteError::NotPorted(what)) => {
                log::warn(
                    "job left for a later delivery: this write is not ported",
                    &[
                        ("job", V::S(&job.job_id)),
                        ("action", V::S(&job.action)),
                        ("path", V::S(what)),
                    ],
                );
                Outcome::Leave(what.to_owned())
            }
            Err(e) => {
                let (code, class) = classify(&e);
                log::error(
                    "job failed",
                    &[
                        ("job", V::S(&job.job_id)),
                        ("action", V::S(&job.action)),
                        ("code", V::S(&code)),
                        ("class", V::S(class.wire())),
                    ],
                );
                Outcome::Ack(Ack::fail(
                    &job.job_id,
                    &summary(&e),
                    Some(code),
                    Some(class),
                ))
            }
        };
        match outcome {
            Outcome::Ack(a) => Outcome::Ack(a.with_attempts(job.attempts)),
            leave => leave,
        }
    }

    /// `DispatchAsync`.
    ///
    /// # Errors
    /// Any failure `ProcessOneAsync` turns into a failed ack.
    pub fn dispatch(&self, job: &Job) -> Result<Ack, WriteError> {
        let Some(action) = JobAction::parse(&job.action) else {
            return Ok(Ack::fail(
                &job.job_id,
                &format!("JobAction không hỗ trợ: {}", job.action),
                None,
                None,
            ));
        };
        if let Some(gate) = &self.gate
            && is_gated(action)
        {
            let d = gate.evaluate(Instant::now())?;
            if !d.allowed {
                return Ok(denied(&job.job_id, &d));
            }
        }
        let payload =
            guest::apply(
                parse_payload(&job.payload_json).map_err(|m| WriteError::Coded {
                    code: UNKNOWN.into(),
                    message: m,
                })?,
            );
        let result = match action {
            JobAction::Create => self.create(job, &payload),
            JobAction::Modify => self.modify(job, &payload),
            JobAction::Cancel => self.cancel(job, &payload),
            JobAction::RedeemVoucher => self.redeem(job, &payload),
            JobAction::EarnPoints => self.earn(job, &payload),
            JobAction::Availability => Ok(Ack::ok(&job.job_id)),
            JobAction::GrantReward => self.grant_reward(job, &payload),
            JobAction::LinkFolio => self.link_folio(job),
        };
        let booking = job.booking_id.unwrap_or(payload.booking_id).to_string();
        match result {
            Err(WriteError::Refused { code, message }) => {
                log::warn(
                    "SMILE write refused; handed to the operator",
                    &[
                        ("action", V::S(action.name())),
                        ("booking", V::S(&booking)),
                        ("code", V::S(&code)),
                    ],
                );
                Ok(Ack::needs_operator(&job.job_id, &code, &message))
            }
            other => other,
        }
    }

    fn booking_id(job: &Job, p: &JobPayload) -> Uuid {
        job.booking_id.unwrap_or(p.booking_id)
    }

    /// `ExternalKey`.
    fn external_key(job: &Job, p: &JobPayload) -> String {
        if p.confirmation_code.trim().is_empty() {
            Self::booking_id(job, p).to_string()
        } else {
            p.confirmation_code.clone()
        }
    }

    fn context(&self, job: &Job, p: &JobPayload) -> WriteContext {
        WriteContext {
            confirmation_code: Self::external_key(job, p),
            booking_id: Self::booking_id(job, p),
            origin_code: self.settings.origin_code.clone(),
            member_id: p.member_id,
            force_create: p.force_create,
            note: p.special_requests.clone(),
            travel_agent: None,
        }
    }

    /// `Decide`: `FOLD_LEGACY` without inputs, whatever the mode (the map and the diff
    /// only; the writer decides with the configured mode).
    fn decide(room: &Room, inputs: Option<&PreflightInputs>) -> BedDecision {
        decide_extra_bed(room, inputs, MODE_FOLD_LEGACY)
    }

    /// `ReadExtraBedInputsAsync` with the payload's fingerprint and special code.
    fn bed_inputs(&self, p: &JobPayload) -> Result<Option<PreflightInputs>, WriteError> {
        self.pms.read_extra_bed_inputs(
            p.extra_bed_verified_fingerprint.as_deref(),
            p.extra_bed_special_code.as_deref(),
        )
    }

    fn create(&self, job: &Job, p: &JobPayload) -> Result<Ack, WriteError> {
        let travel_agent = self.travel_agent(job, p)?;
        let ctx = WriteContext {
            travel_agent,
            ..self.context(job, p)
        };
        let profile_number = self.profile_number(p)?;
        let bed_inputs = self.bed_inputs(p)?;
        let booking = ctx.booking_id;
        let written = self.pms.write_booking(&BookingPlan {
            ctx,
            shared_confirm_num: None,
            profile_number,
            creates: p.rooms.iter().filter(|r| !r.removed).cloned().collect(),
            updates: Vec::new(),
            cancels: Vec::new(),
            note_folios: None,
            extra_bed: bed_inputs.clone(),
        })?;
        let maps = written
            .created
            .iter()
            .map(|(room, folio)| {
                self.folio_map(
                    *room,
                    *folio,
                    None,
                    p.rooms.iter().find(|r| r.room_item_id == *room),
                    bed_inputs.as_ref(),
                )
            })
            .collect::<Vec<_>>();
        log::info(
            "CREATE booking written",
            &[
                ("booking", V::S(&booking.to_string())),
                ("folios", V::I(maps.len() as i64)),
            ],
        );
        Ok(Ack::ok_with(&job.job_id, written.confirm_num, Some(maps)))
    }

    fn modify(&self, job: &Job, p: &JobPayload) -> Result<Ack, WriteError> {
        let mut ctx = self.context(job, p);
        let existing = self.find_booking_folios(&ctx.confirmation_code, p)?;
        if existing.is_empty() {
            if p.is_known_in_smile() {
                return Ok(self.folio_not_found(job, ctx.booking_id, "MODIFY"));
            }
            return self.create(job, p);
        }
        let bed_inputs = self.bed_inputs(p)?;
        let result = diff(&existing, &p.rooms, &|r| {
            Self::decide(r, bed_inputs.as_ref())
        });
        if result.requires_manual {
            let reason = result.manual_reason.as_deref().unwrap_or("folio đã khoá");
            log::warn(
                "MODIFY booking needs the operator",
                &[
                    ("booking", V::S(&ctx.booking_id.to_string())),
                    ("reason", V::S(reason)),
                ],
            );
            return Ok(if result.bed_locked {
                Ack::needs_operator(
                    &job.job_id,
                    EXTRA_BED_LOCKED,
                    &format!("{reason} — giường phụ là dòng EXTRA BED riêng, lễ tân chỉnh tay."),
                )
            } else {
                Ack::manual(&job.job_id, reason)
            });
        }
        let has_creates = result.of(DiffKind::Create).next().is_some();
        if has_creates {
            ctx.travel_agent = self.travel_agent(job, p)?;
        }
        let profile_number = if has_creates {
            self.profile_number(p)?
        } else {
            Decimal::ZERO
        };
        let existing_of = |d: &super::room_diff::RoomDiff| d.existing.clone();
        let plan = BookingPlan {
            ctx: ctx.clone(),
            shared_confirm_num: p.smile_confirm_num,
            profile_number,
            creates: result
                .of(DiffKind::Create)
                .filter_map(|d| d.payload.clone())
                .collect(),
            updates: result
                .of(DiffKind::Update)
                .filter_map(|d| Some((existing_of(d)?.folio_num, d.payload.clone()?)))
                .collect(),
            cancels: result
                .of(DiffKind::Cancel)
                .filter_map(|d| d.existing.as_ref().map(|e| e.folio_num))
                .collect(),
            note_folios: p.special_requests.as_ref().map(|_| {
                result
                    .diffs
                    .iter()
                    .filter(|d| matches!(d.kind, DiffKind::Update | DiffKind::Unchanged))
                    .filter_map(|d| d.existing.as_ref().map(|e| e.folio_num))
                    .collect()
            }),
            extra_bed: bed_inputs.clone(),
        };
        let written = self.pms.write_booking(&plan)?;
        let mut maps: Vec<_> = written
            .created
            .iter()
            .map(|(room, folio)| {
                self.folio_map(
                    *room,
                    *folio,
                    None,
                    p.rooms.iter().find(|r| r.room_item_id == *room),
                    bed_inputs.as_ref(),
                )
            })
            .collect();
        for d in result.of(DiffKind::Update) {
            if let (Some(e), Some(room)) = (&d.existing, &d.payload) {
                maps.push(self.folio_map(
                    d.room_item_id,
                    e.folio_num,
                    Some(e),
                    Some(room),
                    bed_inputs.as_ref(),
                ));
            }
        }
        for d in result.of(DiffKind::Unchanged) {
            if let Some(e) = d.existing.as_ref().filter(|e| e.beds.is_some()) {
                maps.push(self.folio_map(d.room_item_id, e.folio_num, Some(e), None, None));
            }
        }
        log::info(
            "MODIFY booking written",
            &[
                ("booking", V::S(&ctx.booking_id.to_string())),
                ("created", V::I(result.of(DiffKind::Create).count() as i64)),
                ("updated", V::I(result.of(DiffKind::Update).count() as i64)),
                (
                    "cancelled",
                    V::I(result.of(DiffKind::Cancel).count() as i64),
                ),
            ],
        );
        Ok(Ack::ok_with(&job.job_id, written.confirm_num, Some(maps)))
    }

    fn cancel(&self, job: &Job, p: &JobPayload) -> Result<Ack, WriteError> {
        let ext = Self::external_key(job, p);
        let existing = self.find_booking_folios(&ext, p)?;
        if existing.is_empty() && p.is_known_in_smile() {
            return Ok(self.folio_not_found(job, Self::booking_id(job, p), "CANCEL"));
        }
        let live: Vec<_> = existing.iter().filter(|f| f.cancel_num.is_zero()).collect();
        if live.is_empty() {
            log::info(
                "CANCEL booking: no live folio left (idempotent)",
                &[("booking", V::S(&ext))],
            );
            return Ok(Ack::ok(&job.job_id));
        }
        if let Some(f) = live
            .iter()
            .find(|f| !f.room_code.is_empty() || f.folio_status > 1)
        {
            return Ok(Ack::manual(
                &job.job_id,
                &format!("folio {} đã xếp phòng/check-in, không tự huỷ.", f.folio_num),
            ));
        }
        for f in &live {
            self.pms.cancel_folio(f.folio_num)?;
        }
        log::info(
            "CANCEL booking written",
            &[("booking", V::S(&ext)), ("folios", V::I(live.len() as i64))],
        );
        Ok(Ack::ok(&job.job_id))
    }

    /// `FindBookingFoliosAsync`.
    fn find_booking_folios(
        &self,
        ext: &str,
        p: &JobPayload,
    ) -> Result<Vec<ExistingFolio>, WriteError> {
        let mut existing = self.pms.lookup_by_confirmation(ext)?;
        let Some(expected) = p.expected_folios.as_ref().filter(|e| !e.is_empty()) else {
            return Ok(existing);
        };
        let mut missing: Vec<Decimal> = Vec::new();
        for e in expected {
            if !existing.iter().any(|f| f.folio_num == e.folio_num)
                && !missing.contains(&e.folio_num)
            {
                missing.push(e.folio_num);
            }
        }
        if missing.is_empty() {
            return Ok(existing);
        }
        for mut f in self.pms.lookup_by_folio_nums(&missing)? {
            if f.room_item_id.is_nil()
                && let Some(e) = expected.iter().find(|e| e.folio_num == f.folio_num)
            {
                f.room_item_id = e.room_item_id;
            }
            existing.push(f);
        }
        Ok(existing)
    }

    fn folio_not_found(&self, job: &Job, booking: Uuid, action: &str) -> Ack {
        log::warn(
            "the hub knows SMILE folios for this booking but SMILE has none; not recreating",
            &[
                ("action", V::S(action)),
                ("booking", V::S(&booking.to_string())),
            ],
        );
        Ack::needs_operator(
            &job.job_id,
            FOLIO_NOT_FOUND,
            "Không tìm thấy folio SMILE mà Hub đã ghi nhận cho booking này (lễ tân đã xoá/đổi?) — kiểm tra trên PMS.",
        )
    }

    fn live_folios(&self, ext: &str) -> Result<Vec<ExistingFolio>, WriteError> {
        Ok(self
            .pms
            .lookup_by_confirmation(ext)?
            .into_iter()
            .filter(|f| f.cancel_num.is_zero())
            .collect())
    }

    fn redeem(&self, job: &Job, p: &JobPayload) -> Result<Ack, WriteError> {
        let Some(voucher) = &p.voucher else {
            return Ok(Ack::manual(&job.job_id, "payload thiếu thông tin voucher."));
        };
        let live = self.live_folios(&Self::external_key(job, p))?;
        if live.is_empty() {
            return Ok(Ack::manual(&job.job_id, "không có folio để áp voucher."));
        }
        let amount = match voucher.kind.as_str() {
            "AmountOff" => voucher.value,
            "PercentOff" => {
                let total: Decimal = live.iter().map(|f| f.rate_amount).sum();
                percent_discount(voucher.value, voucher.max_discount_amount, total)
            }
            "FreeService" => {
                return Ok(Ack::manual(
                    &job.job_id,
                    "FreeService cần xử lý tay tại quầy.",
                ));
            }
            other => {
                return Ok(Ack::manual(
                    &job.job_id,
                    &format!("loại voucher không hỗ trợ: {other}"),
                ));
            }
        };
        let code = &self.settings.member_discount_code;
        if code.is_empty() {
            return Ok(Ack::manual(
                &job.job_id,
                "Earn:MemberDiscountCode để trống (tắt ghi giảm giá voucher lên folio) — áp voucher tay tại quầy.",
            ));
        }
        if code.chars().count() > MAX_TRANSACTION_CODE_LENGTH {
            return Ok(Ack::manual(
                &job.job_id,
                &format!(
                    "Earn:MemberDiscountCode '{code}' dài {} ký tự, vượt giới hạn {MAX_TRANSACTION_CODE_LENGTH} của FolioTransaction.TransactionCode — SMILE sẽ cắt hoặc từ chối. Đặt mã ngắn hơn hoặc để trống để tắt ghi giảm giá voucher lên folio.",
                    code.chars().count()
                ),
            ));
        }
        let nums: Vec<Decimal> = live.iter().map(|f| f.folio_num).collect();
        if self.pms.has_posted_ref(&nums, &voucher.code)? {
            log::info(
                "REDEEM: voucher already posted by an earlier delivery; not posting again",
                &[("booking", V::S(&p.booking_id.to_string()))],
            );
            return Ok(Ack::ok(&job.job_id));
        }
        self.pms
            .post_discount(live[0].folio_num, amount, code, &voucher.code)?;
        log::info(
            "REDEEM: voucher posted",
            &[
                ("booking", V::S(&p.booking_id.to_string())),
                ("amount", V::S(&amount.to_string())),
            ],
        );
        Ok(Ack::ok(&job.job_id))
    }

    fn grant_reward(&self, job: &Job, p: &JobPayload) -> Result<Ack, WriteError> {
        let Some(reward) = &p.reward else {
            return Ok(Ack::manual(&job.job_id, "payload thiếu thông tin reward."));
        };
        let live = self.live_folios(&Self::external_key(job, p))?;
        if live.is_empty() {
            return Ok(Ack::manual(
                &job.job_id,
                "không có folio để hiện thực ưu đãi.",
            ));
        }
        let reference = reward_ref(reward.grant_id);
        match reward.kind.as_str() {
            "SpaCredit" | "FnbCredit" => {
                let credit = rule_string(reward.rule_json.as_deref(), "creditFolioCode");
                let (Some(code), Some(value)) =
                    (credit.filter(|c| !c.trim().is_empty()), reward.value)
                else {
                    return Ok(Ack::manual(
                        &job.job_id,
                        &format!(
                            "{} thiếu creditFolioCode hoặc value trong rule.",
                            reward.kind
                        ),
                    ));
                };
                let nums: Vec<Decimal> = live.iter().map(|f| f.folio_num).collect();
                if !self.pms.has_posted_ref(&nums, &reference)? {
                    self.pms
                        .post_discount(live[0].folio_num, value, &code, &reference)?;
                }
                Ok(Ack::ok_reward(&job.job_id, reward.grant_id))
            }
            "LateCheckout" | "EarlyCheckin" => Ok(Ack::ok_reward(&job.job_id, reward.grant_id)),
            "RoomUpgrade" => Ok(Ack::manual(
                &job.job_id,
                "ROOM_UPGRADE cần xử lý tay tại quầy — Agent chưa có method đổi loại phòng an toàn.",
            )),
            "FreeNight" => Ok(Ack::manual(
                &job.job_id,
                "FREE_NIGHT cần xử lý tay tại quầy.",
            )),
            other => Ok(Ack::manual(
                &job.job_id,
                &format!("loại reward không hỗ trợ: {other}"),
            )),
        }
    }

    fn earn(&self, job: &Job, p: &JobPayload) -> Result<Ack, WriteError> {
        let ext = Self::external_key(job, p);
        self.pms.report_earn(p.booking_id, &ext)?;
        Ok(Ack::ok(&job.job_id))
    }

    fn link_folio(&self, job: &Job) -> Result<Ack, WriteError> {
        let link = parse_link_folio(&job.payload_json).map_err(|m| WriteError::Coded {
            code: UNKNOWN.into(),
            message: m,
        })?;
        self.pms.link_folios(&link)?;
        log::info(
            "LINK_FOLIO stamped",
            &[
                (
                    "booking",
                    V::S(&job.booking_id.unwrap_or(link.booking_id).to_string()),
                ),
                ("folios", V::I(link.folios.len() as i64)),
            ],
        );
        Ok(Ack::ok(&job.job_id))
    }

    /// `EnsureProfileNumberAsync`: 0 without a member; a SQL failure is `PROFILE_UNAVAILABLE`.
    fn profile_number(&self, p: &JobPayload) -> Result<Decimal, ProfileFailure> {
        let Some(member_id) = p.member_id else {
            return Ok(Decimal::ZERO);
        };
        let (mut last, first) =
            split_full_name(p.member.as_ref().and_then(|m| m.full_name.as_deref()));
        if last.is_none() && first.is_none() {
            last = Some("MEMBER".into());
        }
        let req = ProfileRequest {
            member_id,
            member_code: p.member_code.clone(),
            last_name: last.unwrap_or_default(),
            first_name: first.unwrap_or_default(),
            contact: p.member.clone(),
        };
        match self.pms.ensure_profile(&req) {
            Err(WriteError::Smile(SmileError::Server(n))) => {
                log::warn(
                    "member profile could not be ensured on SMILE; no folio written",
                    &[
                        ("booking", V::S(&p.booking_id.to_string())),
                        ("sql", V::I(i64::from(n))),
                    ],
                );
                Err(profile_unavailable(n))
            }
            other => other,
        }
    }

    /// `ResolveTravelAgentAsync`.
    fn travel_agent(&self, job: &Job, p: &JobPayload) -> Result<Option<TravelAgent>, WriteError> {
        let Some(code) = p.travel_agent_code else {
            return Ok(None);
        };
        let booking = Self::booking_id(job, p).to_string();
        match self.pms.find_travel_agent(code)? {
            Some(ta) if !ta.is_deleted => {
                let (clean, warnings) = ta.sanitize();
                if !warnings.is_empty() {
                    log::warn(
                        "travel agent classification wider than the folio columns; those columns stay empty",
                        &[
                            ("booking", V::S(&booking)),
                            ("warnings", V::S(&warnings.join(" | "))),
                        ],
                    );
                }
                Ok(Some(clean))
            }
            _ => {
                log::warn(
                    "travel agent code not found in SMILE (ClientSts/CLIENT) or deleted; folio written with the code only",
                    &[("booking", V::S(&booking)), ("code", V::I(code))],
                );
                Ok(Some(TravelAgent::unresolved(code)))
            }
        }
    }

    /// `FolioMap`.
    fn folio_map(
        &self,
        room_item_id: Uuid,
        folio_num: Decimal,
        before: Option<&ExistingFolio>,
        written: Option<&Room>,
        inputs: Option<&PreflightInputs>,
    ) -> super::result::RoomFolioMap {
        let beds = before.and_then(|b| b.beds.as_ref());
        let decision = written.map(|r| Self::decide(r, inputs));
        super::result::RoomFolioMap {
            extra_bed_written: decision.as_ref().map(|d| d.extra_bed_to_write),
            extra_bed_found: beds.and_then(|b| b.extra_bed),
            extra_bed_special_qty_written: decision
                .as_ref()
                .filter(|d| d.is_special())
                .map(|d| d.special_rows.iter().map(|r| r.qty).max().unwrap_or(0)),
            extra_bed_special_qty_found: beds.and_then(|b| b.special_qty_found()),
            ..super::result::RoomFolioMap::new(room_item_id, folio_num)
        }
    }
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "dispatch_bed_tests.rs"]
mod bed_tests;
