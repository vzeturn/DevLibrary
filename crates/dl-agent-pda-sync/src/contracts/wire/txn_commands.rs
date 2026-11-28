//! Cloud-authoritative transaction write-down (`Wire/TxnCommands.cs`, ADR 0159 on ADR 0148).
//! One verb carries the full state of one aggregate at one version; the agent replaces the
//! aggregate's rows table by table in one SQL transaction, so a replay or a newer snapshot
//! always lands on the same store state.

use dl_json::{Map, Value};

use crate::contracts::json::WireJson;
use crate::util::stj::{Obj, ReadError, Writer};

/// Command types of the transaction verbs (`TxnCommandTypes`).
pub mod txn_command_types {
    /// Full-state snapshot of one aggregate ([`TxnSnapshotPayload`](super::TxnSnapshotPayload)),
    /// answered with [`TxnSnapshotResult`](super::TxnSnapshotResult).
    pub const SNAPSHOT: &str = "txn.snapshot";
    /// Opens a shift (`TxnShiftPayload`); answered
    /// with `TxnShiftResult` (ADR 0160).
    pub const SHIFT_OPEN: &str = "txn.shift_open";
    /// Closes a shift (same state plus the closing fields).
    pub const SHIFT_CLOSE: &str = "txn.shift_close";
    /// Cash put into the drawer during a shift
    /// (`TxnCashPayload`).
    pub const CASH_IN: &str = "txn.cash_in";
    /// Cash taken out of the drawer during a shift.
    pub const CASH_OUT: &str = "txn.cash_out";
    /// Raises the store's per-RVC counters to the cloud's
    /// (`TxnCountersPayload`, ADR 0161).
    pub const COUNTERS: &str = "txn.counters";
    /// Posts a bill's room charge to the Front Office folio
    /// (`TxnRoomChargePayload`, ADR 0162).
    pub const ROOM_CHARGE: &str = "txn.room_charge";
    /// Reverses a bill's room charge on the folio
    /// (`TxnRoomChargeVoidPayload`).
    pub const ROOM_CHARGE_VOID: &str = "txn.room_charge_void";
    /// Command type prefix of every transaction verb.
    pub const PREFIX: &str = "txn.";
    /// The verbs that carry the full state of their aggregate (`Covering`).
    pub const COVERING: [&str; 3] = [SNAPSHOT, SHIFT_OPEN, SHIFT_CLOSE];

    /// True for a transaction verb (`IsTxn`, ordinal prefix).
    pub fn is_txn(type_: Option<&str>) -> bool {
        type_.is_some_and(|t| t.starts_with(PREFIX))
    }

    /// True for a full-state verb (`IsCovering`, ordinal).
    pub fn is_covering(type_: Option<&str>) -> bool {
        type_.is_some_and(|t| COVERING.contains(&t))
    }
}

/// The code allow-list of ADR 0159 §3 (`TxnScopes`).
pub mod txn_scopes {
    /// The bill scope.
    pub const BILL: &str = "bill";
    /// Column every row of a bill snapshot carries, equal to the scope key.
    pub const BILL_SCOPE_COLUMN: &str = "CheckNo";
    /// The six bill tables: open bill in `Tmp*`, closed bill in the other three.
    pub const BILL_TABLES: [&str; 6] = [
        "TmpInvoice",
        "TmpTrn",
        "TmpPayment",
        "Invoice",
        "Trn",
        "Payment",
    ];

    /// Scope column and tables of `scope`; `None` outside the allow-list (ordinal).
    pub fn find(scope: &str) -> Option<(&'static str, &'static [&'static str])> {
        (scope == BILL).then_some((BILL_SCOPE_COLUMN, &BILL_TABLES[..]))
    }

    /// Aggregate of one scope key, e.g. `bill:123456`.
    pub fn aggregate(scope: &str, scope_key: i64) -> String {
        format!("{scope}:{scope_key}")
    }

    /// Queue aggregate key (`sync.commands.aggregate_key`): `txn:` + the aggregate.
    pub fn aggregate_key(scope: &str, scope_key: i64) -> String {
        format!("txn:{}", aggregate(scope, scope_key))
    }

    /// The shift scope (ADR 0160); the key is the cloud's shift id.
    pub const SHIFT: &str = "shift";
    /// The shift's store key column (identity `CashierShift.ShiftNo`, plain
    /// `ShiftMoney.ShiftNo`).
    pub const SHIFT_SCOPE_COLUMN: &str = "ShiftNo";
    /// The tables of a shift aggregate.
    pub const SHIFT_TABLES: [&str; 2] = ["CashierShift", "ShiftMoney"];
    /// The vendor cash log a cash verb appends to (identity `TrnSeq`).
    pub const CASH_LOG_TABLE: &str = "DailyCashLog";
    /// The identity column of [`CASH_LOG_TABLE`].
    pub const CASH_LOG_KEY_COLUMN: &str = "TrnSeq";
}

/// Outcomes of an applied snapshot (`TxnOutcomes`).
pub mod txn_outcomes {
    /// The snapshot replaced the aggregate's rows.
    pub const APPLIED: &str = "applied";
    /// A version at or below the applied one changed nothing.
    pub const STALE: &str = "stale";
}

/// Rejection reasons of the transaction verbs, identical in the .NET and Rust agents
/// (`TxnReasons`).
pub mod txn_reasons {
    /// A row names a column the live table does not have.
    pub const COLUMN_UNKNOWN: &str = "txn_column_unknown";
    /// A value cannot be converted to its column's SQL type.
    pub const VALUE_INVALID: &str = "txn_value_invalid";
    /// A row's key collides with a row of another aggregate; nothing was written.
    pub const KEY_CONFLICT: &str = "txn_key_conflict";
    /// A cash verb names a shift the store has no row for yet.
    pub const SHIFT_UNKNOWN: &str = "txn_shift_unknown";
    /// The store keeps a counter in a per-RVC object of its own (`dbo.LastCheck<n>`), not
    /// `dbo.Para`; nothing was written.
    pub const COUNTER_OBJECT_UNSUPPORTED: &str = "txn_counter_object_unsupported";
    /// No in-house, postable folio matches the room charge.
    pub const FOLIO_UNAVAILABLE: &str = "txn_folio_unavailable";
    /// The room has several in-house folios and none was named.
    pub const FOLIO_AMBIGUOUS: &str = "txn_folio_ambiguous";
    /// The store maps the room charge tender to no folio transaction code (`TrnLinkByRVC`).
    pub const FOLIO_LINK_UNKNOWN: &str = "txn_folio_link_unknown";
    /// The Front Office database named by `Para InterfaceDB` is not a valid name or does not
    /// exist.
    pub const FOLIO_DATABASE_INVALID: &str = "txn_folio_database_invalid";
}

/// Rows of one vendor table in a snapshot (`TxnTableRows`). Each row maps a live column
/// name (verbatim) to its JSON value. `rows` is `None` for a JSON null list and a row is
/// `None` for a JSON null row, both refused by [`TxnSnapshotPayload::problem`]. A JSON
/// null table entry reads as an entry with an empty table name (refused as not allowed).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TxnTableRows {
    /// Vendor table name.
    pub table: String,
    /// The rows; `None` when the list is JSON null or missing.
    pub rows: Option<Vec<Option<Map>>>,
}

impl WireJson for TxnTableRows {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("table").string(&self.table);
        w.key("rows");
        match &self.rows {
            None => {
                w.null();
            }
            Some(rows) => {
                w.begin_array();
                for row in rows {
                    match row {
                        None => {
                            w.null();
                        }
                        Some(map) => {
                            w.begin_object();
                            for (k, v) in map.iter() {
                                w.key(k).value(v);
                            }
                            w.end_object();
                        }
                    }
                }
                w.end_array();
            }
        }
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        let rows = o.opt_list("rows", |v, _, path| match v {
            Value::Null => Ok(None),
            Value::Object(m) => Ok(Some(m.clone())),
            other => Err(ReadError::new(
                path,
                format!(
                    "The JSON value could not be converted (found {}).",
                    other.type_name()
                ),
            )),
        })?;
        Ok(TxnTableRows {
            table: o.string("table")?,
            rows,
        })
    }
}

/// Payload of [`txn_command_types::SNAPSHOT`] (`TxnSnapshotPayload`, ADR 0159 §1-§3).
/// `version` is the queue's `aggregate_seq`; the agent never applies a version at or
/// below the one recorded in `dbo.Pda_TxnApplied`. `tables` lists every table of the
/// scope exactly once (an empty row list deletes the aggregate's rows there).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TxnSnapshotPayload {
    /// Scope (`bill`).
    pub scope: String,
    /// Scope key (the `CheckNo` of a bill).
    pub scope_key: i64,
    /// Aggregate version.
    pub version: i64,
    /// The rows of every scope table.
    pub tables: Vec<TxnTableRows>,
    /// Who changed the aggregate.
    pub actor: String,
}

impl TxnSnapshotPayload {
    /// Most rows one table of one snapshot may carry.
    pub const MAX_ROWS_PER_TABLE: usize = 500;

    /// The aggregate, e.g. `bill:123456`.
    pub fn aggregate(&self) -> String {
        txn_scopes::aggregate(&self.scope, self.scope_key)
    }

    /// Shape problems both sides check before any SQL runs; `None` when well-formed
    /// (`Problem`, same messages).
    pub fn problem(&self) -> Option<String> {
        let Some((scope_column, scope_tables)) = txn_scopes::find(&self.scope) else {
            return Some("Scope is not allowed.".into());
        };
        if self.scope_key <= 0 {
            return Some("ScopeKey must be positive.".into());
        }
        if self.version <= 0 {
            return Some("Version must be positive.".into());
        }
        if self.actor.trim().is_empty() {
            return Some("Actor is required.".into());
        }
        if self.tables.len() != scope_tables.len() {
            return Some("Tables must list every table of the scope exactly once.".into());
        }
        let mut seen: Vec<&str> = Vec::new();
        for t in &self.tables {
            if !scope_tables.contains(&t.table.as_str()) {
                return Some("Table is not allowed.".into());
            }
            if seen.contains(&t.table.as_str()) {
                return Some(format!("Table {} is listed twice.", t.table));
            }
            seen.push(&t.table);
            let Some(rows) = &t.rows else {
                return Some(format!("Rows of {} are required.", t.table));
            };
            if rows.len() > Self::MAX_ROWS_PER_TABLE {
                return Some(format!("Too many rows for {}.", t.table));
            }
            for row in rows {
                let key = row
                    .as_ref()
                    .and_then(|r| r.get(scope_column))
                    .and_then(json_int64);
                if key != Some(self.scope_key) {
                    return Some(format!(
                        "Every row of {} must carry {scope_column} = ScopeKey.",
                        t.table
                    ));
                }
            }
        }
        None
    }

    /// The rows of `table` (first entry with that name, ordinal).
    pub fn rows_of(&self, table: &str) -> &[Option<Map>] {
        self.tables
            .iter()
            .find(|t| t.table == table)
            .and_then(|t| t.rows.as_deref())
            .unwrap_or(&[])
    }
}

/// `JsonElement.TryGetInt64` of a JSON number: the integer lexeme only (no fraction or
/// exponent), in `i64` range; `None` for any other token.
pub fn json_int64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => match n.as_raw_str() {
            Some(raw) => raw.parse::<i64>().ok(),
            None => n.as_i64(),
        },
        _ => None,
    }
}

impl WireJson for TxnSnapshotPayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("scope").string(&self.scope);
        w.key("scope_key").int(self.scope_key);
        w.key("version").int(self.version);
        w.key("tables").begin_array();
        for t in &self.tables {
            t.write_json(w);
        }
        w.end_array();
        w.key("actor").string(&self.actor);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        let tables = o.list("tables", |v, opts, path| match Obj::of(v, opts, path)? {
            None => Ok(TxnTableRows::default()),
            Some(t) => TxnTableRows::read_json(t),
        })?;
        Ok(TxnSnapshotPayload {
            scope: o.string("scope")?,
            scope_key: o.int("scope_key")?,
            version: o.int("version")?,
            tables,
            actor: o.string("actor")?,
        })
    }
}

/// Result of an applied snapshot (`TxnSnapshotResult`, ADR 0159 §4). `hash` is SHA-256
/// (lower hex) over the UTF-8 lines `{table}|{rowHash}\n` sorted ordinally; `row_counts`
/// maps each scope table, in scope order, to its row count after the command.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TxnSnapshotResult {
    /// The aggregate, e.g. `bill:123456`.
    pub aggregate: String,
    /// Version of the command.
    pub version: i64,
    /// [`txn_outcomes::APPLIED`] or [`txn_outcomes::STALE`].
    pub outcome: String,
    /// Version recorded in `dbo.Pda_TxnApplied` after the command.
    pub applied_version: i64,
    /// Aggregate hash of the store rows re-read in the same transaction.
    pub hash: String,
    /// Row count per scope table.
    pub row_counts: Vec<(String, i32)>,
}

impl WireJson for TxnSnapshotResult {
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
        Ok(TxnSnapshotResult {
            aggregate: o.string("aggregate")?,
            version: o.int("version")?,
            outcome: o.string("outcome")?,
            applied_version: o.int("applied_version")?,
            hash: o.string("hash")?,
            row_counts,
        })
    }
}
