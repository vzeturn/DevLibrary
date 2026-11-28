//! Runs `fo.inhouse_search` (`FoInHouseSearchCommandHandler.cs`, RT-1296, ADR 0172 §7): a
//! SELECT-only read of `Folio` in the Front Office database. It opens no write transaction
//! and writes no `Pda_SyncCommandLog` row (a redelivery reads again). A Front Office database
//! that is not usable is a rejection with [`txn_reasons::FOLIO_DATABASE_INVALID`]; SQL
//! failures propagate to the channel's failure count. The log carries the RVC and the row
//! count only: never the query, a room, a folio number or a name.

use std::sync::Arc;
use std::time::Duration;

use dl_tds::{Client, Param, ParamValue, Value as SqlValue};

use super::error::{Result, check_ctx};
use super::executor::{reasons, utc_now};
use super::fo_inhouse_match;
use super::sql::{self, COMMAND_TIMEOUT};
use super::sql_store::ConfigSource;
use crate::config::time::SharedClock;
use crate::contracts::WireJson;
use crate::contracts::wire::{
    CommandStatus, FoInHouseGuest, FoInHouseSearchPayload, FoInHouseSearchResult, SyncCommand,
    SyncCommandResult, txn_reasons,
};
use crate::reporting::{Logger, Value};
use crate::util::ctx::Ctx;
use crate::util::store_text;

/// Category of the handler's log lines.
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.Commands.FoInHouseSearchCommandHandler";

/// What [`FoInHouseSource`] read: the in-house folios found, or the
/// [`txn_reasons`] value that refused the read (nothing was read then).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FoInHouseRead {
    /// Why the read was refused.
    pub refusal: Option<String>,
    /// The folios found.
    pub guests: Vec<FoInHouseGuest>,
}

impl FoInHouseRead {
    /// A refused read.
    pub fn refused(reason: &str) -> FoInHouseRead {
        FoInHouseRead {
            refusal: Some(reason.to_owned()),
            guests: Vec::new(),
        }
    }
}

/// Read seam of `fo.inhouse_search`: in-house folios of one RVC's Front Office database.
pub trait FoInHouseSource: Send + Sync {
    /// Up to `limit` in-house folios whose room, folio number or name matches `query`, by
    /// room.
    fn search(&self, rvc: i32, query: &str, limit: i32, ctx: &Ctx) -> Result<FoInHouseRead>;
}

/// The `fo.inhouse_search` handler.
pub struct FoInHouseSearchCommandHandler {
    source: Arc<dyn FoInHouseSource>,
    clock: SharedClock,
    log: Logger,
}

impl FoInHouseSearchCommandHandler {
    /// A handler reading through `source`; `log` is the root logger.
    pub fn new(
        source: Arc<dyn FoInHouseSource>,
        clock: SharedClock,
        log: &Logger,
    ) -> FoInHouseSearchCommandHandler {
        FoInHouseSearchCommandHandler {
            source,
            clock,
            log: log.for_category(CATEGORY),
        }
    }

    /// Validates the payload, reads and answers; never writes.
    pub fn execute(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let p = FoInHouseSearchPayload::from_element(&c.payload);
        let problem = match &p {
            None => Some("payload is not a fo.inhouse_search object"),
            Some(p) => p.problem(),
        };
        let p = match (p, problem) {
            (Some(p), None) => p,
            (_, problem) => {
                let reason = format!(
                    "{}: {}",
                    reasons::INVALID_PAYLOAD,
                    problem.unwrap_or("invalid")
                );
                return Ok(self.rejected(c, &reason));
            }
        };
        let query = p.query.as_deref().unwrap_or("").trim();
        let read = self.source.search(p.rvc, query, p.limit, ctx)?;
        if let Some(refusal) = &read.refusal {
            self.log.warn(
                "fo.inhouse_search {Id}: rvc {Rvc} refused: {Reason}",
                &[
                    Value::from(c.command_id),
                    Value::from(p.rvc),
                    Value::from(refusal.as_str()),
                ],
            );
            return Ok(self.rejected(c, refusal));
        }
        self.log.info(
            "fo.inhouse_search {Id}: rvc {Rvc}: {Count} in-house folio(s)",
            &[
                Value::from(c.command_id),
                Value::from(p.rvc),
                Value::from(read.guests.len() as i64),
            ],
        );
        let result = FoInHouseSearchResult {
            rvc: p.rvc,
            guests: read.guests,
        };
        Ok(SyncCommandResult {
            command_id: c.command_id,
            status: CommandStatus::Applied,
            reason: None,
            result: Some(result.to_element()),
            at_utc: utc_now(&self.clock),
            exec_ms: None,
            hops: Default::default(),
        })
    }

    fn rejected(&self, c: &SyncCommand, reason: &str) -> SyncCommandResult {
        SyncCommandResult {
            command_id: c.command_id,
            status: CommandStatus::Rejected,
            reason: Some(reason.to_owned()),
            result: None,
            at_utc: utc_now(&self.clock),
            exec_ms: None,
            hops: Default::default(),
        }
    }
}

/// The Front Office database when `dbo.Para` names none (`SqlTxnRoomChargeUnit.DefaultDatabase`).
pub const DEFAULT_DATABASE: &str = "SMILE_FO";

/// The RVC's configured Front Office database (`SqlTxnRoomChargeUnit.InterfaceDbSql`).
pub const INTERFACE_DB_SQL: &str = "SELECT TOP 1 LTRIM(RTRIM(ParaStr)) FROM dbo.Para WHERE ParaName = 'InterfaceDB' AND RVC = @Rvc;";

/// `HAS_DBACCESS`, not `DB_ID` (G-03): only 1 (usable) resolves
/// (`SqlTxnRoomChargeUnit.DatabaseAccessSql`).
pub const DATABASE_ACCESS_SQL: &str = "SELECT HAS_DBACCESS(@Name);";

/// The selected columns (`SqlFoInHouseSource.Columns`).
macro_rules! columns {
    () => {
        "FolioNum, RTRIM(RoomCode) AS RoomCode,
       CAST(LTRIM(RTRIM(COALESCE(FirstName, '') + ' ' + COALESCE(LastName, ''))) AS varbinary(400)) AS GuestName,
       ArrivalDate, DepartureDate"
    };
}

/// The selected columns (`SqlFoInHouseSource.Columns`).
pub const COLUMNS: &str = columns!();

/// A room or folio number query (`SqlFoInHouseSource.Sql`): FolioStatus 2 = in house; the
/// trimmed room or the folio number matches exactly; `{0}` is the bracketed Front Office
/// database.
pub const SEARCH_SQL: &str = concat!(
    "SELECT TOP (@Limit) ",
    columns!(),
    "
  FROM {0}.dbo.Folio
 WHERE FolioStatus = 2
   AND (RTRIM(RoomCode) = @Q OR CAST(FolioNum AS varchar(20)) = @Q)
 ORDER BY RoomCode, FolioNum;"
);

/// A name query (`SqlFoInHouseSource.SqlInHouse`): names are TCVN3 bytes and the user types
/// Unicode, so SQL cannot match them; every in-house folio is read (at most
/// [`IN_HOUSE_CAP`]) and [`fo_inhouse_match::filter`](super::fo_inhouse_match::filter)
/// ranks the decoded names.
pub const IN_HOUSE_SQL: &str = concat!(
    "SELECT TOP (@Limit) ",
    columns!(),
    "
  FROM {0}.dbo.Folio
 WHERE FolioStatus = 2
 ORDER BY RoomCode, FolioNum;"
);

/// Most folios a name search reads (`SqlFoInHouseSource.InHouseCap`).
pub const IN_HOUSE_CAP: i32 = 500;

/// `SqlFoInHouseSource.TimeoutSeconds` of the search.
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(5);

/// The statement and parameters of a search: [`SEARCH_SQL`] with `@Limit` int and `@Q`
/// varchar for a number, else [`IN_HOUSE_SQL`] with `@Limit` = [`IN_HOUSE_CAP`].
pub fn search_statement(query: &str, limit: i32) -> (&'static str, Vec<Param>) {
    if fo_inhouse_match::is_number(query) {
        (
            SEARCH_SQL,
            vec![
                Param::new("Limit", limit),
                Param::new("Q", ParamValue::VarChar(Some(query.to_owned()))),
            ],
        )
    } else {
        (IN_HOUSE_SQL, vec![Param::new("Limit", IN_HOUSE_CAP)])
    }
}

/// `^[A-Za-z0-9_]+$`: the only database names that are interpolated (bracketed).
pub fn is_database_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// `SqlTxnRoomChargeUnit.ResolveDatabaseAsync`: the RVC's `dbo.Para InterfaceDB` (default
/// [`DEFAULT_DATABASE`]) when it is a plain name the login may use, else `None`.
pub fn resolve_database(c: &mut Client, rvc: i32, ctx: &Ctx) -> Result<Option<String>> {
    let r = sql::query(c, INTERFACE_DB_SQL, &[Param::new("Rvc", rvc)], ctx)?;
    let configured = match sql::scalar(&r) {
        Some(SqlValue::String(s)) if !s.trim().is_empty() => s.trim().to_owned(),
        _ => DEFAULT_DATABASE.to_owned(),
    };
    if !is_database_name(&configured) {
        return Ok(None);
    }
    let r = sql::query(
        c,
        DATABASE_ACCESS_SQL,
        &[sql::text_param("Name", Some(&configured))],
        ctx,
    )?;
    Ok(matches!(sql::scalar(&r), Some(SqlValue::I32(1))).then_some(configured))
}

/// `yyyy-MM-dd` of a `datetime`/`date` column, `None` for anything else (NULL).
fn day(v: Option<&SqlValue>) -> Option<String> {
    let (y, m, d) = match v? {
        SqlValue::DateTime(t) => (t.year(), u8::from(t.month()), t.day()),
        SqlValue::Date(t) => (t.year(), u8::from(t.month()), t.day()),
        _ => return None,
    };
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

/// [`FoInHouseSource`] on the Front Office database as `pda_sync` (SELECT on `Folio`,
/// granted for room charge). A fresh connection per search; it never begins a transaction
/// and writes nothing. Names are legacy varchar, read as bytes and TCVN3-decoded.
pub struct SqlFoInHouseSource {
    config: ConfigSource,
}

impl SqlFoInHouseSource {
    /// A source connecting with the settings `config` returns.
    pub fn new(config: ConfigSource) -> SqlFoInHouseSource {
        SqlFoInHouseSource { config }
    }
}

impl FoInHouseSource for SqlFoInHouseSource {
    fn search(&self, rvc: i32, query: &str, limit: i32, ctx: &Ctx) -> Result<FoInHouseRead> {
        check_ctx(ctx)?;
        let cfg = (self.config)()?;
        let mut c = Client::connect(&cfg)?;
        c.set_timeout(Some(COMMAND_TIMEOUT))?;
        let Some(fo) = resolve_database(&mut c, rvc, ctx)? else {
            return Ok(FoInHouseRead::refused(txn_reasons::FOLIO_DATABASE_INVALID));
        };
        c.set_timeout(Some(SEARCH_TIMEOUT))?;
        let (statement, params) = search_statement(query, limit);
        let text = statement.replace("{0}", &format!("[{fo}]"));
        let r = sql::query(&mut c, &text, &params, ctx)?;
        let mut guests = Vec::new();
        for row in r.rows() {
            guests.push(FoInHouseGuest {
                room_no: match sql::value(row, 1) {
                    Some(SqlValue::String(s)) => store_text::clean(s.clone()),
                    _ => String::new(),
                },
                folio_num: sql::int64(row, 0)?,
                guest_name: sql::bytes(row, 2)
                    .map(|b| store_text::clean(dl_vn::tcvn3::decode_bytes(b)))
                    .unwrap_or_default(),
                arrival_date: day(sql::value(row, 3)),
                departure_date: day(sql::value(row, 4)),
            });
        }
        if !fo_inhouse_match::is_number(query) {
            guests = fo_inhouse_match::filter(guests, query, limit);
        }
        Ok(FoInHouseRead {
            refusal: None,
            guests,
        })
    }
}
