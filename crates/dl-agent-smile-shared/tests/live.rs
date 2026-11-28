//! Live check against a real SQL Server (`SA_TEST_MSSQL_URL`, sysadmin, no database);
//! skipped when unset. Creates and drops its own database.

use dl_tds::{Client, Config};

const DB: &str = "sa_shared_live_rs";
const FIXTURE: &str = include_str!("../sql/test_fixture.sql");

#[test]
fn resolve_handles_blank_padded_duplicate_and_mistyped_subs() {
    let Some(url) = std::env::var("SA_TEST_MSSQL_URL")
        .ok()
        .filter(|u| !u.is_empty())
    else {
        eprintln!("note: skipping, set SA_TEST_MSSQL_URL to run against a real SQL Server");
        return;
    };
    let cfg: Config = url.parse().expect("SA_TEST_MSSQL_URL");
    let mut c = Client::connect(&cfg).expect("connect");
    c.batch(&format!("IF DB_ID('{DB}') IS NOT NULL BEGIN ALTER DATABASE {DB} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {DB} END; CREATE DATABASE {DB}"))
        .expect("create db");
    c.batch(&format!("USE {DB}")).expect("use");
    c.batch(FIXTURE).expect("fixture");
    dl_agent_smile_shared::install_into(&mut c).expect("install");
    dl_agent_smile_shared::install_into(&mut c).expect("install is idempotent");
    // (code, sub, fallback_sub0, fallback_code) -> expected Description (None = no row)
    let cases: &[(&str, &str, u8, u8, Option<&str>)] = &[
        ("110", "1", 0, 0, Some("Room Plus")),
        ("110", "", 0, 0, Some("Room")),
        ("110", "00", 0, 0, Some("Room")),
        ("120", "0", 0, 0, Some("Food")),
        ("150", "7", 0, 0, Some("Padded")),
        ("160", "2", 0, 0, Some("Dup B")),
        ("170", "0", 0, 0, Some("Blank sub")),
        ("180", "0", 0, 0, None),
        ("180", "0", 1, 0, None),
        ("180", "0", 0, 1, Some("Only sub1")),
        ("180", "5", 1, 1, Some("Only sub1")),
        ("190", " ab ", 0, 0, Some("Text sub")),
        ("110", "2", 1, 0, Some("Room")),
        ("999", "0", 1, 1, None),
    ];
    for (code, sub, f0, fc, want) in cases {
        let sql = format!(
            "SELECT Description FROM dbo.SA_TrnCode_Resolve('{code}', '{sub}', {f0}, {fc})"
        );
        let rs = c.query(&sql, &[]).expect("query");
        let got: Option<String> = rs.rows().first().map(|r| r.get::<String>(0).expect("col"));
        assert_eq!(got.as_deref(), *want, "case {code}/{sub}/{f0}/{fc}");
    }
    c.batch("USE master").ok();
    c.batch(&format!(
        "ALTER DATABASE {DB} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {DB}"
    ))
    .ok();
}

/// RT-1484: restaurant and minibar room charges come out as their items, the rest stays one line.
#[test]
fn folio_bill_expands_restaurant_and_minibar_room_charges() {
    let Some(url) = std::env::var("SA_TEST_MSSQL_URL")
        .ok()
        .filter(|u| !u.is_empty())
    else {
        eprintln!("note: skipping, set SA_TEST_MSSQL_URL to run against a real SQL Server");
        return;
    };
    let cfg: Config = url.parse().expect("SA_TEST_MSSQL_URL");
    let mut c = Client::connect(&cfg).expect("connect");
    for db in ["sa_bill_live_fo", "sa_bill_live_pos"] {
        c.batch(&format!("IF DB_ID('{db}') IS NOT NULL BEGIN ALTER DATABASE {db} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {db} END; CREATE DATABASE {db}")).expect("create db");
    }
    c.batch("USE sa_bill_live_pos").expect("use pos");
    c.batch("CREATE TABLE dbo.Trn (CheckNo numeric(18,0), RVC tinyint, TrnSeq numeric(18,0), TrnCode int, ItemCode varchar(40), TrnDesc varchar(100), ItemPrice numeric(18,4), TrnQty numeric(18,4), BaseTTL numeric(18,4))").expect("trn");
    c.batch("INSERT dbo.Trn VALUES (7001,1,1,1,'PHO','Pho bo',100000,2,200000),(7001,1,2,1,'COKE','Coca',50000,3,150000),(7001,1,3,200,'ROOM','Tender',0,1,-330000),(7002,1,1,1,'X','Small',10000,1,10000)").expect("trn rows");
    c.batch("USE sa_bill_live_fo").expect("use fo");
    c.batch("CREATE TABLE dbo.TransactionCode (TransactionCode varchar(10), TransactionSubCode varchar(10), Description nvarchar(100))").expect("tc");
    c.batch("CREATE TABLE dbo.FolioTransaction (TransactionID numeric(18,0) IDENTITY(1,1), FolioNum varchar(20), FolioBalanceCode varchar(5), TransactionDate datetime, TransactionCode varchar(10), TransactionSubCode numeric(18,0), RefNumber varchar(30), TransactionAmount numeric(18,4), Qty numeric(18,4), UnitPrice numeric(18,4), Description nvarchar(100))").expect("ft");
    c.batch("CREATE TABLE dbo.ItemPosted (nSeq bigint IDENTITY(1,1), FolioNum numeric(18,0), ICode varchar(40), IName varchar(100), Price numeric(18,4), Qty numeric(18,4), Amount numeric(18,4), RefNumber varchar(30), PostedTime datetime, TrnID bigint)").expect("ip");
    c.batch("INSERT dbo.FolioTransaction (FolioNum,FolioBalanceCode,TransactionDate,TransactionCode,TransactionSubCode,RefNumber,TransactionAmount) VALUES ('1001','A','2026-11-20','110',0,'',1000000),('1001','A','2026-11-21','500',0,'7001',330000),('1001','A','2026-11-22','510',0,'M55',110000),('1001','A','2026-11-22','900',0,'7002',50000),('1001','C','2026-11-23','110',0,'',99)").expect("ft rows");
    c.batch("INSERT dbo.ItemPosted (FolioNum,ICode,IName,Price,Qty,Amount,RefNumber,TrnID) SELECT 1001,'WTR','Water',20000,2,40000,'MB-9',TransactionID FROM dbo.FolioTransaction WHERE RefNumber='M55'; INSERT dbo.ItemPosted (FolioNum,ICode,IName,Price,Qty,Amount,RefNumber,TrnID) SELECT 1001,'BEER','Beer',35000,2,70000,'MB-9',TransactionID FROM dbo.FolioTransaction WHERE RefNumber='M55'").expect("ip rows");
    dl_agent_smile_shared::install_into(&mut c).expect("install");
    dl_agent_smile_shared::install_into(&mut c).expect("install is idempotent");
    let rs = c
        .query("SELECT Source, ItemName, Qty, UnitPrice, Amount, RefNo FROM dbo.SA_FolioBill(1001, NULL) ORDER BY TransDate, TransactionID, LineSeq", &[])
        .expect("query");
    let got: Vec<(String, String, String, String, String, String)> = rs
        .rows()
        .iter()
        .map(|r| {
            (
                r.get::<String>(0).expect("source"),
                r.get::<String>(1).unwrap_or_default(),
                r.get::<String>(2).unwrap_or_default(),
                r.get::<String>(3).unwrap_or_default(),
                r.get::<String>(4).expect("amount"),
                r.get::<String>(5).unwrap_or_default(),
            )
        })
        .collect();
    let sources: Vec<&str> = got.iter().map(|g| g.0.as_str()).collect();
    assert_eq!(
        sources,
        ["FO", "POS", "POS", "ADJ", "MINIBAR", "MINIBAR", "FO"],
        "{got:?}"
    );
    assert!(got[1].5 == "7001" && got[1].1 == "Pho bo", "{got:?}");
    assert!(got[4].5 == "MB-9" && got[4].1 == "Water", "{got:?}");
    c.batch("USE master").ok();
    for db in ["sa_bill_live_fo", "sa_bill_live_pos"] {
        c.batch(&format!(
            "ALTER DATABASE {db} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE {db}"
        ))
        .ok();
    }
}
