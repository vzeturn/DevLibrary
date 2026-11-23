//! SQL Server access: the queries of `collectsource*.go`, `sqlsource.go`,
//! `driftcheck.go` and `installsql*.go`, over dl-tds.
//!
//! One connection per source, opened lazily and reopened after a broken session, down the
//! transport ladder when the server resets the handshake (see [`conn`]).
//! Parameters are sent through `sp_executesql` with the same names as the Go agent.

use std::sync::Arc;
use std::time::Duration;

use dl_agent::sql::SqlPool;
use dl_json::Value;
use dl_tds::{Client, Param, Row};
use dl_time::{DateTime, OffsetDateTime};

use crate::fo::{FoDetailRow, FoHeaderRow, FoLineRow, FoRows};
use crate::install::{INSTALL_TIMEOUT, install_writeback_into, is_default_writeback_sp};
use crate::log::V;
use crate::pending::{
    MODE_LEGACY, MODE_NEW, MODE_SWEEP, PendingCall, PendingPlanner, PendingQuerier, PendingRow,
};
use crate::pos::{PosRawCheck, PosRawHeader, PosRawLine};
use crate::pos_names;
use crate::profile::PosItemName;

mod conn;
mod probe;
use conn::Conn;
pub use conn::StepDown;
pub use probe::{BillMark, WindowBill, WritebackProbe};

/// SQL Server's "Could not find stored procedure".
const MISSING_PROCEDURE: i32 = 2812;

/// One pending invoice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectKey {
    /// "FO" | "POS".
    pub source: String,
    /// FO BillID or POS check number.
    pub id: String,
}

impl CollectKey {
    /// `"<Source>:<ID>"`.
    pub fn source_ref(&self) -> String {
        format!("{}:{}", self.source, self.id)
    }
}

/// `sp_ProbeFOSchema`'s row.
#[derive(Debug, Clone, Default, PartialEq)]
#[allow(missing_docs)] // fields mirror the Go struct one for one; see its doc
pub struct FoSchemaProbe {
    pub has_transaction_group: bool,
    pub has_folio_balance_code: bool,
    pub has_is_hk_post: bool,
    pub has_tax_def: bool,
    pub has_tax_def_rate: bool,
    pub distinct_folio_balance_codes: String,
    pub group1_count: i64,
}

/// A row of `sp_ListTrnTypes`.
#[derive(Debug, Clone, PartialEq)]
pub struct TrnTypeInfo {
    /// The transaction type code.
    pub trn_type: String,
    /// Its description in SMILE.
    pub description: String,
}

/// The issued values to write back onto one source bill.
#[derive(Debug, Clone, Default, PartialEq)]
#[allow(missing_docs)] // fields mirror the Go struct one for one; see its doc
pub struct WritebackApply {
    pub bill_id: String,
    pub bill_type: String,
    pub invoice_number: String,
    pub ky_hieu: String,
    pub ma_cqt: String,
    pub issued_at: Option<OffsetDateTime>,
}

/// Stored-procedure names (`AgentConfig` fields).
#[derive(Debug, Clone, PartialEq)]
#[allow(missing_docs)] // fields mirror the Go struct one for one; see its doc
pub struct SpNames {
    pub select_fo: String,
    pub fetch_fo: String,
    pub select_pos: String,
    pub fetch_pos: String,
    pub mark_collected: String,
    pub list_trn_types: String,
    pub probe_fo_schema: String,
    pub writeback: String,
}

/// T-SQL cannot bind object names, so every configured SP name must look like
/// `name` or `schema.name` (`^(?:\w+\.)?\w+$`, `\w` = ASCII word characters in RE2).
pub fn validate_sp_name(name: &str) -> Result<(), String> {
    let word = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    let ok = match name.split_once('.') {
        Some((schema, rest)) => word(schema) && word(rest),
        None => word(name),
    };
    if ok {
        Ok(())
    } else {
        Err(format!("agent: invalid stored procedure name {name:?}"))
    }
}

/// The SMILE databases.
pub struct Sql {
    fo: Conn,
    pos: Option<Conn>,
    sp: SpNames,
    pos_item_name: PosItemName,
    pending: PendingPlanner,
}

/// `select_pending`'s result.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PendingKeys {
    /// The keys to read this cycle.
    pub keys: Vec<CollectKey>,
    /// Edit detection (RT-0784 mode 2 or 3) failed; `keys` are still valid.
    pub edit_error: Option<String>,
}

struct SelectQuerier<'a> {
    sql: &'a Sql,
    timeout: Duration,
}

impl PendingQuerier for SelectQuerier<'_> {
    fn supports_split(&self, source: &str) -> Result<bool, String> {
        let sp = self.sql.select_sp(source);
        let rs = self
            .sql
            .conn(source)
            .with(self.timeout, |c| {
                c.query(
                    "SELECT COUNT(*) FROM sys.parameters WHERE object_id = OBJECT_ID(@sp) AND name = '@Mode'",
                    &[Param::new("sp", sp)],
                )
            })
            .map_err(|e| err(&format!("agent: probe select sp ({source})"), e))?;
        let n: i32 = rs
            .rows()
            .first()
            .map(|r| r.get(0))
            .transpose()
            .map_err(|e| err("agent: scan select sp probe", e))?
            .unwrap_or(0);
        Ok(n > 0)
    }

    fn query(&self, source: &str, call: &PendingCall) -> Result<Vec<PendingRow>, String> {
        let sp = self.sql.select_sp(source);
        let mut q = format!("EXEC {sp} @Limit = @Limit, @CollectFrom = @CollectFrom");
        let mut params = vec![
            Param::new("Limit", call.limit),
            Param::new("CollectFrom", call.collect_from),
        ];
        if call.mode != MODE_LEGACY {
            q.push_str(", @Mode = @Mode, @ChangedFrom = @ChangedFrom, @After = @After");
            params.push(Param::new("Mode", call.mode));
            params.push(Param::new("ChangedFrom", call.changed_from));
            params.push(Param::new("After", call.after.as_deref()));
        }
        let rs = self
            .sql
            .conn(source)
            .with(self.timeout, |c| c.query(&q, &params))
            .map_err(|e| {
                err(
                    &format!(
                        "agent: select pending collect ({source}, mode {})",
                        call.mode
                    ),
                    e,
                )
            })?;
        rs.rows()
            .iter()
            .map(|r| {
                Ok(PendingRow {
                    id: r.get::<String>(0)?,
                    changed: call.mode == MODE_SWEEP && r.get::<bool>(1)?,
                })
            })
            .collect::<Result<_, dl_tds::Error>>()
            .map_err(|e| err("agent: scan collect key", e))
    }
}

fn collect_keys(source: &str, ids: Vec<String>) -> Vec<CollectKey> {
    ids.into_iter()
        .map(|id| CollectKey {
            source: source.to_owned(),
            id,
        })
        .collect()
}

fn err(ctx: &str, e: dl_tds::Error) -> String {
    format!("{ctx}: {e}")
}

fn opt_time(r: &Row, i: usize) -> Result<Option<DateTime>, dl_tds::Error> {
    r.get::<Option<DateTime>>(i)
}

impl Sql {
    /// `fo_dsn` for SMILE_FO; `pos_dsn` for SMILE_POS, or empty to use the FO database
    /// for both (single-database installs).
    pub fn new(fo_dsn: &str, pos_dsn: &str, sp: SpNames) -> Sql {
        Sql {
            fo: Conn::sealed(fo_dsn, "SMILE_FO"),
            pos: (!pos_dsn.is_empty()).then(|| Conn::sealed(pos_dsn, "SMILE_POS")),
            sp,
            pos_item_name: PosItemName::default(),
            pending: PendingPlanner::default(),
        }
    }

    /// Through dl-agent's `sql.json` pools: `fo` for SMILE_FO, `pos` for SMILE_POS (`None`:
    /// the FO database holds both).
    pub fn site(fo: (&str, Arc<SqlPool>), pos: Option<(&str, Arc<SqlPool>)>, sp: SpNames) -> Sql {
        Sql {
            fo: Conn::site(fo.0, fo.1),
            pos: pos.map(|(name, pool)| Conn::site(name, pool)),
            sp,
            pos_item_name: PosItemName::default(),
            pending: PendingPlanner::default(),
        }
    }

    /// Transport step downs since the last call, FO first.
    pub fn take_step_downs(&self) -> Vec<StepDown> {
        let mut v = self.fo.take_step_downs();
        if let Some(p) = &self.pos {
            v.extend(p.take_step_downs());
        }
        v
    }

    /// How each source reaches SQL Server: `{"FO": {...}, "POS": {...}}` with database,
    /// source (`config.enc` | `sql.json`), auth (`windows` | `sql`), sealed transport and the
    /// rung in use (`null` before the first connection).
    pub fn describe(&self) -> Value {
        let mut m = dl_json::Map::new();
        m.insert("FO", self.fo.describe());
        m.insert("POS", self.conn("POS").describe());
        Value::Object(m)
    }

    /// The database name of `source` (`FO` | `POS`).
    pub fn database(&self, source: &str) -> &str {
        self.conn(source).database()
    }

    /// The current POS line-name source (collect profile).
    pub fn pos_item_name(&self) -> PosItemName {
        self.pos_item_name
    }

    /// Set the POS line-name source (from the hub's collect profile).
    pub fn set_pos_item_name(&mut self, v: PosItemName) {
        self.pos_item_name = v;
    }

    /// `ItemDef.IName1` per trimmed ItemCode for one check (see [`pos_names`]).
    fn lookup_pos_item_names(
        &self,
        check_no: &str,
        timeout: Duration,
    ) -> Result<std::collections::HashMap<String, String>, String> {
        let Some(n) = pos_names::check_no_param(check_no) else {
            return Ok(Default::default());
        };
        let rs = self
            .conn("POS")
            .with(timeout, |c| {
                c.query(pos_names::ITEM_NAMES_QUERY, &[Param::new("CheckNo", n)])
            })
            .map_err(|e| {
                err(
                    &format!("agent: look up POS item names (check {check_no})"),
                    e,
                )
            })?;
        let mut rows = Vec::new();
        for set in rs.sets() {
            for r in set.rows() {
                let row = (|| Ok::<_, dl_tds::Error>((r.get::<String>(0)?, r.get::<String>(1)?)))()
                    .map_err(|e| {
                        err(&format!("agent: scan POS item name (check {check_no})"), e)
                    })?;
                rows.push(row);
            }
        }
        Ok(pos_names::collect_names(rows))
    }

    fn conn(&self, source: &str) -> &Conn {
        match (source, &self.pos) {
            ("POS", Some(p)) => p,
            _ => &self.fo,
        }
    }

    /// Pings FO, and POS when it has its own database.
    pub fn ping(&self, timeout: Duration) -> Result<(), String> {
        self.fo
            .with(timeout, Client::ping)
            .map_err(|e| err("agent: ping FO source", e))?;
        if let Some(p) = &self.pos {
            p.with(timeout, Client::ping)
                .map_err(|e| err("agent: ping POS source", e))?;
        }
        Ok(())
    }

    /// `SelectPendingCollect`: never-sent keys every call, edited ones on the
    /// RT-0784 cadence (see [`crate::pending`]). `Err` only when the new-bill select
    /// failed; an edit-detection failure comes back in `PendingKeys::edit_error`.
    pub fn select_pending(
        &self,
        source: &str,
        limit: i64,
        from: Option<OffsetDateTime>,
        timeout: Duration,
    ) -> Result<PendingKeys, String> {
        validate_sp_name(self.select_sp(source))?;
        let q = SelectQuerier { sql: self, timeout };
        let sel = self.pending.select(&q, source, limit, from)?;
        Ok(PendingKeys {
            keys: collect_keys(source, sel.ids),
            edit_error: sel.edit_error,
        })
    }

    /// The never-sent keys only (the legacy combined call on an old SP), without
    /// touching the collector's edit-detection schedule: for the self-test.
    pub fn peek_pending(
        &self,
        source: &str,
        limit: i64,
        from: Option<OffsetDateTime>,
        timeout: Duration,
    ) -> Result<Vec<CollectKey>, String> {
        validate_sp_name(self.select_sp(source))?;
        let q = SelectQuerier { sql: self, timeout };
        let mode = if q.supports_split(source)? {
            MODE_NEW
        } else {
            MODE_LEGACY
        };
        let call = PendingCall {
            mode,
            limit,
            collect_from: from,
            changed_from: None,
            after: None,
        };
        let rows = q.query(source, &call)?;
        Ok(collect_keys(
            source,
            rows.into_iter().map(|r| r.id).collect(),
        ))
    }

    /// Replaces the planner clock (tests pin the Vietnam time of day).
    pub fn with_pending_clock(mut self, now: Box<dyn Fn() -> OffsetDateTime + Send + Sync>) -> Sql {
        self.pending = PendingPlanner::new(now);
        self
    }

    fn select_sp(&self, source: &str) -> &str {
        if source == "POS" {
            &self.sp.select_pos
        } else {
            &self.sp.select_fo
        }
    }

    /// `fetchPOSRaw` (sp_FetchPOS v2: header + Trn lines).
    pub fn fetch_pos_raw(&self, id: &str, timeout: Duration) -> Result<PosRawCheck, String> {
        let sp = &self.sp.fetch_pos;
        validate_sp_name(sp)?;
        let key = CollectKey {
            source: "POS".into(),
            id: id.to_owned(),
        };
        let q = format!("EXEC {sp} @ID = @ID");
        let rs = self
            .conn("POS")
            .with(timeout, |c| c.query(&q, &[Param::new("ID", id)]))
            .map_err(|e| err(&format!("agent: fetch pos raw ({})", key.source_ref()), e))?;
        let sets = rs.sets();
        let mut raw = PosRawCheck {
            check_no: id.to_owned(),
            ..Default::default()
        };
        let scan_h = |r: &Row| -> Result<PosRawHeader, dl_tds::Error> {
            Ok(PosRawHeader {
                sub_total: r.get(0)?,
                discount: r.get(1)?,
                sur_charge: r.get(2)?,
                before_tax: r.get(3)?,
                tax_total: r.get(4)?,
                stax_total: r.get(5)?,
                total_base: r.get(6)?,
                inv_status: r.get(7)?,
                tax_omit: r.get(8)?,
                rvc: r.get(9)?,
                folio_num: r.get(10)?,
                inv_date: Some(r.get::<DateTime>(11)?),
                ex_rate: r.get(12)?,
                client_vat: r.get(13)?,
                client_name: r.get(14)?,
                company: r.get(15)?,
                address: r.get(16)?,
                inv_content: r.get(17)?,
            })
        };
        if let Some(set) = sets.first() {
            for r in set.rows() {
                raw.header = scan_h(r).map_err(|e| {
                    err(&format!("agent: scan pos header ({})", key.source_ref()), e)
                })?;
            }
        }
        let Some(lines) = sets.get(1) else {
            return Err(format!(
                "agent: fetch pos raw ({}): missing lines result set",
                key.source_ref()
            ));
        };
        for r in lines.rows() {
            let l = (|| -> Result<PosRawLine, dl_tds::Error> {
                Ok(PosRawLine {
                    trn_seq: r.get(0)?,
                    trn_code: r.get(1)?,
                    item_code: r.get(2)?,
                    trn_desc: r.get(3)?,
                    unit_name: r.get(4)?,
                    qty: r.get(5)?,
                    unit_price: r.get(6)?,
                    base_sub: r.get(7)?,
                    base_src: r.get(8)?,
                    base_tax: r.get(9)?,
                    tax_no: r.get(10)?,
                    base_stax: r.get(11)?,
                    stax_no: r.get(12)?,
                    rvc: r.get(13)?,
                })
            })()
            .map_err(|e| err(&format!("agent: scan pos line ({})", key.source_ref()), e))?;
            raw.lines.push(l);
        }
        if self.pos_item_name == PosItemName::IName1 {
            let names = self
                .lookup_pos_item_names(id, timeout)
                .map_err(|e| format!("agent: fetch pos raw ({}): {e}", key.source_ref()))?;
            pos_names::apply_names(&mut raw.lines, &names);
        }
        Ok(raw)
    }

    /// The three result sets of sp_FetchFO.
    pub fn fetch_fo_rows(&self, id: &str, timeout: Duration) -> Result<FoRows, String> {
        let sp = &self.sp.fetch_fo;
        validate_sp_name(sp)?;
        let src = format!("FO:{id}");
        let q = format!("EXEC {sp} @ID = @ID");
        let rs = self
            .conn("FO")
            .with(timeout, |c| c.query(&q, &[Param::new("ID", id)]))
            .map_err(|e| err(&format!("agent: fetch collected ({src})"), e))?;
        let sets = rs.sets();
        let mut rows = FoRows::default();
        if let Some(set) = sets.first() {
            for r in set.rows() {
                let h = (|| -> Result<FoHeaderRow, dl_tds::Error> {
                    Ok(FoHeaderRow {
                        seller_tax_code: r.get(0)?,
                        company_name: r.get(1)?,
                        customer_tax: r.get(2)?,
                        customer_addr: r.get(3)?,
                        customer_email: r.get(4)?,
                        customer_tel: r.get(5)?,
                        invoice_type: r.get(6)?,
                        code_mode: r.get(7)?,
                        ky_hieu: r.get(8)?,
                        mau_so: r.get(9)?,
                        inv_date: opt_time(r, 10)?,
                        subtotal: r.get(11)?,
                        service_charge: r.get(12)?,
                        tax_amount: r.get(13)?,
                        total_amount: r.get(14)?,
                        guest_name: r.get(15)?,
                        address2: r.get(16)?,
                        inv_content: r.get(17)?,
                        confirm_no: r.get(18)?,
                        arrival: r.get(19)?,
                        departure: r.get(20)?,
                        room_no: r.get(21)?,
                    })
                })()
                .map_err(|e| err(&format!("agent: scan collected header ({src})"), e))?;
                rows.header.push(h);
            }
        }
        let Some(set) = sets.get(1) else {
            return Err(format!(
                "agent: fetch collected ({src}): missing merged-lines result set"
            ));
        };
        for r in set.rows() {
            let l = (|| -> Result<FoLineRow, dl_tds::Error> {
                Ok(FoLineRow {
                    line_key: r.get(0)?,
                    source_ref: r.get(1)?,
                    trn_code: r.get(2)?,
                    trn_sub_code: r.get(3)?,
                    desc: r.get(4)?,
                    unit: r.get(5)?,
                    qty: r.get(6)?,
                    unit_price: r.get(7)?,
                    amount: r.get(8)?,
                    tax_code: r.get(9)?,
                    tax_amount: r.get(10)?,
                    stax_code: r.get(11)?,
                    stax_amount: r.get(12)?,
                    service_charge_code: r.get(13)?,
                    service_charge_amount: r.get(14)?,
                    rvc: r.get(15)?,
                    outlet: r.get(16)?,
                    folio_link: r.get(17)?,
                    origin_folio_number: r.get(18)?,
                    trn_type: r.get(19)?,
                    raw_discount: r.get(20)?,
                    trn_group: r.get(21)?,
                    is_hk_post: r.get(22)?,
                    tax_rate: r.get(23)?,
                    origin_room: r.get(24)?,
                    room_code: r.get(25)?,
                    transaction_date: r.get(26)?,
                    post_to_acc: r.get(27)?,
                    number_cover: r.get(28)?,
                    xref: r.get(29)?,
                    origin_currency: r.get(30)?,
                    origin_amount: r.get(31)?,
                    rpt_ex_rate: r.get(32)?,
                })
            })()
            .map_err(|e| err(&format!("agent: scan merged line ({src})"), e))?;
            rows.lines.push(l);
        }
        let Some(set) = sets.get(2) else {
            return Err(format!(
                "agent: fetch collected ({src}): missing detail-items result set"
            ));
        };
        for r in set.rows() {
            let d = (|| -> Result<FoDetailRow, dl_tds::Error> {
                Ok(FoDetailRow {
                    parent_line_key: r.get(0)?,
                    line_key: r.get(1)?,
                    icode: r.get(2)?,
                    name: r.get(3)?,
                    group: r.get(4)?,
                    unit: r.get(5)?,
                    qty: r.get(6)?,
                    unit_price: r.get(7)?,
                    amount: r.get(8)?,
                    tax_code: r.get(9)?,
                    tax_amount: r.get(10)?,
                    stax_code: r.get(11)?,
                    stax_amount: r.get(12)?,
                    service_charge_code: r.get(13)?,
                    service_charge_amount: r.get(14)?,
                })
            })()
            .map_err(|e| err(&format!("agent: scan detail item ({src})"), e))?;
            rows.detail.push(d);
        }
        Ok(rows)
    }

    /// `MarkCollected`.
    pub fn mark_collected(
        &self,
        key: &CollectKey,
        ok: bool,
        msg: &str,
        timeout: Duration,
    ) -> Result<(), String> {
        let sp = &self.sp.mark_collected;
        validate_sp_name(sp)?;
        let q = format!("EXEC {sp} @Source = @Source, @ID = @ID, @Ok = @Ok, @Message = @Message");
        self.conn(&key.source)
            .with(timeout, |c| {
                c.execute(
                    &q,
                    &[
                        Param::new("Source", key.source.as_str()),
                        Param::new("ID", key.id.as_str()),
                        Param::new("Ok", ok),
                        Param::new("Message", msg),
                    ],
                )
            })
            .map(|_| ())
            .map_err(|e| err(&format!("agent: mark collected ({})", key.source_ref()), e))
    }

    /// `ApplyWriteback`. A site enrolled before the procedure was embedded lacks it: on
    /// SQL Server error 2812 the agent installs its own procedure and retries once (never
    /// a configured, DBA-owned name).
    pub fn apply_writeback(&self, a: &WritebackApply, timeout: Duration) -> Result<(), String> {
        let sp = &self.sp.writeback;
        validate_sp_name(sp)?;
        let ctx = || format!("agent: apply writeback for {} {}", a.bill_type, a.bill_id);
        match self.exec_writeback(a, timeout) {
            Err(e) if e.number() == Some(MISSING_PROCEDURE) && is_default_writeback_sp(sp) => {
                crate::log::warn(
                    "agent: write-back procedure missing; installing it",
                    &[("procedure", V::S(sp)), ("bill_type", V::S(&a.bill_type))],
                );
                self.install_writeback(&a.bill_type).map_err(|ie| {
                    format!("{}: procedure missing and install failed: {e}; {ie}", ctx())
                })?;
                crate::log::info(
                    "agent: write-back procedure installed",
                    &[("procedure", V::S(sp)), ("bill_type", V::S(&a.bill_type))],
                );
                self.exec_writeback(a, timeout).map_err(|e| err(&ctx(), e))
            }
            r => r.map_err(|e| err(&ctx(), e)),
        }
    }

    fn exec_writeback(&self, a: &WritebackApply, timeout: Duration) -> Result<(), dl_tds::Error> {
        let q = format!(
            "EXEC {} @BillID = @p1, @BillType = @p2, @InvoiceNumber = @p3, @KyHieu = @p4, @MaCQT = @p5, @IssuedAt = @p6",
            self.sp.writeback
        );
        self.conn(&a.bill_type)
            .with(timeout, |c| {
                c.execute(
                    &q,
                    &[
                        Param::new("p1", a.bill_id.as_str()),
                        Param::new("p2", a.bill_type.as_str()),
                        Param::new("p3", a.invoice_number.as_str()),
                        Param::new("p4", a.ky_hieu.as_str()),
                        Param::new("p5", a.ma_cqt.as_str()),
                        Param::new("p6", a.issued_at),
                    ],
                )
            })
            .map(|_| ())
    }

    fn install_writeback(&self, source: &str) -> Result<(), String> {
        let mut installed = Ok(());
        self.conn(source)
            .with(INSTALL_TIMEOUT, |c| {
                installed = install_writeback_into(c);
                Ok(())
            })
            .map_err(|e| {
                err(
                    &format!("agent: install write-back procedure ({source})"),
                    e,
                )
            })?;
        installed
    }

    /// Installs the agent's own write-back procedure into the database of `source` when it
    /// is missing, so a site enrolled before it was embedded heals before its first
    /// write-back (and its self-test passes). A DBA-owned name is never created. Returns
    /// whether it was installed.
    pub fn ensure_writeback(&self, source: &str, timeout: Duration) -> Result<bool, String> {
        if !is_default_writeback_sp(&self.sp.writeback) {
            return Ok(false);
        }
        if self
            .missing_procs(source, &[self.sp.writeback.as_str()], timeout)?
            .is_empty()
        {
            return Ok(false);
        }
        self.install_writeback(source)?;
        Ok(true)
    }

    /// `ListTrnTypes` (FO). An empty SP name disables it.
    pub fn list_trn_types(&self, timeout: Duration) -> Result<Vec<TrnTypeInfo>, String> {
        let sp = &self.sp.list_trn_types;
        if sp.is_empty() {
            return Ok(Vec::new());
        }
        validate_sp_name(sp)?;
        let q = format!("EXEC {sp}");
        let rs = self
            .fo
            .with(timeout, |c| c.query(&q, &[]))
            .map_err(|e| err("agent: list trn-types", e))?;
        rs.rows()
            .iter()
            .map(|r| {
                Ok(TrnTypeInfo {
                    trn_type: r.get(0).map_err(|e| err("agent: scan trn-type", e))?,
                    description: r.get(1).map_err(|e| err("agent: scan trn-type", e))?,
                })
            })
            .collect()
    }

    /// The stored procedures among `names` that the database of `source` lacks (a
    /// read-only `OBJECT_ID` lookup; the self-test's schema probe).
    pub fn missing_procs(
        &self,
        source: &str,
        names: &[&str],
        timeout: Duration,
    ) -> Result<Vec<String>, String> {
        let mut missing = Vec::new();
        for name in names.iter().filter(|n| !n.is_empty()) {
            let rs = self
                .conn(source)
                .with(timeout, |c| {
                    c.query(
                        "SELECT CAST(CASE WHEN OBJECT_ID(@Name, 'P') IS NULL THEN 0 ELSE 1 END AS bit)",
                        &[Param::new("Name", *name)],
                    )
                })
                .map_err(|e| err(&format!("agent: look up {name} ({source})"), e))?;
            let present = rs.rows().first().map(|r| r.get::<bool>(0)).transpose();
            match present.map_err(|e| err("agent: scan procedure lookup", e))? {
                Some(true) => {}
                _ => missing.push((*name).to_owned()),
            }
        }
        Ok(missing)
    }

    /// `ProbeFOSchema`. An empty SP name gives `group1_count = -1`.
    pub fn probe_fo_schema(&self, timeout: Duration) -> Result<FoSchemaProbe, String> {
        let sp = &self.sp.probe_fo_schema;
        if sp.is_empty() {
            return Ok(FoSchemaProbe {
                group1_count: -1,
                ..Default::default()
            });
        }
        validate_sp_name(sp)?;
        let q = format!("EXEC {sp}");
        let rs = self
            .fo
            .with(timeout, |c| c.query(&q, &[]))
            .map_err(|e| err("agent: probe fo schema", e))?;
        let Some(r) = rs.rows().first() else {
            return Err("agent: probe fo schema: sql: no rows in result set".into());
        };
        (|| -> Result<FoSchemaProbe, dl_tds::Error> {
            Ok(FoSchemaProbe {
                has_transaction_group: r.get(0)?,
                has_folio_balance_code: r.get(1)?,
                has_is_hk_post: r.get(2)?,
                has_tax_def: r.get(3)?,
                has_tax_def_rate: r.get(4)?,
                distinct_folio_balance_codes: r.get(5)?,
                group1_count: r.get(6)?,
            })
        })()
        .map_err(|e| err("agent: probe fo schema", e))
    }

    /// `VerifyFOChargeCodeSync` over the FO connection.
    pub fn verify_fo_charge_code_sync(
        &self,
        charge_codes: &[String],
        timeout: Duration,
    ) -> Result<(), String> {
        validate_sp_name(&self.sp.fetch_fo)?;
        validate_sp_name(&self.sp.fetch_pos)?;
        let read = |name: &str| -> Result<String, String> {
            let rs = self
                .fo
                .with(timeout, |c| {
                    c.query(
                        "SELECT m.definition FROM sys.sql_modules m WHERE m.object_id = OBJECT_ID(@name)",
                        &[Param::new("name", name)],
                    )
                })
                .map_err(|e| format!("agent: drift check: read {name} definition: {e}"))?;
            match rs.rows().first().map(|r| r.get::<Option<String>>(0)) {
                Some(Ok(Some(def))) => Ok(def),
                Some(Err(e)) => Err(format!("agent: drift check: read {name} definition: {e}")),
                _ => Err(format!(
                    "agent: drift check: installed {name} not found in sys.sql_modules"
                )),
            }
        };
        let fo_def = read(&self.sp.fetch_fo)?;
        let pos_def = read(&self.sp.fetch_pos)?;
        let hash_def = read(drift::SOURCE_HASH_FUNC)?;
        drift::verify(
            &self.sp.fetch_fo,
            &fo_def,
            &self.sp.fetch_pos,
            &pos_def,
            &hash_def,
            charge_codes,
        )
    }
}

/// The charge-code drift guard (`driftcheck.go`), pure text checks.
pub mod drift;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sp_names() {
        for ok in ["sp_FetchFO", "dbo.sp_FetchFO", "x1_"] {
            validate_sp_name(ok).unwrap();
        }
        for bad in [
            "",
            "dbo.",
            ".x",
            "a.b.c",
            "sp; DROP TABLE x",
            "sp FetchFO",
            "đ",
        ] {
            assert!(validate_sp_name(bad).is_err(), "{bad}");
        }
    }
}
