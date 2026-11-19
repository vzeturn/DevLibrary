//! Port of `Staff/SqlStaffSourceTests.cs`: the staff source on a real SQL Server as a
//! read-only login (ADR 0101). Gated on `PDA_SYNC_TEST_SQL` (a dl-tds URL of a login that
//! may create databases and logins); the test creates its own `pda_sync_test_*` database
//! and reader login and drops both afterwards.

use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::wire::{
    StaffClerk, StaffSnapshot, staff_function_ids, staff_pin_status,
};
use dl_agent_pda_sync::staff::{SqlStaffSource, StaffSource};
use dl_agent_pda_sync::util::ctx::Ctx;
use std::collections::HashMap;

const CS_TESTS: &str = include_str!("../testdata/cs/Staff/StaffPinHasherTests.cs");

fn golden(marker: &str) -> &'static str {
    let at = CS_TESTS.find(marker).unwrap() + marker.len();
    let rest = &CS_TESTS[at..];
    let start = rest.find('"').unwrap() + 1;
    let len = rest[start..].find('"').unwrap();
    &rest[start..start + len]
}

struct SmilePos {
    admin: dl_tds::Config,
    database: String,
    login: String,
    password: String,
}

impl SmilePos {
    fn create(url: &str) -> SmilePos {
        let admin: dl_tds::Config = url.parse().expect("PDA_SYNC_TEST_SQL is a dl-tds URL");
        let tag = dl_uuid::Uuid::new_v4().simple().to_string();
        let pos = SmilePos {
            admin: admin.with_database("master"),
            database: format!("pda_sync_test_{}", &tag[..12]),
            login: format!("pda_sync_test_{}", &tag[..12]),
            password: format!("P{tag}"),
        };
        // DDL cannot take parameters; names and password are generated alphanumeric.
        pos.exec_in(
            "master",
            &format!(
                "CREATE DATABASE [{db}]; CREATE LOGIN [{l}] WITH PASSWORD = N'{p}', CHECK_POLICY = OFF, CHECK_EXPIRATION = OFF;",
                db = pos.database,
                l = pos.login,
                p = pos.password
            ),
        );
        pos.exec(&format!(
            "CREATE USER [{l}] FOR LOGIN [{l}]; ALTER ROLE [db_datareader] ADD MEMBER [{l}];",
            l = pos.login
        ));
        pos
    }

    fn exec_in(&self, database: &str, sql: &str) {
        let mut c = dl_tds::Client::connect(&self.admin.clone().with_database(database)).unwrap();
        c.batch(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        c.close();
    }

    fn exec(&self, sql: &str) {
        self.exec_in(&self.database, sql);
    }

    fn user_object_count(&self) -> i64 {
        let mut c =
            dl_tds::Client::connect(&self.admin.clone().with_database(&self.database)).unwrap();
        let r = c
            .query(
                "SELECT COUNT(*) FROM sys.objects WHERE is_ms_shipped = 0",
                &[],
            )
            .unwrap();
        let n: i64 = r.rows()[0].get(0).unwrap();
        c.close();
        n
    }

    fn source(&self) -> SqlStaffSource {
        let cfg = self
            .admin
            .clone()
            .credentials(&self.login, &self.password)
            .with_database(&self.database)
            .with_app_name("PdaSyncAgent");
        SqlStaffSource::new(move || Ok(cfg.clone()))
    }

    fn read(&self) -> HashMap<String, StaffClerk> {
        let key = dl_encoding::hex::decode(golden("GoldenKey =")).unwrap();
        self.source()
            .read(&key, &Ctx::background())
            .unwrap()
            .into_iter()
            .map(|c| (c.key.clone(), c))
            .collect()
    }
}

impl Drop for SmilePos {
    fn drop(&mut self) {
        if let Ok(mut c) = dl_tds::Client::connect(&self.admin) {
            let _ = c.batch(&format!(
                "IF DB_ID(N'{db}') IS NOT NULL BEGIN ALTER DATABASE [{db}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{db}]; END;
IF SUSER_ID(N'{l}') IS NOT NULL DROP LOGIN [{l}];",
                db = self.database,
                l = self.login
            ));
            c.close();
        }
    }
}

#[test]
fn reads_the_clerks_hashed_with_their_rvcs_and_mapped_permissions_and_survives_an_older_schema() {
    let Some(url) = std::env::var("PDA_SYNC_TEST_SQL")
        .ok()
        .filter(|s| !s.trim().is_empty())
    else {
        eprintln!("skipped: PDA_SYNC_TEST_SQL is not set");
        return;
    };
    let pos = SmilePos::create(&url);
    pos.exec(
        "CREATE TABLE dbo.CashierDef (Initial varchar(10) NOT NULL PRIMARY KEY, FullName varchar(50) NULL, Security int NULL,
    TouchID varchar(20) NULL, PassWord varchar(20) NULL, CardID varchar(20) NULL, IsDisable bit NULL);
CREATE TABLE dbo.CashierRVC (Cashier varchar(10) NOT NULL, RVC int NOT NULL, PRIMARY KEY (Cashier, RVC));
CREATE TABLE dbo.ClerkPermission (ClerkID varchar(10) NOT NULL, FunctionID varchar(30) NOT NULL, Allowed bit NULL);
INSERT dbo.CashierDef VALUES ('LAN  ', 'Lan Anh  ', 1, '482193  ', 'secret-pw', 'card-77', 0),
    ('BINH', 'Binh', 5, '12ab', NULL, NULL, 0),
    ('OFF', 'Off', 1, '555555', NULL, NULL, 1),
    ('SML', 'Platform', 9, '999999', NULL, NULL, 0),
    ('NOPIN', NULL, NULL, NULL, NULL, NULL, NULL);
INSERT dbo.CashierRVC VALUES ('LAN', 2), ('LAN', 1), ('OFF', 1);
INSERT dbo.ClerkPermission VALUES ('LAN', 'PT_Payment', 1), ('LAN', 'pt_newcheck', 1), ('LAN', 'PT_Void', 1),
    ('LAN', 'SS_Kitchen', 0), ('NOPIN', 'Cashier', NULL);",
    );
    let objects = pos.user_object_count();

    let clerks = pos.read();

    let mut keys: Vec<_> = clerks.keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["BINH", "LAN", "NOPIN", "OFF", "SML"]); // ADR 0114: SML too
    assert_eq!(
        clerks["LAN"],
        StaffClerk {
            key: "LAN".into(),
            name: "Lan Anh".into(),
            active: true,
            pin_lookup: Some(golden("GoldenLookup =").into()),
            pin_status: staff_pin_status::SET.into(),
            security: Some(1),
            rvcs: vec![1, 2],
            functions: vec![
                staff_function_ids::NEW_CHECK.into(),
                staff_function_ids::PAYMENT.into()
            ],
        }
    );
    let b = &clerks["BINH"];
    assert!(
        b.security == Some(5)
            && b.pin_status == staff_pin_status::UNUSABLE
            && b.pin_lookup.is_none()
            && b.rvcs.is_empty()
            && b.functions.is_empty()
    );
    let off = &clerks["OFF"];
    assert!(!off.active && off.pin_lookup.is_none() && off.pin_status == staff_pin_status::NONE);
    let n = &clerks["NOPIN"];
    assert!(
        n.active
            && n.name.is_empty()
            && n.security.is_none()
            && n.pin_status == staff_pin_status::NONE
            && n.functions.is_empty()
    );
    let wire = StaffSnapshot {
        agent_id: "a".into(),
        agent_version: "v".into(),
        key_id: "k".into(),
        clerks: clerks.values().cloned().collect(),
    }
    .to_json();
    for s in ["482193", "secret-pw", "card-77", "555555", "PT_Void"] {
        assert!(!wire.contains(s), "{s}");
    }

    // An older POS build: no IsDisable, no Security. Every clerk is then active and
    // carries no security level.
    pos.exec("ALTER TABLE dbo.CashierDef DROP COLUMN IsDisable, Security;");
    let older = pos.read();
    let off = &older["OFF"];
    assert!(off.active && off.pin_status == staff_pin_status::SET && off.security.is_none());
    assert!(older.values().all(|c| c.security.is_none()));

    assert_eq!(pos.user_object_count(), objects); // the source created nothing
}
