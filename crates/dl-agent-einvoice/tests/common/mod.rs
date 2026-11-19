//! Shared by the live SQL Server tests (`MMEI_TEST_MSSQL_URL`): the SMILE-shaped schema and
//! small connection helpers. Not every test uses every item.
#![allow(dead_code)]

use std::time::Duration;

use dl_tds::{Client, Config};

/// The sysadmin URL of the live tests (`sqlserver://sa:...@host:port`, no `database`), or
/// `None` (the test then skips with a note).
pub fn base_url() -> Option<String> {
    let url = std::env::var("MMEI_TEST_MSSQL_URL")
        .ok()
        .filter(|u| !u.is_empty());
    if url.is_none() {
        eprintln!("note: skipping, set MMEI_TEST_MSSQL_URL to run against a real SQL Server");
    }
    url
}

/// `base` with `database=db` added.
pub fn with_database(base: &str, db: &str) -> String {
    let sep = if base.contains('?') { '&' } else { '?' };
    format!("{base}{sep}database={db}")
}

/// A database created for one test and dropped when the guard goes.
pub struct TestDb {
    master: Client,
    drop_sql: String,
    /// The test database's URL.
    pub dsn: String,
}

impl TestDb {
    pub fn create(base: &str, name: &str) -> TestDb {
        let mut master = connect(&base.parse().expect("MMEI_TEST_MSSQL_URL"));
        let drop_sql = format!(
            "IF DB_ID('{name}') IS NOT NULL BEGIN ALTER DATABASE {name} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {name} END"
        );
        exec(&mut master, &drop_sql);
        exec(&mut master, &format!("CREATE DATABASE {name}"));
        TestDb {
            master,
            drop_sql,
            dsn: with_database(base, name),
        }
    }

    pub fn master(&mut self) -> &mut Client {
        &mut self.master
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        if let Err(e) = self.master.batch(&self.drop_sql) {
            eprintln!("drop test database: {e}");
        }
    }
}

/// The smallest SMILE-shaped schema `MMEI_SourceHash` reads (same as the Go test).
pub const SCHEMA: &str = "
CREATE TABLE dbo.BillInfor (BillID numeric PRIMARY KEY, FolioNum numeric, CompanyName nvarchar(200), VATCode varchar(20),
  Address1 nvarchar(200), Address2 nvarchar(200), EMail varchar(100), BillSeri1 varchar(20), BillNumber1From numeric,
  InvDate datetime, SubAmount money, ServiceCharge money, TaxAmount money, TotalAmount money, GuestName nvarchar(200),
  BillNote nvarchar(max), Cancel bit, Printed int, PrintDate datetime, ID int IDENTITY(1,1));
CREATE TABLE dbo.GuestEveryDay (GuestID int, FolioNum numeric, FolioSubNum int, IsAdditionalName bit, ProfileID int);
CREATE TABLE dbo.Profile (ProfileNumber int, Email varchar(100));
CREATE TABLE dbo.TransactionCode (TransactionCode varchar(10), TransactionSubCode varchar(10), Description nvarchar(100), TransactionGroup int,
  TransactionType varchar(10));
CREATE TABLE dbo.TaxDef (TaxCode varchar(10), TaxRate money);
CREATE TABLE dbo.FolioTransaction (TransactionID numeric PRIMARY KEY, BillID numeric, TransactionCode varchar(10),
  TransactionSubCode int, Description nvarchar(200), Quantity money, SubAmount money, TaxAmount money, STaxCode varchar(10),
  STaxAmount money, ServiceCharge money, TaxCode varchar(10), FolioBalanceCode varchar(2), isHKPost bit,
  RefNumber varchar(20), OriginFolioNumber numeric, Discount money);
CREATE TABLE dbo.ItemPosted (TrnID numeric, nSeq int, ICode varchar(20), IName nvarchar(100), Price money, Qty money, Amount money,
  TaxCode varchar(10), TaxAmount money, STaxCode varchar(10), STaxAmount money, ServiceChargeCode varchar(10),
  ServiceChargeAmount money);
CREATE TABLE dbo.ItemList (ICode varchar(20), IGroup varchar(20), IUnit varchar(20));
CREATE TABLE dbo.ItemUnit (UnitCode varchar(20), UnitName nvarchar(50));
CREATE TABLE dbo.Folio (FolioNum numeric, ConfirmNum numeric, ArrivalDate datetime, DepartureDate datetime, RoomCode varchar(10));
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
INSERT dbo.BillInfor (BillID, FolioNum, CompanyName, VATCode, InvDate, SubAmount, ServiceCharge, TaxAmount, TotalAmount, BillNote, Cancel, Printed, PrintDate)
  VALUES (7, 70, N'Công ty Ví Dụ', '0101243150', '2026-11-01T10:00:00', 100000, 0, 10000, 110000, NULL, 0, 1, '2026-11-01T10:00:00');
INSERT dbo.Folio VALUES (70, 7001, '2026-10-30', '2026-11-01', '101');
INSERT dbo.TransactionCode VALUES ('110', '0', N'Room', 1, NULL);
INSERT dbo.TaxDef VALUES ('V10', 10);
INSERT dbo.FolioTransaction VALUES (501, 7, '110', 0, N'Tiền phòng', 1, 100000, 10000, NULL, 0, 0, 'V10', 'R', 0, NULL, NULL, 0);
INSERT dbo.ItemPosted VALUES (501, 1, 'RM', N'Phòng', 100000, 1, 100000, 'V10', 10000, NULL, 0, NULL, 0);
INSERT dbo.Invoice (CheckNo, CloseTime, IYear, IMonth, IDay, SubTotal, TaxTotal, STaxTotal, Discount, SurCharge, BeforeTax,
  TotalBase, InvStatus, Status) VALUES (9, '2026-11-01T12:00:00', 2026, 11, 1, 50000, 5000, 0, 0, 0, 50000, 55000, 0, 1);
INSERT dbo.Trn VALUES (1, 9, 100, 'F1', 1, N'Phở', 50000, 1, 50000, 5000, 1, 0, 0, 0);";

pub fn connect(cfg: &Config) -> Client {
    let mut c = Client::connect(&cfg.clone().with_connect_timeout(Duration::from_secs(30)))
        .expect("connect");
    c.set_timeout(Some(Duration::from_secs(120))).unwrap();
    c
}

pub fn exec(c: &mut Client, sql: &str) {
    if let Err(e) = c.batch(sql) {
        panic!("{}: {e}", sql.trim().lines().next().unwrap_or(""));
    }
}
