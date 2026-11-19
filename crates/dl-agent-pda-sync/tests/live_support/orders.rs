//! The command harness of the live order tests (B9): a [`SmilePos`] bootstrapped by the
//! real `010`–`090` scripts as the administrator, then every command through the
//! production [`SiteCommandExecutor`] and [`SqlSiteCommandStore`] as the `pda_sync` login
//! those scripts create (port of the C# `Sql*IntegrationTests` set-up, on the faithful
//! SMILE_POS fixture instead of per-class stand-in tables).
//!
//! [`Live::once`] delivers a command twice and holds the exactly-once contract: the same
//! answer both times and exactly one new `Pda_SyncCommandLog` row.

use std::str::FromStr;
use std::sync::Arc;

use dl_agent_pda_sync::bootstrap::admin::SqlServerAdmin;
use dl_agent_pda_sync::bootstrap::discovery::LocalSqlDiscovery;
use dl_agent_pda_sync::bootstrap::{SchemaFingerprint, SqlBootstrapper};
use dl_agent_pda_sync::commands::{ConfigSource, SiteCommandExecutor, SqlSiteCommandStore};
use dl_agent_pda_sync::config::sql::agent_connection;
use dl_agent_pda_sync::config::{AgentPaths, SystemClock};
use dl_agent_pda_sync::contracts::catalog::SyncCatalog;
use dl_agent_pda_sync::contracts::wire::{
    CheckStateFingerprint, CheckStateHeader, CheckStateLine, CommandStatus, OrderCheckResult,
    OrderMultiCheckResult, SyncCommand, SyncCommandResult,
};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::security::{FileSecretStore, SharedSecretStore, names};
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_decimal::Decimal;
use dl_tds::{Param, Value};

use super::{SmilePos, admin_url};

/// The clerk every live command runs as.
pub const CLERK: &str = "clerk-7";

/// A bootstrapped SMILE_POS and the `pda_sync` connection to it.
pub struct Live {
    /// The database (dropped with its `pda_sync` login at the end).
    pub pos: SmilePos,
    /// `pda_sync` with the password the bootstrap stored, `Encrypt=Required`.
    pub agent: dl_tds::Config,
    dir: std::path::PathBuf,
}

impl Drop for Live {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.dir) {
            eprintln!("cleanup of {} failed: {e}", self.dir.display());
        }
    }
}

/// Decimal from text (test literals).
pub fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

/// A command as the cloud sends it.
pub fn command(
    type_: &str,
    payload: dl_json::Value,
    aggregate: &str,
    expected: Option<&str>,
) -> SyncCommand {
    SyncCommand {
        command_id: dl_uuid::Uuid::new_v4(),
        type_: type_.to_owned(),
        aggregate_key: aggregate.to_owned(),
        expected_hash: expected.map(str::to_owned),
        payload,
        created_by: CLERK.to_owned(),
        expires_at_utc: None,
        contract_version: 1,
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
    }
}

/// `value` as the C# tests print it: trimmed text, `1`/`0` for bits, empty for NULL.
fn cell(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::Bool(b)) => if *b { "1" } else { "0" }.to_owned(),
        Some(Value::String(s)) => s.trim().to_owned(),
        Some(Value::Decimal(d)) => d.normalize().to_string(),
        Some(Value::F64(f)) => f.to_string(),
        Some(Value::F32(f)) => f.to_string(),
        Some(Value::I8(n)) => n.to_string(),
        Some(Value::I16(n)) => n.to_string(),
        Some(Value::I32(n)) => n.to_string(),
        Some(Value::I64(n)) => n.to_string(),
        Some(other) => format!("{other:?}"),
    }
}

fn number(value: Option<&Value>) -> Decimal {
    match value {
        Some(Value::Decimal(d)) => *d,
        Some(Value::I32(n)) => Decimal::from(i64::from(*n)),
        Some(Value::I64(n)) => Decimal::from(*n),
        None | Some(Value::Null) => Decimal::ZERO,
        Some(other) => panic!("not a number: {other:?}"),
    }
}

impl Live {
    /// The fixture, bootstrapped; `None` (skipped) without `PDA_SYNC_TEST_SQL` or when the
    /// server already has a `pda_sync` login.
    pub fn start() -> Option<Live> {
        let Some(url) = admin_url() else {
            eprintln!("skipped: PDA_SYNC_TEST_SQL is not set");
            return None;
        };
        let pos = SmilePos::create(&url)?;
        let dir = std::env::temp_dir()
            .join("pda-sync-agent-tests")
            .join(dl_uuid::Uuid::new_v4().simple().to_string());
        let paths = AgentPaths::new(&dir);
        paths.ensure_created().unwrap();
        let secrets: SharedSecretStore = Arc::new(FileSecretStore::new(paths.secrets_dir()));
        let log = Logger::none();
        let report = SqlBootstrapper::new(
            Arc::new(LocalSqlDiscovery::new(&log)),
            Arc::new(SqlServerAdmin::new(pos.admin_credential())),
            SchemaFingerprint::new(SyncCatalog::load().unwrap(), 0.85),
            secrets.clone(),
            &log,
        )
        .run(
            false,
            Some(&pos.instance),
            Some(&pos.database),
            &Ctx::background(),
        )
        .unwrap_or_else(|e| panic!("bootstrap: {e}"));
        assert!(
            report.steps.iter().all(|s| s.executed),
            "{:?}",
            report.steps
        );
        let pw = String::from_utf8(secrets.load(names::SQL_PASSWORD).unwrap().unwrap()).unwrap();
        let agent = agent_connection(&pos.instance, &pos.database, &pw);
        Some(Live { pos, agent, dir })
    }

    /// A fresh executor over a fresh store, as each C# test builds one per call.
    pub fn executor(&self) -> SiteCommandExecutor {
        let cfg = self.agent.clone();
        let config: ConfigSource = Arc::new(move || Ok(cfg.clone()));
        SiteCommandExecutor::new(
            Arc::new(SqlSiteCommandStore::new(config)),
            SystemClock::shared(),
            &Logger::none(),
        )
    }

    /// One delivery.
    pub fn run(&self, c: &SyncCommand) -> SyncCommandResult {
        self.executor()
            .execute(c, &Ctx::background())
            .unwrap_or_else(|e| panic!("{}: {e}", c.type_))
    }

    /// Two deliveries of the same command id: the same answer, one journal row, and the
    /// logged outcome is that answer.
    pub fn once(&self, c: &SyncCommand) -> SyncCommandResult {
        let before = self.logged();
        let first = self.run(c);
        let again = self.run(c);
        assert_eq!(
            again, first,
            "{}: the journal answers the redelivery",
            c.type_
        );
        assert_eq!(self.logged(), before + 1, "{}: one journal row", c.type_);
        let recorded = self
            .executor()
            .find_recorded(c.command_id, &Ctx::background())
            .unwrap();
        assert_eq!(recorded.as_ref(), Some(&first), "{}", c.type_);
        first
    }

    /// Applied exactly once; the check it answers with.
    pub fn applied(&self, c: &SyncCommand) -> OrderCheckResult {
        let r = self.once(c);
        assert_eq!(
            r.status,
            CommandStatus::Applied,
            "{}: {:?}",
            c.type_,
            r.reason
        );
        OrderCheckResult::result_from(r.result.as_ref()).expect("an order check result")
    }

    /// Applied exactly once; the checks it answers with.
    pub fn applied_multi(&self, c: &SyncCommand) -> OrderMultiCheckResult {
        let r = self.once(c);
        assert_eq!(
            r.status,
            CommandStatus::Applied,
            "{}: {:?}",
            c.type_,
            r.reason
        );
        OrderMultiCheckResult::multi_result_from(r.result.as_ref()).expect("a multi-check result")
    }

    /// A logged business refusal (answered the same on redelivery).
    pub fn refused(&self, c: &SyncCommand, reason: &str) {
        let r = self.once(c);
        assert_eq!(r.status, CommandStatus::Rejected, "{}", c.type_);
        assert_eq!(r.reason.as_deref(), Some(reason), "{}", c.type_);
    }

    /// The `Pda_SyncCommandLog` rows.
    pub fn logged(&self) -> i64 {
        self.pos
            .scalar_i64("SELECT CAST(COUNT(*) AS bigint) FROM dbo.Pda_SyncCommandLog")
    }

    fn admin_client(&self) -> dl_tds::Client {
        dl_tds::Client::connect(&self.pos.admin.clone().with_database(&self.pos.database)).unwrap()
    }

    /// Runs `sql` with `params` as the administrator (POS WIN's own writes).
    pub fn admin(&self, sql: &str, params: &[Param]) {
        let mut c = self.admin_client();
        c.query(sql, params)
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
        c.close();
    }

    /// Each row of `sql` as its trimmed values joined by `|`, read as the administrator.
    pub fn rows(&self, sql: &str, params: &[Param]) -> Vec<String> {
        let mut c = self.admin_client();
        let r = c
            .query(sql, params)
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
        let out = r
            .rows()
            .iter()
            .map(|row| {
                (0..row.values().len())
                    .map(|i| cell(row.value(i)))
                    .collect::<Vec<_>>()
                    .join("|")
            })
            .collect();
        c.close();
        out
    }

    /// The single value of `sql`.
    pub fn one(&self, sql: &str, params: &[Param]) -> String {
        let rows = self.rows(sql, params);
        assert_eq!(rows.len(), 1, "{sql}: {rows:?}");
        rows.into_iter().next().unwrap_or_default()
    }

    /// A `Para` counter (`LastCheck`, `LastTrn`) of `rvc`.
    pub fn para(&self, rvc: i32, name: &str) -> i64 {
        self.one(
            "SELECT ParaStr FROM dbo.Para WHERE RVC = @rvc AND ParaName = @name",
            &[Param::new("rvc", rvc), Param::new("name", name)],
        )
        .parse()
        .unwrap()
    }

    /// The check state as the cloud saw it last (ADR 0100), read as the administrator:
    /// what a user who looks at the check now would send as its expected state.
    pub fn seen(&self, check: i64) -> String {
        let mut c = self.admin_client();
        let r = c
            .query(
                "SELECT CAST(CheckNo AS bigint), TableNo, CAST(ISNULL(NGuest, 0) AS int), SubTotal, TotalPaid \
                 FROM dbo.tmpInvoice WHERE CheckNo = @c; \
                 SELECT CAST(TrnSeq AS bigint), CONVERT(varchar(12), TrnCode), ItemCode, \
                 CAST(TrnQty AS decimal(38, 6)), ItemPrice FROM dbo.tmpTrn WHERE CheckNo = @c;",
                &[Param::new("c", check)],
            )
            .unwrap();
        let sets = r.sets();
        let head = &sets[0].rows()[0];
        let header = CheckStateHeader {
            check_no: head.get(0).unwrap(),
            table_no: head.get::<Option<String>>(1).unwrap(),
            guests: head.get(2).unwrap(),
            sub_total: number(head.value(3)),
            total_paid: number(head.value(4)),
        };
        let lines: Vec<CheckStateLine> = sets[1]
            .rows()
            .iter()
            .map(|row| CheckStateLine {
                line_seq: row.get(0).unwrap(),
                transaction_code: row.get::<Option<String>>(1).unwrap(),
                item_code: row.get::<Option<String>>(2).unwrap(),
                qty: number(row.value(3)),
                price: number(row.value(4)),
            })
            .collect();
        c.close();
        CheckStateFingerprint::compute(&header, &lines)
    }
}
