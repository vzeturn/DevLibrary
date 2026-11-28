//! Per-RVC counters of a cloud-authoritative tenant (`Wire/TxnCounters.cs`, ADR 0161, RT-0848):
//! the `dbo.Para` rows `GetRVCSeqNumber` advances. The values are raw per-RVC counters (the
//! issued number is `RVC * 100,000,000 + counter` for checks, lines and closes; a payment number
//! is the raw counter).

use crate::contracts::json::WireJson;
use crate::contracts::wire::txn_commands::txn_command_types;
use crate::util::stj::{Obj, ReadError, Writer};

/// The counter allow-list (`TxnCounterNames`).
pub mod txn_counter_names {
    use super::txn_command_types;

    /// Last check number.
    pub const LAST_CHECK: &str = "LastCheck";
    /// Last line number.
    pub const LAST_TRN: &str = "LastTrn";
    /// Last payment number.
    pub const LAST_PAYMENT: &str = "LastPayment";
    /// Last close number.
    pub const CLOSE_NUMBER: &str = "CloseNumber";
    /// The allow-list, in the order every result lists them.
    pub const ALL: [&str; 4] = [LAST_CHECK, LAST_TRN, LAST_PAYMENT, CLOSE_NUMBER];
    /// Highest per-RVC counter value.
    pub const MAX_COUNTER: i64 = 99_999_999;

    /// True for a counter of the allow-list (ordinal).
    pub fn is_known(name: Option<&str>) -> bool {
        name.is_some_and(|n| ALL.contains(&n))
    }

    /// Queue aggregate key of the counters of one RVC, e.g. `txn.counters:4`.
    pub fn aggregate_key(rvc_no: i32) -> String {
        format!("{}counters:{rvc_no}", txn_command_types::PREFIX)
    }
}

/// One counter of a [`TxnCountersPayload`] (`TxnCounterFloor`): the store's counter is raised
/// to at least `floor`. A JSON null entry reads as the default entry (refused).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TxnCounterFloor {
    /// Counter name.
    pub name: String,
    /// Lowest value the store must hold.
    pub floor: i64,
}

impl WireJson for TxnCounterFloor {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("name").string(&self.name);
        w.key("floor").int(self.floor);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(TxnCounterFloor {
            name: o.string("name")?,
            floor: o.int("floor")?,
        })
    }
}

/// Payload of [`txn_command_types::COUNTERS`] (`TxnCountersPayload`): for each listed counter
/// of `rvc_no` the agent raises the stored value to the floor when it is lower and never lowers
/// it, all in one transaction. A floor of 0 is a pure read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TxnCountersPayload {
    /// The outlet.
    pub rvc_no: i32,
    /// The counters to raise.
    pub counters: Vec<TxnCounterFloor>,
    /// Who sent it.
    pub actor: String,
}

impl TxnCountersPayload {
    /// Shape problems both sides check before any SQL runs; `None` when well-formed
    /// (`Problem`, same messages and order).
    pub fn problem(&self) -> Option<String> {
        if !(1..=255).contains(&self.rvc_no) {
            return Some("RvcNo must be 1..255.".into());
        }
        if self.actor.trim().is_empty() {
            return Some("Actor is required.".into());
        }
        if self.counters.is_empty() {
            return Some("Counters must list at least one counter.".into());
        }
        let mut seen: Vec<&str> = Vec::new();
        for c in &self.counters {
            if !txn_counter_names::is_known(Some(&c.name)) {
                return Some("Counter is not allowed.".into());
            }
            if seen.contains(&c.name.as_str()) {
                return Some(format!("Counter {} is listed twice.", c.name));
            }
            seen.push(&c.name);
            if !(0..=txn_counter_names::MAX_COUNTER).contains(&c.floor) {
                return Some(format!(
                    "Floor of {} must be 0..{}.",
                    c.name,
                    txn_counter_names::MAX_COUNTER
                ));
            }
        }
        None
    }
}

impl WireJson for TxnCountersPayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(i64::from(self.rvc_no));
        w.key("counters").begin_array();
        for c in &self.counters {
            c.write_json(w);
        }
        w.end_array();
        w.key("actor").string(&self.actor);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        let counters = o.list("counters", |v, opts, path| match Obj::of(v, opts, path)? {
            None => Ok(TxnCounterFloor::default()),
            Some(c) => TxnCounterFloor::read_json(c),
        })?;
        Ok(TxnCountersPayload {
            rvc_no: o.int("rvc_no")?,
            counters,
            actor: o.string("actor")?,
        })
    }
}

/// One counter after a counters command (`TxnCounterState`): `before` and `stored` are the
/// `dbo.Para` value before and after (equal when nothing was raised), `in_use` the highest
/// per-RVC counter already present in the store's backing tables (`None` when none).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TxnCounterState {
    /// Counter name.
    pub name: String,
    /// Value before the command.
    pub before: i64,
    /// Value after the command.
    pub stored: i64,
    /// Highest counter the backing tables hold.
    pub in_use: Option<i64>,
}

impl WireJson for TxnCounterState {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("name").string(&self.name);
        w.key("before").int(self.before);
        w.key("stored").int(self.stored);
        w.opt_int_member("in_use", self.in_use);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(TxnCounterState {
            name: o.string("name")?,
            before: o.int("before")?,
            stored: o.int("stored")?,
            in_use: o.opt_int("in_use")?,
        })
    }
}

/// Result of an applied counters command (`TxnCountersResult`): every requested counter, in
/// [`txn_counter_names::ALL`] order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TxnCountersResult {
    /// The outlet.
    pub rvc_no: i32,
    /// The counters.
    pub counters: Vec<TxnCounterState>,
}

impl TxnCountersResult {
    /// The value the cloud must start above for `name`: `max(stored, in_use)`; `None` when
    /// absent (`HighWater`).
    pub fn high_water(&self, name: &str) -> Option<i64> {
        self.counters
            .iter()
            .find(|c| c.name == name)
            .map(|c| c.stored.max(c.in_use.unwrap_or(0)))
    }
}

impl WireJson for TxnCountersResult {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(i64::from(self.rvc_no));
        w.key("counters").begin_array();
        for c in &self.counters {
            c.write_json(w);
        }
        w.end_array();
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        let counters = o.list("counters", |v, opts, path| match Obj::of(v, opts, path)? {
            None => Ok(TxnCounterState::default()),
            Some(c) => TxnCounterState::read_json(c),
        })?;
        Ok(TxnCountersResult {
            rvc_no: o.int("rvc_no")?,
            counters,
        })
    }
}
