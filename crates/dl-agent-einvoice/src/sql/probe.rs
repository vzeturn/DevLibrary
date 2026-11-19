//! Read-only probes behind the hub's check commands: the bills of a date window with their
//! collect state (`collect_dry_run`), the write-back target and the login's rights on it
//! (`writeback_probe`), and when each collector procedure last changed (selftest).
//! Nothing here writes; every statement is a SELECT. The SQL stays inside SQL Server 2005
//! syntax (no inline `DECLARE ... =`, no `CONVERT(..., 2)` on binary) so legacy sites answer.

use std::time::Duration;

use dl_tds::{Param, Row};
use dl_time::DateTime;

use super::{Sql, err};

/// One bill of a dry-run window, as the shipped select procedures filter it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowBill {
    /// FO BillID or POS check number.
    pub id: String,
    /// Bills in the whole window (the same on every row).
    pub total: i64,
    /// `dbo.MMEI_SourceHash` now.
    pub source_hash: Option<Vec<u8>>,
    /// `MMEI_Collected.Ok`; `None` = never marked.
    pub marked_ok: Option<bool>,
    /// The hash recorded when it was last pushed.
    pub stored_hash: Option<Vec<u8>>,
    /// The invoice number written back, when issued.
    pub invoice_number: Option<String>,
}

impl WindowBill {
    /// What the collector does with the bill on its next cycle: `new` (never pushed),
    /// `retry` (the last push failed), `changed` (pushed, source changed since: the hub
    /// keeps the issued version and records a new one) or `unchanged` (skipped).
    pub fn state(&self) -> &'static str {
        match (self.marked_ok, &self.stored_hash) {
            (None, _) | (Some(true), None) => "new",
            (Some(false), _) => "retry",
            (Some(true), Some(h)) if Some(h) == self.source_hash.as_ref() => "unchanged",
            (Some(true), Some(_)) => "changed",
        }
    }
}

/// The write-back target in one database, and what the agent's login may do there.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WritebackProbe {
    /// `DB_NAME()`.
    pub database: String,
    /// `SUSER_SNAME()`: the SQL login or the Windows account (the service identity).
    pub login: String,
    /// `USER_NAME()` in the database.
    pub user: String,
    /// The configured write-back procedure.
    pub procedure: String,
    /// The procedure exists.
    pub proc_exists: bool,
    /// `dbo.MMEI_Collected` exists.
    pub table_exists: bool,
    /// The login may EXECUTE the procedure.
    pub can_execute: bool,
    /// The login may UPDATE `dbo.MMEI_Collected` directly.
    pub can_update: bool,
    /// Procedure and table share an owner, so EXECUTE alone reaches the table.
    pub same_owner: bool,
    /// The login may create the procedure (the agent installs it when it is missing).
    pub can_install: bool,
    /// The database accepts writes (`Updateability` = `READ_WRITE`).
    pub read_write: bool,
    /// Rows of this source in `dbo.MMEI_Collected`.
    pub marked: Option<i64>,
    /// Of those, rows that already carry an invoice number.
    pub invoiced: Option<i64>,
    /// The asked bill's row, when a bill was named: `Some(None)` = no row.
    pub bill: Option<Option<BillMark>>,
}

/// One `dbo.MMEI_Collected` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BillMark {
    /// Last push succeeded.
    pub ok: bool,
    /// Written-back invoice number.
    pub invoice_number: Option<String>,
    /// Written-back series.
    pub ky_hieu: Option<String>,
}

impl WritebackProbe {
    /// A write-back would land: the target exists, the database is writable and the login
    /// may run the procedure that updates it.
    pub fn may_write(&self) -> bool {
        self.proc_exists
            && self.table_exists
            && self.read_write
            && self.can_execute
            && (self.same_owner || self.can_update)
    }

    /// Why a write-back would not land; `None` when [`Self::may_write`].
    pub fn blocker(&self) -> Option<String> {
        let p = &self.procedure;
        let (db, login) = (&self.database, &self.login);
        if !self.table_exists {
            Some(format!(
                "{db} has no dbo.MMEI_Collected: install the collector procedures (einvoice-enroll --force)"
            ))
        } else if !self.read_write {
            Some(format!("{db} is read-only"))
        } else if !self.proc_exists && !self.can_install {
            Some(format!(
                "{db} lacks {p} and {login} may not create it: run einvoice-enroll --force as a db owner"
            ))
        } else if self.proc_exists && !self.can_execute {
            Some(format!("{login} may not EXECUTE {p} in {db}"))
        } else if self.proc_exists && !self.same_owner && !self.can_update {
            Some(format!(
                "{p} and dbo.MMEI_Collected have different owners and {login} may not UPDATE the table"
            ))
        } else {
            None
        }
    }
}

const WINDOW_FO: &str = "SET NOCOUNT ON;
DECLARE @F datetime, @T datetime;
SET @F = @From; SET @T = @To;
SELECT k.ID, k.Total, Hash = dbo.MMEI_SourceHash('FO', k.ID), c.Ok, c.SourceHash, c.InvoiceNumber
FROM (SELECT TOP (@Limit) ID = CAST(b.BillID AS varchar(64)), Total = COUNT(*) OVER (), Ord = b.BillID
      FROM dbo.BillInfor b
      WHERE b.Printed = 1
        AND (b.Cancel = 0 OR b.Cancel IS NULL)
        AND b.BillID IS NOT NULL
        AND b.InvDate >= @F AND b.InvDate < @T
      ORDER BY b.BillID DESC) k
LEFT JOIN dbo.MMEI_Collected c ON c.Source = 'FO' AND c.ID = k.ID
ORDER BY k.Ord DESC;";

const WINDOW_POS: &str = "SET NOCOUNT ON;
DECLARE @F datetime, @T datetime;
SET @F = @From; SET @T = @To;
SELECT k.ID, k.Total, Hash = dbo.MMEI_SourceHash('POS', k.ID), c.Ok, c.SourceHash, c.InvoiceNumber
FROM (SELECT TOP (@Limit) ID = CAST(i.CheckNo AS varchar(64)), Total = COUNT(*) OVER (), Ord = i.CheckNo
      FROM dbo.Invoice i
      WHERE i.InvStatus = 0
        AND i.Status = 1
        AND (i.Refund = 0 OR i.Refund IS NULL)
        AND i.CheckNo IS NOT NULL
        AND ISNULL(i.FolioNum, 0) = 0
        AND ISNULL(i.CloseTime, i.OpenTime) >= @F AND ISNULL(i.CloseTime, i.OpenTime) < @T
      ORDER BY i.CheckNo DESC) k
LEFT JOIN dbo.MMEI_Collected c ON c.Source = 'POS' AND c.ID = k.ID
ORDER BY k.Ord DESC;";

const WRITEBACK_RIGHTS: &str = "SELECT
  Db = DB_NAME(), LoginName = SUSER_SNAME(), UserName = USER_NAME(),
  ProcExists = CAST(CASE WHEN OBJECT_ID(@Sp, 'P') IS NULL THEN 0 ELSE 1 END AS bit),
  TableExists = CAST(CASE WHEN OBJECT_ID('dbo.MMEI_Collected', 'U') IS NULL THEN 0 ELSE 1 END AS bit),
  CanExec = CAST(ISNULL(HAS_PERMS_BY_NAME(@Sp, 'OBJECT', 'EXECUTE'), 0) AS bit),
  CanUpdate = CAST(ISNULL(HAS_PERMS_BY_NAME('dbo.MMEI_Collected', 'OBJECT', 'UPDATE'), 0) AS bit),
  SameOwner = CAST(CASE WHEN OBJECTPROPERTY(OBJECT_ID(@Sp), 'OwnerId')
                          = OBJECTPROPERTY(OBJECT_ID('dbo.MMEI_Collected'), 'OwnerId')
                        THEN 1 ELSE 0 END AS bit),
  CanInstall = CAST(CASE WHEN ISNULL(HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'CREATE PROCEDURE'), 0) = 1
                          AND ISNULL(HAS_PERMS_BY_NAME('dbo', 'SCHEMA', 'ALTER'), 0) = 1
                        THEN 1 ELSE 0 END AS bit),
  ReadWrite = CAST(CASE WHEN CAST(DATABASEPROPERTYEX(DB_NAME(), 'Updateability') AS varchar(20)) = 'READ_WRITE'
                        THEN 1 ELSE 0 END AS bit);";

const WRITEBACK_MARKS: &str = "SELECT
  Marked = (SELECT COUNT(*) FROM dbo.MMEI_Collected WHERE Source = @Source),
  Invoiced = (SELECT COUNT(*) FROM dbo.MMEI_Collected WHERE Source = @Source AND InvoiceNumber IS NOT NULL);";

const BILL_MARK: &str =
    "SELECT Ok, InvoiceNumber, KyHieu FROM dbo.MMEI_Collected WHERE Source = @Source AND ID = @ID;";

const PROC_DATE: &str = "SELECT o.modify_date FROM sys.objects o WHERE o.object_id = OBJECT_ID(@Name) AND o.type IN ('P', 'FN')";

impl Sql {
    /// The bills of `[from, to)` that the shipped select procedure of `source` would
    /// consider (printed, not void, POS checks not on a folio), newest first, at most
    /// `limit`, with their current source hash and their `MMEI_Collected` mark.
    ///
    /// # Errors
    /// SQL failures (a missing `dbo.MMEI_SourceHash` means the procedures are not installed).
    pub fn window(
        &self,
        source: &str,
        from: DateTime,
        to: DateTime,
        limit: i64,
        timeout: Duration,
    ) -> Result<Vec<WindowBill>, String> {
        let q = if source == "POS" {
            WINDOW_POS
        } else {
            WINDOW_FO
        };
        let ctx = format!("agent: dry-run window ({source})");
        let rs = self
            .conn(source)
            .with(timeout, |c| {
                c.query(
                    q,
                    &[
                        Param::new("From", from),
                        Param::new("To", to),
                        Param::new("Limit", limit),
                    ],
                )
            })
            .map_err(|e| err(&ctx, e))?;
        rs.rows()
            .iter()
            .map(|r| window_bill(r).map_err(|e| err(&ctx, e)))
            .collect()
    }

    /// The write-back target of `source` and the login's rights on it; `bill` also reads
    /// that bill's mark. Read only.
    ///
    /// # Errors
    /// SQL failures or an invalid procedure name.
    pub fn writeback_probe(
        &self,
        source: &str,
        bill: Option<&str>,
        timeout: Duration,
    ) -> Result<WritebackProbe, String> {
        let sp = self.sp.writeback.clone();
        super::validate_sp_name(&sp)?;
        let ctx = format!("agent: write-back probe ({source})");
        let conn = self.conn(source);
        let rs = conn
            .with(timeout, |c| {
                c.query(WRITEBACK_RIGHTS, &[Param::new("Sp", sp.as_str())])
            })
            .map_err(|e| err(&ctx, e))?;
        let row = rs.rows().first().ok_or_else(|| format!("{ctx}: no row"))?;
        let mut p = rights(row, sp).map_err(|e| err(&ctx, e))?;
        if !p.table_exists {
            return Ok(p);
        }
        let rs = conn
            .with(timeout, |c| {
                c.query(WRITEBACK_MARKS, &[Param::new("Source", source)])
            })
            .map_err(|e| err(&ctx, e))?;
        if let Some(r) = rs.rows().first() {
            p.marked = Some(r.get::<i64>(0).map_err(|e| err(&ctx, e))?);
            p.invoiced = Some(r.get::<i64>(1).map_err(|e| err(&ctx, e))?);
        }
        if let Some(id) = bill {
            let rs = conn
                .with(timeout, |c| {
                    c.query(
                        BILL_MARK,
                        &[Param::new("Source", source), Param::new("ID", id)],
                    )
                })
                .map_err(|e| err(&ctx, e))?;
            let mark = match rs.rows().first() {
                None => None,
                Some(r) => Some(BillMark {
                    ok: r.get::<bool>(0).map_err(|e| err(&ctx, e))?,
                    invoice_number: r.get::<Option<String>>(1).map_err(|e| err(&ctx, e))?,
                    ky_hieu: r.get::<Option<String>>(2).map_err(|e| err(&ctx, e))?,
                }),
            };
            p.bill = Some(mark);
        }
        Ok(p)
    }

    /// When each of `names` last changed in the database of `source` (`None` = missing):
    /// the installed version of the collector procedures, as far as SQL Server records it.
    ///
    /// # Errors
    /// SQL failures.
    pub fn proc_dates(
        &self,
        source: &str,
        names: &[&str],
        timeout: Duration,
    ) -> Result<Vec<(String, Option<DateTime>)>, String> {
        let mut out = Vec::new();
        for name in names.iter().filter(|n| !n.is_empty()) {
            let rs = self
                .conn(source)
                .with(timeout, |c| {
                    c.query(PROC_DATE, &[Param::new("Name", *name)])
                })
                .map_err(|e| err(&format!("agent: look up {name} ({source})"), e))?;
            let at = match rs.rows().first() {
                Some(r) => Some(
                    r.get::<DateTime>(0)
                        .map_err(|e| err("agent: scan procedure date", e))?,
                ),
                None => None,
            };
            out.push(((*name).to_owned(), at));
        }
        Ok(out)
    }
}

fn window_bill(r: &Row) -> Result<WindowBill, dl_tds::Error> {
    Ok(WindowBill {
        id: r.get::<String>(0)?,
        total: r.get::<i64>(1)?,
        source_hash: r.get::<Option<Vec<u8>>>(2)?,
        marked_ok: r.get::<Option<bool>>(3)?,
        stored_hash: r.get::<Option<Vec<u8>>>(4)?,
        invoice_number: r.get::<Option<String>>(5)?,
    })
}

fn rights(r: &Row, procedure: String) -> Result<WritebackProbe, dl_tds::Error> {
    Ok(WritebackProbe {
        database: r.get::<String>(0)?,
        login: r.get::<Option<String>>(1)?.unwrap_or_default(),
        user: r.get::<Option<String>>(2)?.unwrap_or_default(),
        procedure,
        proc_exists: r.get::<bool>(3)?,
        table_exists: r.get::<bool>(4)?,
        can_execute: r.get::<bool>(5)?,
        can_update: r.get::<bool>(6)?,
        same_owner: r.get::<bool>(7)?,
        can_install: r.get::<bool>(8)?,
        read_write: r.get::<bool>(9)?,
        ..WritebackProbe::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bill(marked_ok: Option<bool>, stored: Option<&[u8]>) -> WindowBill {
        WindowBill {
            id: "1".into(),
            total: 1,
            source_hash: Some(vec![1, 2]),
            marked_ok,
            stored_hash: stored.map(<[u8]>::to_vec),
            invoice_number: None,
        }
    }

    #[test]
    fn window_states_follow_the_select_procedures() {
        assert_eq!(bill(None, None).state(), "new");
        assert_eq!(bill(Some(true), None).state(), "new", "no hash: re-pushed");
        assert_eq!(bill(Some(false), Some(&[1, 2])).state(), "retry");
        assert_eq!(bill(Some(true), Some(&[1, 2])).state(), "unchanged");
        assert_eq!(bill(Some(true), Some(&[9])).state(), "changed");
    }

    fn ready() -> WritebackProbe {
        WritebackProbe {
            database: "SMILE_FO".into(),
            login: "NT AUTHORITY\\SYSTEM".into(),
            procedure: "sp_ApplyInvoiceWriteback".into(),
            proc_exists: true,
            table_exists: true,
            can_execute: true,
            same_owner: true,
            read_write: true,
            ..WritebackProbe::default()
        }
    }

    #[test]
    fn writeback_needs_execute_and_an_ownership_chain_or_update() {
        assert!(ready().may_write());
        assert_eq!(ready().blocker(), None);
        let no_exec = WritebackProbe {
            can_execute: false,
            ..ready()
        };
        assert!(!no_exec.may_write());
        assert!(no_exec.blocker().unwrap().contains("EXECUTE"));
        let broken_chain = WritebackProbe {
            same_owner: false,
            ..ready()
        };
        assert!(!broken_chain.may_write());
        assert!(broken_chain.blocker().unwrap().contains("UPDATE"));
        let direct = WritebackProbe {
            can_update: true,
            ..broken_chain
        };
        assert!(direct.may_write());
    }

    #[test]
    fn a_missing_procedure_blocks_only_without_install_rights() {
        let missing = WritebackProbe {
            proc_exists: false,
            can_execute: false,
            same_owner: false,
            ..ready()
        };
        assert!(!missing.may_write(), "nothing to run until it is installed");
        assert!(missing.blocker().unwrap().contains("may not create"));
        let installable = WritebackProbe {
            can_install: true,
            ..missing
        };
        assert_eq!(
            installable.blocker(),
            None,
            "installed on the first write-back"
        );
    }

    #[test]
    fn a_missing_table_or_read_only_database_blocks() {
        let no_table = WritebackProbe {
            table_exists: false,
            ..ready()
        };
        assert!(no_table.blocker().unwrap().contains("MMEI_Collected"));
        let ro = WritebackProbe {
            read_write: false,
            ..ready()
        };
        assert!(ro.blocker().unwrap().contains("read-only"));
    }
}
