//! The `Bool`, `Int`, `Tcvn3Text` and `Unicode` column kinds of `master.upsert` (RT-0831) on
//! a real SQL Server: written through the command unit, read back the way the row-hash
//! recheck reads them (TCVN3 columns as varbinary) and decoded with the repo's TCVN3 decoder;
//! plus the behaviour of the vendor `dbo.Unicode2TCVN` the rules rest on. Gated on
//! `PDA_SYNC_TEST_SQL` (a dl-tds URL of a login that may create databases); each test creates
//! its own `CT_RT0831K_*` database with `dbo.Unicode2TCVN` copied verbatim from CT_SMILE_POS
//! and drops it afterwards.

use std::sync::Arc;

use dl_agent_pda_sync::commands::master::{
    ITEM_CLASS, MasterColumn, MasterEntity, MasterKey, MasterKeyColumn, encode_tcvn3,
};
use dl_agent_pda_sync::commands::{
    CommandError, ConfigSource, SiteCommandStore, SqlSiteCommandStore,
};
use dl_agent_pda_sync::contracts::mapping::{RawValue, VendorRow, tcvn3};
use dl_agent_pda_sync::contracts::time::DateTimeOffset;
use dl_agent_pda_sync::contracts::wire::{CommandStatus, SyncCommandResult};
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_tds::Param;
use dl_uuid::Uuid;

const COMMAND_LOG: &str = include_str!("../assets/sql/030_command_log.sql");

/// 17 characters: the `VName` column is `varchar(17)`.
const SPA: &str = "Gội đầu thảo dược";

const PROBE_COLUMNS: &[MasterColumn] = &[
    MasterColumn::boolean("Flag"),
    MasterColumn::int("Qty", -5, 100_000),
    MasterColumn::tcvn3("Name", 40),
    MasterColumn::tcvn3("VName", 17),
    MasterColumn::unicode("UName", 100),
];

/// The parsed single-integer key.
fn key_of(entity: &MasterEntity, key: i32) -> MasterKey {
    entity.parse_key(&key.to_string()).unwrap()
}

/// A compile-time constant: the TCVN3 columns go through the vendor function, the nvarchar
/// one does not.
const PROBE: MasterEntity = MasterEntity {
    wire: "kinds_probe",
    table: "KindsProbe",
    key: &[MasterKeyColumn::int("Id")],
    writable: PROBE_COLUMNS,
    update_sql: concat!(
        "UPDATE dbo.KindsProbe SET Flag = @Flag, Qty = @Qty, ",
        dl_agent_pda_sync::master_tcvn3_set!("Name"),
        ", ",
        dl_agent_pda_sync::master_tcvn3_set!("VName"),
        ", UName = @UName WHERE Id = @Key;"
    ),
    ..ITEM_CLASS
};

/// Declared longer than the table column on purpose: the length rule is what normally
/// refuses an overlong value.
const WIDE: MasterEntity = MasterEntity {
    writable: &[MasterColumn::tcvn3("VName", 40)],
    update_sql: concat!(
        "UPDATE dbo.KindsProbe SET ",
        dl_agent_pda_sync::master_tcvn3_set!("VName"),
        " WHERE Id = @Key;"
    ),
    ..PROBE
};

const WIDE_UNICODE: MasterEntity = MasterEntity {
    writable: &[MasterColumn::unicode("UName", 200)],
    update_sql: "UPDATE dbo.KindsProbe SET UName = @UName WHERE Id = @Key;",
    ..PROBE
};

const PLAIN_TCVN3: MasterEntity = MasterEntity {
    writable: &[MasterColumn::tcvn3("VName", 17)],
    update_sql: "UPDATE dbo.KindsProbe SET VName = @VName WHERE Id = @Key;",
    ..PROBE
};

// Verbatim from CT_SMILE_POS (sys.sql_modules, SQL Server 16.0, collation
// SQL_Latin1_General_CP1_CI_AS).
const UNICODE2TCVN: &str = r#"create FUNCTION [dbo].[Unicode2TCVN] (@strInput NVARCHAR(4000))
RETURNS VARCHAR(4000)
AS
BEGIN
DECLARE @TCVN CHAR(671)
DECLARE @UNICODE CHAR(671)
SET @TCVN = ',184 ,181 ,182 ,183 ,185 ,168 ,190 ,187 ,188 ,189 ,198 ,169 ,202 ,199 ,200 ,201 ,203 ,208 ,204 ,206 ,207 ,209 ,170 ,213 ,210 ,211 ,212 ,214 ,221 ,215 ,216 ,220 ,222 ,227 ,223 ,225 ,226 ,228 ,171 ,232 ,229 ,230 ,231 ,233 ,172 ,237 ,234 ,235 ,236 ,238 ,243 ,239 ,241 ,242 ,244 ,173 ,248 ,245 ,246 ,247 ,249 ,253 ,250 ,251 ,252 ,254 ,174 ,184 ,181 ,182 ,183 ,185 ,161 ,190 ,187 ,188 ,189 ,198 ,162 ,202 ,199 ,200 ,201 ,203 ,208 ,204 ,206 ,207 ,209 ,163 ,213 ,210 ,211 ,212 ,214 ,221 ,215 ,216 ,220 ,222 ,227 ,223 ,225 ,226 ,228 ,164 ,232 ,229 ,230 ,231 ,233 ,165 ,237 ,234 ,235 ,236 ,238 ,243 ,239 ,241 ,242 ,244 ,166 ,248 ,245 ,246 ,247 ,249 ,253 ,250 ,251 ,252 ,254 ,167 ,'
SET @UNICODE = ',225 ,224 ,7843,227 ,7841,259 ,7855,7857,7859,7861,7863,226 ,7845,7847,7849,7851,7853,233 ,232 ,7867,7869,7865,234 ,7871,7873,7875,7877,7879,237 ,236 ,7881,297 ,7883,243 ,242 ,7887,245 ,7885,244 ,7889,7891,7893,7895,7897,417 ,7899,7901,7903,7905,7907,250 ,249 ,7911,361 ,7909,432 ,7913,7915,7917,7919,7921,253 ,7923,7927,7929,7925,273 ,193 ,192 ,7842,195 ,7840,258 ,7854,7856,7858,7860,7862,194 ,7844,7846,7848,7850,7852,201 ,200 ,7866,7868,7864,202 ,7870,7872,7874,7876,7878,205 ,204 ,7880,296 ,7882,211 ,210 ,7886,213 ,7884,212 ,7888,7890,7892,7894,7896,416 ,7898,7900,7902,7904,7906,218 ,217 ,7910,360 ,7908,431 ,7912,7914,7916,7918,7920,221 ,7922,7926,7928,7924,272 ,'
IF @strInput IS NULL RETURN NULL
IF @strInput = '' RETURN NULL
DECLARE @strOutput VARCHAR(4000)
DECLARE @COUNTER INT
DECLARE @POSITION INT
SET @COUNTER = 1
SET @strOutput = ''
WHILE (@COUNTER <= LEN(@strInput))
BEGIN
SET @POSITION = CHARINDEX(','+CONVERT(CHAR(4),UNICODE(SUBSTRING(@strInput, @COUNTER, 1)))+',', @UNICODE, 1)
IF @POSITION > 0
SET @strOutput = @strOutput + CHAR(CONVERT(INT,SUBSTRING(@TCVN, @POSITION+1, 4)))
ELSE
SET @strOutput = @strOutput + SUBSTRING(@strInput, @COUNTER, 1)
SET @COUNTER = @COUNTER + 1
END
RETURN @strOutput
END
"#;

fn admin_url() -> Option<String> {
    std::env::var("PDA_SYNC_TEST_SQL")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

struct Store {
    admin: dl_tds::Config,
    database: String,
}

impl Store {
    fn create(url: &str) -> Store {
        let admin: dl_tds::Config = url.parse().expect("PDA_SYNC_TEST_SQL is a dl-tds URL");
        let tag = Uuid::new_v4().simple().to_string();
        let s = Store {
            admin: admin.with_database("master"),
            database: format!("CT_RT0831K_{}", &tag[..10]),
        };
        s.exec_in("master", &format!("CREATE DATABASE [{}];", s.database));
        s.exec(COMMAND_LOG);
        s.exec(
            "CREATE TABLE dbo.KindsProbe (Id int NOT NULL PRIMARY KEY, Flag bit NULL, Qty int NULL, Name char(40) NULL,
    VName varchar(17) NULL, UName nvarchar(100) NULL);
INSERT dbo.KindsProbe (Id) VALUES (1), (2), (3);",
        );
        s.exec(UNICODE2TCVN);
        s
    }

    fn exec_in(&self, database: &str, sql: &str) {
        let mut c = dl_tds::Client::connect(&self.admin.clone().with_database(database)).unwrap();
        c.batch(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        c.close();
    }

    fn exec(&self, sql: &str) {
        self.exec_in(&self.database, sql);
    }

    fn config(&self) -> dl_tds::Config {
        self.admin.clone().with_database(&self.database)
    }

    fn store(&self) -> SqlSiteCommandStore {
        let cfg = self.config();
        let source: ConfigSource = Arc::new(move || Ok(cfg.clone()));
        SqlSiteCommandStore::new(source)
    }

    /// The command unit's update in its own transaction (rolled back on error).
    fn write(
        &self,
        entity: &MasterEntity,
        key: i32,
        values: &[(&str, &str)],
    ) -> Result<bool, CommandError> {
        let id = Uuid::new_v4();
        let mut changed = false;
        self.store().run_once(
            id,
            &mut |unit, ctx| {
                changed = unit.update_master_row(entity, &key_of(entity, key), values, ctx)?;
                Ok(done(id))
            },
            &Ctx::background(),
        )?;
        Ok(changed)
    }

    /// The row the way the row-hash recheck reads it: TCVN3 columns as raw bytes.
    fn read(&self, key: i32) -> VendorRow {
        let id = Uuid::new_v4();
        let mut row = None;
        let bytes = ["Name".to_owned(), "VName".to_owned()];
        self.store()
            .run_once(
                id,
                &mut |unit, ctx| {
                    row = unit.read_master_row(
                        &PROBE,
                        &key_of(&PROBE, key),
                        false,
                        &[],
                        &bytes,
                        ctx,
                    )?;
                    Ok(done(id))
                },
                &Ctx::background(),
            )
            .unwrap();
        row.unwrap()
    }

    /// What `dbo.Unicode2TCVN` returns for `value`, as bytes; `None` for NULL.
    fn vendor(&self, value: &str) -> Option<Vec<u8>> {
        let mut c = dl_tds::Client::connect(&self.config()).unwrap();
        let r = c
            .query(
                "SELECT CONVERT(varbinary(8000), dbo.Unicode2TCVN(@v));",
                &[Param::new("v", value)],
            )
            .unwrap();
        let v: Option<Vec<u8>> = r.rows()[0].get(0).unwrap();
        c.close();
        v
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        if let Ok(mut c) = dl_tds::Client::connect(&self.admin) {
            let _ = c.batch(&format!(
                "IF DB_ID(N'{db}') IS NOT NULL BEGIN ALTER DATABASE [{db}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{db}]; END;",
                db = self.database
            ));
            c.close();
        }
    }
}

fn done(id: Uuid) -> SyncCommandResult {
    SyncCommandResult {
        command_id: id,
        status: CommandStatus::Applied,
        reason: None,
        result: None,
        at_utc: DateTimeOffset::now_utc(),
        exec_ms: None,
        hops: Default::default(),
    }
}

fn bytes(row: &VendorRow, column: &str) -> Vec<u8> {
    match row.get(column) {
        Some(RawValue::Bytes(b)) => b.clone(),
        other => panic!("{column}: {other:?}"),
    }
}

macro_rules! store {
    () => {{
        let Some(url) = admin_url() else {
            eprintln!("skipped: PDA_SYNC_TEST_SQL is not set");
            return;
        };
        Store::create(&url)
    }};
}

#[test]
fn writes_bool_int_tcvn3_and_unicode_columns_and_reads_them_back() {
    let db = store!();
    let unicode = format!("{SPA} 🌿");
    let mut values = vec![
        ("Flag", "true"),
        ("Qty", "-5"),
        ("Name", SPA),
        ("VName", SPA),
        ("UName", unicode.as_str()),
    ];
    for (column, (name, value)) in PROBE_COLUMNS.iter().zip(&values) {
        assert_eq!(column.problem(value), None, "{name}");
    }

    assert!(db.write(&PROBE, 1, &values).unwrap());

    let row = db.read(1);
    assert_eq!(row.get("Flag"), Some(&RawValue::Bool(true)));
    assert_eq!(row.get("Qty"), Some(&RawValue::I32(-5)));
    let vname = bytes(&row, "VName");
    assert_eq!(Some(vname.clone()), encode_tcvn3(SPA));
    assert_eq!(vname.len(), 17);
    assert_eq!(tcvn3::decode(&vname), SPA);
    let name = bytes(&row, "Name");
    assert_eq!(name.len(), 40, "char(40) pads with blanks");
    assert_eq!(tcvn3::decode(&name).trim_end_matches(' '), SPA);
    assert_eq!(row.get("UName"), Some(&RawValue::String(unicode.clone())));

    values[0].1 = "0";
    values[1].1 = "100000";
    values[2].1 = "Ăn uống";
    values[4].1 = "Ấn Độ";
    assert!(db.write(&PROBE, 1, &values).unwrap());
    let row = db.read(1);
    assert_eq!(row.get("Flag"), Some(&RawValue::Bool(false)));
    assert_eq!(row.get("Qty"), Some(&RawValue::I32(100_000)));
    assert_eq!(
        tcvn3::decode(&bytes(&row, "Name")).trim_end_matches(' '),
        "Ăn uống"
    );
    assert_eq!(row.get("UName"), Some(&RawValue::String("Ấn Độ".into())));

    values[0].1 = "1";
    assert!(db.write(&PROBE, 1, &values).unwrap());
    assert_eq!(db.read(1).get("Flag"), Some(&RawValue::Bool(true)));
}

#[test]
fn every_character_the_rule_accepts_is_stored_as_the_encoder_says_and_reads_back() {
    let db = store!();
    let probe = MasterColumn::tcvn3("C", 2);
    // A trailing "x" lets the blank be judged as a character, not as a trailing blank.
    let accepted: String = (0u32..0x1F00)
        .filter_map(char::from_u32)
        .filter(|ch| probe.problem(&format!("{ch}x")).is_none())
        .collect();
    assert_eq!(accepted.chars().count(), 95 + 74);

    assert_eq!(db.vendor(&accepted), encode_tcvn3(&accepted));
    for ch in accepted.chars().filter(|ch| *ch > '~') {
        let one = ch.to_string();
        assert_eq!(tcvn3::decode(&db.vendor(&one).unwrap()), one);
    }
}

#[test]
fn capital_toned_letters_lose_their_case_in_the_vendor_function_so_they_are_refused() {
    let db = store!();
    let one = MasterColumn::tcvn3("C", 1);
    let capitals: String = (0u32..0x1F00)
        .filter_map(char::from_u32)
        .filter(|ch| *ch > '~' && encode_tcvn3(&ch.to_string()).is_some())
        .filter(|ch| one.problem(&ch.to_string()).is_some())
        .collect();
    assert_eq!(capitals.chars().count(), 60);

    let stored = db.vendor(&capitals);

    assert_eq!(stored, encode_tcvn3(&capitals));
    assert_eq!(tcvn3::decode(&stored.unwrap()), capitals.to_lowercase());
}

#[test]
fn the_vendor_function_drops_trailing_blanks_and_turns_blank_text_into_null() {
    let db = store!();
    assert_eq!(db.vendor(" ab  "), Some(b" ab".to_vec()));
    assert_eq!(db.vendor(""), None);
    assert_eq!(db.vendor("   "), None);
    let name = MasterColumn::tcvn3("Name", 40);
    for value in [" ab  ", ""] {
        assert_eq!(
            name.problem(value).as_deref(),
            Some("Name must not be empty or end with a blank")
        );
    }
}

#[test]
fn a_value_longer_than_the_column_raises_instead_of_truncating() {
    let db = store!();
    let long = format!("{SPA}!");
    let e = db.write(&WIDE, 2, &[("VName", long.as_str())]).unwrap_err();
    assert!(matches!(e.sql_number(), Some(2628 | 8152)), "{e}");
    let long = "ộ".repeat(101);
    let e = db
        .write(&WIDE_UNICODE, 2, &[("UName", long.as_str())])
        .unwrap_err();
    assert!(matches!(e.sql_number(), Some(2628 | 8152)), "{e}");

    let row = db.read(2);
    assert_eq!(row.get("VName"), Some(&RawValue::Null));
    assert_eq!(row.get("UName"), Some(&RawValue::Null));
}

#[test]
fn a_tcvn3_column_written_without_the_vendor_function_is_refused_before_it_runs() {
    let db = store!();
    let e = db.write(&PLAIN_TCVN3, 3, &[("VName", SPA)]).unwrap_err();
    assert!(
        e.to_string().contains(
            "kinds_probe: VName is a TCVN3 column and must be written only as dbo.Unicode2TCVN(@VName)."
        ),
        "{e}"
    );
    assert_eq!(db.read(3).get("VName"), Some(&RawValue::Null));
}
