//! Shift and cash verbs of a cloud-authoritative tenant (`Wire/TxnShiftCommands.cs`, ADR 0160,
//! RT-0935): `txn.shift_open` / `txn.shift_close` carry the full state of one shift header and
//! its counted lines, `txn.cash_in` / `txn.cash_out` one cash movement of the shift. Moments are
//! store-local `yyyy-MM-ddTHH:mm:ss[.fff]` without offset.

use dl_decimal::Decimal;

use crate::contracts::json::WireJson;
use crate::contracts::utf16_len;
use crate::contracts::wire::table_commands::is_local_timestamp;
use crate::contracts::wire::txn_commands::{json_int64, txn_command_types, txn_scopes};
use crate::util::stj::{Obj, ReadError, Writer};

/// One counted cash line of a shift (`TxnShiftMoneyLine`, `ShiftMoney`): currency, note name,
/// quantity, amount. The agent adds the shift's store key, cashier, business date and RVC and
/// never writes the identity `SeqNo`. A JSON null line reads as the default line, refused by
/// [`TxnShiftPayload::problem`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TxnShiftMoneyLine {
    /// Currency code.
    pub cr_code: i32,
    /// Note name.
    pub note_name: String,
    /// Number of notes.
    pub qty: i32,
    /// Amount of the line.
    pub amount: i64,
    /// Transaction code.
    pub trn_code: i32,
    /// Store-local moment of the count.
    pub trn_time: String,
}

impl WireJson for TxnShiftMoneyLine {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("cr_code").int(i64::from(self.cr_code));
        w.key("note_name").string(&self.note_name);
        w.key("qty").int(i64::from(self.qty));
        w.key("amount").int(self.amount);
        w.key("trn_code").int(i64::from(self.trn_code));
        w.key("trn_time").string(&self.trn_time);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(TxnShiftMoneyLine {
            cr_code: o.int("cr_code")?,
            note_name: o.string("note_name")?,
            qty: o.int("qty")?,
            amount: o.int("amount")?,
            trn_code: o.int("trn_code")?,
            trn_time: o.string("trn_time")?,
        })
    }
}

/// Payload of [`txn_command_types::SHIFT_OPEN`] and [`txn_command_types::SHIFT_CLOSE`]
/// (`TxnShiftPayload`): the full state of the shift header and its counted lines at `version`.
/// The open verb has no closing fields, the close verb has all three. `shift_key` is the cloud's
/// shift id (the aggregate); the store's `ShiftNo` is an identity the agent allocates on the
/// first write. `money` is `None` for a JSON null list (refused).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TxnShiftPayload {
    /// The cloud's shift id.
    pub shift_key: i64,
    /// Aggregate version.
    pub version: i64,
    /// Outlet.
    pub rvc: i32,
    /// Cashier code.
    pub cashier: String,
    /// Store-local start of the shift.
    pub start_time: String,
    /// Store-local business date of the shift.
    pub open_date: String,
    /// The store's status code, chosen by the caller.
    pub shift_status: i32,
    /// Opening cash.
    pub cash_open: Decimal,
    /// Cash put in so far.
    pub cash_in: Decimal,
    /// Cash taken out so far.
    pub cash_out: Decimal,
    /// Closing cash (close only).
    pub cash_end: Option<Decimal>,
    /// Store-local end of the shift (close only).
    pub end_time: Option<String>,
    /// Store-local close date (close only).
    pub close_date: Option<String>,
    /// Notes of the shift.
    pub notes: Option<String>,
    /// Counted lines.
    pub money: Option<Vec<TxnShiftMoneyLine>>,
    /// Who sent it.
    pub actor: String,
}

impl TxnShiftPayload {
    /// Most counted lines one shift carries.
    pub const MAX_MONEY_LINES: usize = 100;
    /// Longest cashier code.
    pub const MAX_CASHIER_LENGTH: usize = 50;
    /// Longest notes.
    pub const MAX_NOTES_LENGTH: usize = 200;
    /// Longest note name.
    pub const MAX_NOTE_NAME_LENGTH: usize = 50;

    /// The aggregate, e.g. `shift:42`.
    pub fn aggregate(&self) -> String {
        txn_scopes::aggregate(txn_scopes::SHIFT, self.shift_key)
    }

    /// Shape problems both sides check before any SQL runs for the verb `type_`; `None` when
    /// well-formed (`Problem`, same messages and order).
    pub fn problem(&self, type_: Option<&str>) -> Option<String> {
        let closing = type_ == Some(txn_command_types::SHIFT_CLOSE);
        if !closing && type_ != Some(txn_command_types::SHIFT_OPEN) {
            return Some("Type is not a shift verb.".into());
        }
        if let Some(header) = header_problem(
            self.shift_key,
            self.version,
            self.rvc,
            &self.cashier,
            &self.actor,
        ) {
            return Some(header);
        }
        if !is_moment(Some(&self.start_time)) {
            return Some("StartTime must be yyyy-MM-ddTHH:mm:ss[.fff].".into());
        }
        if !is_moment(Some(&self.open_date)) {
            return Some("OpenDate must be yyyy-MM-ddTHH:mm:ss[.fff].".into());
        }
        if self.shift_status < 0 {
            return Some("ShiftStatus must not be negative.".into());
        }
        if !is_amount(self.cash_open) || !is_amount(self.cash_in) || !is_amount(self.cash_out) {
            return Some(
                "CashOpen, CashIn and CashOut must be non-negative with at most 2 decimals.".into(),
            );
        }
        if self
            .notes
            .as_deref()
            .is_some_and(|n| utf16_len(n) > Self::MAX_NOTES_LENGTH)
        {
            return Some("Notes is too long.".into());
        }
        if closing {
            if !self.cash_end.is_some_and(is_amount) {
                return Some(
                    "CashEnd is required to close and must be non-negative with at most 2 decimals."
                        .into(),
                );
            }
            if !is_moment(self.end_time.as_deref()) {
                return Some("EndTime is required to close (yyyy-MM-ddTHH:mm:ss[.fff]).".into());
            }
            if !is_moment(self.close_date.as_deref()) {
                return Some("CloseDate is required to close (yyyy-MM-ddTHH:mm:ss[.fff]).".into());
            }
        } else if self.cash_end.is_some() || self.end_time.is_some() || self.close_date.is_some() {
            return Some("An open shift has no CashEnd, EndTime or CloseDate.".into());
        }
        let Some(money) = &self.money else {
            return Some("Money is required (empty when nothing was counted).".into());
        };
        if money.len() > Self::MAX_MONEY_LINES {
            return Some("Too many Money lines.".into());
        }
        for line in money {
            if line.note_name.trim().is_empty()
                || utf16_len(&line.note_name) > Self::MAX_NOTE_NAME_LENGTH
            {
                return Some(format!(
                    "Every Money line needs a NoteName of 1..{} characters.",
                    Self::MAX_NOTE_NAME_LENGTH
                ));
            }
            if line.cr_code < 0 || line.qty < 0 || line.amount < 0 || line.trn_code < 0 {
                return Some("A Money line has a negative number.".into());
            }
            if !is_moment(Some(&line.trn_time)) {
                return Some("A Money line's TrnTime must be yyyy-MM-ddTHH:mm:ss[.fff].".into());
            }
        }
        None
    }

    /// The counted lines (empty for a null list).
    pub fn money_lines(&self) -> &[TxnShiftMoneyLine] {
        self.money.as_deref().unwrap_or(&[])
    }
}

fn opt_decimal_member(w: &mut Writer, name: &str, v: Option<&Decimal>) {
    if let Some(d) = v {
        w.key(name).decimal(d);
    }
}

fn opt_decimal(o: &Obj<'_>, snake: &str) -> Result<Option<Decimal>, ReadError> {
    match o.get_non_null(snake) {
        None => Ok(None),
        Some(v) => crate::util::stj::read_decimal(v, o.options(), &o.path_of(snake)).map(Some),
    }
}

impl WireJson for TxnShiftPayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("shift_key").int(self.shift_key);
        w.key("version").int(self.version);
        w.key("rvc").int(i64::from(self.rvc));
        w.key("cashier").string(&self.cashier);
        w.key("start_time").string(&self.start_time);
        w.key("open_date").string(&self.open_date);
        w.key("shift_status").int(i64::from(self.shift_status));
        w.key("cash_open").decimal(&self.cash_open);
        w.key("cash_in").decimal(&self.cash_in);
        w.key("cash_out").decimal(&self.cash_out);
        opt_decimal_member(w, "cash_end", self.cash_end.as_ref());
        w.opt_str_member("end_time", self.end_time.as_deref());
        w.opt_str_member("close_date", self.close_date.as_deref());
        w.opt_str_member("notes", self.notes.as_deref());
        if let Some(money) = &self.money {
            w.key("money").begin_array();
            for line in money {
                line.write_json(w);
            }
            w.end_array();
        }
        w.key("actor").string(&self.actor);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        let money = o.opt_list("money", |v, opts, path| match Obj::of(v, opts, path)? {
            None => Ok(TxnShiftMoneyLine::default()),
            Some(l) => TxnShiftMoneyLine::read_json(l),
        })?;
        Ok(TxnShiftPayload {
            shift_key: o.int("shift_key")?,
            version: o.int("version")?,
            rvc: o.int("rvc")?,
            cashier: o.string("cashier")?,
            start_time: o.string("start_time")?,
            open_date: o.string("open_date")?,
            shift_status: o.int("shift_status")?,
            cash_open: o.decimal("cash_open")?,
            cash_in: o.decimal("cash_in")?,
            cash_out: o.decimal("cash_out")?,
            cash_end: opt_decimal(&o, "cash_end")?,
            end_time: o.opt_string("end_time")?,
            close_date: o.opt_string("close_date")?,
            notes: o.opt_string("notes")?,
            money,
            actor: o.string("actor")?,
        })
    }
}

/// Payload of [`txn_command_types::CASH_IN`] and [`txn_command_types::CASH_OUT`]
/// (`TxnCashPayload`): one cash movement of a shift the agent appends to `DailyCashLog` exactly
/// once. `amount` is positive (the verb gives the direction); `shift_cash_in` /
/// `shift_cash_out` are the shift's running totals after this movement (absolute values, so a
/// replay never double counts).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TxnCashPayload {
    /// The cloud's shift id.
    pub shift_key: i64,
    /// Aggregate version.
    pub version: i64,
    /// The cloud's movement id (traceability only).
    pub movement_key: i64,
    /// Outlet.
    pub rvc: i32,
    /// Cashier code.
    pub cashier: String,
    /// Amount moved (positive).
    pub amount: Decimal,
    /// Why the cash moved.
    pub reason: String,
    /// Store-local business date.
    pub business_date: String,
    /// Store-local moment of the movement.
    pub at: String,
    /// The shift's cash-in total after this movement.
    pub shift_cash_in: Decimal,
    /// The shift's cash-out total after this movement.
    pub shift_cash_out: Decimal,
    /// Who sent it.
    pub actor: String,
}

impl TxnCashPayload {
    /// Longest reason.
    pub const MAX_REASON_LENGTH: usize = 200;

    /// The aggregate, e.g. `shift:42`.
    pub fn aggregate(&self) -> String {
        txn_scopes::aggregate(txn_scopes::SHIFT, self.shift_key)
    }

    /// Shape problems both sides check before any SQL runs for the verb `type_`; `None` when
    /// well-formed (`Problem`, same messages and order).
    pub fn problem(&self, type_: Option<&str>) -> Option<String> {
        if type_ != Some(txn_command_types::CASH_IN) && type_ != Some(txn_command_types::CASH_OUT) {
            return Some("Type is not a cash verb.".into());
        }
        if let Some(header) = header_problem(
            self.shift_key,
            self.version,
            self.rvc,
            &self.cashier,
            &self.actor,
        ) {
            return Some(header);
        }
        if self.movement_key <= 0 {
            return Some("MovementKey must be positive.".into());
        }
        if !is_amount(self.amount) || self.amount.is_zero() {
            return Some("Amount must be positive with at most 2 decimals.".into());
        }
        if self.reason.trim().is_empty() || utf16_len(&self.reason) > Self::MAX_REASON_LENGTH {
            return Some(format!(
                "Reason is required (at most {} characters).",
                Self::MAX_REASON_LENGTH
            ));
        }
        if !is_moment(Some(&self.business_date)) {
            return Some("BusinessDate must be yyyy-MM-ddTHH:mm:ss[.fff].".into());
        }
        if !is_moment(Some(&self.at)) {
            return Some("At must be yyyy-MM-ddTHH:mm:ss[.fff].".into());
        }
        if !is_amount(self.shift_cash_in) || !is_amount(self.shift_cash_out) {
            return Some(
                "ShiftCashIn and ShiftCashOut must be non-negative with at most 2 decimals.".into(),
            );
        }
        None
    }
}

impl WireJson for TxnCashPayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("shift_key").int(self.shift_key);
        w.key("version").int(self.version);
        w.key("movement_key").int(self.movement_key);
        w.key("rvc").int(i64::from(self.rvc));
        w.key("cashier").string(&self.cashier);
        w.key("amount").decimal(&self.amount);
        w.key("reason").string(&self.reason);
        w.key("business_date").string(&self.business_date);
        w.key("at").string(&self.at);
        w.key("shift_cash_in").decimal(&self.shift_cash_in);
        w.key("shift_cash_out").decimal(&self.shift_cash_out);
        w.key("actor").string(&self.actor);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(TxnCashPayload {
            shift_key: o.int("shift_key")?,
            version: o.int("version")?,
            movement_key: o.int("movement_key")?,
            rvc: o.int("rvc")?,
            cashier: o.string("cashier")?,
            amount: o.decimal("amount")?,
            reason: o.string("reason")?,
            business_date: o.string("business_date")?,
            at: o.string("at")?,
            shift_cash_in: o.decimal("shift_cash_in")?,
            shift_cash_out: o.decimal("shift_cash_out")?,
            actor: o.string("actor")?,
        })
    }
}

/// Result of an applied shift or cash verb (`TxnShiftResult`, ADR 0160 §4): the shape of
/// [`TxnSnapshotResult`](super::TxnSnapshotResult) plus the store keys. `hash` is the aggregate
/// hash of the shift's `CashierShift` and `ShiftMoney` rows read by `shift_no`. An applied cash
/// verb also answers the identity `trn_seq` it got in `DailyCashLog` and `cash_log_hash`, the
/// aggregate hash of that one row (a stale answer has neither).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TxnShiftResult {
    /// The aggregate, e.g. `shift:42`.
    pub aggregate: String,
    /// Version of the command.
    pub version: i64,
    /// `applied` or `stale`.
    pub outcome: String,
    /// Version recorded after the command.
    pub applied_version: i64,
    /// Aggregate hash of the shift rows.
    pub hash: String,
    /// Row count per shift table.
    pub row_counts: Vec<(String, i32)>,
    /// The store's `ShiftNo` (0 when the shift has none yet).
    pub shift_no: i64,
    /// `DailyCashLog.TrnSeq` of an applied cash verb.
    pub trn_seq: Option<i64>,
    /// Aggregate hash of that cash log row.
    pub cash_log_hash: Option<String>,
}

impl WireJson for TxnShiftResult {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("aggregate").string(&self.aggregate);
        w.key("version").int(self.version);
        w.key("outcome").string(&self.outcome);
        w.key("applied_version").int(self.applied_version);
        w.key("hash").string(&self.hash);
        w.key("row_counts").begin_object();
        for (table, n) in &self.row_counts {
            w.key(table).int(i64::from(*n));
        }
        w.end_object();
        w.key("shift_no").int(self.shift_no);
        w.opt_int_member("trn_seq", self.trn_seq);
        w.opt_str_member("cash_log_hash", self.cash_log_hash.as_deref());
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        let mut row_counts = Vec::new();
        if let Some(v) = o.get_non_null("row_counts") {
            let path = o.path_of("row_counts");
            let Some(map) = v.as_object() else {
                return Err(ReadError::new(&path, "The JSON value is not an object."));
            };
            for (k, v) in map.iter() {
                let n = json_int64(v)
                    .and_then(|n| i32::try_from(n).ok())
                    .ok_or_else(|| ReadError::new(&format!("{path}.{k}"), "Not an Int32."))?;
                row_counts.push((k.clone(), n));
            }
        }
        Ok(TxnShiftResult {
            aggregate: o.string("aggregate")?,
            version: o.int("version")?,
            outcome: o.string("outcome")?,
            applied_version: o.int("applied_version")?,
            hash: o.string("hash")?,
            row_counts,
            shift_no: o.int("shift_no")?,
            trn_seq: o.opt_int("trn_seq")?,
            cash_log_hash: o.opt_string("cash_log_hash")?,
        })
    }
}

/// `TxnShiftChecks.Header`: the checks both payloads share; `None` when well-formed.
fn header_problem(
    shift_key: i64,
    version: i64,
    rvc: i32,
    cashier: &str,
    actor: &str,
) -> Option<String> {
    if shift_key <= 0 {
        return Some("ShiftKey must be positive.".into());
    }
    if version <= 0 {
        return Some("Version must be positive.".into());
    }
    if rvc <= 0 {
        return Some("Rvc must be positive.".into());
    }
    if cashier.trim().is_empty() || utf16_len(cashier) > TxnShiftPayload::MAX_CASHIER_LENGTH {
        return Some(format!(
            "Cashier is required (at most {} characters).",
            TxnShiftPayload::MAX_CASHIER_LENGTH
        ));
    }
    if actor.trim().is_empty() {
        return Some("Actor is required.".into());
    }
    None
}

/// `TxnShiftChecks.IsMoment`: exactly `yyyy-MM-ddTHH:mm:ss` with an optional `.f`, `.ff` or
/// `.fff` (`DateTime.TryParseExact`, invariant culture, no white space).
pub fn is_moment(value: Option<&str>) -> bool {
    let Some(value) = value else {
        return false;
    };
    let (Some(head), Some(tail)) = (value.get(..19), value.get(19..)) else {
        return false;
    };
    if !is_local_timestamp(head) {
        return false;
    }
    match tail.strip_prefix('.') {
        None => tail.is_empty(),
        Some(fraction) => {
            (1..=3).contains(&fraction.len()) && fraction.bytes().all(|b| b.is_ascii_digit())
        }
    }
}

/// `TxnShiftChecks.IsAmount`: non-negative with at most two decimals and within
/// `numeric(18,2)`.
pub fn is_amount(value: Decimal) -> bool {
    value >= Decimal::ZERO
        && value < Decimal::from(10_000_000_000_000_000_i64)
        && value.round(2) == value
}
