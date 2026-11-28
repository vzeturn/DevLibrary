//! Two-way integration of the master-data group "cashier accounts, PIN, branch (RVC)
//! permissions, roles" on a real SQL Server (RT-0988, ADR 0150): cloud -> SMILE_POS
//! (`master.upsert` entity `cashier`), SMILE_POS -> cloud (the staff sync read of the same
//! clerk), the conflict in both orders, the branch scope of a write, and the exactly-once
//! audit row (`Pda_SyncCommandLog`). Synthetic clerks only. Gated on `PDA_SYNC_TEST_SQL`
//! (a dl-tds URL of a login that may create databases); each test creates its own
//! `CT_RT0988_*` database and drops it afterwards.

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
    CommandStatus, MasterUpsertPayload, MasterUpsertResult, StaffClerk, SyncCommand,
    SyncCommandResult, site_command_types, staff_function_ids,
};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::staff::{SqlStaffSource, StaffError, StaffPinKey, StaffSource};
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_uuid::Uuid;

const COMMAND_LOG: &str = include_str!("../assets/sql/030_command_log.sql");
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
            database: format!("CT_RT0988_{}", &tag[..10]),
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

struct Edit<'a> {
    key: &'a str,
    expected: Option<&'a str>,
    name: &'a str,
    pin: Option<&'a str>,
    active: bool,
    security: i32,
    rvcs: &'a [i32],
    functions: &'a [&'a str],
}

impl<'a> Edit<'a> {
    fn new(key: &'a str, expected: Option<&'a str>) -> Edit<'a> {
        Edit {
            key,
            expected,
            name: "Lan",
            pin: None,
            active: true,
            security: 1,
            rvcs: &[1],
            functions: &[staff_function_ids::PAYMENT],
        }
    }

    fn command(&self) -> SyncCommand {
        static NONCE: AtomicU32 = AtomicU32::new(1);
        let pin_sealed = self.pin.map(|p| {
            let mut nonce = [0u8; 12];
            nonce[..4].copy_from_slice(&NONCE.fetch_add(1, Ordering::SeqCst).to_le_bytes());
            cashier::seal_pin(&PIN_KEY, self.key, p, nonce).unwrap()
        });
        let write = CashierWrite {
            name: self.name.to_owned(),
            active: self.active,
            security: self.security,
            rvcs: self.rvcs.to_vec(),
            functions: self.functions.iter().map(|f| (*f).to_owned()).collect(),
            pin_sealed,
            pin_key_id: self.pin.map(|_| staff::key_id(&PIN_KEY)),
        };
        let payload = MasterUpsertPayload {
            entity: cashier::ENTITY.to_owned(),
            key: self.key.to_owned(),
            fields: write.to_fields(),
            expected_hash: self.expected.map(str::to_owned),
            actor: "admin-1".to_owned(),
        };
        SyncCommand {
            command_id: Uuid::new_v4(),
            type_: site_command_types::MASTER_UPSERT.into(),
            aggregate_key: format!("master:cashier:{}", self.key),
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

impl Store {
    /// The clerk exactly as the staff sync reads it for the cloud.
    fn clerk(&self, key: &str) -> StaffClerk {
        let cfg = self.config();
        SqlStaffSource::new(move || Ok(cfg.clone()))
            .read(&PIN_KEY, &Ctx::background())
            .unwrap()
            .into_iter()
            .find(|c| c.key == key)
            .unwrap()
    }

    fn rvcs(&self, key: &str) -> String {
        self.text(&format!("SELECT ISNULL(STRING_AGG(CAST(RVC AS varchar(5)), ',') WITHIN GROUP (ORDER BY RVC), '') FROM dbo.CashierRVC WHERE Cashier = '{key}'"))
    }

    fn log_rows(&self, id: Uuid) -> i64 {
        self.count(&format!(
            "SELECT CAST(COUNT(*) AS bigint) FROM dbo.Pda_SyncCommandLog WHERE CommandId = '{id}'"
        ))
    }
}

#[test]
fn cloud_to_pos_edit_writes_name_role_branches_functions_and_pin_which_the_staff_sync_reads_back() {
    let db = store!();
    let key = db.seed("1111");
    let hash = db.staff_hash(&key);
    let mut e = Edit::new(&key, Some(&hash));
    e.name = "Lan Pham";
    e.pin = Some("2468");
    e.security = 5;
    e.rvcs = &[1, 3];
    let res = applied(&db.run(&e.command()));
    let clerk = db.clerk(&key);
    assert_eq!(cashier::row_hash(&clerk), res.row_hash);
    assert_eq!(clerk.name, "Lan Pham");
    assert_eq!(clerk.security, Some(5));
    assert_eq!(clerk.rvcs, vec![1, 3]);
    assert_eq!(
        clerk.functions,
        vec![staff_function_ids::PAYMENT.to_owned()]
    );
    let lookup = dl_encoding::base64::STANDARD.encode(staff::lookup(&PIN_KEY, "2468").unwrap());
    assert_eq!(clerk.pin_lookup.as_deref(), Some(lookup.as_str()));
    assert_eq!(
        db.text(&format!(
            "SELECT PassWord FROM dbo.CashierDef WHERE Initial = '{key}'"
        )),
        "secret",
        "the vendor password is out of reach"
    );
}

#[test]
fn pos_to_cloud_edit_in_pos_win_changes_the_staff_sync_clerk_and_its_hash() {
    let db = store!();
    let key = db.seed("1111");
    let before = db.clerk(&key);
    db.exec(&format!(
        "UPDATE dbo.CashierDef SET Security = 9, TouchID = '4321', IsDisable = 1 WHERE Initial = '{key}';
DELETE FROM dbo.CashierRVC WHERE Cashier = '{key}';
INSERT dbo.CashierRVC (Cashier, RVC) VALUES ('{key}', 7), ('{key}', 8);
UPDATE dbo.ClerkPermission SET Allowed = 0 WHERE ClerkID = '{key}' AND FunctionID = 'PT_NewCheck';"
    ));
    let after = db.clerk(&key);
    assert!(before.active && !after.active);
    assert_eq!(after.security, Some(9));
    assert_eq!(after.rvcs, vec![7, 8]);
    assert_ne!(after.pin_lookup, before.pin_lookup);
    assert_ne!(cashier::row_hash(&before), cashier::row_hash(&after));
    assert_eq!(db.staff_hash(&key), cashier::row_hash(&after));
}

#[test]
fn conflict_pos_edit_first_refuses_the_cloud_write_and_cloud_write_first_refuses_the_older_hash() {
    let db = store!();
    let key = db.seed("1111");
    let hash = db.staff_hash(&key);
    // POS first: the clerk's PIN changes after the editor read the hash.
    db.exec(&format!(
        "UPDATE dbo.CashierDef SET TouchID = '5151' WHERE Initial = '{key}'"
    ));
    let mut e = Edit::new(&key, Some(&hash));
    e.name = "Cloud";
    let stale = e.command();
    let r = db.run(&stale);
    assert_eq!(r.reason.as_deref(), Some("master_changed"));
    assert_eq!(
        db.text(&format!(
            "SELECT FullName FROM dbo.CashierDef WHERE Initial = '{key}'"
        )),
        "Lan"
    );
    // Cloud first: a write on the fresh hash wins, the second one on the old hash loses.
    let fresh = db.staff_hash(&key);
    e.expected = Some(&fresh);
    applied(&db.run(&e.command()));
    e.name = "Second";
    let r = db.run(&e.command());
    assert_eq!(r.reason.as_deref(), Some("master_changed"));
    assert_eq!(
        db.text(&format!(
            "SELECT FullName FROM dbo.CashierDef WHERE Initial = '{key}'"
        )),
        "Cloud"
    );
}

#[test]
fn permission_branch_scope_a_write_replaces_only_this_clerks_branches_and_keeps_unmapped_functions()
{
    let db = store!();
    let key = db.seed("1111");
    let other = db.seed("2222");
    db.exec(&format!(
        "INSERT dbo.CashierRVC (Cashier, RVC) VALUES ('{key}', 2), ('{key}', 9);"
    ));
    let other_rvcs = db.rvcs(&other);
    let hash = db.staff_hash(&key);
    let mut e = Edit::new(&key, Some(&hash));
    e.rvcs = &[2];
    e.functions = &[];
    applied(&db.run(&e.command()));
    assert_eq!(db.rvcs(&key), "2", "branches 1 and 9 are revoked, 2 stays");
    assert_eq!(
        db.rvcs(&other),
        other_rvcs,
        "another clerk keeps its branches"
    );
    assert_eq!(
        db.text(&format!("SELECT STRING_AGG(FunctionID + ':' + CAST(Allowed AS varchar(1)), ',') WITHIN GROUP (ORDER BY FunctionID) FROM dbo.ClerkPermission WHERE ClerkID = '{key}'")),
        "PT_NewCheck:0,PT_Pickup:0,ZZ_Other:1",
        "mapped functions off, the vendor-only one untouched"
    );
    assert_eq!(
        db.text(&format!("SELECT STRING_AGG(FunctionID + ':' + CAST(Allowed AS varchar(1)), ',') WITHIN GROUP (ORDER BY FunctionID) FROM dbo.ClerkPermission WHERE ClerkID = '{other}'")),
        "PT_NewCheck:1,PT_Pickup:1,ZZ_Other:1"
    );
}

#[test]
fn audit_row_each_applied_command_leaves_one_log_row_and_a_redelivery_does_not_write_again() {
    let db = store!();
    let key = db.seed("1111");
    let hash = db.staff_hash(&key);
    let mut e = Edit::new(&key, Some(&hash));
    e.name = "Audited";
    let cmd = e.command();
    assert_eq!(db.log_rows(cmd.command_id), 0);
    let first = applied(&db.run(&cmd));
    assert_eq!(db.log_rows(cmd.command_id), 1);
    assert_eq!(
        db.text(&format!(
            "SELECT Status FROM dbo.Pda_SyncCommandLog WHERE CommandId = '{}'",
            cmd.command_id
        )),
        "Applied"
    );
    // A POS edit after the first apply must survive the redelivery of the same command.
    db.exec(&format!(
        "UPDATE dbo.CashierDef SET FullName = 'POS' WHERE Initial = '{key}'"
    ));
    let again = applied(&db.run(&cmd));
    assert_eq!(again.row_hash, first.row_hash);
    assert_eq!(db.log_rows(cmd.command_id), 1);
    assert_eq!(
        db.text(&format!(
            "SELECT FullName FROM dbo.CashierDef WHERE Initial = '{key}'"
        )),
        "POS"
    );
    // A refused command is logged too, with its reason.
    let stale = e.command();
    let r = db.run(&stale);
    assert_eq!(r.reason.as_deref(), Some("master_changed"));
    assert_eq!(db.log_rows(stale.command_id), 1);
}
