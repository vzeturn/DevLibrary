//! Integration tests against a real SQL Server.
//!
//! Set `DL_TDS_TEST_URL`, e.g.
//! `sqlserver://sa:<password>@localhost:14333?encrypt=disable`, to a **disposable** server
//! (the tests create databases `dltds_test` and `dltds_vn`). Without it every test prints a
//! note and passes.

use std::time::Duration;

use dl_decimal::Decimal;
use dl_tds::{Client, Config, Encrypt, Error, MessageKind, Param, ParamValue, Value};
use dl_time::{Date, DateTime, OffsetDateTime, Time};
use dl_uuid::Uuid;

fn url() -> Option<String> {
    match std::env::var("DL_TDS_TEST_URL") {
        Ok(u) if !u.is_empty() => Some(u),
        _ => {
            eprintln!("note: DL_TDS_TEST_URL is not set; skipping SQL Server integration test");
            None
        }
    }
}

fn config() -> Option<Config> {
    url().map(|u| {
        u.parse()
            .expect("DL_TDS_TEST_URL is not a valid connection string")
    })
}

fn conn() -> Option<Client> {
    let cfg = config()?;
    let mut c = Client::connect(&cfg).expect("connect");
    setup(&mut c);
    Some(c)
}

/// Creates the test databases once per server (idempotent).
fn setup(c: &mut Client) {
    // Tests run in parallel; concurrent CREATE DATABASE of the same name races in the
    // server, so only the first connection creates them.
    static ONCE: std::sync::Mutex<bool> = std::sync::Mutex::new(false);
    let mut done = ONCE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if *done {
        return;
    }
    *done = true;
    c.batch(
        "IF DB_ID('dltds_test') IS NULL CREATE DATABASE dltds_test;
         IF DB_ID('dltds_vn') IS NULL CREATE DATABASE dltds_vn COLLATE Vietnamese_CI_AS;",
    )
    .expect("create databases");
}

fn dt(s: &str) -> DateTime {
    s.parse().unwrap()
}

fn one(c: &mut Client, sql: &str) -> Value {
    let r = c.query(sql, &[]).unwrap_or_else(|e| panic!("{sql}: {e}"));
    r.rows()[0].values()[0].clone()
}

const CP1252_80_FF: &str = "\u{20AC}\u{FFFD}\u{201A}\u{0192}\u{201E}\u{2026}\u{2020}\u{2021}\u{02C6}\u{2030}\u{0160}\u{2039}\u{0152}\u{FFFD}\u{017D}\u{FFFD}\u{FFFD}\u{2018}\u{2019}\u{201C}\u{201D}\u{2022}\u{2013}\u{2014}\u{02DC}\u{2122}\u{0161}\u{203A}\u{0153}\u{FFFD}\u{017E}\u{0178}\u{00A0}\u{00A1}\u{00A2}\u{00A3}\u{00A4}\u{00A5}\u{00A6}\u{00A7}\u{00A8}\u{00A9}\u{00AA}\u{00AB}\u{00AC}\u{00AD}\u{00AE}\u{00AF}\u{00B0}\u{00B1}\u{00B2}\u{00B3}\u{00B4}\u{00B5}\u{00B6}\u{00B7}\u{00B8}\u{00B9}\u{00BA}\u{00BB}\u{00BC}\u{00BD}\u{00BE}\u{00BF}\u{00C0}\u{00C1}\u{00C2}\u{00C3}\u{00C4}\u{00C5}\u{00C6}\u{00C7}\u{00C8}\u{00C9}\u{00CA}\u{00CB}\u{00CC}\u{00CD}\u{00CE}\u{00CF}\u{00D0}\u{00D1}\u{00D2}\u{00D3}\u{00D4}\u{00D5}\u{00D6}\u{00D7}\u{00D8}\u{00D9}\u{00DA}\u{00DB}\u{00DC}\u{00DD}\u{00DE}\u{00DF}\u{00E0}\u{00E1}\u{00E2}\u{00E3}\u{00E4}\u{00E5}\u{00E6}\u{00E7}\u{00E8}\u{00E9}\u{00EA}\u{00EB}\u{00EC}\u{00ED}\u{00EE}\u{00EF}\u{00F0}\u{00F1}\u{00F2}\u{00F3}\u{00F4}\u{00F5}\u{00F6}\u{00F7}\u{00F8}\u{00F9}\u{00FA}\u{00FB}\u{00FC}\u{00FD}\u{00FE}\u{00FF}";
const CP1258_80_FF: &str = "\u{20AC}\u{FFFD}\u{201A}\u{0192}\u{201E}\u{2026}\u{2020}\u{2021}\u{02C6}\u{2030}\u{FFFD}\u{2039}\u{0152}\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}\u{2018}\u{2019}\u{201C}\u{201D}\u{2022}\u{2013}\u{2014}\u{02DC}\u{2122}\u{FFFD}\u{203A}\u{0153}\u{FFFD}\u{FFFD}\u{0178}\u{00A0}\u{00A1}\u{00A2}\u{00A3}\u{00A4}\u{00A5}\u{00A6}\u{00A7}\u{00A8}\u{00A9}\u{00AA}\u{00AB}\u{00AC}\u{00AD}\u{00AE}\u{00AF}\u{00B0}\u{00B1}\u{00B2}\u{00B3}\u{00B4}\u{00B5}\u{00B6}\u{00B7}\u{00B8}\u{00B9}\u{00BA}\u{00BB}\u{00BC}\u{00BD}\u{00BE}\u{00BF}\u{00C0}\u{00C1}\u{00C2}\u{0102}\u{00C4}\u{00C5}\u{00C6}\u{00C7}\u{00C8}\u{00C9}\u{00CA}\u{00CB}\u{0300}\u{00CD}\u{00CE}\u{00CF}\u{0110}\u{00D1}\u{0309}\u{00D3}\u{00D4}\u{01A0}\u{00D6}\u{00D7}\u{00D8}\u{00D9}\u{00DA}\u{00DB}\u{00DC}\u{01AF}\u{0303}\u{00DF}\u{00E0}\u{00E1}\u{00E2}\u{0103}\u{00E4}\u{00E5}\u{00E6}\u{00E7}\u{00E8}\u{00E9}\u{00EA}\u{00EB}\u{0301}\u{00ED}\u{00EE}\u{00EF}\u{0111}\u{00F1}\u{0323}\u{00F3}\u{00F4}\u{01A1}\u{00F6}\u{00F7}\u{00F8}\u{00F9}\u{00FA}\u{00FB}\u{00FC}\u{01B0}\u{20AB}\u{00FF}";

/// `0x808182...FF` as a T-SQL binary literal.
fn high_bytes_literal() -> String {
    let mut s = String::from("0x");
    for b in 0x80..=0xFFu8 {
        s.push_str(&format!("{b:02X}"));
    }
    s
}

#[test]
fn login_ping_and_metadata() {
    let Some(mut c) = conn() else { return };
    c.ping().unwrap();
    assert!(c.server_version().0 >= 11, "{:?}", c.server_version());
    assert_eq!(c.tds_version(), 0x7400_0004);
    assert_eq!(c.server_name(), "Microsoft SQL Server");
    assert_eq!(c.database(), "master");
    assert_eq!(c.packet_size(), 4096);
    assert!(!c.is_encrypted());
    let msgs = c.take_messages();
    assert!(
        msgs.iter().any(|m| m.number == 5701),
        "changed database context message"
    );
    // Database from the connection string.
    let cfg = config().unwrap().with_database("dltds_test");
    let mut c2 = Client::connect(&cfg).unwrap();
    assert_eq!(c2.database(), "dltds_test");
    assert_eq!(
        one(&mut c2, "SELECT DB_NAME()"),
        Value::String("dltds_test".into())
    );
    assert_eq!(
        one(&mut c2, "SELECT APP_NAME()"),
        Value::String("dl-tds".into())
    );
    c2.batch("USE master").unwrap();
    assert_eq!(c2.database(), "master");
    c2.close();
}

#[test]
fn login_failure_is_server_error() {
    let Some(cfg) = config() else { return };
    let bad = cfg.credentials("sa", "definitely-wrong");
    match Client::connect(&bad) {
        Err(Error::Server { number, class, .. }) => {
            assert_eq!(number, 18456);
            assert_eq!(class, 14);
        }
        other => panic!("{other:?}"),
    }
    // Unknown database: the server sends 4060 then 18456; the last one is reported.
    let bad = config().unwrap().with_database("no_such_db_dltds");
    match Client::connect(&bad) {
        Err(Error::Server { number, .. }) => {
            assert!([4060, 4063, 18456].contains(&number), "{number}")
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn every_type_round_trips_through_select() {
    let Some(mut c) = conn() else { return };
    let u: Uuid = "6f9619ff-8b86-d011-b42d-00c04fc964ff".parse().unwrap();
    let cases: Vec<(&str, Value)> = vec![
        ("CAST(255 AS tinyint)", Value::I8(255)),
        ("CAST(-32768 AS smallint)", Value::I16(-32768)),
        ("CAST(-2147483648 AS int)", Value::I32(i32::MIN)),
        ("CAST(-9223372036854775808 AS bigint)", Value::I64(i64::MIN)),
        ("CAST(1 AS bit)", Value::Bool(true)),
        ("CAST(0 AS bit)", Value::Bool(false)),
        ("CAST(1.5 AS float)", Value::F64(1.5)),
        ("CAST(1.0E308 AS float)", Value::F64(1.0e308)),
        (
            "CAST(12345.6789012345 AS decimal(38,10))",
            Value::Decimal("12345.6789012345".parse().unwrap()),
        ),
        (
            "CAST(-999.99 AS numeric(5,2))",
            Value::Decimal("-999.99".parse().unwrap()),
        ),
        (
            "CAST(99999999999999999999999999999999999999 AS decimal(38,0))",
            Value::Decimal(Decimal::MAX),
        ),
        (
            "CAST(0.00 AS decimal(5,2))",
            Value::Decimal("0.00".parse().unwrap()),
        ),
        (
            "CAST(922337203685477.5807 AS money)",
            Value::Decimal("922337203685477.5807".parse().unwrap()),
        ),
        (
            "CAST(-922337203685477.5808 AS money)",
            Value::Decimal("-922337203685477.5808".parse().unwrap()),
        ),
        (
            "CAST(12.5 AS money)",
            Value::Decimal("12.5000".parse().unwrap()),
        ),
        (
            "CAST(-214748.3648 AS smallmoney)",
            Value::Decimal("-214748.3648".parse().unwrap()),
        ),
        (
            "CAST('2024-02-29' AS date)",
            Value::Date("2024-02-29".parse().unwrap()),
        ),
        (
            "CAST('0001-01-01' AS date)",
            Value::Date("0001-01-01".parse().unwrap()),
        ),
        (
            "CAST('9999-12-31' AS date)",
            Value::Date("9999-12-31".parse().unwrap()),
        ),
        (
            "CAST('23:59:59.9999999' AS time(7))",
            Value::Time("23:59:59.9999999".parse().unwrap()),
        ),
        (
            "CAST('10:20:30' AS time(0))",
            Value::Time("10:20:30".parse().unwrap()),
        ),
        (
            "CAST('10:20:30.123' AS time(3))",
            Value::Time("10:20:30.123".parse().unwrap()),
        ),
        (
            "CAST('2024-05-01T10:20:30.997' AS datetime)",
            Value::DateTime(dt("2024-05-01T10:20:30.997")),
        ),
        (
            "CAST('2024-05-01T10:20:30.003' AS datetime)",
            Value::DateTime(dt("2024-05-01T10:20:30.003")),
        ),
        (
            "CAST('1753-01-01' AS datetime)",
            Value::DateTime(dt("1753-01-01T00:00:00")),
        ),
        (
            "CAST('2024-05-01T10:20:00' AS smalldatetime)",
            Value::DateTime(dt("2024-05-01T10:20:00")),
        ),
        (
            "CAST('2024-05-01T10:20:30.1234567' AS datetime2(7))",
            Value::DateTime(dt("2024-05-01T10:20:30.1234567")),
        ),
        (
            "CAST('9999-12-31T23:59:59' AS datetime2(0))",
            Value::DateTime(dt("9999-12-31T23:59:59")),
        ),
        (
            "CAST('2024-05-01T10:20:30.55' AS datetime2(2))",
            Value::DateTime(dt("2024-05-01T10:20:30.55")),
        ),
        (
            "CAST('2024-05-01T10:20:30.1234567+07:00' AS datetimeoffset(7))",
            Value::DateTimeOffset("2024-05-01T10:20:30.1234567+07:00".parse().unwrap()),
        ),
        (
            "CAST('2024-05-01T10:20:30-03:30' AS datetimeoffset(0))",
            Value::DateTimeOffset("2024-05-01T10:20:30-03:30".parse().unwrap()),
        ),
        (
            "CAST('6F9619FF-8B86-D011-B42D-00C04FC964FF' AS uniqueidentifier)",
            Value::Uuid(u),
        ),
        ("CAST('ab' AS char(5))", Value::String("ab   ".into())),
        ("CAST('abc' AS varchar(10))", Value::String("abc".into())),
        (
            "CAST(N'Tiếng Việt' AS nchar(12))",
            Value::String("Tiếng Việt  ".into()),
        ),
        (
            "CAST(N'Tiếng Việt 😀' AS nvarchar(50))",
            Value::String("Tiếng Việt 😀".into()),
        ),
        ("CAST(N'' AS nvarchar(50))", Value::String(String::new())),
        (
            "CAST('legacy text' AS text)",
            Value::String("legacy text".into()),
        ),
        (
            "CAST(N'legacy ntext ư' AS ntext)",
            Value::String("legacy ntext ư".into()),
        ),
        (
            "CAST(0xDEADBEEF AS binary(4))",
            Value::Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF]),
        ),
        ("CAST(0x0102 AS varbinary(10))", Value::Bytes(vec![1, 2])),
        ("CAST(0x00FF AS image)", Value::Bytes(vec![0, 0xFF])),
        (
            "CAST('<a b=\"1\">x</a>' AS xml)",
            Value::String("<a b=\"1\">x</a>".into()),
        ),
        ("CAST(CAST(7 AS int) AS sql_variant)", Value::I32(7)),
        (
            "CAST(N'variant' AS sql_variant)",
            Value::String("variant".into()),
        ),
        (
            "CAST(CAST(1.25 AS decimal(5,2)) AS sql_variant)",
            Value::Decimal("1.25".parse().unwrap()),
        ),
        (
            "CAST(CAST('2024-01-02' AS date) AS sql_variant)",
            Value::Date("2024-01-02".parse().unwrap()),
        ),
        ("CAST(CAST(1 AS bit) AS sql_variant)", Value::Bool(true)),
        (
            "CAST(CAST(5 AS money) AS sql_variant)",
            Value::Decimal("5.0000".parse().unwrap()),
        ),
        (
            "CAST(hierarchyid::Parse('/1/') AS hierarchyid)",
            Value::Bytes(vec![0x58]),
        ),
    ];
    for (expr, want) in &cases {
        let got = one(&mut c, &format!("SELECT {expr}"));
        assert_eq!(&got, want, "{expr}");
    }
    // real: F32 for NOT NULL metadata, F64 (widened) when nullable.
    match one(&mut c, "SELECT CAST(1.5 AS real)") {
        Value::F32(f) => assert_eq!(f, 1.5),
        Value::F64(f) => assert_eq!(f, 1.5),
        v => panic!("{v:?}"),
    }
    // NULL of every type.
    let types = [
        "tinyint",
        "smallint",
        "int",
        "bigint",
        "bit",
        "real",
        "float",
        "decimal(10,2)",
        "numeric(38,5)",
        "money",
        "smallmoney",
        "date",
        "time(7)",
        "datetime",
        "smalldatetime",
        "datetime2(3)",
        "datetimeoffset(7)",
        "uniqueidentifier",
        "char(3)",
        "varchar(10)",
        "varchar(max)",
        "nchar(3)",
        "nvarchar(10)",
        "nvarchar(max)",
        "text",
        "ntext",
        "binary(3)",
        "varbinary(10)",
        "varbinary(max)",
        "image",
        "xml",
        "sql_variant",
    ];
    let sql = format!(
        "SELECT {}",
        types
            .iter()
            .map(|t| format!("CAST(NULL AS {t})"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let r = c.query(&sql, &[]).unwrap();
    let row = &r.rows()[0];
    assert_eq!(row.len(), types.len());
    for (i, t) in types.iter().enumerate() {
        assert!(row.values()[i].is_null(), "NULL {t}");
        assert!(row.get::<Option<String>>(i).unwrap().is_none());
    }
    // Column metadata.
    let r = c
        .query("SELECT CAST(1 AS int) AS a, CAST(NULL AS nvarchar(20)) AS b, CAST(1 AS decimal(18,4)) AS c", &[])
        .unwrap();
    let cols = r.first().unwrap().columns();
    assert_eq!(cols[0].name(), "a");
    assert_eq!(cols[0].type_name(), "INT");
    assert_eq!(cols[1].type_name(), "NVARCHAR");
    assert_eq!(cols[1].max_length(), Some(40));
    assert_eq!(
        (cols[2].type_name(), cols[2].precision(), cols[2].scale()),
        ("DECIMAL", 18, 4)
    );
}

#[test]
fn max_types_use_plp() {
    let Some(mut c) = conn() else { return };
    // 1 048 576 characters (2 MiB of UTF-16).
    let r = c
        .query("SELECT REPLICATE(CAST(N'aư' AS nvarchar(max)), 524288), REPLICATE(CAST('x' AS varchar(max)), 1048576), CAST(REPLICATE(CAST('z' AS varchar(max)), 100000) AS varbinary(max))", &[])
        .unwrap();
    let row = &r.rows()[0];
    let s: String = row.get(0).unwrap();
    assert_eq!(s.chars().count(), 1_048_576);
    assert!(s.starts_with("aưaư") && s.ends_with("aư"));
    let v: String = row.get(1).unwrap();
    assert_eq!(v.len(), 1_048_576);
    let b: Vec<u8> = row.get(2).unwrap();
    assert_eq!(b.len(), 100_000);
    // A 1 MiB parameter goes out as nvarchar(max) and comes back unchanged.
    let big = "Đ".repeat(1 << 20);
    let r = c
        .query("SELECT LEN(@p), @p", &[Param::new("p", big.as_str())])
        .unwrap();
    assert_eq!(r.rows()[0].get::<i64>(0).unwrap(), 1 << 20);
    assert_eq!(r.rows()[0].get::<String>(1).unwrap(), big);
    // Empty and NULL max values.
    let r = c.query("SELECT CAST(N'' AS nvarchar(max)), CAST(NULL AS nvarchar(max)), CAST(0x AS varbinary(max))", &[]).unwrap();
    assert_eq!(
        r.rows()[0].values(),
        &[
            Value::String(String::new()),
            Value::Null,
            Value::Bytes(vec![])
        ]
    );
}

#[test]
fn sys_sql_modules_definition() {
    let Some(mut c) = conn() else { return };
    c.batch("USE dltds_test").unwrap();
    let body = format!(
        "CREATE OR ALTER PROCEDURE dbo.dl_def_test AS\nBEGIN\n  -- {}\n  SELECT 1;\nEND",
        "Ư".repeat(5000)
    );
    c.batch(&body).unwrap();
    let r = c
        .query(
            "SELECT m.definition FROM sys.sql_modules m WHERE m.object_id = OBJECT_ID(@name)",
            &[Param::new("name", "dbo.dl_def_test")],
        )
        .unwrap();
    let def: String = r.rows()[0].get(0).unwrap();
    // SQL Server stores CREATE OR ALTER as "CREATE   " (OR ALTER blanked out).
    assert_eq!(def, body.replace("CREATE OR ALTER ", "CREATE   "));
    assert_eq!(r.first().unwrap().columns()[0].type_name(), "NVARCHAR");
}

#[test]
fn varchar_code_pages_match_python() {
    let Some(mut c) = conn() else { return };
    let lit = high_bytes_literal();
    // Default collation of the container: SQL_Latin1_General_CP1_CI_AS (cp1252).
    let coll = one(
        &mut c,
        "SELECT CONVERT(nvarchar(128), DATABASEPROPERTYEX('master', 'Collation'))",
    );
    assert_eq!(coll, Value::String("SQL_Latin1_General_CP1_CI_AS".into()));
    let r = c.query(&format!("SELECT CAST({lit} AS varchar(128)), CAST({lit} AS char(128)), CAST(CAST({lit} AS varchar(128)) AS text)"), &[]).unwrap();
    for i in 0..3 {
        assert_eq!(
            r.rows()[0].get::<String>(i).unwrap(),
            CP1252_80_FF,
            "column {i}"
        );
    }
    let col = &r.first().unwrap().columns()[0];
    assert_eq!(col.collation().unwrap().code_page(), Some(1252));

    // A Vietnamese_CI_AS database (cp1258).
    c.batch("USE dltds_vn").unwrap();
    let r = c.query(&format!("SELECT CAST({lit} AS varchar(128)), CAST(CAST({lit} AS varchar(max)) AS varchar(max))"), &[]).unwrap();
    assert_eq!(r.rows()[0].get::<String>(0).unwrap(), CP1258_80_FF);
    assert_eq!(r.rows()[0].get::<String>(1).unwrap(), CP1258_80_FF);
    assert_eq!(
        r.first().unwrap().columns()[0]
            .collation()
            .unwrap()
            .code_page(),
        Some(1258)
    );
    // A column with an explicit collation in a table.
    c.batch(&format!(
        "IF OBJECT_ID('dbo.cp_test') IS NOT NULL DROP TABLE dbo.cp_test;
         CREATE TABLE dbo.cp_test (v varchar(200) COLLATE Vietnamese_CI_AS, l varchar(200) COLLATE SQL_Latin1_General_CP1_CI_AS);
         INSERT dbo.cp_test (v) VALUES (CAST({lit} AS varchar(200)));"
    ))
    .unwrap();
    let r = c.query("SELECT v FROM dbo.cp_test", &[]).unwrap();
    assert_eq!(r.rows()[0].get::<String>(0).unwrap(), CP1258_80_FF);
    // Vietnamese text written as nvarchar, read back as varchar(cp1258): the server stores
    // precomposed letters with combining tone marks where cp1258 needs them.
    c.batch("UPDATE dbo.cp_test SET v = N'Khách sạn Đà Lạt ư ơ'")
        .unwrap();
    let s: String = c.query("SELECT v FROM dbo.cp_test", &[]).unwrap().rows()[0]
        .get(0)
        .unwrap();
    let n: String = c
        .query("SELECT CAST(v AS nvarchar(200)) FROM dbo.cp_test", &[])
        .unwrap()
        .rows()[0]
        .get(0)
        .unwrap();
    assert_eq!(
        s, n,
        "dl-tds cp1258 decoding equals the server's own conversion"
    );
    c.batch("USE master").unwrap();
}

#[test]
fn stored_procedure_with_three_result_sets_and_return_value() {
    let Some(mut c) = conn() else { return };
    c.batch("USE dltds_test").unwrap();
    // 22, 33 and 15 columns of mixed types, like sp_FetchFO.
    let mix = |n: usize, tag: &str| -> String {
        (0..n)
            .map(|i| match i % 11 {
                0 => format!("CAST('{tag}{i}' AS varchar(20)) AS c{i}"),
                1 => format!("CAST(N'{tag}ư{i}' AS nvarchar(20)) AS c{i}"),
                2 => format!("CAST({i} AS int) AS c{i}"),
                3 => format!("CAST({i}000000000 AS bigint) AS c{i}"),
                4 => format!("CAST({i}.25 AS decimal(18,2)) AS c{i}"),
                5 => format!("CAST({i}.5 AS money) AS c{i}"),
                6 => format!("CAST({i}.125 AS float) AS c{i}"),
                7 => format!("CAST({} AS bit) AS c{i}", i % 2),
                8 => format!(
                    "CAST('2024-05-0{} 10:00:00' AS datetime) AS c{i}",
                    1 + i % 9
                ),
                9 => format!("CAST('2024-05-0{}' AS date) AS c{i}", 1 + i % 9),
                _ => format!("CAST(NULL AS datetime) AS c{i}"),
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    c.batch(&format!(
        "CREATE OR ALTER PROCEDURE dbo.sp_FetchFO_test @ID nvarchar(50) AS
         BEGIN
           SET NOCOUNT ON;
           SELECT {} WHERE @ID = '123';
           SELECT {} UNION ALL SELECT {};
           SELECT {};
           RETURN 42;
         END",
        mix(22, "a"),
        mix(33, "b"),
        mix(33, "c"),
        mix(15, "d")
    ))
    .unwrap();
    let r = c
        .query(
            "EXEC dbo.sp_FetchFO_test @ID = @ID",
            &[Param::new("ID", "123")],
        )
        .unwrap();
    let sets = r.sets();
    assert_eq!(sets.len(), 3);
    assert_eq!(sets[0].columns().len(), 22);
    assert_eq!(sets[1].columns().len(), 33);
    assert_eq!(sets[2].columns().len(), 15);
    assert_eq!(sets[0].len(), 1);
    assert_eq!(sets[1].len(), 2);
    let row = &sets[0].rows()[0];
    assert_eq!(row.get::<String>(0).unwrap(), "a0");
    assert_eq!(row.get::<String>(1).unwrap(), "aư1");
    assert_eq!(row.get::<i32>(2).unwrap(), 2);
    assert_eq!(row.get::<i64>(2).unwrap(), 2);
    assert_eq!(row.get::<i64>(3).unwrap(), 3_000_000_000);
    assert_eq!(row.get::<f64>(4).unwrap(), 4.25);
    assert_eq!(row.get::<String>(4).unwrap(), "4.25");
    assert_eq!(row.get::<f64>(5).unwrap(), 5.5);
    assert_eq!(row.get::<String>(5).unwrap(), "5.5000");
    assert_eq!(row.get::<f64>(6).unwrap(), 6.125);
    assert!(row.get::<bool>(7).unwrap());
    assert_eq!(
        row.get::<Option<DateTime>>(8).unwrap(),
        Some(dt("2024-05-09T10:00:00"))
    );
    assert_eq!(row.get::<DateTime>(9).unwrap(), dt("2024-05-01T00:00:00"));
    assert_eq!(row.get::<Option<DateTime>>(10).unwrap(), None);
    assert!(matches!(
        row.get::<DateTime>(10),
        Err(Error::Null { column: 10 })
    ));
    assert!(matches!(row.get::<String>(10), Err(Error::Null { .. })));
    assert_eq!(sets[1].rows()[1].get::<String>("c0").unwrap(), "c0");
    eprintln!(
        "return statuses via sp_executesql: {:?}",
        r.return_statuses()
    );
    assert_eq!(
        r.return_status(),
        Some(0),
        "sp_executesql's own status comes last"
    );
    // As a plain batch the procedure's RETURN value is the status.
    let r = c.batch("EXEC dbo.sp_FetchFO_test @ID = N'123'").unwrap();
    assert_eq!(r.sets().len(), 3);
    assert_eq!(r.return_status(), Some(42));
    // Capturing it explicitly works with parameters too.
    let r = c
        .query(
            "DECLARE @r int; EXEC @r = dbo.sp_FetchFO_test @ID = @ID; SELECT @r",
            &[Param::new("ID", "123")],
        )
        .unwrap();
    assert_eq!(r.sets()[3].rows()[0].get::<i32>(0).unwrap(), 42);
    // No row for another ID: the first set is empty but present.
    let r = c
        .query(
            "EXEC dbo.sp_FetchFO_test @ID = @ID",
            &[Param::new("ID", "999")],
        )
        .unwrap();
    assert_eq!(r.sets().len(), 3);
    assert!(r.sets()[0].is_empty());
}

#[test]
fn agent_procedure_patterns() {
    let Some(mut c) = conn() else { return };
    c.batch("USE dltds_test").unwrap();
    c.batch(
        "CREATE OR ALTER PROCEDURE dbo.sp_X_test @Limit int, @CollectFrom datetime = NULL AS
         SELECT TOP (@Limit) CONCAT('row', n, '|', ISNULL(CONVERT(varchar(30), @CollectFrom, 126), 'null')) AS payload
         FROM (SELECT TOP 100 ROW_NUMBER() OVER (ORDER BY (SELECT 1)) AS n FROM sys.all_objects) t ORDER BY n",
    )
    .unwrap();
    let r = c
        .query(
            "EXEC dbo.sp_X_test @Limit = @Limit, @CollectFrom = @CollectFrom",
            &[
                Param::new("Limit", 50i32),
                Param::new("CollectFrom", Option::<DateTime>::None),
            ],
        )
        .unwrap();
    assert_eq!(r.rows().len(), 50);
    assert_eq!(r.rows()[0].get::<String>(0).unwrap(), "row1|null");
    let r = c
        .query(
            "EXEC dbo.sp_X_test @Limit = @Limit, @CollectFrom = @CollectFrom",
            &[
                Param::new("Limit", 2i32),
                Param::new("CollectFrom", Some(dt("2024-05-01T10:20:30.5"))),
            ],
        )
        .unwrap();
    assert_eq!(
        r.rows()[1].get::<String>(0).unwrap(),
        "row2|2024-05-01T10:20:30.500"
    );

    c.batch(
        "IF OBJECT_ID('dbo.collected') IS NULL CREATE TABLE dbo.collected (Source varchar(10), ID nvarchar(50), Ok bit, Message nvarchar(max), IssuedAt datetime NULL);",
    )
    .unwrap();
    c.batch(
        "CREATE OR ALTER PROCEDURE dbo.sp_MarkCollected_test @Source varchar(10), @ID nvarchar(50), @Ok bit, @Message nvarchar(max) AS
           INSERT dbo.collected (Source, ID, Ok, Message) VALUES (@Source, @ID, @Ok, @Message);",
    )
    .unwrap();
    c.batch(
        "CREATE OR ALTER PROCEDURE dbo.sp_Writeback_test @ID nvarchar(50), @IssuedAt datetime AS
           UPDATE dbo.collected SET IssuedAt = @IssuedAt WHERE ID = @ID;",
    )
    .unwrap();
    c.batch("DELETE dbo.collected").unwrap();
    let n = c
        .execute(
            "EXEC sp_MarkCollected_test @Source=@Source, @ID=@ID, @Ok=@Ok, @Message=@Message",
            &[
                Param::new("Source", "FO"),
                Param::new("ID", "123"),
                Param::new("Ok", true),
                Param::new("Message", "Đã thu thập"),
            ],
        )
        .unwrap();
    assert!(n >= 1, "rows affected {n}");
    c.execute(
        "EXEC sp_Writeback_test @ID=@ID, @IssuedAt=@p6",
        &[
            Param::new("ID", "123"),
            Param::new("p6", Some(dt("2024-05-01T10:20:30"))),
        ],
    )
    .unwrap();
    let r = c
        .query(
            "SELECT Source, ID, Ok, Message, IssuedAt FROM dbo.collected",
            &[],
        )
        .unwrap();
    let row = &r.rows()[0];
    assert_eq!(row.get::<String>("Source").unwrap(), "FO");
    assert!(row.get::<bool>("Ok").unwrap());
    assert_eq!(row.get::<String>("Message").unwrap(), "Đã thu thập");
    assert_eq!(
        row.get::<Option<DateTime>>("IssuedAt").unwrap(),
        Some(dt("2024-05-01T10:20:30"))
    );
    c.execute(
        "EXEC sp_Writeback_test @ID=@ID, @IssuedAt=@p6",
        &[
            Param::new("ID", "123"),
            Param::new("p6", Option::<DateTime>::None),
        ],
    )
    .unwrap();
    let r = c.query("SELECT IssuedAt FROM dbo.collected", &[]).unwrap();
    assert_eq!(r.rows()[0].get::<Option<DateTime>>(0).unwrap(), None);

    // Four bits in one row.
    let r = c
        .query(
            "SELECT CAST(CASE WHEN 1=1 THEN 1 ELSE 0 END AS bit), CAST(CASE WHEN 1=0 THEN 1 ELSE 0 END AS bit),
                    CAST(CASE WHEN OBJECT_ID('dbo.collected') IS NOT NULL THEN 1 ELSE 0 END AS bit), CAST(NULL AS bit)",
            &[],
        )
        .unwrap();
    let row = &r.rows()[0];
    assert!(row.get::<bool>(0).unwrap());
    assert!(!row.get::<bool>(1).unwrap());
    assert!(row.get::<bool>(2).unwrap());
    assert_eq!(row.get::<Option<bool>>(3).unwrap(), None);
}

#[test]
fn rpc_parameters_of_every_type() {
    let Some(mut c) = conn() else { return };
    let u = Uuid::from_u128(0x0011_2233_4455_6677_8899_aabb_ccdd_eeff);
    let o: OffsetDateTime = "2024-05-01T10:20:30.1234567+07:00".parse().unwrap();
    let params = vec![
        Param::new("b", true),
        Param::new("u8", 200u8),
        Param::new("i16", -300i16),
        Param::new("i32", 70_000i32),
        Param::new("i64", i64::MAX),
        Param::new("f32", 0.5f32),
        Param::new("f64", -1.25e-10f64),
        Param::new(
            "dec",
            "-12345678901234567890.123456".parse::<Decimal>().unwrap(),
        ),
        Param::new("s", "Xin chào 😀"),
        Param::new("bin", vec![0u8, 1, 2, 255]),
        Param::new("g", u),
        Param::new("d", "2024-02-29".parse::<Date>().unwrap()),
        Param::new("t", "13:14:15.1234567".parse::<Time>().unwrap()),
        Param::new("dt", dt("2024-05-01T10:20:30.1234567")),
        Param::new("dto", o),
        Param::new("empty", ""),
    ];
    let names: Vec<String> = params.iter().map(|p| format!("@{}", p.name())).collect();
    let r = c
        .query(&format!("SELECT {}", names.join(", ")), &params)
        .unwrap();
    let row = &r.rows()[0];
    let want = [
        Value::Bool(true),
        Value::I8(200),
        Value::I16(-300),
        Value::I32(70_000),
        Value::I64(i64::MAX),
        Value::F64(0.5),
        Value::F64(-1.25e-10),
        Value::Decimal("-12345678901234567890.123456".parse().unwrap()),
        Value::String("Xin chào 😀".into()),
        Value::Bytes(vec![0, 1, 2, 255]),
        Value::Uuid(u),
        Value::Date("2024-02-29".parse().unwrap()),
        Value::Time("13:14:15.1234567".parse().unwrap()),
        Value::DateTime(dt("2024-05-01T10:20:30.1234567")),
        Value::DateTimeOffset(o),
        Value::String(String::new()),
    ];
    for (i, w) in want.iter().enumerate() {
        let got = &row.values()[i];
        match (got, w) {
            (Value::F32(a), Value::F64(b)) => assert_eq!(*a as f64, *b, "{}", names[i]),
            _ => assert_eq!(got, w, "{}", names[i]),
        }
    }
    // The datetimeoffset keeps its offset.
    assert_eq!(
        row.get::<OffsetDateTime>(14)
            .unwrap()
            .offset()
            .whole_minutes(),
        420
    );
    // Declared types as the server sees them.
    let r = c
        .query(
            "SELECT CAST(SQL_VARIANT_PROPERTY(@i32, 'BaseType') AS nvarchar(30)), CAST(SQL_VARIANT_PROPERTY(@s, 'BaseType') AS nvarchar(30)), CAST(SQL_VARIANT_PROPERTY(@dt, 'BaseType') AS nvarchar(30)), CAST(SQL_VARIANT_PROPERTY(@dec, 'Scale') AS int)",
            &params,
        )
        .unwrap();
    let row = &r.rows()[0];
    assert_eq!(row.get::<String>(0).unwrap(), "int");
    assert_eq!(row.get::<String>(1).unwrap(), "nvarchar");
    assert_eq!(row.get::<String>(2).unwrap(), "datetime2");
    assert_eq!(row.get::<i32>(3).unwrap(), 6);

    // Typed NULLs of every kind.
    let nulls = vec![
        Param::new("a", Option::<bool>::None),
        Param::new("b", Option::<u8>::None),
        Param::new("c", Option::<i16>::None),
        Param::new("d", Option::<i32>::None),
        Param::new("e", Option::<i64>::None),
        Param::new("f", Option::<f32>::None),
        Param::new("g", Option::<f64>::None),
        Param::new("h", Option::<Decimal>::None),
        Param::new("i", Option::<String>::None),
        Param::new("j", Option::<Vec<u8>>::None),
        Param::new("k", Option::<Uuid>::None),
        Param::new("l", Option::<Date>::None),
        Param::new("m", Option::<Time>::None),
        Param::new("n", Option::<DateTime>::None),
        Param::new("o", Option::<OffsetDateTime>::None),
        Param::new("p", ParamValue::Null),
    ];
    let names: Vec<String> = nulls.iter().map(|p| format!("@{}", p.name())).collect();
    let r = c
        .query(&format!("SELECT {}", names.join(", ")), &nulls)
        .unwrap();
    assert!(r.rows()[0].values().iter().all(Value::is_null));
    // Large varbinary goes as varbinary(max).
    let blob: Vec<u8> = (0..20_000u32).map(|i| i as u8).collect();
    let r = c
        .query(
            "SELECT DATALENGTH(@b), @b",
            &[Param::new("b", blob.clone())],
        )
        .unwrap();
    assert_eq!(r.rows()[0].get::<i64>(0).unwrap(), 20_000);
    assert_eq!(r.rows()[0].get::<Vec<u8>>(1).unwrap(), blob);
}

#[test]
fn server_errors_carry_line_number() {
    let Some(mut c) = conn() else { return };
    match c.batch("SELECT 1;\nSELECT 2;\nSELEC 3;") {
        Err(Error::Server {
            number,
            line,
            class,
            ref message,
            ..
        }) => {
            assert_eq!(number, 102);
            assert_eq!(line, 3);
            assert_eq!(class, 15);
            assert!(message.contains("Incorrect syntax"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    // Run-time error inside a procedure: procedure name and line.
    c.batch("USE dltds_test").unwrap();
    c.batch("CREATE OR ALTER PROCEDURE dbo.dl_err_test AS\nBEGIN\n  SELECT 1;\n  RAISERROR('boom %d', 16, 7, 42);\nEND").unwrap();
    match c.query("EXEC dbo.dl_err_test", &[]) {
        Err(Error::Server {
            number,
            state,
            class,
            ref message,
            ref procedure,
            line,
            ..
        }) => {
            assert_eq!((number, state, class, line), (50000, 7, 16, 4));
            assert_eq!(message, "boom 42");
            // SQL Server 2022 qualifies the name with the schema; older versions do not.
            assert!(procedure.ends_with("dl_err_test"), "{procedure}");
        }
        other => panic!("{other:?}"),
    }
    // Several errors: the last one is reported, all are in the messages.
    c.take_messages();
    let e = c
        .batch("SELECT * FROM dbo.no_such_table_1 UNION ALL SELECT * FROM dbo.no_such_table_2")
        .unwrap_err();
    assert_eq!(e.number(), Some(208));
    let errs: Vec<_> = c
        .take_messages()
        .into_iter()
        .filter(|m| m.kind == MessageKind::Error)
        .collect();
    assert!(!errs.is_empty());
    // Errors through sp_executesql, and the connection keeps working.
    assert_eq!(
        c.query("SELECT 1/@z", &[Param::new("z", 0i32)])
            .unwrap_err()
            .number(),
        Some(8134)
    );
    c.ping().unwrap();
    // INFO messages from PRINT.
    c.batch("PRINT 'hello from print'; RAISERROR('low severity', 5, 1);")
        .unwrap();
    let msgs = c.take_messages();
    assert!(
        msgs.iter()
            .any(|m| m.kind == MessageKind::Info && m.message == "hello from print")
    );
    assert!(
        msgs.iter()
            .any(|m| m.kind == MessageKind::Info && m.message == "low severity" && m.class == 5)
    );
}

#[test]
fn rows_affected_and_transactions() {
    let Some(mut c) = conn() else { return };
    c.batch("USE dltds_test; IF OBJECT_ID('dbo.ra') IS NOT NULL DROP TABLE dbo.ra; CREATE TABLE dbo.ra (id int, v nvarchar(10));")
        .unwrap();
    assert_eq!(
        c.execute("INSERT dbo.ra VALUES (1,'a'),(2,'b'),(3,'c')", &[])
            .unwrap(),
        3
    );
    assert_eq!(
        c.execute(
            "UPDATE dbo.ra SET v = @v WHERE id >= @min",
            &[Param::new("v", "x"), Param::new("min", 2i32)]
        )
        .unwrap(),
        2
    );
    assert_eq!(c.execute("DELETE dbo.ra WHERE id = 99", &[]).unwrap(), 0);
    let r = c
        .batch("UPDATE dbo.ra SET v = 'y'; DELETE dbo.ra WHERE id = 1;")
        .unwrap();
    assert_eq!(r.row_counts(), &[3, 1]);

    assert!(!c.in_transaction());
    c.batch("BEGIN TRAN").unwrap();
    assert!(c.in_transaction());
    c.execute(
        "INSERT dbo.ra VALUES (@id, @v)",
        &[Param::new("id", 10i32), Param::new("v", "t")],
    )
    .unwrap();
    assert_eq!(
        one(&mut c, "SELECT COUNT(*) FROM dbo.ra WHERE id = 10"),
        Value::I32(1)
    );
    assert_eq!(one(&mut c, "SELECT @@TRANCOUNT"), Value::I32(1));
    c.batch("ROLLBACK").unwrap();
    assert!(!c.in_transaction());
    assert_eq!(
        one(&mut c, "SELECT COUNT(*) FROM dbo.ra WHERE id = 10"),
        Value::I32(0)
    );
    c.batch("BEGIN TRAN; INSERT dbo.ra VALUES (11, 'c'); COMMIT;")
        .unwrap();
    assert!(!c.in_transaction());
    assert_eq!(
        one(&mut c, "SELECT COUNT(*) FROM dbo.ra WHERE id = 11"),
        Value::I32(1)
    );
}

#[test]
fn three_hundred_line_procedure_batch() {
    let Some(mut c) = conn() else { return };
    c.batch("USE dltds_test").unwrap();
    let mut body = String::from(
        "CREATE OR ALTER PROCEDURE dbo.dl_big_test @n int OUTPUT AS\nBEGIN\n  SET NOCOUNT ON;\n",
    );
    for i in 0..300 {
        body.push_str(&format!("  SET @n = @n + {i}; -- line {i} Tiếng Việt\n"));
    }
    body.push_str("  SELECT @n AS total;\nEND\n");
    assert!(body.lines().count() > 300);
    c.batch(&body).unwrap();
    let r = c
        .batch("DECLARE @x int = 0; EXEC dbo.dl_big_test @x OUTPUT; SELECT @x;")
        .unwrap();
    assert_eq!(
        r.sets()[0].rows()[0].get::<i64>(0).unwrap(),
        (0..300).sum::<i64>()
    );
    // A syntax error on line 250 of a large batch.
    let mut bad = body.replace("dl_big_test", "dl_big_bad");
    let lines: Vec<&str> = bad.lines().collect();
    let mut v: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    v[249] = "  SET @n = = 1;".into();
    bad = v.join("\n");
    match c.batch(&bad) {
        Err(Error::Server { number, line, .. }) => {
            assert_eq!(number, 102);
            assert_eq!(line, 250);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn ten_thousand_rows() {
    let Some(mut c) = conn() else { return };
    let r = c
        .query(
            "SELECT TOP (@n) ROW_NUMBER() OVER (ORDER BY (SELECT 1)) AS n, CONCAT(N'row ', ROW_NUMBER() OVER (ORDER BY (SELECT 1))) AS s,
                    CAST(ROW_NUMBER() OVER (ORDER BY (SELECT 1)) * 1.5 AS decimal(18,2)) AS d
             FROM sys.all_objects a CROSS JOIN sys.all_objects b",
            &[Param::new("n", 10_000i32)],
        )
        .unwrap();
    let rows = r.rows();
    assert_eq!(rows.len(), 10_000);
    assert_eq!(rows[9_999].get::<i64>(0).unwrap(), 10_000);
    assert_eq!(rows[9_999].get::<String>(1).unwrap(), "row 10000");
    assert_eq!(rows[9_999].get::<String>(2).unwrap(), "15000.00");
    assert_eq!(r.rows_affected(), 10_000);
}

#[test]
fn packet_size_32767() {
    let Some(cfg) = config() else { return };
    let mut c = Client::connect(&cfg.with_packet_size(32767)).unwrap();
    // The server may round the size down (SQL Server 2022 answers 32576).
    eprintln!("requested 32767, server granted {}", c.packet_size());
    assert!(
        (32_000..=32_767).contains(&c.packet_size()),
        "{}",
        c.packet_size()
    );
    let big = "y".repeat(300_000);
    let r = c
        .query("SELECT @p, LEN(@p)", &[Param::new("p", big.as_str())])
        .unwrap();
    assert_eq!(r.rows()[0].get::<String>(0).unwrap(), big);
    let r = c
        .query(
            "SELECT TOP 5000 a.name FROM sys.all_objects a CROSS JOIN sys.all_objects b",
            &[],
        )
        .unwrap();
    assert_eq!(r.rows().len(), 5000);
    let r: Config = format!("{}&packet+size=512", url().unwrap())
        .parse()
        .unwrap();
    let mut c = Client::connect(&r).unwrap();
    assert_eq!(c.packet_size(), 512);
    let r = c
        .query("SELECT @p", &[Param::new("p", big.as_str())])
        .unwrap();
    assert_eq!(r.rows()[0].get::<String>(0).unwrap(), big);
}

#[test]
fn timeout_sends_attention_and_connection_survives() {
    let Some(mut c) = conn() else { return };
    c.set_timeout(Some(Duration::from_secs(1))).unwrap();
    let start = std::time::Instant::now();
    assert!(matches!(
        c.batch("WAITFOR DELAY '00:00:10'; SELECT 1"),
        Err(Error::Timeout)
    ));
    assert!(start.elapsed() < Duration::from_secs(8));
    assert!(!c.is_broken());
    c.set_timeout(None).unwrap();
    assert_eq!(one(&mut c, "SELECT 5"), Value::I32(5));
}

/// Login-only and full encryption. Requires a working dl-tls; reports and skips otherwise.
#[test]
fn encrypted_connections() {
    let Some(base) = url() else { return };
    let base = base.replace("encrypt=disable", "x=1");
    for (extra, mode, full) in [
        (
            "&encrypt=false&TrustServerCertificate=true",
            Encrypt::Off,
            false,
        ),
        (
            "&encrypt=true&TrustServerCertificate=true",
            Encrypt::Required,
            true,
        ),
    ] {
        let cfg: Config = format!("{base}{extra}").parse().unwrap();
        assert_eq!(cfg.encrypt(), mode);
        match Client::connect(&cfg) {
            Ok(mut c) => {
                assert_eq!(c.is_encrypted(), full, "{mode:?}");
                assert_eq!(one(&mut c, "SELECT 1"), Value::I32(1));
                let r = c
                    .query(
                        "SELECT encrypt_option FROM sys.dm_exec_connections WHERE session_id = @@SPID",
                        &[],
                    )
                    .unwrap();
                let e: String = r.rows()[0].get(0).unwrap();
                assert_eq!(e, if full { "TRUE" } else { "FALSE" }, "{mode:?}");
                let big = "z".repeat(100_000);
                let r = c
                    .query("SELECT @p", &[Param::new("p", big.as_str())])
                    .unwrap();
                assert_eq!(r.rows()[0].get::<String>(0).unwrap(), big);
                c.close();
                eprintln!("encrypted connection {mode:?}: ok");
            }
            Err(Error::Tls(dl_tls::Error::Unsupported(why))) => {
                eprintln!("note: dl-tls is not available ({why}); skipping {mode:?}");
            }
            Err(e) => panic!("{mode:?}: {e}"),
        }
    }
    // Default (no encrypt key): Off with TrustServerCertificate=true.
    let cfg: Config = base.parse().unwrap();
    assert_eq!(cfg.encrypt(), Encrypt::Off);
    assert!(cfg.trust_server_certificate());
}
