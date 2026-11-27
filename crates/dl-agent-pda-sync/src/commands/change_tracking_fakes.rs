//! Hand-written fakes of the Change Tracking seams (RT-1318), shared by the tests of the
//! round, of the service check and of the `grants` step.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::bootstrap::admin::SqlAdminCredential;
use crate::bootstrap::change_tracking::{CtProbe, CtStep, CtTable, StepState};
use crate::bootstrap::change_tracking_sql::{ChangeTrackingProbe, CtExecutor, CtSession};
use crate::bootstrap::error::Error;
use crate::config::AgentError;
use crate::hosting::enroll_attempts::Attempt;
use crate::reporting::{SiteEvents, SqlFailure};
use crate::security::{SecretError, SecretStore, names};
use crate::util::ctx::Ctx;

/// The SMILE default password of the fake identities (never in an outcome).
pub const PASSWORD: &str = "Sm1le-default-pw";

type Answer = dyn Fn(&str, &CtStep) -> Result<StepState, Error> + Send + Sync;
type Opener = dyn Fn(&str) -> Result<(), Error> + Send + Sync;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Answers each identity's connection and steps, and records `identity: step`.
pub struct FakeExec {
    open: Box<Opener>,
    answer: Arc<Answer>,
    ran: Arc<Mutex<Vec<String>>>,
}

struct FakeSession {
    who: String,
    answer: Arc<Answer>,
    ran: Arc<Mutex<Vec<String>>>,
}

fn who(c: &SqlAdminCredential) -> String {
    c.user.clone().unwrap_or_else(|| "windows".into())
}

impl CtExecutor for FakeExec {
    fn open(
        &self,
        credential: &SqlAdminCredential,
        _instance: &str,
        _database: &str,
        _ctx: &Ctx,
    ) -> Result<Box<dyn CtSession>, Error> {
        let who = who(credential);
        lock(&self.ran).push(format!("{who}: open"));
        (self.open)(&who)?;
        Ok(Box::new(FakeSession {
            who,
            answer: self.answer.clone(),
            ran: self.ran.clone(),
        }))
    }
}

impl CtSession for FakeSession {
    fn run(&mut self, step: &CtStep, _ctx: &Ctx) -> Result<StepState, Error> {
        lock(&self.ran).push(format!("{}: {}", self.who, step.describe()));
        (self.answer)(&self.who, step)
    }
}

/// An executor whose connections answer `open(identity)` and whose steps answer
/// `answer(identity, step)`; the identity is `windows` or the SQL login.
pub fn exec(
    open: impl Fn(&str) -> Result<(), Error> + Send + Sync + 'static,
    answer: impl Fn(&str, &CtStep) -> Result<StepState, Error> + Send + Sync + 'static,
) -> FakeExec {
    FakeExec {
        open: Box::new(open),
        answer: Arc::new(answer),
        ran: Arc::new(Mutex::new(Vec::new())),
    }
}

/// An executor where every identity connects and every step is done.
pub fn done() -> FakeExec {
    exec(|_| Ok(()), |_, _| Ok(StepState::Done))
}

/// What `e` was asked, in order.
pub fn ran(e: &FakeExec) -> Vec<String> {
    lock(&e.ran).clone()
}

/// The ADR 0010 order: Windows, then the SMILE default login.
pub fn attempts() -> Vec<Attempt> {
    vec![
        Attempt::Try {
            label: "windows".into(),
            credential: SqlAdminCredential::windows(),
        },
        Attempt::Try {
            label: "sql:smile".into(),
            credential: SqlAdminCredential::sql("smile", PASSWORD),
        },
    ]
}

/// A connection refused with the agent code `code`.
pub fn refused(code: &str) -> Error {
    Error::Agent(AgentError::new(code, "refused"))
}

/// An escaped `SqlException` with SQL Server error `number`.
pub fn sql(number: i32) -> Error {
    Error::sql_exception(&SqlFailure::single(number, 14, "server message"))
}

/// A present captured table.
pub fn t(name: &str, has_pk: bool, tracked: bool, can_view: bool) -> CtTable {
    CtTable {
        name: name.into(),
        present: true,
        has_pk,
        tracked,
        can_view,
    }
}

/// A database without Change Tracking: two keyed tables and one without a key.
pub fn fresh() -> CtProbe {
    CtProbe {
        database_enabled: false,
        current_version: None,
        tables: vec![
            t("TmpInvoice", true, false, false),
            t("Trn", true, false, false),
            t("CashierRVC", false, false, false),
        ],
    }
}

/// The same tables once enabled and granted.
pub fn tracked() -> CtProbe {
    CtProbe {
        database_enabled: true,
        current_version: Some(42),
        tables: vec![
            t("TmpInvoice", true, true, true),
            t("Trn", true, true, true),
            t("CashierRVC", false, false, false),
        ],
    }
}

type ProbeAnswer = dyn Fn() -> Result<CtProbe, Error> + Send + Sync;

/// A probe that answers `answer` and counts its calls.
pub struct FakeProbe {
    answer: Box<ProbeAnswer>,
    calls: AtomicUsize,
}

impl FakeProbe {
    /// A probe answering `answer` each time.
    pub fn new(answer: impl Fn() -> Result<CtProbe, Error> + Send + Sync + 'static) -> FakeProbe {
        FakeProbe {
            answer: Box::new(answer),
            calls: AtomicUsize::new(0),
        }
    }

    /// How often it was asked.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl ChangeTrackingProbe for FakeProbe {
    fn probe(
        &self,
        _instance: &str,
        _database: &str,
        _password: &str,
        _tables: &[String],
        _ctx: &Ctx,
    ) -> Result<CtProbe, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        (self.answer)()
    }
}

/// The site events reported, as `<level> <code>: <message>`.
#[derive(Default)]
pub struct FakeEvents(Mutex<Vec<String>>);

impl FakeEvents {
    /// Every event so far.
    pub fn lines(&self) -> Vec<String> {
        lock(&self.0).clone()
    }
}

impl SiteEvents for FakeEvents {
    fn report(&self, level: &str, code: &str, message: &str, _detail: Option<dl_json::Map>) {
        lock(&self.0).push(format!("{level} {code}: {message}"));
    }
}

/// A secret store holding the `pda_sync` password, or nothing.
pub struct FakeSecrets(pub Option<&'static str>);

impl SecretStore for FakeSecrets {
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>, SecretError> {
        Ok(self
            .0
            .filter(|_| name == names::SQL_PASSWORD)
            .map(|p| p.as_bytes().to_vec()))
    }

    fn save(&self, _name: &str, _value: &[u8]) -> Result<(), SecretError> {
        Ok(())
    }

    fn delete(&self, _name: &str) -> Result<(), SecretError> {
        Ok(())
    }
}
