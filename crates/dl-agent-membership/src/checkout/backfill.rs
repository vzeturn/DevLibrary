//! The walk-in backfill (`WalkInBackfillPlan`, `WalkInBackfillState(Store)`,
//! `WalkInBackfillRunner`): one run over a date range of checked-out walk-in stays,
//! reported to the hub to earn after the fact, then exit.
//!
//! Only reads SMILE. Running it again never double-earns (the hub dedups by
//! `folio_ref = CONF:{ConfirmNum}`). It pages by a keyset cursor on `(CheckedOutAt,
//! ConfirmNum)` and saves the cursor after each page, but only while no group of the run
//! has failed, so a rerun of the same command picks up the failed ones.

use std::path::{Path, PathBuf};

use dl_decimal::Decimal;
use dl_time::{Date, DateTime, OffsetDateTime};

use super::scanner::{CheckoutDb, CheckoutError};
use super::sql::{self, WalkIn, WalkInCursor};
use crate::earn::EarnEnv;
use crate::earn::pairs::PairSource;
use crate::earn::sql::{EarnDb, FolioScope, Stay, classification, folio_transactions, revenue_sum};
use crate::earn::wire::{WalkInEarnReport, WalkInOutcome};
use crate::folio::earn::ROOM_CODE_AMBIGUOUS;
use crate::hub::earn::EarnHub;
use crate::log::{self, V};
use crate::stj::{self, J};
use crate::wire::parse;

/// `MaxPageSize`.
pub const MAX_PAGE_SIZE: i32 = 500;
/// `DefaultPageSize`.
pub const DEFAULT_PAGE_SIZE: i32 = 200;
/// `MaxRangeDays`.
pub const MAX_RANGE_DAYS: i32 = 400;
/// The state file name next to `config.dpapi` (`DefaultStatePath`).
pub const STATE_FILE: &str = "backfill-walkin.json";

/// `WalkInBackfillPlan`: a validated run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// First day (inclusive).
    pub from: Date,
    /// Last day (inclusive).
    pub to: Date,
    /// Groups per page, clamped to 1..=500.
    pub page_size: i32,
    /// Where the cursor is saved.
    pub state_path: PathBuf,
}

fn parse_date(raw: &str) -> Option<Date> {
    let raw = raw.trim();
    let b = raw.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let y = raw[0..4].parse().ok()?;
    let m = raw[5..7].parse().ok()?;
    let d = raw[8..10].parse().ok()?;
    Date::new(y, m, d).ok()
}

impl Plan {
    /// `TryCreate`: the plan, or the message the operator reads.
    ///
    /// # Errors
    /// A missing or unreadable date, a reversed range, a range over 400 days.
    pub fn try_create(
        from: Option<&str>,
        to: Option<&str>,
        page_size: Option<i32>,
        state_path: &Path,
        today: Date,
    ) -> Result<Plan, String> {
        let from = from.map(str::trim).filter(|s| !s.is_empty()).ok_or(
            "Thiếu --backfill-from. Khai ngày bắt đầu theo định dạng yyyy-MM-dd, ví dụ --backfill-from 2026-01-01.",
        )?;
        let from_date = parse_date(from).ok_or_else(|| {
            format!("--backfill-from '{from}' không đọc được. Định dạng bắt buộc là yyyy-MM-dd, ví dụ 2026-01-01.")
        })?;
        let to_date = match to.map(str::trim).filter(|s| !s.is_empty()) {
            None => today,
            Some(t) => parse_date(t).ok_or_else(|| {
                format!("--backfill-to '{t}' không đọc được. Định dạng bắt buộc là yyyy-MM-dd, ví dụ 2026-09-19.")
            })?,
        };
        if to_date < from_date {
            return Err(format!(
                "Khoảng backfill đảo ngược: --backfill-from {} sau --backfill-to {}.",
                stj::format_date(from_date),
                stj::format_date(to_date)
            ));
        }
        let days = from_date.days_until(to_date) + 1;
        if days > MAX_RANGE_DAYS {
            return Err(format!(
                "Khoảng backfill {days} ngày vượt trần {MAX_RANGE_DAYS} ngày. Hub cũng chỉ chịu nêu tên tối đa {MAX_RANGE_DAYS} ngày (Earn:PendingCheckoutLookbackDays), nên quét rộng hơn chỉ tạo ảo giác đã xong. Chia thành nhiều lượt."
            ));
        }
        Ok(Plan {
            from: from_date,
            to: to_date,
            page_size: page_size
                .unwrap_or(DEFAULT_PAGE_SIZE)
                .clamp(1, MAX_PAGE_SIZE),
            state_path: state_path.to_owned(),
        })
    }

    /// Days in the range, both ends included.
    pub fn range_days(&self) -> i32 {
        self.from.days_until(self.to) + 1
    }
}

/// `WalkInBackfillState`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    /// The plan's `from`.
    pub from: Date,
    /// The plan's `to`.
    pub to: Date,
    /// The cursor after the last fully processed page.
    pub cursor: Option<WalkInCursor>,
    /// Groups answered so far (progress only).
    pub processed: i64,
    /// When it was written.
    pub updated_at: OffsetDateTime,
}

impl State {
    /// The JSON of the .NET record (`from`, `to`, `cursorCheckedOutAt`, `cursorConfirmNum`,
    /// `processed`, `updatedAt`).
    pub fn to_json(&self) -> String {
        J::Obj(vec![
            ("from", J::date(self.from)),
            ("to", J::date(self.to)),
            (
                "cursorCheckedOutAt",
                J::opt_datetime(self.cursor.map(|c| c.checked_out_at)),
            ),
            (
                "cursorConfirmNum",
                J::opt_dec(self.cursor.map(|c| c.confirm_num)),
            ),
            ("processed", J::Int(self.processed)),
            ("updatedAt", J::dto(self.updated_at)),
        ])
        .to_json()
    }

    fn from_json(body: &[u8]) -> Option<State> {
        let v = parse::json(body).ok()?;
        let date = |k| parse::str_prop(&v, k).and_then(|s| parse_date(&s));
        let at: Option<DateTime> = parse::datetime_prop(&v, "cursorCheckedOutAt");
        let conf: Option<Decimal> = parse::dec_prop(&v, "cursorConfirmNum");
        Some(State {
            from: date("from")?,
            to: date("to")?,
            cursor: at
                .zip(conf)
                .map(|(checked_out_at, confirm_num)| WalkInCursor {
                    checked_out_at,
                    confirm_num,
                }),
            processed: parse::int_prop(&v, "processed").unwrap_or(0),
            updated_at: parse::str_prop(&v, "updatedAt")
                .and_then(|s| OffsetDateTime::parse_rfc3339(&s).ok())
                .unwrap_or_else(OffsetDateTime::now_utc),
        })
    }
}

/// `WalkInBackfillStateStore.Load`: `None` when absent, unreadable or of another range.
pub fn load_state(plan: &Plan) -> Option<State> {
    let path = &plan.state_path;
    let body = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            log::warn(
                "backfill state unreadable; starting from the beginning of the range (no double earn: the hub dedups by folio_ref)",
                &[
                    ("path", V::S(&path.display().to_string())),
                    ("error", V::S(&e.to_string())),
                ],
            );
            return None;
        }
    };
    let Some(state) = State::from_json(&body) else {
        log::warn(
            "backfill state unreadable; starting from the beginning of the range (no double earn: the hub dedups by folio_ref)",
            &[("path", V::S(&path.display().to_string()))],
        );
        return None;
    };
    if state.from != plan.from || state.to != plan.to {
        log::warn(
            "backfill state belongs to another range; ignoring its cursor",
            &[
                ("path", V::S(&path.display().to_string())),
                ("state_from", V::S(&stj::format_date(state.from))),
                ("state_to", V::S(&stj::format_date(state.to))),
            ],
        );
        return None;
    }
    Some(state)
}

/// `WalkInBackfillStateStore.Save`: a failure is logged and the run goes on.
pub fn save_state(path: &Path, state: &State) {
    let result = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(path, state.to_json()));
    if let Err(e) = result {
        log::warn(
            "backfill state not saved; the run goes on but an interrupted run restarts from the beginning of the range",
            &[
                ("path", V::S(&path.display().to_string())),
                ("error", V::S(&e.to_string())),
            ],
        );
    }
}

/// `WalkInBackfillSummary`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    /// Pages read.
    pub pages: i64,
    /// Groups answered (this run and the resumed state).
    pub processed: i64,
    /// Groups that earned.
    pub awarded: i64,
    /// Groups whose report failed.
    pub failed: i64,
    /// Whether the whole range was read.
    pub completed: bool,
}

impl Summary {
    /// `WalkInBackfillOutcome.IsClean`: exit code 0.
    pub fn is_clean(&self) -> bool {
        self.completed && self.failed == 0
    }
}

/// `WalkInBackfillRunner.RunAsync`. A SMILE failure on a page read ends the run (the
/// summary says how far it got).
pub fn run(
    plan: &Plan,
    db: &dyn CheckoutDb,
    hub: &dyn EarnHub,
    env: &dyn EarnEnv,
    pairs: &PairSource,
    phone_column: &str,
) -> (Summary, Result<(), CheckoutError>) {
    let resumed = load_state(plan);
    let mut cursor = resumed.as_ref().and_then(|s| s.cursor);
    let mut s = Summary {
        processed: resumed.as_ref().map_or(0, |r| r.processed),
        ..Summary::default()
    };
    log::info(
        "walk-in backfill started (SMILE is only read)",
        &[
            ("from", V::S(&stj::format_date(plan.from))),
            ("to", V::S(&stj::format_date(plan.to))),
            ("days", V::I(i64::from(plan.range_days()))),
            ("page_size", V::I(i64::from(plan.page_size))),
            ("resumed_processed", V::I(s.processed)),
        ],
    );
    let from = plan.from.midnight();
    let to = plan.to.add_days(1).midnight();
    let mut can_resume = true;
    let result = db.with_db(&mut |c| {
        loop {
            let page = sql::scan_walk_ins_page(
                c,
                phone_column,
                from,
                to,
                cursor,
                plan.page_size,
            )?;
            if page.items.is_empty() && page.next.is_none() {
                break;
            }
            s.pages += 1;
            let mut page_failed = 0;
            for group in &page.items {
                match report(c, hub, env, pairs, group) {
                    Ok(outcome) => {
                        s.processed += 1;
                        if outcome == WalkInOutcome::Awarded {
                            s.awarded += 1;
                        }
                    }
                    Err(e) => {
                        page_failed += 1;
                        s.failed += 1;
                        log::warn(
                            "backfill: group report failed, skipped; rerun the backfill to ask again",
                            &[("folio", V::S(&group.folio_ref())), ("error", V::S(&e))],
                        );
                    }
                }
            }
            cursor = page.next;
            if page_failed > 0 {
                can_resume = false;
            }
            if can_resume {
                save_state(
                    &plan.state_path,
                    &State {
                        from: plan.from,
                        to: plan.to,
                        cursor,
                        processed: s.processed,
                        updated_at: OffsetDateTime::now_utc(),
                    },
                );
            }
            log::info(
                "backfill page done",
                &[
                    ("page", V::I(s.pages)),
                    ("groups", V::I(page.items.len() as i64)),
                    ("processed", V::I(s.processed)),
                    ("awarded", V::I(s.awarded)),
                    ("failed", V::I(s.failed)),
                ],
            );
            if cursor.is_none() {
                break;
            }
        }
        Ok(())
    });
    s.completed = result.is_ok();
    log::info(
        "walk-in backfill finished",
        &[
            ("pages", V::I(s.pages)),
            ("processed", V::I(s.processed)),
            ("awarded", V::I(s.awarded)),
            ("failed", V::I(s.failed)),
            ("completed", V::B(s.completed)),
        ],
    );
    (s, result)
}

/// `ReportAsync`: pairs for this group, the whitelisted total, the breakdown (a failure
/// sends none), the classification, `POST earn/walk-in`.
fn report(
    c: &mut dyn EarnDb,
    hub: &dyn EarnHub,
    env: &dyn EarnEnv,
    pairs: &PairSource,
    group: &WalkIn,
) -> Result<WalkInOutcome, String> {
    let earn_pairs = pairs
        .resolve(
            c,
            &FolioScope::WalkIns(vec![group.confirm_num]),
            env.hub_extra_bed_code().as_deref(),
        )
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "{ROOM_CODE_AMBIGUOUS}: không xác định được mã tiền phòng SMILE — bỏ qua nhóm {}.",
                group.folio_ref()
            )
        })?;
    let stay = Stay::WalkIn(group.confirm_num);
    let revenue = revenue_sum(c, &stay, &earn_pairs).map_err(|e| e.to_string())?;
    let lines = match folio_transactions(c, &stay) {
        Ok(l) => Some(l).filter(|l| !l.is_empty()),
        Err(e) => {
            log::warn(
                "backfill: breakdown read failed; the hub uses the whitelisted revenue",
                &[
                    ("folio", V::S(&group.folio_ref())),
                    ("error", V::S(&e.to_string())),
                ],
            );
            None
        }
    };
    let classification = classification(c, &stay).map_err(|e| e.to_string())?;
    let result = hub
        .report_walk_in_earn(&WalkInEarnReport {
            guest_phone: group.guest_phone.clone(),
            folio_ref: group.folio_ref(),
            qualifying_revenue: revenue,
            checked_out_at: group.checked_out_at,
            guest_name: group.guest_name.clone(),
            folio_lines: lines,
            classification: Some(classification),
            db_identity_fingerprint: env.fingerprint(),
        })
        .map_err(|e| e.to_string())?;
    Ok(result.outcome)
}

#[cfg(test)]
#[path = "backfill_tests.rs"]
mod tests;
