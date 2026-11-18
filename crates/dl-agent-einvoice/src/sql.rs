//! SQL Server access: the queries of `collectsource*.go`, `sqlsource.go`,
//! `driftcheck.go` and `installsql*.go`, over dl-tds.
//!
//! One connection per source, opened lazily and reopened after a broken session.
//! Parameters are sent through `sp_executesql` with the same names as the Go agent.

use std::sync::Mutex;
use std::time::Duration;

use dl_tds::{Client, Config, Param, Row};
use dl_time::{DateTime, OffsetDateTime};

use crate::fo::{FoDetailRow, FoHeaderRow, FoLineRow, FoRows};
use crate::pos::{PosRawCheck, PosRawHeader, PosRawLine};

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

struct Conn {
    dsn: String,
    client: Mutex<Option<Client>>,
}

impl Conn {
    fn new(dsn: &str) -> Conn {
        Conn {
            dsn: dsn.to_owned(),
            client: Mutex::new(None),
        }
    }

    /// Runs `f` on a live connection, connecting first if needed. A connection that
    /// broke (I/O error, protocol error) is dropped so the next call reconnects.
    fn with<T>(
        &self,
        timeout: Duration,
        f: impl FnOnce(&mut Client) -> Result<T, dl_tds::Error>,
    ) -> Result<T, dl_tds::Error> {
        let mut guard = self
            .client
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.as_ref().is_none_or(Client::is_broken) {
            let cfg: Config = self.dsn.parse()?;
            *guard = Some(Client::connect(
                &cfg.with_connect_timeout(timeout.min(Duration::from_secs(30))),
            )?);
        }
        let Some(c) = guard.as_mut() else {
            unreachable!("connected above")
        };
        c.set_timeout(Some(timeout))?;
        let r = f(c);
        if c.is_broken() {
            *guard = None;
        }
        r
    }
}

/// The SMILE databases.
pub struct Sql {
    fo: Conn,
    pos: Option<Conn>,
    sp: SpNames,
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
            fo: Conn::new(fo_dsn),
            pos: (!pos_dsn.is_empty()).then(|| Conn::new(pos_dsn)),
            sp,
        }
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

    /// `SelectPendingCollect`.
    pub fn select_pending(
        &self,
        source: &str,
        limit: i64,
        from: Option<OffsetDateTime>,
        timeout: Duration,
    ) -> Result<Vec<CollectKey>, String> {
        let sp = if source == "POS" {
            &self.sp.select_pos
        } else {
            &self.sp.select_fo
        };
        validate_sp_name(sp)?;
        let q = format!("EXEC {sp} @Limit = @Limit, @CollectFrom = @CollectFrom");
        let rs = self
            .conn(source)
            .with(timeout, |c| {
                c.query(
                    &q,
                    &[Param::new("Limit", limit), Param::new("CollectFrom", from)],
                )
            })
            .map_err(|e| err(&format!("agent: select pending collect ({source})"), e))?;
        rs.rows()
            .iter()
            .map(|r| {
                r.get::<String>(0)
                    .map(|id| CollectKey {
                        source: source.to_owned(),
                        id,
                    })
                    .map_err(|e| err("agent: scan collect key", e))
            })
            .collect()
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

    /// `ApplyWriteback`.
    pub fn apply_writeback(&self, a: &WritebackApply, timeout: Duration) -> Result<(), String> {
        let sp = &self.sp.writeback;
        validate_sp_name(sp)?;
        let q = format!(
            "EXEC {sp} @BillID = @p1, @BillType = @p2, @InvoiceNumber = @p3, @KyHieu = @p4, @MaCQT = @p5, @IssuedAt = @p6"
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
            .map_err(|e| {
                err(
                    &format!("agent: apply writeback for {} {}", a.bill_type, a.bill_id),
                    e,
                )
            })
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
