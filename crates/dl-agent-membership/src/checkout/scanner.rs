//! `CheckoutScanner.ScanOnceAsync`: one reconciliation of the bookings the hub still holds
//! as staying (`bookings/pending-checkout`) against SMILE, plus the walk-in stays.
//!
//! There is no cursor: the hub names the bookings that have not earned yet, and a booking
//! leaves that list once the hub closed it (`CHECKED_OUT`). The walk-in path remembers the
//! groups it reported in memory only (the hub dedups by `folio_ref`). SMILE is only read.

use std::fmt;

use dl_decimal::Decimal;
use dl_time::{Date, Duration, OffsetDateTime};

use super::settings::CheckoutSettings;
use super::sql::{self, CheckedOut, WalkIn};
use super::tally::{REASON_NO_MEMBER, REASON_REPORT_FAILED, Tally};
use super::walk_in::{IN_PARAM_LIMIT, SeenWalkIns, WalkInEarnState};
use crate::earn::EarnEnv;
use crate::earn::pairs::PairSource;
use crate::earn::sql::{
    EarnDb, FolioScope, Stay, classification, folio_transactions, read_revenue,
    resolve_checkout_time, wire_lines,
};
use crate::earn::wire::{EarnReport, RevenueLine, WalkInEarnReport, WalkInOutcome};
use crate::folio::earn::EarnPair;
use crate::hub::HubError;
use crate::hub::earn::EarnHub;
use crate::log::{self, V};
use crate::smile::{SmileError, TdsSmile, hotel_offset};

/// `ZeroRevenueAlarmThreshold`.
pub const ZERO_REVENUE_ALARM: usize = 5;

/// Why a scan stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutError {
    /// SMILE failed.
    Smile(SmileError),
    /// The hub failed (pending-checkout).
    Hub(HubError),
}

impl fmt::Display for CheckoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CheckoutError::Smile(e) => write!(f, "checkout scan: {e}"),
            CheckoutError::Hub(e) => write!(f, "checkout scan: {e}"),
        }
    }
}

impl std::error::Error for CheckoutError {}

impl From<SmileError> for CheckoutError {
    fn from(e: SmileError) -> CheckoutError {
        CheckoutError::Smile(e)
    }
}

/// `ISmileConnectionFactory.OpenAsync`: one SMILE session for the length of `f`.
pub trait CheckoutDb: Send + Sync {
    /// Runs `f` on an open session.
    fn with_db(
        &self,
        f: &mut dyn FnMut(&mut dyn EarnDb) -> Result<(), CheckoutError>,
    ) -> Result<(), CheckoutError>;
}

impl CheckoutDb for TdsSmile {
    fn with_db(
        &self,
        f: &mut dyn FnMut(&mut dyn EarnDb) -> Result<(), CheckoutError>,
    ) -> Result<(), CheckoutError> {
        self.session(None, |c| f(c))
    }
}

/// `HotelClock.ToHotelDate`.
pub fn hotel_day(at: OffsetDateTime) -> Date {
    at.to_offset(hotel_offset()).date()
}

/// What one scan did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CheckoutOutcome {
    /// Bookings and walk-in groups reported.
    pub reported: usize,
    /// Bookings the hub asked about.
    pub pending: usize,
}

/// The scanner state (one per scan thread).
#[derive(Debug)]
pub struct CheckoutScanner {
    settings: CheckoutSettings,
    phone_column: String,
    agent_version: String,
    pairs: PairSource,
    tally: Tally,
    seen: SeenWalkIns,
    walk_in_state: std::sync::Arc<WalkInEarnState>,
    walk_in_disabled: bool,
    warned_hub_lacks_pms_cancel: bool,
    revenue_computed: usize,
    zero_revenue: usize,
    warned_all_zero: bool,
    clock: fn() -> OffsetDateTime,
}

impl CheckoutScanner {
    /// A scanner reading `phone_column` for walk-ins, sharing `walk_in_state` with the
    /// heartbeat.
    pub fn new(
        settings: CheckoutSettings,
        phone_column: &str,
        pairs: PairSource,
        walk_in_state: std::sync::Arc<WalkInEarnState>,
        agent_version: &str,
    ) -> CheckoutScanner {
        let codes = if pairs.settings().charge_codes.is_empty() {
            "derived from the SMILE configuration (ADR-0105)".to_owned()
        } else {
            format!(
                "Earn:ChargeCodes={}",
                pairs.settings().charge_codes.join(",")
            )
        };
        log::info(
            "checkout scan on",
            &[
                ("interval_s", V::I(settings.interval.as_secs() as i64)),
                ("earn_codes", V::S(&codes)),
                ("walk_in", V::B(settings.walk_in_enabled)),
            ],
        );
        CheckoutScanner {
            settings,
            phone_column: phone_column.to_owned(),
            agent_version: agent_version.to_owned(),
            pairs,
            tally: Tally::default(),
            seen: SeenWalkIns::default(),
            walk_in_state,
            walk_in_disabled: false,
            warned_hub_lacks_pms_cancel: false,
            revenue_computed: 0,
            zero_revenue: 0,
            warned_all_zero: false,
            clock: OffsetDateTime::now_utc,
        }
    }

    /// Reads the time from `clock` instead of the system clock (tests).
    pub fn with_clock(mut self, clock: fn() -> OffsetDateTime) -> CheckoutScanner {
        self.clock = clock;
        self
    }

    /// The settings.
    pub fn settings(&self) -> &CheckoutSettings {
        &self.settings
    }

    /// `ScanOnceAsync`: the scan, then (whatever it did) the all-zero alarm and the daily
    /// summary push.
    ///
    /// # Errors
    /// The pending-checkout call, opening SMILE, the checked-out read or the earn-pair read
    /// failed; nothing is lost, the next scan asks again.
    pub fn scan_once(
        &mut self,
        db: &dyn CheckoutDb,
        hub: &dyn EarnHub,
        env: &dyn EarnEnv,
    ) -> Result<CheckoutOutcome, CheckoutError> {
        self.revenue_computed = 0;
        self.zero_revenue = 0;
        let result = self.core(db, hub, env);
        self.warn_if_all_zero();
        self.push_summary(hub);
        result
    }

    fn core(
        &mut self,
        db: &dyn CheckoutDb,
        hub: &dyn EarnHub,
        env: &dyn EarnEnv,
    ) -> Result<CheckoutOutcome, CheckoutError> {
        let pending = hub
            .pending_checkout(self.settings.max_per_scan, self.settings.grace_days)
            .map_err(CheckoutError::Hub)?;
        let walk_ins = self.settings.walk_in_enabled && !self.walk_in_disabled;
        let mut out = CheckoutOutcome {
            reported: 0,
            pending: pending.len(),
        };
        if pending.is_empty() && !walk_ins {
            return Ok(out);
        }
        db.with_db(&mut |c| {
            let mut reported = if walk_ins {
                self.scan_walk_ins(c, hub, env)?
            } else {
                0
            };
            if !pending.is_empty() {
                reported += self.scan_bookings(c, hub, env, &pending)?;
            }
            out.reported = reported;
            Ok(())
        })?;
        Ok(out)
    }

    fn scan_bookings(
        &mut self,
        c: &mut dyn EarnDb,
        hub: &dyn EarnHub,
        env: &dyn EarnEnv,
        pending: &[crate::earn::wire::PendingBooking],
    ) -> Result<usize, CheckoutError> {
        let codes: Vec<String> = pending
            .iter()
            .map(|p| p.confirmation_code.clone())
            .collect();
        let mut checked_out: Vec<CheckedOut> = Vec::new();
        for chunk in codes.chunks(IN_PARAM_LIMIT) {
            checked_out.extend(sql::scan_checked_out(c, chunk)?);
        }
        let pairs = if checked_out.is_empty() {
            None
        } else {
            let scope = FolioScope::Bookings(
                checked_out
                    .iter()
                    .map(|b| b.confirmation_code.clone())
                    .collect(),
            );
            self.pairs
                .resolve(c, &scope, env.hub_extra_bed_code().as_deref())?
        };
        let mut reported = 0;
        if let Some(pairs) = &pairs {
            for booking in &checked_out {
                let day = hotel_day((self.clock)());
                let key = format!("B:{}", booking.confirmation_code);
                if booking.member_id.is_none() {
                    self.tally.skipped(day, &key, REASON_NO_MEMBER);
                    continue;
                }
                match self.report_booking(c, hub, env, booking, pairs) {
                    Ok((revenue, lines, unavailable)) => {
                        reported += 1;
                        self.tally.reported(day, &key);
                        log::info(
                            "earn reported",
                            &[
                                ("booking", V::S(&booking.booking_id.to_string())),
                                ("code", V::S(&booking.confirmation_code)),
                                ("revenue", V::S(&revenue.to_string())),
                            ],
                        );
                        self.report_folio_revenue(
                            c,
                            hub,
                            &booking.confirmation_code,
                            lines,
                            unavailable,
                        );
                    }
                    Err(e) => {
                        self.tally.skipped(day, &key, REASON_REPORT_FAILED);
                        log::warn(
                            "earn report failed; asked again next scan",
                            &[
                                ("booking", V::S(&booking.booking_id.to_string())),
                                ("code", V::S(&booking.confirmation_code)),
                                ("error", V::S(&e)),
                            ],
                        );
                    }
                }
            }
        }
        self.report_pms_cancelled(c, hub, pending);
        Ok(reported)
    }

    /// The `try` body of the booking loop: revenue, classification, `ReportEarnAsync`.
    /// Returns the revenue, the wire breakdown and whether the breakdown was unavailable.
    fn report_booking(
        &mut self,
        c: &mut dyn EarnDb,
        hub: &dyn EarnHub,
        env: &dyn EarnEnv,
        booking: &CheckedOut,
        pairs: &[EarnPair],
    ) -> Result<(Decimal, Option<Vec<RevenueLine>>, bool), String> {
        let stay = Stay::Booking(booking.confirmation_code.clone());
        let (revenue, lines) =
            read_revenue(c, &stay, pairs, &booking.confirmation_code).map_err(|e| e.to_string())?;
        self.track_revenue(revenue);
        let unavailable = lines.is_none();
        let folio_lines = wire_lines(lines);
        let classification = classification(c, &stay).map_err(|e| e.to_string())?;
        let (at, source) = resolve_checkout_time(booking.checked_out_at);
        hub.report_earn(&EarnReport {
            booking_id: booking.booking_id,
            folio_ref: booking.confirmation_code.clone(),
            qualifying_revenue: revenue,
            checked_out_at: at,
            folio_lines: folio_lines.clone(),
            classification: Some(classification),
            db_identity_fingerprint: env.fingerprint(),
            checked_out_at_source: Some(source.to_owned()),
        })
        .map_err(|e| e.to_string())?;
        Ok((revenue, folio_lines, unavailable))
    }

    /// `ReportFolioRevenueAsync`: audit only; a failure is logged and never fails the earn.
    fn report_folio_revenue(
        &self,
        c: &mut dyn EarnDb,
        hub: &dyn EarnHub,
        code: &str,
        scanned: Option<Vec<RevenueLine>>,
        unavailable: bool,
    ) {
        let lines = if unavailable {
            match folio_transactions(c, &Stay::Booking(code.to_owned())) {
                Ok(l) => wire_lines(Some(l)),
                Err(e) => {
                    log::warn(
                        "folio revenue breakdown not reported (audit only, earn unaffected)",
                        &[("code", V::S(code)), ("error", V::S(&e.to_string()))],
                    );
                    return;
                }
            }
        } else {
            scanned
        };
        let Some(lines) = lines else {
            return;
        };
        if let Err(e) = hub.report_folio_revenue(code, &lines) {
            log::warn(
                "folio revenue breakdown not reported (audit only, earn unaffected)",
                &[("code", V::S(code)), ("error", V::S(&e.to_string()))],
            );
        }
    }

    /// `ReportPmsCancelledAsync` (ADR-0070): best effort, after earn on purpose.
    fn report_pms_cancelled(
        &mut self,
        c: &mut dyn EarnDb,
        hub: &dyn EarnHub,
        pending: &[crate::earn::wire::PendingBooking],
    ) {
        let codes: Vec<String> = pending
            .iter()
            .map(|p| p.confirmation_code.clone())
            .collect();
        let mut cancelled = Vec::new();
        for chunk in codes.chunks(IN_PARAM_LIMIT) {
            match sql::scan_cancelled(c, chunk) {
                Ok(v) => cancelled.extend(v),
                Err(e) => {
                    log::warn(
                        "PMS-cancelled read failed; asked again next scan",
                        &[("error", V::S(&e.to_string()))],
                    );
                    return;
                }
            }
        }
        for code in &cancelled {
            let Some(p) = pending.iter().find(|p| p.confirmation_code == *code) else {
                continue;
            };
            match hub.report_pms_cancelled(p.booking_id, code) {
                Ok(None) => {
                    if !self.warned_hub_lacks_pms_cancel {
                        self.warned_hub_lacks_pms_cancel = true;
                        log::info(
                            "the hub has no bookings/pms-cancelled yet (older hub); PMS-cancelled bookings are reported once it is upgraded",
                            &[],
                        );
                    }
                    return;
                }
                Ok(Some(done)) => log::info(
                    "booking cancelled in the PMS reported",
                    &[
                        ("booking", V::S(&p.booking_id.to_string())),
                        ("code", V::S(code)),
                        ("hub_cancelled", V::B(done)),
                    ],
                ),
                Err(e) => log::warn(
                    "PMS-cancelled report failed; asked again next scan",
                    &[
                        ("booking", V::S(&p.booking_id.to_string())),
                        ("error", V::S(&e.to_string())),
                    ],
                ),
            }
        }
    }

    /// `ScanWalkInsAsync`: the groups the hub answered for good, counted.
    fn scan_walk_ins(
        &mut self,
        c: &mut dyn EarnDb,
        hub: &dyn EarnHub,
        env: &dyn EarnEnv,
    ) -> Result<usize, CheckoutError> {
        let since = (self.clock)() - Duration::from_days(self.settings.walk_in_lookback_days);
        self.seen.forget_before(since);
        let exclude = self.seen.exclusion(
            self.settings.max_walk_ins_per_scan,
            self.settings.walk_in_lookback_days,
        );
        let max = i32::try_from(self.settings.max_walk_ins_per_scan).unwrap_or(i32::MAX);
        let groups = match sql::scan_walk_ins(c, &self.phone_column, since, &exclude, max) {
            Ok(g) => g,
            Err(e @ (SmileError::Server(207 | 208) | SmileError::Invalid(_))) => {
                self.walk_in_disabled = true;
                self.walk_in_state.mark_disabled();
                log::error(
                    "walk-in earn turned off: SMILE has no AdditionalName phone column (or TeleNum), or guest_phone_column is not a valid name; fix guest_phone_column (or set walk_in_earn_enabled false) and restart; booking earn is not affected",
                    &[
                        ("column", V::S(&self.phone_column)),
                        ("error", V::S(&e.to_string())),
                    ],
                );
                return Ok(0);
            }
            Err(e) => return Err(e.into()),
        };
        self.walk_in_state.mark_healthy();
        if groups.is_empty() {
            return Ok(0);
        }
        let scope = FolioScope::WalkIns(groups.iter().map(|g| g.confirm_num).collect());
        let pairs = match self
            .pairs
            .resolve(c, &scope, env.hub_extra_bed_code().as_deref())
        {
            Ok(Some(p)) => p,
            Ok(None) => return Ok(0),
            Err(e) => {
                log::error(
                    "earn codes not readable from SMILE; walk-ins skipped this scan",
                    &[("error", V::S(&e.to_string()))],
                );
                return Ok(0);
            }
        };
        let mut reported = 0;
        for group in &groups {
            if self.seen.contains(&group.confirm_num) {
                continue;
            }
            let day = hotel_day(group.checked_out_at);
            let key = format!("W:{}", group.confirm_num);
            match self.report_walk_in(c, hub, env, group, &pairs) {
                Ok((revenue, result)) => {
                    self.seen.mark(group.confirm_num, group.checked_out_at);
                    self.tally.reported(day, &key);
                    reported += 1;
                    let attrs = [
                        ("folio", V::S(&group.folio_ref())),
                        ("outcome", V::S(result.outcome.name())),
                        ("revenue", V::S(&revenue.to_string())),
                        ("points", V::I(result.points_awarded)),
                    ];
                    if result.outcome == WalkInOutcome::Awarded {
                        log::info("walk-in earned", &attrs);
                    } else {
                        log::debug("walk-in not earned", &attrs);
                    }
                }
                Err(e) => {
                    self.tally.skipped(day, &key, REASON_REPORT_FAILED);
                    log::warn(
                        "walk-in earn report failed; asked again next scan",
                        &[("folio", V::S(&group.folio_ref())), ("error", V::S(&e))],
                    );
                }
            }
        }
        Ok(reported)
    }

    fn report_walk_in(
        &mut self,
        c: &mut dyn EarnDb,
        hub: &dyn EarnHub,
        env: &dyn EarnEnv,
        group: &WalkIn,
        pairs: &[EarnPair],
    ) -> Result<(Decimal, crate::earn::wire::WalkInResult), String> {
        let stay = Stay::WalkIn(group.confirm_num);
        let (revenue, lines) =
            read_revenue(c, &stay, pairs, &group.folio_ref()).map_err(|e| e.to_string())?;
        self.track_revenue(revenue);
        let classification = classification(c, &stay).map_err(|e| e.to_string())?;
        let result = hub
            .report_walk_in_earn(&WalkInEarnReport {
                guest_phone: group.guest_phone.clone(),
                folio_ref: group.folio_ref(),
                qualifying_revenue: revenue,
                checked_out_at: group.checked_out_at,
                guest_name: group.guest_name.clone(),
                folio_lines: wire_lines(lines),
                classification: Some(classification),
                db_identity_fingerprint: env.fingerprint(),
            })
            .map_err(|e| e.to_string())?;
        Ok((revenue, result))
    }

    /// `TrackRevenue`.
    fn track_revenue(&mut self, revenue: Decimal) {
        self.revenue_computed += 1;
        if revenue.is_zero() {
            self.zero_revenue += 1;
        } else {
            self.warned_all_zero = false;
        }
    }

    /// `WarnIfWholeScanEarnedNothing`: once until money flows again.
    fn warn_if_all_zero(&mut self) {
        if self.revenue_computed < ZERO_REVENUE_ALARM
            || self.zero_revenue < self.revenue_computed
            || self.warned_all_zero
        {
            return;
        }
        self.warned_all_zero = true;
        let codes = if self.pairs.settings().charge_codes.is_empty() {
            "derived from the SMILE configuration".to_owned()
        } else {
            self.pairs.settings().charge_codes.join(",")
        };
        log::error(
            "every checked-out folio of this scan has zero earn revenue: the earn codes almost surely do not match this SMILE install; drop earn_charge_codes to derive them from SMILE, or compare the logged earn codes with FolioTransaction (ADR-0105)",
            &[
                ("zero", V::I(self.zero_revenue as i64)),
                ("total", V::I(self.revenue_computed as i64)),
                ("codes", V::S(&codes)),
            ],
        );
    }

    /// `PushSummaryAsync`: a failure is logged and pushed again next scan.
    fn push_summary(&mut self, hub: &dyn EarnHub) {
        let result = (|| -> Result<(), HubError> {
            for report in self.tally.pending(Some(&self.agent_version)) {
                hub.report_scan_summary(&report)?;
                self.tally.mark_pushed(&report);
            }
            Ok(())
        })();
        match result {
            Ok(()) => self.tally.prune(hotel_day((self.clock)())),
            Err(e) => log::warn(
                "checkout summary not pushed; pushed again next scan",
                &[("error", V::S(&e.to_string()))],
            ),
        }
    }
}
