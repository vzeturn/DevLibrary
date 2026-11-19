//! Schema probe of the table and multi-check verbs (`TableSchemaGuard.cs`, OW1e ADR 0112
//! §2). Before a verb runs its legacy batch, the agent checks that every store column the
//! batch reads or writes exists in this SMILE_POS build (`INFORMATION_SCHEMA.COLUMNS`,
//! read only). A missing column refuses the verb with `schema_unsupported`: the agent
//! never guesses another column. A build that has them is remembered for the life of the
//! process; one that lacks them is probed again on the next command (an upgraded POS
//! needs no restart) and reported once per verb per process (`SCHEMA_UNSUPPORTED`).

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

use super::error::{CommandError, Result};
use super::units::{StoreColumn, TableCommandUnit};
use crate::config::codes;
use crate::contracts::wire::{site_event_levels, table_command_types as t};
use crate::reporting::{Logger, SiteEvents, Value};
use crate::util::ctx::Ctx;

const fn c(table: &'static str, column: &'static str) -> StoreColumn {
    StoreColumn::new(table, column)
}

const TABLE_CHANGE: &[StoreColumn] = &[
    c("tmpInvoice", "TableNo"),
    c("tmpInvoice", "NGuest"),
    c("tmpInvoice", "NChild"),
    c("tmpInvoice", "CheckNo"),
    c("tmpInvoice", "RVC"),
    c("tablelist", "TableNum"),
    c("tablelist", "RVC"),
    c("tablelist", "TableStatus"),
];

const TABLE_TRANSFER: &[StoreColumn] = &[
    c("TmpInvoice", "TableNo"),
    c("TmpInvoice", "CheckNo"),
    c("TmpInvoice", "RVC"),
    c("TmpInvoice", "Status"),
    c("tmpTrnInfo", "TableNo"),
    c("tmpTrnInfo", "CheckNo"),
];

const CHECK_MERGE: &[StoreColumn] = &[
    c("TmpInvoice", "CheckNo"),
    c("TmpInvoice", "RVC"),
    c("TmpInvoice", "Status"),
    c("TmpInvoice", "TotalPaid"),
    c("TmpTrn", "CheckNo"),
    c("TmpTrn", "RVC"),
    c("TmpTrn", "TrnCode"),
    c("tmpTrnInfo", "CheckNo"),
    c("tmpInvoiceInfo", "CheckNo"),
    c("checkopenlog", "isOpen"),
    c("checkopenlog", "CloseTime"),
    c("checkopenlog", "LastAction"),
    c("checkopenlog", "CheckNo"),
    c("checkopenlog", "RVC"),
];

const ITEMS_MOVE: &[StoreColumn] = &[
    c("TmpInvoice", "CheckNo"),
    c("TmpInvoice", "RVC"),
    c("TmpInvoice", "Status"),
    c("TmpInvoice", "TotalPaid"),
    c("TmpTrn", "TrnSeq"),
    c("TmpTrn", "CheckNo"),
    c("TmpTrn", "RVC"),
    c("TmpTrn", "TrnCode"),
    c("tmpTrnInfo", "TrnNo"),
    c("tmpTrnInfo", "CheckNo"),
];

/// The store columns each verb's legacy batch touches (`TableSchemaGuard.Columns`).
pub fn columns(verb: &str) -> Option<&'static [StoreColumn]> {
    match verb {
        t::TABLE_CHANGE => Some(TABLE_CHANGE),
        t::TABLE_TRANSFER => Some(TABLE_TRANSFER),
        t::CHECK_MERGE => Some(CHECK_MERGE),
        t::ITEMS_MOVE => Some(ITEMS_MOVE),
        _ => None,
    }
}

/// Category of the guard's log lines.
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.Commands.TableSchemaGuard";

/// The schema probe; shared by every command of the process.
pub struct TableSchemaGuard {
    events: Arc<dyn SiteEvents>,
    log: Logger,
    supported: Mutex<HashSet<String>>,
    reported: Mutex<HashSet<String>>,
}

impl TableSchemaGuard {
    /// A guard reporting through `events`; `log` is the root logger.
    pub fn new(events: Arc<dyn SiteEvents>, log: &Logger) -> TableSchemaGuard {
        TableSchemaGuard {
            events,
            log: log.for_category(CATEGORY),
            supported: Mutex::new(HashSet::new()),
            reported: Mutex::new(HashSet::new()),
        }
    }

    /// A guard that probes and refuses like the host's one but only logs its event
    /// (`TableSchemaGuard.LogOnly`, an executor built without one). `log` is the
    /// executor's logger, which writes the event line.
    pub fn log_only(log: &Logger) -> TableSchemaGuard {
        TableSchemaGuard {
            events: Arc::new(LogOnlyEvents(log.clone())),
            log: Logger::none(),
            supported: Mutex::new(HashSet::new()),
            reported: Mutex::new(HashSet::new()),
        }
    }

    /// The columns of `verb` missing in this store, probed in the command's transaction;
    /// empty when the verb can run. Reports the first refusal of each verb as a site
    /// event.
    pub fn missing(
        &self,
        verb: &str,
        unit: &mut dyn TableCommandUnit,
        ctx: &Ctx,
    ) -> Result<Vec<String>> {
        if lock(&self.supported).contains(verb) {
            return Ok(Vec::new());
        }
        let Some(cols) = columns(verb) else {
            return Err(CommandError::Argument(format!(
                "No schema columns are declared for verb {verb}. (Parameter 'verb')"
            )));
        };
        let missing = unit.missing_columns(cols, ctx)?;
        if missing.is_empty() {
            lock(&self.supported).insert(verb.to_owned());
            return Ok(Vec::new());
        }
        self.log.warn(
            "Command {Type} refused: SMILE_POS lacks {Missing}",
            &[Value::from(verb), Value::from(missing.join(", "))],
        );
        if lock(&self.reported).insert(verb.to_owned()) {
            let mut detail = dl_json::Map::new();
            detail.insert("verb".to_owned(), dl_json::Value::String(verb.to_owned()));
            detail.insert(
                "missing".to_owned(),
                dl_json::Value::Array(
                    missing
                        .iter()
                        .map(|m| dl_json::Value::String(m.clone()))
                        .collect(),
                ),
            );
            self.events.report(
                site_event_levels::WARN,
                codes::SCHEMA_UNSUPPORTED,
                &format!(
                    "The store database lacks columns the {verb} command writes; the command is refused until POS is upgraded"
                ),
                Some(detail),
            );
        }
        Ok(missing)
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Site events that are only logged (`TableSchemaGuard.LogOnlyEvents`): what a component
/// built without the host's site events reports.
pub struct LogOnlyEvents(pub Logger);

impl SiteEvents for LogOnlyEvents {
    fn report(&self, level: &str, code: &str, message: &str, _detail: Option<dl_json::Map>) {
        self.0.warn(
            "Site event {Code} ({Level}) not sent: {Message}",
            &[Value::from(code), Value::from(level), Value::from(message)],
        );
    }
}
