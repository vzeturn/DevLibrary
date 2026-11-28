//! The SQL Server repository (Go `repo.go`) over a `dl-agent` TDS pool. Every statement is
//! parameterized; table and column names come only from constants in this crate.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use dl_agent::sql::SqlPool;
use dl_tds::{Client, Param, Row};
use dl_time::{Date, DateTime};

use super::audit::{AuditQuery, AuditResult};
use super::entities::{Folio, Val, ymd};
use super::insert::{build_insert, truncation_message};
use super::rows::{AdditionalName, ClientContact, ClientSts, FolioRate, SmileClient};
use super::{
    AGENT_CANCEL_CLERK_ID, AGENT_CLERK_ID, ALLOWED_UPDATE_COLS, FullRepo,
    LEGACY_AGENT_CANCEL_CLERK_ID, PmsRepo, compute_available,
};
use crate::error::{Error, Result};
use crate::gofmt::{f0, scan_int};
use crate::jobmodel::{AvailRow, MappingRow};
use crate::log;

/// The SMILE FO repository.
pub struct SqlRepo {
    pool: Arc<SqlPool>,
    /// `columnMaxLens` cache: table → lower-case column → max length (below 0 = MAX).
    col_lens: Mutex<HashMap<String, HashMap<String, i64>>>,
}

/// Runs a statement, returning the rows affected.
pub(super) fn exec(c: &mut Client, sql: &str, params: &[Param]) -> Result<u64> {
    Ok(c.execute(sql, params)?)
}

/// The rows of the first result set.
pub(super) fn rows(c: &mut Client, sql: &str, params: &[Param]) -> Result<Vec<Row>> {
    let res = c.query(sql, params)?;
    Ok(res.rows().to_vec())
}

/// `SELECT COUNT(..)`'s value.
pub(super) fn count(c: &mut Client, sql: &str, params: &[Param]) -> Result<i64> {
    let r = rows(c, sql, params)?;
    match r.first() {
        Some(row) => Ok(row.get::<i64>(0)?),
        None => Err(Error::other("sql: no rows in result set")),
    }
}

/// `FolioLog` column sizes (`FieldName varchar(50)`, `OldValue`/`NewValue nvarchar(1000)`).
pub(super) const FOLIO_LOG_FIELD_MAX: usize = 50;
pub(super) const FOLIO_LOG_VALUE_MAX: usize = 1000;

/// `clipRunes`: at most `n` characters of `s` (avoids SQL Server's truncation error).
pub(super) fn clip_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((at, _)) => &s[..at],
        None => s,
    }
}

pub(super) fn opt_str(row: &Row, i: usize) -> Result<String> {
    Ok(row.get::<Option<String>>(i)?.unwrap_or_default())
}

impl SqlRepo {
    /// A repository over `pool`.
    pub fn new(pool: Arc<SqlPool>) -> SqlRepo {
        SqlRepo {
            pool,
            col_lens: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn with_conn<T>(&self, f: impl FnOnce(&mut Client) -> Result<T>) -> Result<T> {
        let mut c = self.pool.get()?;
        f(&mut c)
    }

    /// Runs `f` inside `BEGIN TRANSACTION` … `COMMIT`, rolling back on error. A connection
    /// returned while still in a transaction is discarded by the pool.
    pub(super) fn with_tx<T>(&self, f: impl FnOnce(&mut Client) -> Result<T>) -> Result<T> {
        let mut c = self.pool.get()?;
        exec(&mut c, "BEGIN TRANSACTION", &[])?;
        match f(&mut c) {
            Ok(v) => {
                exec(&mut c, "COMMIT TRANSACTION", &[])?;
                Ok(v)
            }
            Err(e) => {
                if let Err(rb) = exec(&mut c, "IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION", &[]) {
                    log::warn("rollback lỗi", &[("error", log::V::S(rb.message()))]);
                }
                Err(e)
            }
        }
    }

    fn query_live_folios(&self, key_col: &str, key: &str) -> Result<Vec<Folio>> {
        let sql = format!(
            "SELECT Id, FolioNum, ConfirmNum, BookStatus, RoomCode, FolioStatus, Booker,
       UserInfo1, RoomTypeCode, ArrivalDate, DepartureDate,
       NumAdult, NumChild, RateAmount, IncludeBF, BreakfastCode, NumGuestBF, NumChildBF,
       LastName, TelNumM, MailM
FROM Folio
WHERE {key_col} = @p1
  AND ISNULL(CancelNum, 0) = 0"
        );
        self.with_conn(|c| {
            rows(c, &sql, &[Param::new("p1", key)])?
                .iter()
                .map(|r| {
                    Ok(Folio {
                        id: r.get(0)?,
                        folio_num: r.get(1)?,
                        confirm_num: r.get(2)?,
                        book_status: r.get(3)?,
                        room_code: opt_str(r, 4)?,
                        folio_status: r.get(5)?,
                        booker: opt_str(r, 6)?,
                        user_info1: opt_str(r, 7)?,
                        room_type_code: opt_str(r, 8)?,
                        arrival_date: r.get(9)?,
                        departure_date: r.get(10)?,
                        num_adult: r.get(11)?,
                        num_child: r.get(12)?,
                        rate_amount: r.get(13)?,
                        include_bf: r.get(14)?,
                        breakfast_code: opt_str(r, 15)?,
                        num_guest_bf: r.get(16)?,
                        num_child_bf: r.get(17)?,
                        // The guest columns, so the modify diff compares for real
                        // instead of rewriting them on every modify (RT-0651).
                        last_name: opt_str(r, 18)?,
                        tel_num_m: opt_str(r, 19)?,
                        mail_m: opt_str(r, 20)?,
                        ..Folio::default()
                    })
                })
                .collect()
        })
    }

    fn scalar_proc(c: &mut Client, proc: &str) -> Result<f64> {
        let r = rows(c, &format!("EXEC {proc}"), &[])
            .map_err(|e| e.context(format!("proc {proc}: ")))?;
        match r.first() {
            Some(row) => row
                .get::<f64>(0)
                .map_err(|e| Error::from(e).context(format!("proc {proc}: "))),
            None => Err(Error::other(format!(
                "proc {proc}: sql: no rows in result set"
            ))),
        }
    }

    /// `columnMaxLens`, read on `c` (the failed transaction's connection) and cached.
    fn column_max_lens(&self, c: &mut Client, table: &str) -> Option<HashMap<String, i64>> {
        if let Ok(cache) = self.col_lens.lock()
            && let Some(m) = cache.get(table)
        {
            return Some(m.clone());
        }
        let res = rows(
            c,
            "SELECT COLUMN_NAME, CHARACTER_MAXIMUM_LENGTH
FROM INFORMATION_SCHEMA.COLUMNS
WHERE TABLE_NAME = @p1 AND CHARACTER_MAXIMUM_LENGTH IS NOT NULL",
            &[Param::new("p1", table)],
        )
        .and_then(|rs| {
            rs.iter()
                .map(|r| {
                    let name: String = r.get(0)?;
                    let max: Option<i64> = r.get(1)?;
                    Ok((name.to_lowercase(), max.unwrap_or(-1)))
                })
                .collect::<Result<HashMap<_, _>>>()
        });
        match res {
            Ok(m) => {
                if let Ok(mut cache) = self.col_lens.lock() {
                    cache.insert(table.to_owned(), m.clone());
                }
                Some(m)
            }
            Err(e) => {
                log::warn(
                    "không đọc được độ dài cột để chẩn đoán truncation",
                    &[
                        ("table", log::V::S(table)),
                        ("error", log::V::S(e.message())),
                    ],
                );
                None
            }
        }
    }

    fn insert_row(
        &self,
        c: &mut Client,
        table: &str,
        fields: &[(&'static str, Val)],
        prefix: String,
    ) -> Result<()> {
        let (sql, params) = build_insert(table, fields);
        if let Err(e) = exec(c, &sql, &params) {
            let limits = if super::insert::is_truncation_err(e.message()) {
                self.column_max_lens(c, table)
            } else {
                None
            };
            let kind = e.kind();
            let msg = truncation_message(e.message().to_owned(), fields, limits.as_ref());
            return Err(Error::new(kind, format!("{prefix}{msg}")));
        }
        Ok(())
    }

    fn scan_client(&self, sql: &str, arg: Param) -> Result<Option<SmileClient>> {
        self.with_conn(|c| {
            let r = rows(c, sql, &[arg])?;
            r.first()
                .map(|row| {
                    Ok(SmileClient {
                        client_folio_num: row.get(0)?,
                        client_name: row.get(1)?,
                    })
                })
                .transpose()
        })
    }
}

impl PmsRepo for SqlRepo {
    fn get_folios(&self, ext: &str) -> Result<Vec<Folio>> {
        self.query_live_folios("ExternalConfirmNumber", ext)
    }

    fn get_folios_by_booker(&self, booker: &str) -> Result<Vec<Folio>> {
        if booker.trim().is_empty() {
            return Ok(Vec::new()); // never match the many folios with a blank Booker
        }
        self.query_live_folios("Booker", booker)
    }

    fn has_user_cancelled_folio(&self, booker: &str, booking_id: &str) -> Result<bool> {
        let mut keys: Vec<&str> = Vec::new();
        if !booker.trim().is_empty() {
            keys.push(booker);
        }
        if !booking_id.trim().is_empty() && booking_id != booker {
            keys.push(booking_id);
        }
        if keys.is_empty() {
            return Ok(false);
        }
        let ph: Vec<String> = (1..=keys.len()).map(|i| format!("@p{i}")).collect();
        let ph = ph.join(",");
        let sql = format!(
            "SELECT TOP 1 1 FROM Folio
WHERE (Booker IN ({ph})
    OR ExternalConfirmNumber IN ({ph}))
  AND ISNULL(CancelNum, 0) <> 0
  AND ISNULL(LastChangeClerkID, '') NOT IN (@p{}, @p{})",
            keys.len() + 1,
            keys.len() + 2
        );
        let mut params: Vec<Param> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| Param::new(&format!("p{}", i + 1), *k))
            .collect();
        params.push(Param::new(
            &format!("p{}", keys.len() + 1),
            AGENT_CANCEL_CLERK_ID,
        ));
        params.push(Param::new(
            &format!("p{}", keys.len() + 2),
            LEGACY_AGENT_CANCEL_CLERK_ID,
        ));
        self.with_conn(|c| Ok(!rows(c, &sql, &params)?.is_empty()))
            .map_err(|e| e.context("has user-cancelled folio: "))
    }

    fn agent_cancelled_folio_nums(&self, booker: &str, booking_id: &str) -> Result<Vec<f64>> {
        let mut keys: Vec<&str> = Vec::new();
        if !booker.trim().is_empty() {
            keys.push(booker);
        }
        if !booking_id.trim().is_empty() && booking_id != booker {
            keys.push(booking_id);
        }
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let n = keys.len();
        let ph = (1..=n)
            .map(|i| format!("@p{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT TOP 50 FolioNum FROM Folio
WHERE (Booker IN ({ph})
    OR ExternalConfirmNumber IN ({ph}))
  AND ISNULL(CancelNum, 0) <> 0
  AND ISNULL(LastChangeClerkID, '') IN (@p{}, @p{})
ORDER BY FolioNum",
            n + 1,
            n + 2
        );
        let mut params: Vec<Param> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| Param::new(&format!("p{}", i + 1), *k))
            .collect();
        params.push(Param::new(&format!("p{}", n + 1), AGENT_CANCEL_CLERK_ID));
        params.push(Param::new(
            &format!("p{}", n + 2),
            LEGACY_AGENT_CANCEL_CLERK_ID,
        ));
        self.with_conn(|c| {
            rows(c, &sql, &params)?
                .iter()
                .map(|r| Ok(r.get::<f64>(0)?))
                .collect()
        })
        .map_err(|e| e.context("agent-cancelled folios: "))
    }

    fn get_new_confirm_num(&self) -> Result<f64> {
        self.with_conn(|c| Self::scalar_proc(c, "GetConfirmNum"))
    }

    fn get_next_folio_num(&self) -> Result<f64> {
        self.with_conn(|c| {
            let res = c.query("EXEC GetFolioNum", &[])?;
            let missing = || Error::other("stored proc GetFolioNum không trả FolioNum");
            let set = res.first().ok_or_else(missing)?;
            let idx = set
                .columns()
                .iter()
                .position(|col| col.name().eq_ignore_ascii_case("FolioNum"))
                .ok_or_else(missing)?;
            let row = set.rows().first().ok_or_else(missing)?;
            Ok(row.get::<f64>(idx)?)
        })
    }

    fn insert_folio_with_rates(
        &self,
        f: &Folio,
        rates: &[FolioRate],
        an: &AdditionalName,
    ) -> Result<()> {
        self.with_tx(|c| {
            self.insert_row(
                c,
                "Folio",
                &f.fields(),
                format!("insert Folio {}: ", f0(f.folio_num)),
            )?;
            for rate in rates {
                self.insert_row(
                    c,
                    "FolioRate",
                    &rate.fields(),
                    format!(
                        "insert FolioRate {} {}: ",
                        f0(rate.folio_num),
                        ymd(rate.h_date)
                    ),
                )?;
            }
            self.insert_row(
                c,
                "AdditionalName",
                &an.fields(),
                format!("insert AdditionalName {}: ", f0(an.folio_num)),
            )
        })
    }

    fn cancel_folios(&self, folios: &[Folio]) -> Result<()> {
        self.with_tx(|c| {
            for f in folios {
                let cancel_num = Self::scalar_proc(c, "GetCancelNum")?;
                exec(
                    c,
                    "UPDATE Folio SET CancelNum = @p1, LastChangeClerkID = @p3, LastChangeTime = GETDATE() WHERE Id = @p2",
                    &[
                        Param::new("p1", cancel_num),
                        Param::new("p2", f.id),
                        Param::new("p3", AGENT_CANCEL_CLERK_ID),
                    ],
                )
                .map_err(|e| e.context(format!("cancel folio {}: ", f.id)))?;
                exec(
                    c,
                    "UPDATE AdditionalName SET ACancelNum = @p1 WHERE FolioNum = @p2",
                    &[Param::new("p1", cancel_num), Param::new("p2", f.folio_num)],
                )
                .map_err(|e| e.context(format!("cancel additionalname {}: ", f0(f.folio_num))))?;
            }
            Ok(())
        })
    }

    fn update_folio_fields(&self, folio_num: f64, set: &BTreeMap<&'static str, Val>) -> Result<()> {
        if set.is_empty() {
            return Ok(());
        }
        for col in set.keys() {
            if !ALLOWED_UPDATE_COLS.contains(col) {
                return Err(Error::other(format!(
                    "cột {} không được phép update qua diff",
                    crate::gofmt::quote(col)
                )));
            }
        }
        // BTreeMap iterates in sorted order, like Go's sort.Strings(cols). The clerk is
        // never NULL: triggerFolioUpdate concatenates it into its FolioLog insert, and a
        // NULL turns the whole statement NULL so no history row is written (RT-0651).
        let mut sql =
            String::from("UPDATE Folio SET LastChangeTime = GETDATE(), LastChangeClerkID = @p1");
        let mut params = Vec::with_capacity(set.len() + 2);
        params.push(Param::new("p1", AGENT_CLERK_ID));
        for (i, (col, v)) in set.iter().enumerate() {
            sql.push_str(&format!(", {col} = @p{}", i + 2));
            params.push(v.param(&format!("p{}", i + 2)));
        }
        let n = set.len() + 2;
        sql.push_str(&format!(
            " WHERE FolioNum = @p{n} AND ISNULL(CancelNum, 0) = 0"
        ));
        params.push(Param::new(&format!("p{n}"), folio_num));
        self.with_conn(|c| exec(c, &sql, &params))
            .map(|_| ())
            .map_err(|e| e.context(format!("update folio {}: ", f0(folio_num))))
    }

    fn get_folio_rates(&self, folio_num: f64) -> Result<Vec<FolioRate>> {
        self.with_conn(|c| {
            rows(
                c,
                "SELECT FolioNum, hDate, RateAmount, ToDate, RateOverride FROM FolioRate WHERE FolioNum = @p1 ORDER BY hDate",
                &[Param::new("p1", folio_num)],
            )?
            .iter()
            .map(|r| {
                Ok(FolioRate {
                    folio_num: r.get(0)?,
                    h_date: r.get(1)?,
                    rate_amount: r.get(2)?,
                    to_date: r.get(3)?,
                    rate_override: r.get(4)?,
                    ..FolioRate::default()
                })
            })
            .collect()
        })
    }

    fn replace_folio_rates(&self, folio_num: f64, rates: &[FolioRate]) -> Result<()> {
        self.with_tx(|c| {
            exec(
                c,
                "DELETE FROM FolioRate WHERE FolioNum = @p1",
                &[Param::new("p1", folio_num)],
            )
            .map_err(|e| e.context(format!("xóa FolioRate {}: ", f0(folio_num))))?;
            for rate in rates {
                let (sql, params) = build_insert("FolioRate", &rate.fields());
                exec(c, &sql, &params)
                    .map_err(|e| e.context(format!("chèn FolioRate {}: ", f0(folio_num))))?;
            }
            Ok(())
        })
    }

    fn update_additional_name(
        &self,
        folio_num: f64,
        last_name: &str,
        email: &str,
        arrival: DateTime,
        departure: DateTime,
    ) -> Result<()> {
        // The dates are explicit: triggerFolioUpdate only narrows ArrivalTime/DepartureTime,
        // so an extended stay kept the old departure (RT-0651).
        self.with_conn(|c| {
            exec(
                c,
                "UPDATE AdditionalName SET LastName = @p1, Email = @p2, ArrivalTime = @p3, DepartureTime = @p4 WHERE FolioNum = @p5 AND ACancelNum = 0",
                &[
                    Param::new("p1", last_name),
                    Param::new("p2", email),
                    Val::Time(arrival).param("p3"),
                    Val::Time(departure).param("p4"),
                    Param::new("p5", folio_num),
                ],
            )
        })
        .map(|_| ())
        .map_err(|e| e.context(format!("update additionalname {}: ", f0(folio_num))))
    }

    fn stamp_folio_change(&self, folio_num: f64) -> Result<()> {
        self.with_conn(|c| {
            exec(
                c,
                "UPDATE Folio SET LastChangeTime = GETDATE(), LastChangeClerkID = @p1 WHERE FolioNum = @p2 AND ISNULL(CancelNum, 0) = 0",
                &[
                    Param::new("p1", AGENT_CLERK_ID),
                    Param::new("p2", folio_num),
                ],
            )
        })
        .map(|_| ())
        .map_err(|e| e.context(format!("stamp folio {}: ", f0(folio_num))))
    }

    fn append_folio_log(&self, folio_num: f64, field: &str, old: &str, new: &str) -> Result<()> {
        // Same row shape as triggerFolioUpdate writes, for the live folio only.
        self.with_conn(|c| {
            exec(
                c,
                "INSERT INTO FolioLog (FolioID, FolioNum, FolioSubNum, FieldName, OldValue, NewValue, ChangedClerk, ChangedTime)
SELECT Id, FolioNum, ISNULL(FolioSubNum, 0), @p1, @p2, @p3, @p4, GETDATE()
FROM Folio WHERE FolioNum = @p5 AND ISNULL(CancelNum, 0) = 0",
                &[
                    Param::new("p1", clip_chars(field, FOLIO_LOG_FIELD_MAX)),
                    Param::new("p2", clip_chars(old, FOLIO_LOG_VALUE_MAX)),
                    Param::new("p3", clip_chars(new, FOLIO_LOG_VALUE_MAX)),
                    Param::new("p4", AGENT_CLERK_ID),
                    Param::new("p5", folio_num),
                ],
            )
        })
        .map(|_| ())
        .map_err(|e| e.context(format!("append FolioLog {} {field}: ", f0(folio_num))))
    }

    fn append_folio_log_any(
        &self,
        folio_num: f64,
        field: &str,
        old: &str,
        new: &str,
    ) -> Result<()> {
        self.with_conn(|c| {
            exec(
                c,
                "INSERT INTO FolioLog (FolioID, FolioNum, FolioSubNum, FieldName, OldValue, NewValue, ChangedClerk, ChangedTime)
SELECT TOP 1 Id, FolioNum, ISNULL(FolioSubNum, 0), @p1, @p2, @p3, @p4, GETDATE()
FROM Folio WHERE FolioNum = @p5 ORDER BY Id DESC",
                &[
                    Param::new("p1", clip_chars(field, FOLIO_LOG_FIELD_MAX)),
                    Param::new("p2", clip_chars(old, FOLIO_LOG_VALUE_MAX)),
                    Param::new("p3", clip_chars(new, FOLIO_LOG_VALUE_MAX)),
                    Param::new("p4", AGENT_CANCEL_CLERK_ID),
                    Param::new("p5", folio_num),
                ],
            )
        })
        .map(|_| ())
        .map_err(|e| e.context(format!("append FolioLog {} {field}: ", f0(folio_num))))
    }

    fn get_global_param_int(&self, name: &str) -> Result<i64> {
        self.with_conn(|c| {
            let r = rows(
                c,
                "SELECT ParaString FROM GlobalParameter WHERE ParaName = @p1",
                &[Param::new("p1", name)],
            )?;
            let Some(row) = r.first() else {
                return Ok(0);
            };
            Ok(scan_int(opt_str(row, 0)?.trim()).unwrap_or(0))
        })
    }

    fn seed_global_param_if_absent(&self, name: &str, value: &str) -> Result<()> {
        self.with_conn(|c| {
            exec(
                c,
                "IF NOT EXISTS (SELECT 1 FROM GlobalParameter WHERE ParaName=@p1)
    INSERT INTO GlobalParameter (ParaName, ParaString) VALUES (@p1, @p2)",
                &[Param::new("p1", name), Param::new("p2", value)],
            )
        })
        .map(|_| ())
        .map_err(|e| {
            e.context(format!(
                "seed GlobalParameter {}: ",
                crate::gofmt::quote(name)
            ))
        })
    }

    fn get_client_by_name(&self, name: &str) -> Result<Option<SmileClient>> {
        self.scan_client(
            "SELECT TOP 1 ClientFolioNum, ClientName FROM CLIENT WHERE ClientName = @p1",
            Param::new("p1", name),
        )
    }

    fn get_client_by_folio_num(&self, num: f64) -> Result<Option<SmileClient>> {
        self.scan_client(
            "SELECT TOP 1 ClientFolioNum, ClientName FROM CLIENT WHERE ClientFolioNum = @p1",
            Param::new("p1", num),
        )
    }

    fn get_client_contact_by_name(&self, name: &str) -> Result<Option<ClientContact>> {
        self.with_conn(|c| {
            let r = rows(
                c,
                "SELECT TOP 1 ClientFolioNum, ContactName FROM ClientContacts WHERE ContactName = @p1",
                &[Param::new("p1", name)],
            )?;
            r.first()
                .map(|row| {
                    Ok(ClientContact {
                        client_folio_num: row.get(0)?,
                        contact_name: row.get(1)?,
                    })
                })
                .transpose()
        })
    }

    fn get_client_sts(&self, code: f64) -> Result<Option<ClientSts>> {
        self.with_conn(|c| {
            let r = rows(
                c,
                "SELECT TOP 1 ClientCode, Origin, MSegment,
       UDef1, UDef2, UDef3, UDef4, UDef5, UDef6, UDef7, UDef8
FROM ClientSts WHERE ClientCode = @p1",
                &[Param::new("p1", code)],
            )?;
            r.first()
                .map(|row| {
                    let mut u_def: [String; 8] = Default::default();
                    for (i, u) in u_def.iter_mut().enumerate() {
                        *u = row.get(3 + i)?;
                    }
                    Ok(ClientSts {
                        client_code: row.get(0)?,
                        origin: row.get(1)?,
                        m_segment: row.get(2)?,
                        u_def,
                    })
                })
                .transpose()
        })
    }

    fn get_hotel_date(&self) -> Result<DateTime> {
        self.with_conn(|c| {
            let r = rows(c, "SELECT TOP 1 HotelDate FROM HotelParameter", &[])
                .map_err(|e| e.context("GetHotelDate: "))?;
            let Some(row) = r.first() else {
                return Err(Error::other("HotelDate không có (HotelParameter rỗng)"));
            };
            match row
                .get::<Option<DateTime>>(0)
                .map_err(|e| Error::from(e).context("GetHotelDate: "))?
            {
                Some(d) => Ok(d),
                None => Err(Error::other("HotelDate không có (NULL)")),
            }
        })
    }
}

impl FullRepo for SqlRepo {
    fn audit_folios(&self, q: &AuditQuery) -> Result<AuditResult> {
        self.audit_folios_impl(q)
    }

    fn test_connection(&self) -> Result<()> {
        self.with_conn(|c| rows(c, "SELECT 1", &[]).map(|_| ()))
    }

    fn ensure_room_type_mapped(&self) -> Result<i64> {
        self.with_conn(|c| {
            exec(
                c,
                "IF OBJECT_ID('RoomTypeMapped') IS NULL
    CREATE TABLE RoomTypeMapped (
        RoomId NVARCHAR(50) NULL,
        RoomTypeCode NVARCHAR(50) NOT NULL,
        SplitPart INT NULL,
        TotalPart INT NULL,
        IsBooking BIT NOT NULL DEFAULT 0
    )",
                &[],
            )
            .map_err(|e| e.context("tạo bảng RoomTypeMapped: "))?;
            let existing = count(c, "SELECT COUNT(*) FROM RoomTypeMapped", &[])
                .map_err(|e| e.context("đếm RoomTypeMapped: "))?;
            if existing > 0 {
                return Ok(0); // keep the hotel's mapping, never seed over it
            }
            match exec(
                c,
                "INSERT INTO RoomTypeMapped (RoomId, RoomTypeCode, SplitPart, TotalPart, IsBooking)
 SELECT '', rt.RoomTypeCode, NULL, NULL, 0
   FROM RoomType rt
  WHERE LTRIM(RTRIM(rt.RoomTypeCode)) <> ''
    AND rt.RoomTypeCode <> '**'
    AND NOT EXISTS (SELECT 1 FROM RoomTypeMapped m WHERE m.RoomTypeCode = rt.RoomTypeCode)",
                &[],
            ) {
                Ok(n) => Ok(n as i64),
                Err(e) => {
                    // Go ignores this (a site without RoomType); the table exists, so go on.
                    log::warn(
                        "seed RoomTypeMapped từ RoomType lỗi — bỏ qua",
                        &[("error", log::V::S(e.message()))],
                    );
                    Ok(0)
                }
            }
        })
    }

    fn get_room_type_mapped(&self) -> Result<Vec<MappingRow>> {
        self.with_conn(|c| {
            rows(
                c,
                "SELECT RoomId, RoomTypeCode, SplitPart, TotalPart, IsBooking FROM RoomTypeMapped",
                &[],
            )?
            .iter()
            .map(|r| {
                Ok(MappingRow {
                    room_id: opt_str(r, 0)?,
                    room_type_code: r.get(1)?,
                    split_part: r.get(2)?,
                    total_part: r.get(3)?,
                    is_booking: r.get(4)?,
                })
            })
            .collect()
        })
    }

    fn get_availability(&self, from: Date, days: i64) -> Result<Vec<AvailRow>> {
        let s_date = format!(
            "{:02}/{:02}/{:04}",
            from.month().number(),
            from.day(),
            from.year()
        );
        self.with_conn(|c| {
            let res = c.query(
                "EXEC AvailabilityChart @RoomType, @sDate, @NumDate",
                &[
                    Param::new("RoomType", "Availability"),
                    Param::new("sDate", s_date.as_str()),
                    Param::new("NumDate", days),
                ],
            )?;
            let Some(set) = res.first() else {
                return Ok(Vec::new());
            };
            let col = |name: &str| {
                set.columns()
                    .iter()
                    .position(|c| c.name().eq_ignore_ascii_case(name))
            };
            let (code, sdate) = (col("roomtypecode"), col("sdate"));
            let counts = [
                "numroom",
                "definite",
                "tentative",
                "seribooking",
                "waiting",
                "allotment",
                "ooo",
                "ooi",
                "hu",
                "phu",
            ]
            .map(col);
            set.rows()
                .iter()
                .map(|r| {
                    let mut n = [0i64; 10];
                    for (slot, idx) in n.iter_mut().zip(counts) {
                        if let Some(i) = idx {
                            *slot = r.get::<Option<i64>>(i)?.unwrap_or(0);
                        }
                    }
                    let code: String = match code {
                        Some(i) => r.get(i)?,
                        None => String::new(),
                    };
                    let date = match sdate {
                        Some(i) => ymd(r.get::<DateTime>(i)?),
                        None => ymd(super::zero_time()),
                    };
                    Ok(AvailRow {
                        room_type_code: code,
                        date,
                        available: compute_available(
                            n[0], n[1], n[2], n[3], n[4], n[5], n[6], n[7], n[8], n[9],
                        ),
                    })
                })
                .collect()
        })
    }

    fn insert_mapping(&self, m: &MappingRow) -> Result<()> {
        self.insert_mapping_sql(m)
    }

    fn update_mapping(&self, orig: &MappingRow, m: &MappingRow) -> Result<()> {
        self.update_mapping_sql(orig, m)
    }

    fn delete_mapping(&self, orig: &MappingRow) -> Result<()> {
        self.delete_mapping_sql(orig)
    }
}

#[cfg(test)]
mod tests {
    use super::clip_chars;

    #[test]
    fn clip_chars_counts_characters_not_bytes() {
        assert_eq!(clip_chars("abc", 5), "abc");
        assert_eq!(clip_chars("abc", 3), "abc");
        assert_eq!(clip_chars("abcdef", 3), "abc");
        assert_eq!(clip_chars("ĐặtPhòng", 4), "ĐặtP");
        assert_eq!(clip_chars("", 0), "");
    }
}
