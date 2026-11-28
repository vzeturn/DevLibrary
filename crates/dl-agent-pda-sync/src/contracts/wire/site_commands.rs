//! Site command contract v1 (`Wire/SiteCommands.cs`, ADR 0029).

use std::time::Duration;

use crate::contracts::json::{self, WireJson};
use crate::contracts::time::DateTime;
use crate::contracts::utf16_len;
use crate::util::stj::{Obj, ReadError, Writer};

/// How a site command is delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SiteCommandDelivery {
    /// A user waits: only queued while an agent is live, expires after the TTL.
    Interactive,
    /// A cloud intent that waits for the agent however long the site is offline.
    Durable,
}

/// Per-type delivery rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SiteCommandSpec {
    /// Wire type.
    pub type_: &'static str,
    /// Delivery.
    pub delivery: SiteCommandDelivery,
    /// Attempts before the command is dead.
    pub max_attempts: i32,
}

/// Wire names of the command types of contract v1.
pub mod site_command_types {
    /// KDS accept / unaccept / done / undone of lines of an open check.
    pub const KDS_LINE_STATE: &str = "kds.line_state";
    /// New table reservation.
    pub const RESERVATION_SAVE: &str = "reservation.save";
    /// Cancel a table reservation.
    pub const RESERVATION_CANCEL: &str = "reservation.cancel";
    /// Resend every row of the named sync streams (ADR 0102).
    pub const SYNC_RESNAPSHOT: &str = "sync.resnapshot";
    /// Store-cloud reconciliation (ADR 0126): compare the agent's shadow with the cloud
    /// manifest per bucket.
    pub const SYNC_VERIFY: &str = "sync.verify";
    /// Read-only daily revenue totals of one RVC from SMILE_POS (ADR 0149, RT-0843).
    pub const POS_REVENUE_SUMMARY: &str = "pos.revenue_summary";
    /// Read-only POS WIN bills of one RVC and business date (RT-0842).
    pub const POS_BILL_LIST: &str = "pos.bill_list";
    /// Read-only search of the in-house guests of an RVC's Front Office database (RT-1296).
    pub const FO_INHOUSE_SEARCH: &str = "fo.inhouse_search";
}

/// Values of [`KdsLineStatePayload::action`].
pub mod kds_line_actions {
    /// Accept.
    pub const ACCEPT: &str = "accept";
    /// Unaccept.
    pub const UNACCEPT: &str = "unaccept";
    /// Done.
    pub const DONE: &str = "done";
    /// Undone.
    pub const UNDONE: &str = "undone";
    /// All actions.
    pub const ALL: &[&str] = &[ACCEPT, UNACCEPT, DONE, UNDONE];
}

/// Rejection reasons both sides key on (ADR 0100).
pub mod site_command_reasons {
    /// The check no longer matches the state the user saw.
    pub const CHECK_CHANGED: &str = "check_changed";
}

/// Cloud to site command contract (ADR 0029).
pub mod site_command_contract {
    use super::*;
    use crate::contracts::wire::{
        money_command_types as m, order_command_types as o, table_command_types as t,
    };

    /// Contract version.
    pub const VERSION: i32 = 1;
    /// A dispatched command not answered within the lease is handed out again.
    pub const DISPATCH_LEASE: Duration = Duration::from_secs(60);
    /// Lifetime of an interactive command.
    pub const INTERACTIVE_TTL: Duration = Duration::from_secs(60);
    /// An agent is live when it polled or sent a heartbeat this recently.
    pub const AGENT_LIVE_WINDOW: Duration = Duration::from_secs(30);
    /// Longest retry delay.
    pub const MAX_BACKOFF: Duration = Duration::from_secs(300);
    /// How long an API request waits for the agent's result (ADR 0100).
    pub const RESULT_WAIT: Duration = Duration::from_secs(4);
    /// Latency budget from queueing to commit; slower is a site event.
    pub const SLOW_COMMAND: Duration = Duration::from_secs(1);

    const fn spec(
        type_: &'static str,
        delivery: SiteCommandDelivery,
        max_attempts: i32,
    ) -> SiteCommandSpec {
        SiteCommandSpec {
            type_,
            delivery,
            max_attempts,
        }
    }

    use SiteCommandDelivery::{Durable, Interactive};

    /// Every command type with its delivery rule.
    pub const SPECS: &[SiteCommandSpec] = &[
        spec(site_command_types::KDS_LINE_STATE, Interactive, 3),
        spec(site_command_types::RESERVATION_SAVE, Durable, 8),
        spec(site_command_types::RESERVATION_CANCEL, Durable, 8),
        spec(site_command_types::SYNC_RESNAPSHOT, Durable, 8),
        spec(site_command_types::SYNC_VERIFY, Durable, 3),
        // RT-1427: the read-only SMILE_POS reads, ported from the PDAOrder agent.
        spec(site_command_types::POS_REVENUE_SUMMARY, Durable, 3),
        spec(site_command_types::POS_BILL_LIST, Durable, 3),
        spec(site_command_types::FO_INHOUSE_SEARCH, Interactive, 1),
        spec(o::CHECK_OPEN, Interactive, 3),
        spec(o::ITEM_POST, Interactive, 3),
        spec(o::SEND, Interactive, 3),
        spec(o::ITEM_VOID, Interactive, 3),
        spec(o::COVERS, Interactive, 3),
        spec(o::COMBO_POST, Interactive, 3),
        spec(t::TABLE_CHANGE, Interactive, 3),
        spec(t::TABLE_TRANSFER, Interactive, 3),
        spec(t::CHECK_MERGE, Interactive, 3),
        spec(t::ITEMS_MOVE, Interactive, 3),
        // ADR 0174 (RT-1297): tenders and close; a cashier waits for them.
        spec(m::TENDER_ADD, Interactive, 3),
        spec(m::TENDER_REMOVE, Interactive, 3),
        spec(m::CHECK_CLOSE, Interactive, 3),
        spec(t::CHECK_CANCEL_EMPTY, Durable, 8),
        // RT-1413: the guest of a check; waits for the store like the empty-bill cleanup.
        spec(t::PO_NUMBER, Durable, 8),
    ];

    /// The spec of a type (ordinal).
    pub fn find(type_: Option<&str>) -> Option<&'static SiteCommandSpec> {
        SPECS.iter().find(|s| Some(s.type_) == type_)
    }

    /// Delay before retry number `attempts + 1`: 5 s, 10 s, 20 s ... capped at 5 min.
    pub fn backoff(attempts: i32) -> Duration {
        let n = attempts.clamp(1, 16);
        let seconds = 5f64 * 2f64.powi(n - 1);
        if seconds >= MAX_BACKOFF.as_secs_f64() {
            MAX_BACKOFF
        } else {
            Duration::from_secs_f64(seconds)
        }
    }
}

/// Payload of [`site_command_types::KDS_LINE_STATE`]; `line_seqs` are `TrnNo` values.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct KdsLineStatePayload {
    /// RVC.
    pub rvc_no: i32,
    /// Check.
    pub check_no: i64,
    /// Lines.
    pub line_seqs: Vec<i64>,
    /// One of [`kds_line_actions::ALL`].
    pub action: String,
    /// Cashier.
    pub cashier: String,
}

impl KdsLineStatePayload {
    /// Most lines per command.
    pub const MAX_LINES: usize = 200;

    /// The validation problem, `None` when valid.
    pub fn problem(&self) -> Option<&'static str> {
        if self.rvc_no <= 0 {
            return Some("rvc_no must be positive");
        }
        if self.check_no <= 0 {
            return Some("check_no must be positive");
        }
        if !(1..=Self::MAX_LINES).contains(&self.line_seqs.len()) {
            return Some("line_seqs must hold 1..200 values");
        }
        if !kds_line_actions::ALL.contains(&self.action.as_str()) {
            return Some("action must be accept, unaccept, done or undone");
        }
        None
    }
}

impl WireJson for KdsLineStatePayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("check_no").int(self.check_no);
        w.key("line_seqs");
        json::write_ints(w, &self.line_seqs);
        w.key("action").string(&self.action);
        w.key("cashier").string(&self.cashier);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(KdsLineStatePayload {
            rvc_no: o.int("rvc_no")?,
            check_no: o.int("check_no")?,
            line_seqs: o.list("line_seqs", json::int_item)?,
            action: o.string("action")?,
            cashier: o.string("cashier")?,
        })
    }
}

/// Payload of [`site_command_types::RESERVATION_SAVE`]; times are the site's wall clock.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReservationSavePayload {
    /// RVC.
    pub rvc_no: i32,
    /// Guest.
    pub guest_info: String,
    /// Adults.
    pub num_adult: i32,
    /// Children.
    pub num_child: i32,
    /// From (site wall clock).
    pub from_date_time: DateTime,
    /// To (site wall clock).
    pub to_date_time: DateTime,
    /// Table.
    pub table_num: String,
    /// Phone.
    pub tel_num: String,
    /// Booker.
    pub booker: Option<String>,
    /// Company.
    pub company_name: Option<String>,
    /// VAT code.
    pub vat_code: Option<String>,
    /// Address.
    pub address: Option<String>,
}

impl ReservationSavePayload {
    /// The validation problem, `None` when valid.
    pub fn problem(&self) -> Option<&'static str> {
        if self.rvc_no <= 0 {
            return Some("rvc_no must be positive");
        }
        if self.guest_info.trim().is_empty() {
            return Some("guest_info is required");
        }
        if self.num_adult < 1 {
            return Some("num_adult must be >= 1");
        }
        if self.num_child < 0 {
            return Some("num_child must be >= 0");
        }
        if self.from_date_time >= self.to_date_time {
            return Some("from_date_time must be before to_date_time");
        }
        None
    }
}

impl WireJson for ReservationSavePayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("guest_info").string(&self.guest_info);
        w.key("num_adult").int(self.num_adult.into());
        w.key("num_child").int(self.num_child.into());
        w.key("from_date_time")
            .unescaped(&self.from_date_time.to_json_string());
        w.key("to_date_time")
            .unescaped(&self.to_date_time.to_json_string());
        w.key("table_num").string(&self.table_num);
        w.key("tel_num").string(&self.tel_num);
        json::opt_str(w, "booker", &self.booker);
        json::opt_str(w, "company_name", &self.company_name);
        json::opt_str(w, "vat_code", &self.vat_code);
        json::opt_str(w, "address", &self.address);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(ReservationSavePayload {
            rvc_no: o.int("rvc_no")?,
            guest_info: o.string("guest_info")?,
            num_adult: o.int("num_adult")?,
            num_child: o.int("num_child")?,
            from_date_time: json::date_time(&o, "from_date_time")?,
            to_date_time: json::date_time(&o, "to_date_time")?,
            table_num: o.string("table_num")?,
            tel_num: o.string("tel_num")?,
            booker: o.opt_string("booker")?,
            company_name: o.opt_string("company_name")?,
            vat_code: o.opt_string("vat_code")?,
            address: o.opt_string("address")?,
        })
    }
}

/// Result of an applied reservation save: the site identity (`nID`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReservationSavedResult {
    /// `nID`.
    pub id: i32,
}

impl WireJson for ReservationSavedResult {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("id").int(self.id.into());
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(ReservationSavedResult { id: o.int("id")? })
    }
}

/// Payload of [`site_command_types::RESERVATION_CANCEL`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReservationCancelPayload {
    /// RVC.
    pub rvc_no: i32,
    /// Reservation id.
    pub id: i32,
    /// Who cancelled.
    pub cancelled_by: String,
}

impl ReservationCancelPayload {
    /// The validation problem, `None` when valid.
    pub fn problem(&self) -> Option<&'static str> {
        if self.rvc_no <= 0 {
            return Some("rvc_no must be positive");
        }
        if self.id <= 0 {
            Some("id must be positive")
        } else {
            None
        }
    }
}

impl WireJson for ReservationCancelPayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("id").int(self.id.into());
        w.key("cancelled_by").string(&self.cancelled_by);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(ReservationCancelPayload {
            rvc_no: o.int("rvc_no")?,
            id: o.int("id")?,
            cancelled_by: o.string("cancelled_by")?,
        })
    }
}

/// Result of an applied KDS line state: how many lines changed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct KdsLineStateResult {
    /// Lines changed.
    pub lines: i32,
}

impl WireJson for KdsLineStateResult {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("lines").int(self.lines.into());
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(KdsLineStateResult {
            lines: o.int("lines")?,
        })
    }
}

/// Payload of [`site_command_types::SYNC_RESNAPSHOT`]: the streams to resend, parents
/// first, and why.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncResnapshotPayload {
    /// Streams.
    pub streams: Vec<String>,
    /// Why.
    pub reason: String,
}

impl SyncResnapshotPayload {
    /// Most streams per command.
    pub const MAX_STREAMS: usize = 16;
    /// Longest reason.
    pub const MAX_REASON: usize = 300;
    /// The one aggregate of every resnapshot command.
    pub const AGGREGATE_KEY: &'static str = "sync:resnapshot";

    /// The validation problem, `None` when valid.
    pub fn problem(&self) -> Option<&'static str> {
        if !(1..=Self::MAX_STREAMS).contains(&self.streams.len()) {
            return Some("streams must hold 1..16 names");
        }
        for s in &self.streams {
            if s.trim().is_empty() || utf16_len(s) > super::sync_streams::MAX_STREAM_LENGTH {
                return Some("each stream must be 1..64 characters");
            }
            if s.starts_with('!') {
                return Some("a set-aside stream cannot be resnapshotted");
            }
        }
        if self.reason.trim().is_empty() {
            return Some("reason is required");
        }
        if utf16_len(&self.reason) > Self::MAX_REASON {
            Some("reason must be at most 300 characters")
        } else {
            None
        }
    }
}

impl WireJson for SyncResnapshotPayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("streams");
        json::write_strings(w, &self.streams);
        w.key("reason").string(&self.reason);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(SyncResnapshotPayload {
            streams: o.list("streams", json::string_item)?,
            reason: o.string("reason")?,
        })
    }
}

/// Result of an applied resnapshot: what the agent scheduled.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncResnapshotResult {
    /// Streams the next pass resends; empty for a full resnapshot.
    pub streams: Vec<String>,
    /// An unknown stream turned it into a full resnapshot.
    pub full: bool,
    /// The recovery cooldown was skipped.
    pub cooldown_bypassed: bool,
    /// The same streams were already pending.
    pub already_pending: bool,
}

impl WireJson for SyncResnapshotResult {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("streams");
        json::write_strings(w, &self.streams);
        w.key("full").bool(self.full);
        w.key("cooldown_bypassed").bool(self.cooldown_bypassed);
        w.key("already_pending").bool(self.already_pending);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(SyncResnapshotResult {
            streams: o.list("streams", json::string_item)?,
            full: o.bool("full")?,
            cooldown_bypassed: o.bool("cooldown_bypassed")?,
            already_pending: o.bool("already_pending")?,
        })
    }
}
