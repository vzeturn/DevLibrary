use std::collections::BTreeMap;
use std::time::Duration;

use dl_json::{Value, json};

use super::*;
use crate::sql::bundle::Bundle;

/// One stored object: definition (None = encrypted) and its `dl_bundle` stamp.
#[derive(Clone, Default)]
struct Obj {
    def: Option<String>,
    stamp: Option<String>,
}

/// A database that understands just the statements the installer sends.
#[derive(Default)]
struct Db {
    version: &'static str,
    compat: &'static str,
    objects: BTreeMap<String, Obj>,
    saved: Option<BTreeMap<String, Obj>>,
    scripts: Vec<String>,
    log: Vec<String>,
}

fn key(name: &str) -> String {
    name.replace(['[', ']'], "").to_lowercase()
}

impl Db {
    fn new(version: &'static str, compat: &'static str) -> Db {
        Db {
            version,
            compat,
            ..Db::default()
        }
    }

    fn with(mut self, name: &str, def: Option<&str>, stamp: Option<&str>) -> Db {
        self.objects.insert(
            key(name),
            Obj {
                def: def.map(str::to_string),
                stamp: stamp.map(str::to_string),
            },
        );
        self
    }
}

fn param<'a>(params: &[(&str, &'a str)], k: &str) -> &'a str {
    params.iter().find(|(n, _)| *n == k).map(|p| p.1).unwrap()
}

impl Connection for Db {
    fn ping(&mut self) -> Result<(), Error> {
        Ok(())
    }
    fn is_broken(&self) -> bool {
        false
    }
    fn in_transaction(&self) -> bool {
        self.saved.is_some()
    }
    fn set_query_timeout(&mut self, _: Option<Duration>) -> Result<(), Error> {
        Ok(())
    }

    fn run(&mut self, sql: &str, params: &[(&str, &str)]) -> Result<(), Error> {
        self.log
            .push(sql.split_whitespace().take(3).collect::<Vec<_>>().join(" "));
        if sql == "BEGIN TRANSACTION" {
            self.saved = Some(self.objects.clone());
            return Ok(());
        }
        if sql == "COMMIT TRANSACTION" {
            self.saved = None;
            return Ok(());
        }
        if sql.contains("ROLLBACK") {
            if let Some(s) = self.saved.take() {
                self.objects = s;
            }
            return Ok(());
        }
        if sql.contains("sp_addextendedproperty") {
            let k = key(param(params, "obj"));
            let o = self
                .objects
                .get_mut(&k)
                .ok_or_else(|| Error::Sql("no object".into()))?;
            o.stamp = Some(param(params, "v").to_string());
            return Ok(());
        }
        if sql.contains("FAIL") {
            return Err(Error::Sql(
                "sql_missing_object: Invalid object name 'dbo.Nope'.".into(),
            ));
        }
        let words: Vec<&str> = sql.split_whitespace().collect();
        match words.first().copied() {
            Some("CREATE" | "ALTER") => {
                let name = key(words[2].split('(').next().unwrap());
                let exists = self.objects.contains_key(&name);
                match (words[0], exists) {
                    ("CREATE", true) => Err(Error::Sql("There is already an object".into())),
                    ("ALTER", false) => {
                        Err(Error::Sql("sql_missing_object: Invalid object".into()))
                    }
                    _ => {
                        let o = self.objects.entry(name).or_default();
                        o.def = Some(sql.to_string());
                        Ok(())
                    }
                }
            }
            _ => {
                self.scripts.push(sql.to_string());
                Ok(())
            }
        }
    }

    fn query_json(&mut self, sql: &str, params: &[(&str, &str)]) -> Result<Vec<Value>, Error> {
        if sql.contains("SERVERPROPERTY") {
            return Ok(vec![
                json!({"product_version": self.version, "compatibility_level": self.compat,
                "code_page": "1258", "db_owner": "1"}),
            ]);
        }
        if sql.contains("OBJECT_DEFINITION") {
            let k = key(param(params, "n"));
            return Ok(vec![match self.objects.get(&k) {
                None => json!({"id": null, "def": null, "stamp": null}),
                Some(o) => json!({"id": "42", "def": o.def.clone(), "stamp": o.stamp.clone()}),
            }]);
        }
        if sql.contains("FROM sys.objects") {
            return Ok(self
                .objects
                .keys()
                .map(|k| {
                    let (s, n) = k.split_once('.').unwrap();
                    json!({"s": s, "n": n, "t": "P"})
                })
                .collect());
        }
        Err(Error::Sql(format!("unexpected query: {sql}")))
    }
}

fn bundle(proc_sql: &str) -> Bundle {
    Bundle::from_json(&json!({
        "product": "einvoice", "version": "1.4.0",
        "objects": [
            {"kind": "script", "name": "dbo.add_MaCQT",
             "sql": "IF COL_LENGTH('dbo.HoaDon', 'MaCQT') IS NULL ALTER TABLE dbo.HoaDon ADD MaCQT varchar(50) NULL"},
            {"kind": "procedure", "name": "dbo.usp_GetBill", "sql": proc_sql}
        ],
        "requires": {"procedures": ["dbo.usp_GetBill"]}
    }))
    .unwrap()
}

const PROC: &str = "CREATE OR ALTER PROCEDURE dbo.usp_GetBill @Id int AS SELECT 1 AS x";

fn names(v: &Value, k: &str) -> Vec<String> {
    v[k].as_array()
        .unwrap()
        .iter()
        .map(|x| {
            x.get("action")
                .and_then(Value::as_str)
                .or(x.as_str())
                .unwrap()
                .to_string()
        })
        .collect()
}

#[test]
fn installs_on_2008_r2_with_create_then_alter_and_is_idempotent() {
    let mut db = Db::new("10.50.1600.1", "100");
    let b = bundle(PROC);
    let r = install(&mut db, &b, &Options::default()).unwrap();
    assert_eq!(r["ok"].as_bool(), Some(true), "{r}");
    assert_eq!(r["level"].as_u64(), Some(100));
    assert_eq!(names(&r, "applied"), ["script", "create"]);
    let stored = &db.objects["dbo.usp_getbill"];
    assert!(
        stored
            .def
            .as_deref()
            .unwrap()
            .starts_with("CREATE PROCEDURE")
    );
    assert!(
        stored
            .stamp
            .as_deref()
            .unwrap()
            .starts_with("einvoice@1.4.0 ")
    );
    assert!(!db.in_transaction());

    // Same bundle again: the procedure is stamped, only the idempotent script runs.
    let r = install(&mut db, &b, &Options::default()).unwrap();
    assert_eq!(names(&r, "applied"), ["script"]);
    assert_eq!(names(&r, "unchanged"), ["dbo.usp_GetBill"]);
}

#[test]
fn replacing_backs_up_the_old_definition_and_uses_alter() {
    let dir = std::env::temp_dir().join(format!("dl-install-{}", std::process::id()));
    let mut db = Db::new("16.0.4135.4", "160").with(
        "dbo.usp_GetBill",
        Some("CREATE PROCEDURE dbo.usp_GetBill AS SELECT 0"),
        Some("einvoice@1.3.0 abc"),
    );
    let opts = Options {
        backup_dir: Some(dir.clone()),
        ..Options::default()
    };
    let r = install(&mut db, &bundle(PROC), &opts).unwrap();
    assert_eq!(r["ok"].as_bool(), Some(true), "{r}");
    assert_eq!(names(&r, "applied"), ["script", "alter"]);
    let saved = std::fs::read_to_string(dir.join("dbo.usp_GetBill.sql")).unwrap();
    assert_eq!(saved, "CREATE PROCEDURE dbo.usp_GetBill AS SELECT 0");
    assert!(
        db.objects["dbo.usp_getbill"]
            .def
            .as_deref()
            .unwrap()
            .starts_with("ALTER PROCEDURE")
    );
    std::fs::remove_dir_all(dir).unwrap();

    // Without a backup folder nothing is touched.
    let mut db = Db::new("16.0.4135.4", "160").with("dbo.usp_GetBill", Some("old"), None);
    assert!(install(&mut db, &bundle(PROC), &Options::default()).is_err());
    assert!(db.log.is_empty());
}

#[test]
fn dry_run_rolls_back_and_a_failure_changes_nothing() {
    let mut db = Db::new("10.50.1600.1", "100");
    let dry = Options {
        dry_run: true,
        ..Options::default()
    };
    let r = install(&mut db, &bundle(PROC), &dry).unwrap();
    assert_eq!(r["ok"].as_bool(), Some(true));
    assert_eq!(names(&r, "applied"), ["script", "create"]);
    assert!(db.objects.is_empty(), "rolled back");
    assert_eq!(db.log.last().map(String::as_str), Some("IF @@TRANCOUNT >"));

    let bad = "CREATE PROCEDURE dbo.usp_GetBill AS SELECT * FROM FAIL";
    let r = install(&mut db, &bundle(bad), &Options::default()).unwrap();
    assert_eq!(r["ok"].as_bool(), Some(false));
    assert_eq!(r["failed"]["code"].as_str(), Some("sql_missing_object"));
    assert_eq!(names(&r, "applied"), ["script"]);
    assert!(db.objects.is_empty());
}

#[test]
fn refuses_newer_syntax_and_unreadable_definitions_before_any_change() {
    let mut db = Db::new("10.50.1600.1", "100");
    let modern = "CREATE PROCEDURE dbo.usp_GetBill AS BEGIN TRY SELECT IIF(1=1,1,0) END TRY BEGIN CATCH THROW; END CATCH";
    let r = install(&mut db, &bundle(modern), &Options::default()).unwrap();
    assert_eq!(r["ok"].as_bool(), Some(false));
    assert_eq!(
        r["refused"][0]["features"],
        json!(["IIF (level 110)", "THROW (level 110)"])
    );
    // 2012 engine but a database left at compatibility 100: still refused.
    let mut db = Db::new("11.0.7001.0", "100");
    assert_eq!(
        install(&mut db, &bundle(modern), &Options::default()).unwrap()["ok"],
        json!(false)
    );
    assert!(db.log.is_empty());

    let mut db = Db::new("16.0.4135.4", "160").with("dbo.usp_GetBill", None, None);
    let r = install(&mut db, &bundle(PROC), &Options::default()).unwrap();
    assert_eq!(r["unreadable"], json!(["dbo.usp_GetBill"]));
    assert!(db.log.is_empty());
    let forced = Options {
        force: true,
        ..Options::default()
    };
    assert_eq!(
        install(&mut db, &bundle(PROC), &forced).unwrap()["ok"],
        json!(true)
    );
}

#[test]
fn a_failing_requirement_rolls_back() {
    let mut db = Db::new("16.0.4135.4", "160");
    let b = Bundle::from_json(&json!({
        "product": "pms", "version": "2.0.0",
        "objects": [{"kind": "procedure", "name": "dbo.p1", "sql": "CREATE PROCEDURE dbo.p1 AS SELECT 1"}],
        "requires": {"procedures": ["dbo.p1", "dbo.p2"]}
    }))
    .unwrap();
    let r = install(&mut db, &b, &Options::default()).unwrap();
    assert_eq!(r["ok"].as_bool(), Some(false));
    assert_eq!(r["probe"]["ok"].as_bool(), Some(false));
    assert!(db.objects.is_empty());
}

#[test]
fn repair_installs_once_after_a_missing_object() {
    use dl_ed25519::SigningKey;
    use dl_encoding::base64::STANDARD as B64;
    let key = SigningKey::from_seed(&[3u8; 32]);
    let trusted = TrustedKeys::new()
        .with_key("rel", &B64.encode(key.verifying_key().as_bytes()))
        .unwrap();
    let mut b = bundle(PROC);
    let mut db = Db::new("10.50.1600.1", "100");
    let call = |c: &mut Db| {
        if c.objects.contains_key("dbo.usp_getbill") {
            Ok("bill")
        } else {
            Err(Error::Sql(
                "sql_missing_object: Could not find stored procedure.".into(),
            ))
        }
    };
    // Unsigned bundle: verification fails, nothing installed.
    assert!(with_repair(&mut db, &b, &trusted, &Options::default(), call).is_err());
    assert!(db.objects.is_empty());
    b.sign("rel", &key);
    let mut calls2 = 0;
    let got = with_repair(&mut db, &b, &trusted, &Options::default(), |c: &mut Db| {
        calls2 += 1;
        if c.objects.contains_key("dbo.usp_getbill") {
            Ok("bill")
        } else {
            Err(Error::Sql(
                "sql_missing_object: Could not find stored procedure.".into(),
            ))
        }
    })
    .unwrap();
    assert_eq!((got, calls2), ("bill", 2));
    // Other errors are returned as they are, without an install.
    let mut db = Db::new("10.50.1600.1", "100");
    let e = with_repair(&mut db, &b, &trusted, &Options::default(), |_: &mut Db| {
        Err::<(), _>(Error::Sql("sql_deadlock: chosen as deadlock victim".into()))
    });
    assert!(e.is_err() && db.log.is_empty());
}

#[test]
#[ignore = "needs a SQL Server in DL_AGENT_LIVE_SQL"]
fn live_install_alters_in_place_backs_up_and_rolls_back_a_failing_bundle() {
    use crate::sql::{Connector, TdsConnector, transport};
    let cs = std::env::var("DL_AGENT_LIVE_SQL").expect("DL_AGENT_LIVE_SQL");
    let want = std::env::var("DL_AGENT_LIVE_SQL_RUNG").unwrap_or("as_configured".into());
    let c = TdsConnector::parse(&cs)
        .unwrap()
        .with_transport(transport::Rung::parse(&want).unwrap());
    let mut s = Connector::connect(&c).unwrap();
    let cleanup = "IF OBJECT_ID('dbo.dl_bundle_v') IS NOT NULL DROP VIEW dbo.dl_bundle_v; \
        IF OBJECT_ID('dbo.dl_bundle_p') IS NOT NULL DROP PROCEDURE dbo.dl_bundle_p; \
        IF OBJECT_ID('dbo.dl_bundle_f') IS NOT NULL DROP FUNCTION dbo.dl_bundle_f; \
        IF OBJECT_ID('dbo.dl_bundle_t') IS NOT NULL DROP TABLE dbo.dl_bundle_t;";
    s.run(cleanup, &[]).unwrap();
    s.run(
        "CREATE TABLE dbo.dl_bundle_t (Id int NOT NULL, Ten varchar(20) NULL)",
        &[],
    )
    .unwrap();
    s.run("CREATE PROCEDURE dbo.dl_bundle_p AS SELECT 0 AS old", &[])
        .unwrap();
    s.run("GRANT EXECUTE ON dbo.dl_bundle_p TO public", &[])
        .unwrap();

    let objects = |view_from: &str| {
        json!([
            {"kind": "script", "name": "dbo.add_MaCQT",
             "sql": "IF COL_LENGTH('dbo.dl_bundle_t', 'MaCQT') IS NULL ALTER TABLE dbo.dl_bundle_t ADD MaCQT varchar(50) NULL"},
            {"kind": "function", "name": "dbo.dl_bundle_f",
             "sql": "CREATE OR ALTER FUNCTION dbo.dl_bundle_f (@s varchar(20)) RETURNS nvarchar(20) AS BEGIN RETURN LTRIM(RTRIM(@s)) END"},
            {"kind": "procedure", "name": "dbo.dl_bundle_p",
             "sql": "-- v2\nCREATE OR ALTER PROCEDURE [dbo].[dl_bundle_p] @Id int = 1 AS SELECT dbo.dl_bundle_f(Ten) AS ten FROM dbo.dl_bundle_t WHERE Id = @Id"},
            {"kind": "view", "name": "dbo.dl_bundle_v",
             "sql": format!("CREATE OR ALTER VIEW dbo.dl_bundle_v AS SELECT Id, MaCQT FROM {view_from}")}
        ])
    };
    let make = |version: &str, view_from: &str| {
        Bundle::from_json(&json!({
            "product": "dltest", "version": version, "objects": objects(view_from),
            "requires": {"tables": {"dbo.dl_bundle_t": ["MaCQT"]},
                         "procedures": ["dbo.dl_bundle_p"], "functions": ["dbo.dl_bundle_f"],
                         "views": ["dbo.dl_bundle_v"]}
        }))
        .unwrap()
    };
    let dir = std::env::temp_dir().join(format!("dl-live-install-{}", std::process::id()));
    let opts = |run: &str| Options {
        backup_dir: Some(dir.join(run)),
        ..Options::default()
    };
    let level = site_level(&mut s).unwrap();
    let first = install(&mut s, &make("1.0.0", "dbo.dl_bundle_t"), &opts("first"));
    let again = install(&mut s, &make("1.0.0", "dbo.dl_bundle_t"), &opts("again"));
    // A failing view in 1.1.0: the whole bundle, including the column script, rolls back.
    let failing = install(
        &mut s,
        &make("1.1.0", "dbo.dl_bundle_nope"),
        &opts("failing"),
    );
    let stamp = s.query_json(
        "SELECT CAST(value AS nvarchar(400)) AS v FROM sys.extended_properties \
         WHERE major_id = OBJECT_ID('dbo.dl_bundle_p') AND name = N'dl_bundle'",
        &[],
    );
    let grant = s.query_json(
        "SELECT COUNT(*) AS n FROM sys.database_permissions \
         WHERE major_id = OBJECT_ID('dbo.dl_bundle_p') AND permission_name = 'EXECUTE'",
        &[],
    );
    let modern = Bundle::from_json(
        &json!({"product": "dltest", "version": "2.0.0", "objects": [
        {"kind": "procedure", "name": "dbo.dl_bundle_p",
         "sql": "CREATE OR ALTER PROCEDURE dbo.dl_bundle_p AS SELECT IIF(1 = 1, 1, 0) AS x"}]}),
    )
    .unwrap();
    let refused = install(&mut s, &modern, &opts("modern"));
    s.run(cleanup, &[]).unwrap();
    let backup = std::fs::read_to_string(dir.join("first").join("dbo.dl_bundle_p.sql"));
    let _ = std::fs::remove_dir_all(&dir);

    let first = first.unwrap();
    eprintln!("level {level}: {}", dl_json::to_string_pretty(&first));
    assert_eq!(first["ok"].as_bool(), Some(true), "{first}");
    let actions: Vec<&str> = first["applied"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, ["script", "create", "alter", "create"]);
    assert_eq!(
        backup.unwrap(),
        "CREATE PROCEDURE dbo.dl_bundle_p AS SELECT 0 AS old"
    );
    let again = again.unwrap();
    assert_eq!(
        again["unchanged"].as_array().map(Vec::len),
        Some(3),
        "{again}"
    );
    let failing = failing.unwrap();
    assert_eq!(failing["ok"].as_bool(), Some(false), "{failing}");
    assert_eq!(failing["failed"]["name"].as_str(), Some("dbo.dl_bundle_v"));
    assert_eq!(
        failing["failed"]["code"].as_str(),
        Some("sql_missing_object")
    );
    let stamp = stamp.unwrap();
    assert!(
        stamp[0]["v"].as_str().unwrap().starts_with("dltest@1.0.0 "),
        "the failed 1.1.0 left 1.0.0 in place: {stamp:?}"
    );
    assert_eq!(
        grant.unwrap()[0]["n"].as_u64(),
        Some(1),
        "ALTER keeps grants"
    );
    let refused = refused.unwrap();
    assert_eq!(refused["ok"].as_bool(), Some(level >= 110), "{refused}");
}
