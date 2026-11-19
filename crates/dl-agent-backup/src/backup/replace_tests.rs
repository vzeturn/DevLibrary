//! End to end against a real SQL Server (same switches as the parent module:
//! `BACKUP_AGENT_TEST_SQL`, `BACKUP_AGENT_TEST_DIR`): a database with a few tables goes
//! through the module's real backup path (BACKUP → checksum → seal under the tenant key →
//! upload to a `LocalFolder` destination → cleanup), its data is changed and a table dropped,
//! and the real restore path (download → decrypt → `RESTORE … WITH REPLACE`) brings it back.
//! Per-table `COUNT_BIG(*)` and `CHECKSUM_AGG(BINARY_CHECKSUM(*))` must match the values
//! taken before the backup, over the original and in a copy under a new name. A backup made
//! without keys (the format before encryption) restores the same way, and another tenant's
//! key restores nothing.

use std::path::{Path, PathBuf};
use std::time::Instant;

use dl_json::json;
use dl_tds::{CancelToken, Client, Param};

use super::*;
use crate::backup::pipeline::run_with;
use crate::backup::restore::{RecordIo, RestoreParams, Source, restore_from_record_with};
use crate::crypto::envelope::{BackupKeys, DataKey, MAGIC, NoKeys};
use crate::upload::Destination;

/// One tenant's keys.
struct Keys(DataKey);

impl BackupKeys for Keys {
    fn current(&self) -> Result<Option<DataKey>, Error> {
        Ok(Some(self.0.clone()))
    }
    fn by_id(&self, id: &str) -> Result<DataKey, Error> {
        if id == self.0.id {
            Ok(self.0.clone())
        } else {
            Err(Error::Credential(format!(
                "the hub does not give this agent backup key {id}"
            )))
        }
    }
}

fn tenant(id: &str, byte: u8) -> Keys {
    Keys(DataKey {
        id: id.into(),
        key: [byte; 32],
    })
}

/// (table, rows, CHECKSUM_AGG(BINARY_CHECKSUM(*))), `None` for a table that is gone.
type Sums = Vec<(String, Option<(i64, i32)>)>;

const TABLES: [&str; 3] = ["customers", "orders", "notes"];

fn client(e: &Env) -> Client {
    let mut cfg = e.target.config();
    if let Some(mode) = e.sql.0 {
        cfg = cfg.with_encrypt(mode);
    }
    Client::connect(&cfg).unwrap_or_else(|err| panic!("connect: {err}"))
}

/// Runs `body` inside database `db` (`USE` in a dynamic batch; `db` is bound, never spliced).
fn in_db(e: &Env, db: &str, body: &str) {
    let sql = "DECLARE @s nvarchar(max) = N'USE ' + QUOTENAME(@db) + N'; ' + @body; EXEC(@s);";
    client(e)
        .query(sql, &[Param::new("db", db), Param::new("body", body)])
        .unwrap_or_else(|err| panic!("{body}: {err}"));
}

fn sums(e: &Env, db: &str) -> Sums {
    let mut c = client(e);
    TABLES
        .iter()
        .map(|t| {
            let sql = format!(
                "IF OBJECT_ID(QUOTENAME(@db) + N'.dbo.{t}') IS NULL SELECT CAST(NULL AS bigint) AS n, \
                 CAST(NULL AS int) AS c ELSE BEGIN DECLARE @s nvarchar(max) = N'SELECT COUNT_BIG(*) AS n, \
                 CHECKSUM_AGG(BINARY_CHECKSUM(*)) AS c FROM ' + QUOTENAME(@db) + N'.dbo.{t}'; \
                 EXEC sp_executesql @s; END"
            );
            let r = c
                .query(&sql, &[Param::new("db", db)])
                .unwrap_or_else(|err| panic!("checksum {t}: {err}"));
            let row = r.rows().last().cloned().expect("one row");
            let n = row.try_get::<Option<i64>>("n").flatten();
            let sum = row.try_get::<Option<i32>>("c").flatten();
            ((*t).to_owned(), n.map(|n| (n, sum.unwrap_or(0))))
        })
        .collect()
}

fn show(what: &str, s: &Sums) {
    let parts: Vec<String> = s
        .iter()
        .map(|(t, v)| match v {
            Some((n, c)) => format!("{t}: rows={n} checksum={c}"),
            None => format!("{t}: (missing)"),
        })
        .collect();
    eprintln!("RESULT {what}: {}", parts.join("; "));
}

/// Three tables with a few thousand rows of mixed types, Vietnamese text and binary data.
fn populate(e: &Env, db: &str) {
    in_db(
        e,
        db,
        "CREATE TABLE dbo.customers (id int PRIMARY KEY, name nvarchar(100) NOT NULL, \
           balance decimal(18,2) NOT NULL, joined date NOT NULL); \
         CREATE TABLE dbo.orders (id int PRIMARY KEY, customer_id int NOT NULL, amount money NOT NULL, \
           created datetime2(3) NOT NULL, status char(1) NOT NULL); \
         CREATE TABLE dbo.notes (id int PRIMARY KEY, body nvarchar(max) NOT NULL, blob varbinary(256) NULL); \
         WITH n AS (SELECT TOP (10000) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS i \
                    FROM sys.all_objects a CROSS JOIN sys.all_objects b) \
         INSERT INTO dbo.orders SELECT i, 1 + i % 2000, CAST(i * 1.37 AS money), \
           DATEADD(minute, i, '2026-01-01'), CHAR(65 + i % 3) FROM n; \
         WITH n AS (SELECT TOP (2000) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS i FROM sys.all_objects a \
                    CROSS JOIN sys.all_objects b) \
         INSERT INTO dbo.customers SELECT i, N'Khách hàng ' + CAST(i AS nvarchar(10)) + N' — Hồ Chí Minh', \
           CAST(i * 10.25 AS decimal(18,2)), DATEADD(day, i % 365, '2025-01-01') FROM n; \
         WITH n AS (SELECT TOP (500) ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) AS i FROM sys.all_objects) \
         INSERT INTO dbo.notes SELECT i, REPLICATE(N'ghi chú đặt phòng ', 1 + i % 20), \
           CAST(HASHBYTES('SHA2_256', CAST(i AS varchar(10))) AS varbinary(256)) FROM n;",
    );
}

/// Changes every table: updated and deleted rows, a new row, one table dropped.
fn damage(e: &Env, db: &str) {
    in_db(
        e,
        db,
        "UPDATE dbo.customers SET balance = balance + 1 WHERE id % 7 = 0; \
         DELETE FROM dbo.orders WHERE id % 3 = 0; \
         INSERT INTO dbo.customers VALUES (999999, N'mới', 0, '2026-11-19'); \
         DROP TABLE dbo.notes;",
    );
}

fn params(e: &Env, db: &str, dest: &Path, id: &str) -> Params {
    Params {
        history_id: id.into(),
        schedule_id: "replace-test".into(),
        database_name: db.into(),
        backup_type: BackupType::Full,
        target: e.target.clone(),
        destination: Some(Destination {
            provider: "LocalFolder".into(),
            tokens: json!({"remoteFolderPath": dest.display().to_string()}),
        }),
        backup_dir: e.dir.clone(),
    }
}

/// The module's restore path for a backup kept only at the destination.
fn restore_cloud(
    e: &Env,
    keys: &dyn BackupKeys,
    r: &BackupResult,
    dest: &Path,
    target_db: &str,
    tag: &str,
) -> Result<Steps, Error> {
    let src = Source {
        local_file_path: r.local_file_path.clone(),
        cloud_file_path: r.cloud_file_path.clone(),
        tag: tag.into(),
        destination: params(e, "x", dest, "x").destination,
    };
    let p = RestoreParams {
        target: e.target.clone(),
        target_database: target_db.into(),
        with_replace: true,
        backup_file: String::new(),
    };
    let io = RecordIo {
        uploaders: &Real,
        keys,
        backup_dir: &e.dir,
    };
    let mut steps = Steps::new();
    let t0 = Instant::now();
    restore_from_record_with(&e.sql, &io, &src, p, &CancelToken::new(), &mut steps)?;
    eprintln!(
        "RESULT restore {tag} → {target_db}: {} ms",
        t0.elapsed().as_millis()
    );
    Ok(steps)
}

fn dest_dir(e: &Env, tag: &str) -> PathBuf {
    let d = Path::new(&e.dir).join(format!("dest_{tag}_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn sealed_backup_restores_with_replace_and_to_a_new_name_with_matching_checksums() {
    let Some(e) = env() else { return };
    let (db, copy) = (name("rep"), name("repnew"));
    let _dbs = Dbs(&e, vec![db.clone(), copy.clone()]);
    create(&e, &db, "FULL");
    populate(&e, &db);
    let before = sums(&e, &db);
    show("before backup", &before);
    assert!(before.iter().all(|(_, v)| v.is_some_and(|(n, _)| n > 0)));

    let dest = dest_dir(&e, "sealed");
    let keys = tenant("tenant-a:1", 0xa1);
    let mut steps = Steps::new();
    let t0 = Instant::now();
    let r = run_with(
        &e.sql,
        &Real,
        &keys,
        &params(&e, &db, &dest, "h-rep"),
        &CancelToken::new(),
        &mut steps,
    )
    .unwrap();
    eprintln!(
        "RESULT backup: {} bytes .bak, sha256 {}…, key {}, {} ms, options {}",
        r.bytes,
        &r.checksum[..12],
        r.key_id,
        t0.elapsed().as_millis(),
        r.options
    );
    assert_eq!(r.key_id, "tenant-a:1");
    assert!(
        r.cloud_file_path.ends_with(".bak.dlbk"),
        "{}",
        r.cloud_file_path
    );
    assert!(!Path::new(&r.local_file_path).exists(), "cleaned up");
    let sealed = std::fs::read(&r.cloud_file_path).unwrap();
    assert_eq!(&sealed[..8], MAGIC);
    eprintln!(
        "RESULT sealed copy at the destination: {} bytes",
        sealed.len()
    );

    damage(&e, &db);
    let damaged = sums(&e, &db);
    show("after damage", &damaged);
    assert_ne!(damaged, before);

    // Another tenant's agent: refused before SQL Server is asked; the database stays as is.
    let err =
        restore_cloud(&e, &tenant("tenant-b:1", 0xb2), &r, &dest, &db, "foreign").unwrap_err();
    eprintln!("RESULT restore with tenant B keys: {} ({err})", err.code());
    assert!(matches!(err, Error::Credential(_)), "{err}");
    assert_eq!(sums(&e, &db), damaged);

    let steps = restore_cloud(&e, &keys, &r, &dest, &db, "replace").unwrap();
    assert_eq!(steps.get("decrypt").unwrap().status, "done");
    let after = sums(&e, &db);
    show("after RESTORE WITH REPLACE", &after);
    assert_eq!(after, before);

    restore_cloud(&e, &keys, &r, &dest, &copy, "newname").unwrap();
    let copied = sums(&e, &copy);
    show("new-name copy", &copied);
    assert_eq!(copied, before);
    assert_eq!(sums(&e, &db), before, "the source is untouched");

    for f in std::fs::read_dir(&e.dir).unwrap() {
        let n = f.unwrap().file_name().to_string_lossy().into_owned();
        assert!(!n.starts_with("restore_"), "left behind: {n}");
    }
    std::fs::remove_dir_all(&dest).unwrap();
}

#[test]
fn an_unencrypted_backup_from_before_tenant_keys_still_restores() {
    let Some(e) = env() else { return };
    let (db, copy) = (name("legacy"), name("legacynew"));
    let _dbs = Dbs(&e, vec![db.clone(), copy.clone()]);
    create(&e, &db, "FULL");
    populate(&e, &db);
    let before = sums(&e, &db);
    show("legacy before backup", &before);

    let dest = dest_dir(&e, "legacy");
    let mut steps = Steps::new();
    let r = run_with(
        &e.sql,
        &Real,
        &NoKeys,
        &params(&e, &db, &dest, "h-legacy"),
        &CancelToken::new(),
        &mut steps,
    )
    .unwrap();
    assert!(r.key_id.is_empty());
    assert!(r.cloud_file_path.ends_with(".bak"), "{}", r.cloud_file_path);
    assert_eq!(steps.get("encrypt").unwrap().status, "skipped");

    damage(&e, &db);
    // Restored by an agent that has tenant keys: the plaintext copy needs none.
    let keys = tenant("tenant-a:1", 0xa1);
    let steps = restore_cloud(&e, &keys, &r, &dest, &db, "legacy").unwrap();
    assert_eq!(steps.get("decrypt").unwrap().status, "skipped");
    let after = sums(&e, &db);
    show("legacy after RESTORE WITH REPLACE", &after);
    assert_eq!(after, before);
    restore_cloud(&e, &keys, &r, &dest, &copy, "legacynew").unwrap();
    assert_eq!(sums(&e, &copy), before);
    std::fs::remove_dir_all(&dest).unwrap();
}
