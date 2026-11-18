//! Port of `TestLegacySQLLive`: installs the collect scripts into a database running at
//! compatibility level 100, where SQL Server rejects `TRY_CONVERT` exactly as a 2008
//! engine does, and exercises the installed fingerprint and procedures. Needs a SQL
//! Server sysadmin URL in `MMEI_TEST_MSSQL_URL` (`sqlserver://sa:...@host:port`, no
//! `database`); skipped otherwise. It creates and drops its own database.

use std::time::Duration;

use dl_agent_einvoice::agent_config::AgentConfig;
use dl_agent_einvoice::sql::Sql;
use dl_tds::{Client, Config, Param};

const DB: &str = "mmei_legacy_live_rs";

/// The smallest SMILE-shaped schema `MMEI_SourceHash` reads (same as the Go test).
const SCHEMA: &str = "
CREATE TABLE dbo.BillInfor (BillID numeric PRIMARY KEY, FolioNum numeric, CompanyName nvarchar(200), VATCode varchar(20),
  Address1 nvarchar(200), Address2 nvarchar(200), EMail varchar(100), BillSeri1 varchar(20), BillNumber1From numeric,
  InvDate datetime, SubAmount money, ServiceCharge money, TaxAmount money, TotalAmount money, GuestName nvarchar(200),
  BillNote nvarchar(max), Cancel bit, Printed int);
CREATE TABLE dbo.GuestEveryDay (GuestID int, FolioNum numeric, FolioSubNum int, IsAdditionalName bit, ProfileID int);
CREATE TABLE dbo.Profile (ProfileNumber int, Email varchar(100));
CREATE TABLE dbo.TransactionCode (TransactionCode varchar(10), TransactionSubCode varchar(10), Description nvarchar(100), TransactionGroup int);
CREATE TABLE dbo.TaxDef (TaxCode varchar(10), TaxRate money);
CREATE TABLE dbo.FolioTransaction (TransactionID numeric PRIMARY KEY, BillID numeric, TransactionCode varchar(10),
  TransactionSubCode int, Description nvarchar(200), Quantity money, SubAmount money, TaxAmount money, STaxCode varchar(10),
  STaxAmount money, ServiceCharge money, TaxCode varchar(10), FolioBalanceCode varchar(2), isHKPost bit);
CREATE TABLE dbo.ItemPosted (TrnID numeric, ICode varchar(20), IName nvarchar(100), Price money, Qty money, Amount money);
CREATE TABLE dbo.ItemList (ICode varchar(20), IGroup varchar(20));
CREATE TABLE dbo.ItemGroup (GroupCode varchar(20), GroupName nvarchar(100));
CREATE TABLE dbo.Invoice (CheckNo numeric PRIMARY KEY, OpenTime datetime, CloseTime datetime, IYear int, IMonth int, IDay int,
  SubTotal money, TaxTotal money, STaxTotal money, TotalBase money, Discount money, SurCharge money, InvStatus int,
  Status int, TaxOmit int, RVC int, FolioNum numeric, ExRate money, BeforeTax money, Refund int);
CREATE TABLE dbo.TaxTable (TCheckNo numeric, ClientVAT varchar(20), Company nvarchar(200), ClientName nvarchar(200),
  Address nvarchar(200), InvContent nvarchar(200));
CREATE TABLE dbo.Trn (TrnSeq numeric, CheckNo numeric, TrnCode int, ItemCode varchar(20), RVC int, TrnDesc nvarchar(100),
  ItemPrice money, TrnQty money, BaseSub money, BaseTax money, TaxNo int, BaseSTax money, STaxNo int, BaseSrc money);
CREATE TABLE dbo.ItemDef (ICode varchar(20), IUnit varchar(20));
CREATE TABLE dbo.IUnitDef (UnitCode varchar(20), UnitName nvarchar(50));
INSERT dbo.BillInfor (BillID, FolioNum, CompanyName, VATCode, InvDate, SubAmount, TotalAmount, BillNote, Cancel, Printed)
  VALUES (7, 70, N'Công ty Ví Dụ', '0101243150', '2026-11-01T10:00:00', 100000, 110000, NULL, 0, 1);
INSERT dbo.TransactionCode VALUES ('110', '0', N'Room', 1);
INSERT dbo.TaxDef VALUES ('V10', 10);
INSERT dbo.FolioTransaction VALUES (501, 7, '110', 0, N'Tiền phòng', 1, 100000, 10000, NULL, 0, 0, 'V10', 'R', 0);
INSERT dbo.ItemPosted VALUES (501, 'RM', N'Phòng', 100000, 1, 100000);
INSERT dbo.Invoice (CheckNo, CloseTime, IYear, IMonth, IDay, SubTotal, TotalBase, InvStatus, Status) VALUES (9, '2026-11-01T12:00:00', 2026, 11, 1, 50000, 55000, 0, 1);
INSERT dbo.Trn VALUES (1, 9, 100, 'F1', 1, N'Phở', 50000, 1, 50000, 5000, 1, 0, 0, 50000);";

fn connect(cfg: &Config) -> Client {
    let mut c = Client::connect(&cfg.clone().with_connect_timeout(Duration::from_secs(30)))
        .expect("connect");
    c.set_timeout(Some(Duration::from_secs(120))).unwrap();
    c
}

fn exec(c: &mut Client, sql: &str) {
    if let Err(e) = c.batch(sql) {
        panic!("{}: {e}", sql.trim().lines().next().unwrap_or(""));
    }
}

#[test]
fn legacy_install_at_compatibility_level_100() {
    let Some(base) = std::env::var("MMEI_TEST_MSSQL_URL")
        .ok()
        .filter(|u| !u.is_empty())
    else {
        eprintln!("note: skipping, set MMEI_TEST_MSSQL_URL to run against a real SQL Server");
        return;
    };
    let master_cfg: Config = base.parse().expect("MMEI_TEST_MSSQL_URL");
    let mut master = connect(&master_cfg);
    let drop_db = format!(
        "IF DB_ID('{DB}') IS NOT NULL BEGIN ALTER DATABASE {DB} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {DB} END"
    );
    exec(&mut master, &drop_db);
    exec(&mut master, &format!("CREATE DATABASE {DB}"));
    struct Cleanup<'a>(&'a mut Client, String);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            if let Err(e) = self.0.batch(&self.1) {
                eprintln!("drop test database: {e}");
            }
        }
    }
    let guard = Cleanup(&mut master, drop_db);
    exec(
        guard.0,
        &format!("ALTER DATABASE {DB} SET COMPATIBILITY_LEVEL = 100"),
    );

    let sep = if base.contains('?') { '&' } else { '?' };
    let dsn = format!("{base}{sep}database={DB}");
    let mut db = connect(&dsn.parse().unwrap());
    exec(&mut db, SCHEMA);

    assert!(
        dl_agent_einvoice::legacy::needs_legacy_sql(&mut db).unwrap(),
        "want legacy at compatibility level 100"
    );
    dl_agent_einvoice::install::install_into(&mut db).expect("install at compatibility level 100");
    dl_agent_einvoice::install::install_into(&mut db).expect("re-install must be idempotent");

    let defaults = AgentConfig::default();
    Sql::new(&dsn, "", defaults.sp_names())
        .verify_fo_charge_code_sync(&defaults.charge_codes(), Duration::from_secs(60))
        .expect("drift check on the legacy install");

    let rs = db
        .query(
            "SELECT m.definition FROM sys.sql_modules m WHERE m.object_id = OBJECT_ID('dbo.MMEI_SourceHash')",
            &[],
        )
        .unwrap();
    let def: String = rs.rows()[0].get(0).unwrap();
    assert_eq!(
        dl_agent_einvoice::legacy::post2008_feature(&def),
        None,
        "installed hash function still uses 2012+ syntax"
    );

    let hash = |db: &mut Client, source: &str, id: &str| -> Vec<u8> {
        let rs = db
            .query(
                "SELECT dbo.MMEI_SourceHash(@s, @id)",
                &[Param::new("s", source), Param::new("id", id)],
            )
            .unwrap_or_else(|e| panic!("MMEI_SourceHash({source}, {id}): {e}"));
        let h: Vec<u8> = rs.rows()[0].get(0).unwrap();
        assert_eq!(h.len(), 32, "MMEI_SourceHash({source}, {id})");
        h
    };
    let changes = |db: &mut Client, label: &str, source: &str, id: &str, edit: &str| {
        let before = hash(db, source, id);
        assert_eq!(before, hash(db, source, id), "{label}: not deterministic");
        exec(db, edit);
        assert_ne!(
            before,
            hash(db, source, id),
            "{label}: edit did not change the fingerprint"
        );
    };
    changes(
        &mut db,
        "FO header",
        "FO",
        "7",
        "UPDATE dbo.BillInfor SET TotalAmount = 120000 WHERE BillID = 7",
    );
    changes(
        &mut db,
        "FO charge line",
        "FO",
        "7",
        "UPDATE dbo.FolioTransaction SET SubAmount = 90000 WHERE TransactionID = 501",
    );
    changes(
        &mut db,
        "FO tax rate",
        "FO",
        "7",
        "UPDATE dbo.TaxDef SET TaxRate = 8 WHERE TaxCode = 'V10'",
    );
    changes(
        &mut db,
        "FO item",
        "FO",
        "7",
        "UPDATE dbo.ItemPosted SET IName = N'Phòng đôi' WHERE TrnID = 501",
    );
    changes(
        &mut db,
        "POS header",
        "POS",
        "9",
        "UPDATE dbo.Invoice SET IDay = 2 WHERE CheckNo = 9",
    );
    changes(
        &mut db,
        "POS line",
        "POS",
        "9",
        "UPDATE dbo.Trn SET BaseTax = 4000 WHERE TrnSeq = 1",
    );
    // A header longer than one HASHBYTES window: the lines after it are still covered.
    exec(
        &mut db,
        "UPDATE dbo.BillInfor SET CompanyName = REPLICATE(N'x', 200), Address1 = REPLICATE(N'y', 200), BillNote = REPLICATE(CAST(N'z' AS nvarchar(max)), 6000) WHERE BillID = 7",
    );
    changes(
        &mut db,
        "FO charge line behind a long header",
        "FO",
        "7",
        "UPDATE dbo.FolioTransaction SET SubAmount = 80000 WHERE TransactionID = 501",
    );
    // Not a number: TRY_CONVERT gave NULL, so must its replacement (no conversion error).
    for id in ["abc", "", "12a", "1234567890123456789012345"] {
        hash(&mut db, "FO", id);
        hash(&mut db, "POS", id);
    }

    let pending = |db: &mut Client, sp: &str| -> Vec<String> {
        let rs = db
            .query(&format!("EXEC {sp} @Limit = 10"), &[])
            .unwrap_or_else(|e| panic!("{sp}: {e}"));
        rs.rows()
            .iter()
            .map(|r| r.get::<String>(0).unwrap())
            .collect()
    };
    assert_eq!(pending(&mut db, "dbo.sp_SelectPendingFO"), ["7"]);
    assert_eq!(pending(&mut db, "dbo.sp_SelectPendingPOS"), ["9"]);
    exec(&mut db, "EXEC dbo.sp_FetchPOS @ID = '9'");
    exec(&mut db, "EXEC dbo.sp_FetchPOS @ID = 'not-a-number'");
    exec(
        &mut db,
        "EXEC dbo.sp_MarkCollected @Source = 'POS', @ID = '9', @Ok = 1",
    );
    assert!(pending(&mut db, "dbo.sp_SelectPendingPOS").is_empty());
    exec(
        &mut db,
        "UPDATE dbo.Trn SET TrnDesc = N'Phở bò' WHERE TrnSeq = 1",
    );
    assert_eq!(
        pending(&mut db, "dbo.sp_SelectPendingPOS").len(),
        1,
        "edit must bring the check back"
    );
    exec(
        &mut db,
        "EXEC dbo.sp_MarkCollected @Source = 'FO', @ID = '7', @Ok = 1, @Message = N'ok'",
    );
    let rs = db
        .query(
            "SELECT SourceHash FROM dbo.MMEI_Collected WHERE Source = 'FO' AND ID = '7'",
            &[],
        )
        .unwrap();
    let stored: Vec<u8> = rs.rows()[0].get(0).unwrap();
    assert_eq!(
        stored,
        hash(&mut db, "FO", "7"),
        "sp_MarkCollected stored another fingerprint"
    );
    db.close();
    drop(guard);
}
