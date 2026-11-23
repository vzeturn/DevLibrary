//! PMS change history (Go `reconcile_history.go`, RT-0702). SMILE's `triggerFolioUpdate`
//! logs Folio column changes only on UPDATE and only for `FolioLogDef.LogIfChanged`
//! columns. Every other agent write gets its own `FolioLog` row: creation (and re-creation
//! after an agent cancel), the summary of a modification, nightly rates and cancellation.
//! A history row that cannot be written does not fail the booking (the reservation itself
//! is right), but it is never silent: it is logged and reported to the hub in
//! `PmsResult.audit_gaps`.

use std::cell::RefCell;
use std::collections::BTreeMap;

use dl_time::DateTime;

use super::Reconciler;
use super::diff::rates_summary;
use crate::error::Result;
use crate::gofmt::f0;
use crate::jobmodel::Job;
use crate::log::{self, V};
use crate::pms::{
    FOLIO_LOG_CANCEL, FOLIO_LOG_CREATE, FOLIO_LOG_RECREATE, Folio, FolioRate, Val, ymd,
};

thread_local! {
    /// History gaps of the `apply` running on this thread.
    static GAPS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Starts a fresh gap list for one `apply`.
pub(super) fn reset_gaps() {
    GAPS.with(|g| g.borrow_mut().clear());
}

/// Takes the gaps collected since [`reset_gaps`].
pub(super) fn take_gaps() -> Vec<String> {
    GAPS.with(|g| std::mem::take(&mut *g.borrow_mut()))
}

fn note_gap(msg: String) {
    GAPS.with(|g| g.borrow_mut().push(msg));
}

/// `FolioLog` field and old value for the folios one save creates: `HL:Create`, or
/// `HL:Recreate` naming the folios the agent had cancelled for this booking (recovery).
#[derive(Debug, Clone)]
pub(super) struct CreateLog {
    pub field: &'static str,
    pub old_val: String,
}

impl CreateLog {
    pub(super) fn new(prev_cancelled: &[f64]) -> CreateLog {
        if prev_cancelled.is_empty() {
            return CreateLog {
                field: FOLIO_LOG_CREATE,
                old_val: String::new(),
            };
        }
        let parts: Vec<String> = prev_cancelled.iter().map(|n| f0(*n)).collect();
        CreateLog {
            field: FOLIO_LOG_RECREATE,
            old_val: format!("cancelled folios {}", parts.join(",")),
        }
    }

    /// A room added by a modification.
    pub(super) fn added_by_modify() -> CreateLog {
        CreateLog {
            field: FOLIO_LOG_CREATE,
            old_val: "room added by modification".into(),
        }
    }
}

impl Reconciler {
    /// One `FolioLog` row on a live folio (best effort, gap reported).
    pub(super) fn log_folio(&self, folio_num: f64, field: &str, old: &str, new: &str) {
        if let Err(e) = self.repo.append_folio_log(folio_num, field, old, new) {
            history_gap(folio_num, field, e.message());
        }
    }

    /// One `FolioLog` row on a folio in any state (cancelled included).
    fn log_folio_any(&self, folio_num: f64, field: &str, old: &str, new: &str) {
        if let Err(e) = self.repo.append_folio_log_any(folio_num, field, old, new) {
            history_gap(folio_num, field, e.message());
        }
    }

    /// Cancels the folios in one transaction, then writes an `HL:Cancel` row per folio
    /// (the cancel stamps `LastChangeClerkID` = `HLC` and `LastChangeTime`).
    pub(super) fn cancel_folios_logged(
        &self,
        job: &Job,
        folios: &[Folio],
        why: &str,
    ) -> Result<()> {
        self.repo.cancel_folios(folios)?;
        for f in folios {
            self.log_folio_any(
                f.folio_num,
                FOLIO_LOG_CANCEL,
                "live",
                &format!(
                    "cancelled: {why} (hl={} ext={})",
                    job.booking_id, job.booker
                ),
            );
        }
        Ok(())
    }
}

fn history_gap(folio_num: f64, field: &str, err: &str) {
    log::warn(
        "không ghi được FolioLog (thay đổi PMS đã áp, thiếu dòng lịch sử)",
        &[
            ("folioNum", V::S(&f0(folio_num))),
            ("field", V::S(field)),
            ("error", V::S(err)),
        ],
    );
    note_gap(format!("FolioLog {field} folio {}: {err}", f0(folio_num)));
}

/// `HL:Create` value: booking codes, room, stay, guest counts and nightly rates. No guest
/// identity (name and contact are Folio columns the PMS shows anyway).
pub(super) fn create_summary(job: &Job, f: &Folio, rates: &[FolioRate]) -> String {
    format!(
        "hl={} ext={} item={} type={} stay={}..{} adults={} children={} rates={}",
        job.booking_id,
        job.booker,
        f.user_info1,
        f.room_type_code,
        ymd(f.arrival_date),
        ymd(f.departure_date),
        f.num_adult,
        f.num_child,
        rates_summary(rates)
    )
}

/// `HL:Update` old/new values: `Col=value; …` for the changed columns, in column order.
/// Guest identity columns are named without values. `name_updated` adds the
/// AdditionalName record the agent rewrote too.
pub(super) fn update_summary(
    f: &Folio,
    set: &BTreeMap<&'static str, Val>,
    name_updated: bool,
) -> (String, String) {
    let mut old_parts: Vec<String> = Vec::new();
    let mut new_parts: Vec<String> = Vec::new();
    for (col, new) in set {
        if *col == "RoomTypeBooked" {
            continue; // mirrors RoomTypeCode
        }
        if matches!(*col, "LastName" | "TelNumM" | "MailM") {
            old_parts.push(format!("{col}=(guest)"));
            new_parts.push(format!("{col}=(guest changed)"));
            continue;
        }
        old_parts.push(format!("{col}={}", fmt_val(&folio_col(f, col))));
        new_parts.push(format!("{col}={}", fmt_val(new)));
    }
    if name_updated {
        new_parts.push("AdditionalName=updated".into());
    }
    (old_parts.join("; "), new_parts.join("; "))
}

/// Current value of a diff-updatable Folio column (`ALLOWED_UPDATE_COLS`).
fn folio_col(f: &Folio, col: &str) -> Val {
    match col {
        "ArrivalDate" => Val::Time(f.arrival_date),
        "DepartureDate" => Val::Time(f.departure_date),
        "NumAdult" => Val::I32(f.num_adult),
        "NumChild" => Val::I32(f.num_child),
        "RateAmount" => Val::F64(f.rate_amount),
        "IncludeBF" => Val::Bool(f.include_bf),
        "BreakfastCode" => Val::Str(f.breakfast_code.clone()),
        "NumGuestBF" => Val::I32(f.num_guest_bf),
        "NumChildBF" => Val::I32(f.num_child_bf),
        "RoomTypeCode" => Val::Str(f.room_type_code.clone()),
        "RoomTypeBooked" => Val::Str(f.room_type_booked.clone()),
        "RoomCode" => Val::Str(f.room_code.clone()),
        _ => Val::Str(String::new()),
    }
}

fn fmt_val(v: &Val) -> String {
    match v {
        Val::Time(t) => ymd_of(*t),
        Val::F64(x) => format!("{x}"),
        Val::Str(s) if s.is_empty() => "\"\"".into(),
        Val::Str(s) => s.clone(),
        Val::I32(x) => x.to_string(),
        Val::I64(x) => x.to_string(),
        Val::Bool(b) => b.to_string(),
        Val::OptStr(Some(s)) => s.clone(),
        Val::OptF64(Some(x)) => format!("{x}"),
        Val::OptStr(None) | Val::OptF64(None) => "<nil>".into(),
    }
}

fn ymd_of(t: DateTime) -> String {
    ymd(t)
}
