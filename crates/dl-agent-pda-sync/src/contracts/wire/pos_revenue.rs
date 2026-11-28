//! `pos.revenue_summary` contract (ADR 0149, RT-0843; ported from the PDAOrder agent by
//! RT-1427): the daily revenue totals of one RVC read from SMILE_POS for the cloud's
//! acceptance check.

use dl_decimal::Decimal;

use crate::contracts::json::{self, WireJson};
use crate::util::stj::{Obj, ReadError, Writer};

/// Payload of
/// [`site_command_types::POS_REVENUE_SUMMARY`](super::site_command_types::POS_REVENUE_SUMMARY)
/// (ADR 0149). Days are store-local
/// `yyyy-MM-dd`, inclusive, at most [`MAX_DAYS`](Self::MAX_DAYS) apart. `reason` is a
/// free-text note for the agent log.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PosRevenueSummaryPayload {
    /// RVC.
    pub rvc_no: i32,
    /// First day, `yyyy-MM-dd`.
    pub date_from: String,
    /// Last day (inclusive), `yyyy-MM-dd`.
    pub date_to: String,
    /// Why the cloud asks.
    pub reason: Option<String>,
}

impl PosRevenueSummaryPayload {
    /// Longest range, in days.
    pub const MAX_DAYS: i64 = 31;

    /// Aggregate key of this command: reads never order against a check.
    pub fn aggregate_key(rvc_no: i32) -> String {
        format!("pos-revenue:{rvc_no}")
    }

    /// Days since 0001-01-01 of a strict `yyyy-MM-dd` text (`TryDay`), `None` when it is
    /// not exactly that shape or not a real calendar day.
    pub fn try_day(text: &str) -> Option<i64> {
        let b = text.as_bytes();
        if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
            return None;
        }
        let digits = |r: std::ops::Range<usize>| -> Option<i64> {
            let part = &b[r];
            if part.iter().all(u8::is_ascii_digit) {
                part.iter()
                    .try_fold(0i64, |a, d| Some(a * 10 + i64::from(d - b'0')))
            } else {
                None
            }
        };
        let (y, m, d) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let in_month = match m {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => return None,
        };
        if y < 1 || d < 1 || d > in_month {
            return None;
        }
        // Days before this year + days before this month + the day (1-based -> 0-based).
        let py = y - 1;
        let before_year = py * 365 + py / 4 - py / 100 + py / 400;
        const CUMULATIVE: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
        let before_month = CUMULATIVE[(m - 1) as usize] + i64::from(m > 2 && leap);
        Some(before_year + before_month + d - 1)
    }

    /// The validation problem, `None` when valid (`PosRevenueSummaryPayload.Problem`).
    pub fn problem(&self) -> Option<&'static str> {
        if self.rvc_no <= 0 {
            return Some("rvc_no must be positive");
        }
        let Some(from) = Self::try_day(&self.date_from) else {
            return Some("date_from must be yyyy-MM-dd");
        };
        let Some(to) = Self::try_day(&self.date_to) else {
            return Some("date_to must be yyyy-MM-dd");
        };
        if from > to {
            return Some("date_from must not be after date_to");
        }
        if to - from + 1 > Self::MAX_DAYS {
            Some("the range must not exceed 31 days")
        } else {
            None
        }
    }
}

impl WireJson for PosRevenueSummaryPayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("date_from").string(&self.date_from);
        w.key("date_to").string(&self.date_to);
        json::opt_str(w, "reason", &self.reason);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(PosRevenueSummaryPayload {
            rvc_no: o.int("rvc_no")?,
            date_from: o.string("date_from")?,
            date_to: o.string("date_to")?,
            reason: o.opt_string("reason")?,
        })
    }
}

/// One store-local day of [`PosRevenueSummaryResult`]: closed bills, their gross and net
/// totals, and the tender sum (`PosRevenueDay`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PosRevenueDay {
    /// The day, `yyyy-MM-dd`.
    pub date: String,
    /// Closed bills.
    pub bills: i32,
    /// Sum of the bills' totals before discount.
    pub gross: Decimal,
    /// Sum of the bills' base totals.
    pub net: Decimal,
    /// Sum of what was tendered.
    pub paid: Decimal,
}

impl WireJson for PosRevenueDay {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("date").string(&self.date);
        w.key("bills").int(self.bills.into());
        w.key("gross").decimal(&self.gross);
        w.key("net").decimal(&self.net);
        w.key("paid").decimal(&self.paid);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(PosRevenueDay {
            date: o.string("date")?,
            bills: o.int("bills")?,
            gross: o.decimal("gross")?,
            net: o.decimal("net")?,
            paid: o.decimal("paid")?,
        })
    }
}

/// Result of
/// [`site_command_types::POS_REVENUE_SUMMARY`](super::site_command_types::POS_REVENUE_SUMMARY):
/// one row per day that has closed bills, ascending; no bill lines, no names.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PosRevenueSummaryResult {
    /// RVC.
    pub rvc_no: i32,
    /// First day asked.
    pub date_from: String,
    /// Last day asked.
    pub date_to: String,
    /// The days.
    pub days: Vec<PosRevenueDay>,
}

impl WireJson for PosRevenueSummaryResult {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("date_from").string(&self.date_from);
        w.key("date_to").string(&self.date_to);
        w.key("days");
        json::write_items(w, &self.days);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(PosRevenueSummaryResult {
            rvc_no: o.int("rvc_no")?,
            date_from: o.string("date_from")?,
            date_to: o.string("date_to")?,
            days: o.list("days", json::item)?,
        })
    }
}
