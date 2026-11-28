//! [`SiteCommandStore`] on the store's SQL Server through the least-privilege `pda_sync`
//! login (`SqlSiteCommandStore.cs`). Its write grants are exactly the statements of
//! [`super::sql_text`] and [`super::batches`]; all SQL is parameterized.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use dl_tds::{Client, Param};
use dl_uuid::Uuid;

use super::error::{CommandError, Result, check_ctx};
use super::sql::{self, COMMAND_TIMEOUT};
use super::sql_text as q;
use super::units::{
    CommandWork, OrderCommandUnit, SiteCommandStore, SiteCommandUnit, TableCommandUnit,
};
use crate::contracts::WireJson;
use crate::contracts::wire::{
    CheckStateFingerprint, CheckStateHeader, CheckStateLine, CommandStatus, ReservationSavePayload,
    SyncCommandResult,
};
use crate::util::ctx::Ctx;

/// Builds the connection settings of each command (`Func<string> connectionString`).
pub type ConfigSource = Arc<dyn Fn() -> Result<dl_tds::Config> + Send + Sync>;

/// SQL Server's deadlock victim (1205) and lock request time-out (1222): SMILE's own
/// writers hold the check rows, so a command that meets them is run again (RT-1318).
const BUSY: [i32; 2] = [1205, 1222];
/// Runs of a command that meets a busy store, the first included.
const BUSY_ATTEMPTS: u32 = 3;
/// The wait before the next run, times the runs so far.
const BUSY_BACKOFF: Duration = Duration::from_millis(100);

/// The SQL Server command store. Idle connections are kept for the next commands, as
/// SqlClient's pool would: up to one per command worker (RT-1318), so commands running
/// side by side never open more connections than there are workers.
pub struct SqlSiteCommandStore {
    config: ConfigSource,
    idle: Mutex<Vec<Client>>,
    max_idle: usize,
}

impl SqlSiteCommandStore {
    /// A store connecting with the settings `config` returns.
    pub fn new(config: ConfigSource) -> SqlSiteCommandStore {
        SqlSiteCommandStore {
            config,
            idle: Mutex::new(Vec::new()),
            max_idle: 1,
        }
    }

    /// The store keeping up to `max_idle` (at least 1) idle connections.
    #[must_use]
    pub fn with_max_idle(mut self, max_idle: usize) -> SqlSiteCommandStore {
        self.max_idle = max_idle.max(1);
        self
    }

    pub(super) fn open(&self, ctx: &Ctx) -> Result<Client> {
        check_ctx(ctx)?;
        loop {
            let pooled = self
                .idle
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop();
            match pooled {
                Some(c) if !c.is_broken() && !c.in_transaction() => return Ok(c),
                Some(_) => continue,
                None => break,
            }
        }
        let cfg = (self.config)()?;
        let mut c = Client::connect(&cfg)?;
        c.set_timeout(Some(COMMAND_TIMEOUT))?;
        Ok(c)
    }

    pub(super) fn release(&self, c: Client) {
        if !c.is_broken() && !c.in_transaction() {
            let mut idle = self.idle.lock().unwrap_or_else(PoisonError::into_inner);
            if idle.len() < self.max_idle {
                idle.push(c);
            }
        }
    }

    fn read_log(
        c: &mut Client,
        sql_text: &str,
        command_id: Uuid,
        ctx: &Ctx,
    ) -> Result<Option<SyncCommandResult>> {
        let r = sql::query(c, sql_text, &[Param::new("CommandId", command_id)], ctx)?;
        let Some(row) = r.rows().first() else {
            return Ok(None);
        };
        let status = sql::text(row, 0).unwrap_or_default();
        if let Some(json) = sql::text(row, 1) {
            return SyncCommandResult::from_json(&json).map(Some).map_err(|_| {
                CommandError::invalid(format!(
                    "Command log row {command_id} holds an unreadable result."
                ))
            });
        }
        let parsed = CommandStatus::MEMBERS
            .iter()
            .find(|(name, _, _)| name.eq_ignore_ascii_case(status.trim()))
            .map(|(_, _, s)| *s)
            .ok_or_else(|| {
                CommandError::invalid(format!(
                    "Command log row {command_id} has unknown status '{status}'."
                ))
            })?;
        let at = match sql::value(row, 2) {
            Some(dl_tds::Value::DateTime(t)) => sql::from_utc_datetime(*t)?,
            _ => {
                return Err(CommandError::invalid(format!(
                    "Command log row {command_id} has no time."
                )));
            }
        };
        Ok(Some(SyncCommandResult {
            command_id,
            status: parsed,
            reason: None,
            result: None,
            at_utc: at,
            exec_ms: None,
            hops: Default::default(),
        }))
    }

    fn run_in(
        c: &mut Client,
        command_id: Uuid,
        work: CommandWork<'_>,
        ctx: &Ctx,
    ) -> Result<SyncCommandResult> {
        sql::begin(c)?;
        if let Some(recorded) = Self::read_log(c, q::LOCK_LOG, command_id, ctx)? {
            sql::commit(c)?;
            return Ok(recorded);
        }
        let result = {
            let mut unit = SqlSiteCommandUnit { c: &mut *c };
            work(&mut unit, ctx)?
        };
        if result.command_id != command_id {
            return Err(CommandError::invalid(format!(
                "Command {command_id} produced the result of command {}.",
                result.command_id
            )));
        }
        sql::execute(
            c,
            q::INSERT_LOG,
            &[
                Param::new("CommandId", command_id),
                Param::new("Status", result.status.name()),
                Param::new("ResultJson", result.to_json()),
                Param::new("AtUtc", sql::utc_datetime(result.at_utc)?),
            ],
            ctx,
        )?;
        sql::commit(c)?;
        Ok(result)
    }
}

impl SiteCommandStore for SqlSiteCommandStore {
    fn find_recorded(&self, command_id: Uuid, ctx: &Ctx) -> Result<Option<SyncCommandResult>> {
        let mut c = self.open(ctx)?;
        let r = Self::read_log(&mut c, q::READ_LOG, command_id, ctx);
        self.release(c);
        r
    }

    fn run_once(
        &self,
        command_id: Uuid,
        work: CommandWork<'_>,
        ctx: &Ctx,
    ) -> Result<SyncCommandResult> {
        let mut runs = 1;
        loop {
            let mut c = self.open(ctx)?;
            let r = Self::run_in(&mut c, command_id, work, ctx);
            // An error leaves neither the business write nor the log row: roll back what is
            // still open; a connection that cannot is dropped.
            if r.is_ok() || !(c.is_broken() || sql::rollback(&mut c).is_err()) {
                self.release(c);
            }
            // A busy store committed nothing, and the log row read first makes a second
            // run of a command that did commit return its logged result.
            let busy = matches!(&r, Err(e) if e.sql_number().is_some_and(|n| BUSY.contains(&n)));
            if !busy || runs >= BUSY_ATTEMPTS {
                return r;
            }
            std::thread::sleep(BUSY_BACKOFF * runs);
            check_ctx(ctx)?;
            runs += 1;
        }
    }
}

/// The store writes of one command, on the connection and transaction of
/// [`SqlSiteCommandStore`] (`SqlSiteCommandUnit`).
pub struct SqlSiteCommandUnit<'c> {
    pub(crate) c: &'c mut Client,
}

impl SqlSiteCommandUnit<'_> {
    /// `rvcNo.ToString(InvariantCulture)`.
    fn rvc(rvc_no: i32) -> String {
        rvc_no.to_string()
    }
}

/// The parameters of [`q::INSERT_RESERVATION`].
pub fn reservation_parameters(r: &ReservationSavePayload) -> Result<Vec<Param>> {
    Ok(vec![
        sql::text_param("GuestInfo", Some(&r.guest_info)),
        Param::new("NumAdult", r.num_adult),
        Param::new("NumChild", r.num_child),
        Param::new("FromDateTime", sql::wall_datetime(r.from_date_time)?),
        Param::new("ToDateTime", sql::wall_datetime(r.to_date_time)?),
        sql::text_param("TableNum", Some(&r.table_num)),
        sql::text_param("TelNum", Some(&r.tel_num)),
        sql::text_param("Booker", r.booker.as_deref()),
        sql::text_param("CompanyName", r.company_name.as_deref()),
        sql::text_param("VATCode", r.vat_code.as_deref()),
        sql::text_param("Address", r.address.as_deref()),
        sql::text_param("Rvc", Some(&r.rvc_no.to_string())),
    ])
}

impl SiteCommandUnit for SqlSiteCommandUnit<'_> {
    fn open_check_belongs_to_rvc(&mut self, rvc_no: i32, check_no: i64, ctx: &Ctx) -> Result<bool> {
        let r = sql::query(
            self.c,
            q::OPEN_CHECK_BELONGS,
            &[
                Param::new("CheckNo", check_no),
                Param::new("Rvc", Self::rvc(rvc_no)),
            ],
            ctx,
        )?;
        Ok(sql::scalar(&r).map_or(Ok(0), sql::to_i32)? > 0)
    }

    fn check_fingerprint(&mut self, check_no: i64, ctx: &Ctx) -> Result<Option<String>> {
        let r = sql::query(
            self.c,
            q::CHECK_STATE,
            &[Param::new("CheckNo", check_no)],
            ctx,
        )?;
        let sets = r.sets();
        let Some(head) = sets.first().and_then(|s| s.rows().first()) else {
            return Ok(None);
        };
        let header = CheckStateHeader {
            check_no: sql::int64(head, 0)?,
            table_no: sql::text(head, 1),
            guests: sql::decimal_to_i32(sql::number(head, 2)?)?,
            sub_total: sql::number(head, 3)?,
            total_paid: sql::number(head, 4)?,
        };
        if sets[0].rows().len() > 1 {
            return Err(CommandError::invalid(format!(
                "Open check {check_no} has more than one dbo.TmpInvoice row."
            )));
        }
        let Some(line_set) = sets.get(1) else {
            return Err(CommandError::invalid(
                "The check state query returned no line set.",
            ));
        };
        let mut lines = Vec::with_capacity(line_set.rows().len());
        for row in line_set.rows() {
            lines.push(CheckStateLine {
                line_seq: sql::int64(row, 0)?,
                transaction_code: sql::text(row, 1),
                item_code: sql::text(row, 2),
                qty: sql::number(row, 3)?,
                price: sql::number(row, 4)?,
            });
        }
        Ok(Some(CheckStateFingerprint::compute(&header, &lines)))
    }

    fn kds_accept_columns_exist(&mut self, ctx: &Ctx) -> Result<bool> {
        let r = sql::query(self.c, q::KDS_ACCEPT_COLUMNS, &[], ctx)?;
        Ok(sql::scalar(&r).map_or(Ok(0), sql::to_i32)? == 1)
    }

    fn apply_kds_line_state(
        &mut self,
        check_no: i64,
        line_seqs: &[i64],
        action: &str,
        cashier: &str,
        ctx: &Ctx,
    ) -> Result<i32> {
        let Some(sql_text) = q::kds_line_sql(action) else {
            return Err(CommandError::Argument(format!(
                "Unknown KDS action. (Parameter 'action')\nActual value was {action}."
            )));
        };
        let accept = action == crate::contracts::wire::kds_line_actions::ACCEPT;
        let mut changed: i64 = 0;
        for seq in line_seqs {
            let mut params = vec![Param::new("CheckNo", check_no), Param::new("TrnNo", *seq)];
            if accept {
                params.push(sql::text_param("Cashier", Some(cashier)));
            }
            changed += i64::try_from(sql::execute(self.c, sql_text, &params, ctx)?).unwrap_or(0);
        }
        Ok(i32::try_from(changed).unwrap_or(i32::MAX))
    }

    fn insert_reservation(
        &mut self,
        reservation: &ReservationSavePayload,
        ctx: &Ctx,
    ) -> Result<i32> {
        let r = sql::query(
            self.c,
            q::INSERT_RESERVATION,
            &reservation_parameters(reservation)?,
            ctx,
        )?;
        match sql::scalar(&r) {
            None => Err(CommandError::invalid(
                "dbo.TableReservation insert returned no identity.",
            )),
            Some(v) => sql::to_i32(v),
        }
    }

    fn delete_reservation(&mut self, rvc_no: i32, id: i32, ctx: &Ctx) -> Result<bool> {
        let n = sql::execute(
            self.c,
            q::DELETE_RESERVATION,
            &[Param::new("Id", id), Param::new("Rvc", Self::rvc(rvc_no))],
            ctx,
        )?;
        Ok(n > 0)
    }

    fn orders(&mut self) -> &mut dyn OrderCommandUnit {
        self
    }

    fn tables(&mut self) -> &mut dyn TableCommandUnit {
        self
    }
}
