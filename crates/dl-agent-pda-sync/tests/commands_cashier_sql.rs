//! `master.upsert` entity `cashier` on a real SQL Server (ADR 0150, RT-0834), mirroring
//! the .NET `SqlCashierUpsertIntegrationTests`: the hash the executor checks is the hash
//! the staff sync computes, both conflict directions, the create with its PIN check, and
//! the `120_cashier_master_grants.sql` grants exercised as the `pda_sync` database user.
//! Gated on `PDA_SYNC_TEST_SQL` (a dl-tds URL of a login that may create databases); each
//! test creates its own `CT_RT0834_*` database and drops it afterwards. The executor runs
//! as that login; the grants test impersonates a user without login.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use dl_agent_pda_sync::commands::{
    CashierCommandSupport, ConfigSource, SiteCommandExecutor, SqlSiteCommandStore,
};
use dl_agent_pda_sync::config::SystemClock;
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::cashier::{self, CashierWrite};
use dl_agent_pda_sync::contracts::staff;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, MasterUpsertPayload, MasterUpsertResult, SyncCommand, SyncCommandResult,
    site_command_types, staff_function_ids,
};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::staff::{SqlStaffSource, StaffError, StaffPinKey, StaffSource};
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_uuid::Uuid;

const COMMAND_LOG: &str = include_str!("../assets/sql/030_command_log.sql");
const CASHIER_GRANTS: &str = include_str!("../assets/sql/120_cashier_master_grants.sql");
const PIN_KEY: [u8; 32] = [7; 32];

fn admin_url() -> Option<String> {
    std::env::var("PDA_SYNC_TEST_SQL")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

struct Support;

impl CashierCommandSupport for Support {
    fn pin_key(&self, key_id: Option<&str>, _: &Ctx) -> Result<Option<StaffPinKey>, StaffError> {
        let id = staff::key_id(&PIN_KEY);
        Ok(
            (key_id.is_none() || key_id == Some(id.as_str())).then(|| StaffPinKey {
                key: PIN_KEY.to_vec(),
                key_id: id,
            }),
        )
    }

    fn clerks_changed(&self) {}
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
            database: format!("CT_RT0834_{}", &tag[..10]),
        };
        s.exec_in("master", &format!("CREATE DATABASE [{}];", s.database));
        s.exec(COMMAND_LOG);
        // The vendor columns of CT_SMILE_POS: FullName is varchar (TCVN3), PassWord/CardID
        // must stay out of reach.
        s.exec(
            "CREATE TABLE dbo.CashierDef (Initial varchar(10) NOT NULL PRIMARY KEY, FullName varchar(50) NULL, Security int NULL,
    TouchID varchar(20) NULL, PassWord varchar(20) NULL, CardID varchar(20) NULL, IsDisable bit NULL);
CREATE TABLE dbo.CashierRVC (Cashier varchar(10) NOT NULL, RVC int NOT NULL);
CREATE TABLE dbo.ClerkPermission (ClerkID varchar(10) NOT NULL, FunctionID varchar(50) NOT NULL, Allowed bit NULL);",
        );
        s.exec(
            "EXEC (N'CREATE FUNCTION dbo.Unicode2TCVN (@s nvarchar(4000)) RETURNS varchar(4000) AS BEGIN RETURN CAST(@s AS varchar(4000)); END');",
        );
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

    fn text(&self, sql: &str) -> String {
        let mut c = dl_tds::Client::connect(&self.config()).unwrap();
        let r = c.query(sql, &[]).unwrap();
        let v: String = r.rows()[0].get(0).unwrap();
        c.close();
        v
    }

    fn count(&self, sql: &str) -> i64 {
        let mut c = dl_tds::Client::connect(&self.config()).unwrap();
        let r = c.query(sql, &[]).unwrap();
        let n: i64 = r.rows()[0].get(0).unwrap();
        c.close();
        n
    }

    fn seed(&self, pin: &str) -> String {
        static NEXT: AtomicU32 = AtomicU32::new(100);
        let key = format!("T{}", NEXT.fetch_add(1, Ordering::SeqCst));
        self.exec(&format!(
            "INSERT dbo.CashierDef (Initial, FullName, Security, TouchID, PassWord, IsDisable) VALUES ('{key}', 'Lan', 1, '{pin}', 'secret', 0);
INSERT dbo.CashierRVC (Cashier, RVC) VALUES ('{key}', 1);
INSERT dbo.ClerkPermission (ClerkID, FunctionID, Allowed) VALUES ('{key}', 'PT_NewCheck', 1), ('{key}', 'PT_Pickup', 1), ('{key}', 'ZZ_Other', 1);"
        ));
        key
    }

    /// The clerk's hash exactly as the staff sync sends it to the cloud.
    fn staff_hash(&self, key: &str) -> String {
        let cfg = self.config();
        let clerks = SqlStaffSource::new(move || Ok(cfg.clone()))
            .read(&PIN_KEY, &Ctx::background())
            .unwrap();
        let clerk = clerks.iter().find(|c| c.key == key).unwrap();
        cashier::row_hash(clerk)
    }

    fn run(&self, c: &SyncCommand) -> SyncCommandResult {
        let cfg = self.config();
        let source: ConfigSource = Arc::new(move || Ok(cfg.clone()));
        SiteCommandExecutor::new(
            Arc::new(SqlSiteCommandStore::new(source)),
            SystemClock::shared(),
            &Logger::none(),
        )
        .with_cashiers(Arc::new(Support))
        .execute(c, &Ctx::background())
        .unwrap()
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

fn upsert(
    key: &str,
    expected: Option<&str>,
    name: &str,
    pin: Option<&str>,
    rvcs: &[i32],
) -> SyncCommand {
    static NONCE: AtomicU32 = AtomicU32::new(1);
    let pin_sealed = pin.map(|p| {
        let mut nonce = [0u8; 12];
        nonce[..4].copy_from_slice(&NONCE.fetch_add(1, Ordering::SeqCst).to_le_bytes());
        cashier::seal_pin(&PIN_KEY, key, p, nonce).unwrap()
    });
    let write = CashierWrite {
        name: name.to_owned(),
        active: true,
        security: 1,
        rvcs: rvcs.to_vec(),
        functions: vec![
            staff_function_ids::PAYMENT.to_owned(),
            staff_function_ids::CASHIER.to_owned(),
        ],
        pin_sealed,
        pin_key_id: pin.map(|_| staff::key_id(&PIN_KEY)),
    };
    let payload = MasterUpsertPayload {
        entity: cashier::ENTITY.to_owned(),
        key: key.to_owned(),
        fields: write.to_fields(),
        expected_hash: expected.map(str::to_owned),
        actor: "admin-1".to_owned(),
    };
    SyncCommand {
        command_id: Uuid::new_v4(),
        type_: site_command_types::MASTER_UPSERT.into(),
        aggregate_key: format!("master:cashier:{key}"),
        expected_hash: None,
        payload: payload.to_element(),
        created_by: "admin-1".into(),
        expires_at_utc: None,
        contract_version: 1,
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
    }
}

fn applied(r: &SyncCommandResult) -> MasterUpsertResult {
    assert_eq!(r.status, CommandStatus::Applied, "{:?}", r.reason);
    MasterUpsertResult::from_element(r.result.as_ref().unwrap()).unwrap()
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
fn edit_on_the_staff_sync_hash_writes_the_three_tables_and_returns_the_hash_the_staff_sync_then_reads()
 {
    let db = store!();
    let key = db.seed("1111");
    let hash = db.staff_hash(&key);
    let res = applied(&db.run(&upsert(
        &key,
        Some(&hash),
        "Lan Pham",
        Some("2222"),
        &[1, 2],
    )));
    assert_eq!(res.row_hash, db.staff_hash(&key));
    assert_ne!(res.row_hash, hash);
    let one = |col: &str| {
        db.text(&format!(
            "SELECT {col} FROM dbo.CashierDef WHERE Initial = '{key}'"
        ))
    };
    assert_eq!(one("TouchID"), "2222");
    assert_eq!(one("PassWord"), "secret");
    assert_eq!(
        db.text(&format!("SELECT STRING_AGG(CAST(RVC AS varchar(5)), ',') WITHIN GROUP (ORDER BY RVC) FROM dbo.CashierRVC WHERE Cashier = '{key}'")),
        "1,2"
    );
    assert_eq!(
        db.text(&format!("SELECT STRING_AGG(FunctionID + ':' + CAST(Allowed AS varchar(1)), ',') WITHIN GROUP (ORDER BY FunctionID) FROM dbo.ClerkPermission WHERE ClerkID = '{key}'")),
        "Cashier:1,PT_NewCheck:0,PT_Payment:1,PT_Pickup:0,ZZ_Other:1",
        "unmapped permissions stay; absent mapped ones turn off"
    );
}

#[test]
fn pos_win_edit_after_the_editor_read_is_refused_and_the_row_is_unchanged() {
    let db = store!();
    let key = db.seed("3333");
    let hash = db.staff_hash(&key);
    db.exec(&format!(
        "INSERT dbo.CashierRVC (Cashier, RVC) VALUES ('{key}', 9);"
    ));
    let r = db.run(&upsert(&key, Some(&hash), "Lan Phạm", None, &[1, 2]));
    assert_eq!(r.reason.as_deref(), Some("master_changed"));
    assert_eq!(
        db.text(&format!(
            "SELECT FullName FROM dbo.CashierDef WHERE Initial = '{key}'"
        )),
        "Lan"
    );
    assert_eq!(
        db.count(&format!(
            "SELECT CAST(COUNT(*) AS bigint) FROM dbo.CashierRVC WHERE Cashier = '{key}'"
        )),
        2
    );
}

#[test]
fn cloud_first_write_makes_an_older_cloud_hash_stale_and_pos_win_still_edits_freely() {
    let db = store!();
    let key = db.seed("4444");
    let hash = db.staff_hash(&key);
    applied(&db.run(&upsert(&key, Some(&hash), "First", None, &[1, 2])));
    let r = db.run(&upsert(&key, Some(&hash), "Second", None, &[1, 2]));
    assert_eq!(r.reason.as_deref(), Some("master_changed"));
    db.exec(&format!(
        "UPDATE dbo.CashierDef SET FullName = 'POS' WHERE Initial = '{key}'"
    ));
    assert_eq!(
        db.text(&format!(
            "SELECT FullName FROM dbo.CashierDef WHERE Initial = '{key}'"
        )),
        "POS"
    );
}

#[test]
fn create_inserts_the_clerk_and_a_pin_of_another_active_clerk_is_refused() {
    let db = store!();
    let taken = db.seed("5555");
    let key = "N101";
    let r = db.run(&upsert(key, None, "Lan Phạm", Some("5555"), &[1, 2]));
    assert_eq!(r.reason.as_deref(), Some("cashier_pin_taken"));
    let count = || {
        db.count(&format!(
            "SELECT CAST(COUNT(*) AS bigint) FROM dbo.CashierDef WHERE Initial = '{key}'"
        ))
    };
    assert_eq!(count(), 0);
    let res = applied(&db.run(&upsert(key, None, "Moi", Some("6666"), &[3])));
    assert_eq!(res.key, key);
    assert_eq!(res.row_hash, db.staff_hash(key));
    assert_eq!(count(), 1);
    assert_eq!(
        db.count(&format!(
            "SELECT CAST(IsDisable AS bigint) FROM dbo.CashierDef WHERE Initial = '{key}'"
        )),
        0
    );
    assert_ne!(taken, key);
}

#[test]
fn the_cashier_grants_let_pda_sync_write_the_clerk_but_not_the_password_the_card_or_another_table()
{
    let db = store!();
    let key = db.seed("7777");
    db.exec(
        "CREATE USER [pda_sync] WITHOUT LOGIN; ALTER ROLE db_datareader ADD MEMBER [pda_sync];
GRANT EXECUTE ON OBJECT::dbo.Unicode2TCVN TO [pda_sync];",
    );
    db.exec(CASHIER_GRANTS);
    // The statements the executor runs, as pda_sync.
    db.exec(&format!(
        "EXECUTE AS USER = 'pda_sync';
UPDATE dbo.CashierDef SET FullName = LEFT(dbo.Unicode2TCVN(N'As agent'), 50), IsDisable = 0, Security = 2, TouchID = '8888' WHERE Initial = '{key}';
INSERT INTO dbo.CashierDef (Initial, FullName, TouchID, IsDisable, Security) VALUES ('N9', 'New', '9999', 0, 1);
DELETE FROM dbo.CashierRVC WHERE Cashier = '{key}';
INSERT INTO dbo.CashierRVC (Cashier, RVC) VALUES ('{key}', 4);
UPDATE dbo.ClerkPermission SET Allowed = 0 WHERE ClerkID = '{key}' AND FunctionID = 'PT_Pickup';
INSERT INTO dbo.ClerkPermission (ClerkID, FunctionID, Allowed) VALUES ('{key}', 'PT_Payment', 1);
REVERT;"
    ));
    assert_eq!(
        db.text(&format!(
            "SELECT FullName FROM dbo.CashierDef WHERE Initial = '{key}'"
        )),
        "As agent"
    );
    for denied in [
        format!("UPDATE dbo.CashierDef SET PassWord = 'x' WHERE Initial = '{key}'"),
        format!("UPDATE dbo.CashierDef SET CardID = 'x' WHERE Initial = '{key}'"),
        format!("DELETE FROM dbo.CashierDef WHERE Initial = '{key}'"),
        format!("DELETE FROM dbo.ClerkPermission WHERE ClerkID = '{key}'"),
        format!("UPDATE dbo.ClerkPermission SET FunctionID = 'x' WHERE ClerkID = '{key}'"),
    ] {
        let mut c = dl_tds::Client::connect(&db.config()).unwrap();
        c.batch("EXECUTE AS USER = 'pda_sync';").unwrap();
        let err = c.batch(&denied).expect_err(&denied);
        assert!(err.to_string().contains("permission"), "{denied}: {err}");
        c.close();
    }
    assert_eq!(
        db.text(&format!(
            "SELECT PassWord FROM dbo.CashierDef WHERE Initial = '{key}'"
        )),
        "secret"
    );
    // Running the script again is a no-op (idempotent).
    db.exec(CASHIER_GRANTS);
}
