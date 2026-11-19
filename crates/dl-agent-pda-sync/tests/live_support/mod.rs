//! The SMILE_POS-shaped database of the live tests, built like the PDAOrder side-by-side
//! fixture (`apps/sql-sync-agent/rust/e2e/.tools/e2etool fixture`): the vendor tables and
//! order routines of `testdata/sql/smile_pos_schema.sql`, the order, combo and table verb
//! shapes of the C# command tests (`testdata/sql/smile_pos_orders.sql`), then every
//! classified catalog table and catalog column that is missing (generated, NULLable, typed
//! as the tool types them), the cancel archive mirrored from the open-check header
//! (`smile_pos_orders_post.sql`), then the rows of `testdata/sql/smile_pos_seed.sql` with
//! `@base` ten days back.
//! Gated on `PDA_SYNC_TEST_SQL` (a dl-tds URL of a login that may create databases and
//! logins); the database and the `pda_sync` login it creates are dropped afterwards.

#![allow(dead_code)]

pub mod orders;

use std::collections::{BTreeMap, HashSet};

const SCHEMA: &str = include_str!("../../testdata/sql/smile_pos_schema.sql");
const SEED: &str = include_str!("../../testdata/sql/smile_pos_seed.sql");
const ORDERS: &str = include_str!("../../testdata/sql/smile_pos_orders.sql");
const ORDERS_POST: &str = include_str!("../../testdata/sql/smile_pos_orders_post.sql");
const CATALOG: &str = include_str!("../../assets/catalog/sync-catalog.json");
const LOGICAL_KEYS: &str = include_str!("../../assets/catalog/logical-keys.json");

/// The admin URL of the live tests, when set.
pub fn admin_url() -> Option<String> {
    std::env::var("PDA_SYNC_TEST_SQL")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// A throwaway SMILE_POS with the real vendor shape.
pub struct SmilePos {
    /// The admin login on `master`.
    pub admin: dl_tds::Config,
    /// `host,port` or `host\instance`, as enroll takes `--sql-instance`.
    pub instance: String,
    /// The database name.
    pub database: String,
    /// Tables the catalog added (not in the vendor schema file).
    pub generated: usize,
    created_login: bool,
}

impl SmilePos {
    /// Builds the database; `None` when the server already has a `pda_sync` login (the
    /// test would take it over).
    pub fn create(url: &str) -> Option<SmilePos> {
        let admin: dl_tds::Config = url.parse().expect("PDA_SYNC_TEST_SQL is a dl-tds URL");
        let admin = admin.with_database("master");
        let instance = match admin.instance() {
            Some(i) => format!("{}\\{i}", admin.host()),
            None => format!("{},{}", admin.host(), admin.port().unwrap_or(1433)),
        };
        let tag = dl_uuid::Uuid::new_v4().simple().to_string();
        let mut pos = SmilePos {
            admin,
            instance,
            database: format!("pda_sync_test_{}", &tag[..12]),
            generated: 0,
            created_login: false,
        };
        let mut c = dl_tds::Client::connect(&pos.admin).unwrap();
        let r = c.query("SELECT SUSER_ID(N'pda_sync')", &[]).unwrap();
        if r.rows()[0].get::<Option<i32>>(0).unwrap().is_some() {
            eprintln!("skipped: the server already has a pda_sync login");
            return None;
        }
        c.batch(&format!("CREATE DATABASE [{}];", pos.database))
            .unwrap();
        c.close();
        pos.created_login = true;
        for batch in batches(SCHEMA) {
            pos.exec(batch);
        }
        pos.exec(ORDERS);
        pos.generated = pos.add_catalog_shape();
        pos.exec(ORDERS_POST);
        let seed = format!(
            "DECLARE @base datetime = DATEADD(day, -10, CAST(CAST(GETDATE() AS date) AS datetime));\n{SEED}"
        );
        pos.exec(&seed);
        Some(pos)
    }

    /// `e2etool`: every classified table and its catalog columns.
    fn add_catalog_shape(&self) -> usize {
        let catalog = dl_json::parse(CATALOG).unwrap();
        let never = never_sync(&catalog);
        let logical = logical_keys();
        let mut generated = 0;
        let mut ddl = String::new();
        for t in catalog.get("tables").and_then(|v| v.as_array()).unwrap() {
            let class = t.get("class").and_then(|v| v.as_str()).unwrap_or("");
            if class == "skip" || class == "unclassified" {
                continue;
            }
            let name = t.get("table").and_then(|v| v.as_str()).unwrap();
            let upper = name.to_uppercase();
            let mut cols = columns(
                t,
                never.get(&upper).map_or(&[][..], Vec::as_slice),
                logical.get(&upper).map_or(&[][..], Vec::as_slice),
            );
            let existing = self.column_names(name);
            let esc = name.replace(']', "]]");
            if existing.is_empty() {
                if cols.is_empty() {
                    cols.push(("Id".into(), "int"));
                }
                let list: Vec<String> = cols
                    .iter()
                    .map(|(c, ty)| format!("[{}] {ty} NULL", c.replace(']', "]]")))
                    .collect();
                ddl.push_str(&format!(
                    "CREATE TABLE dbo.[{esc}] ({});\n",
                    list.join(", ")
                ));
                generated += 1;
                continue;
            }
            let missing: Vec<String> = cols
                .iter()
                .filter(|(c, _)| !existing.contains(&c.to_lowercase()))
                .map(|(c, ty)| format!("[{}] {ty} NULL", c.replace(']', "]]")))
                .collect();
            if !missing.is_empty() {
                ddl.push_str(&format!(
                    "ALTER TABLE dbo.[{esc}] ADD {};\n",
                    missing.join(", ")
                ));
            }
        }
        self.exec(&ddl);
        generated
    }

    fn column_names(&self, table: &str) -> HashSet<String> {
        let mut c = self.connect_db();
        let r = c
            .query(
                "SELECT c.name FROM sys.columns c WHERE c.object_id = OBJECT_ID(@t)",
                &[dl_tds::Param::new(
                    "t",
                    format!("dbo.[{}]", table.replace(']', "]]")),
                )],
            )
            .unwrap();
        let names = r
            .rows()
            .iter()
            .map(|row| row.get::<String>(0).unwrap().to_lowercase())
            .collect();
        c.close();
        names
    }

    fn connect_db(&self) -> dl_tds::Client {
        dl_tds::Client::connect(&self.admin.clone().with_database(&self.database)).unwrap()
    }

    /// Runs `sql` as the admin in the database.
    pub fn exec(&self, sql: &str) {
        let mut c = self.connect_db();
        c.batch(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        c.close();
    }

    /// The first column of the first row of `sql`, as the admin.
    pub fn scalar_i64(&self, sql: &str) -> i64 {
        let mut c = self.connect_db();
        let r = c.query(sql, &[]).unwrap_or_else(|e| panic!("{sql}: {e}"));
        let n: i64 = r.rows()[0].get(0).unwrap();
        c.close();
        n
    }

    /// The first column of the first row of `sql` as text (`None` for NULL).
    pub fn scalar_text(&self, sql: &str) -> Option<String> {
        let mut c = self.connect_db();
        let r = c.query(sql, &[]).unwrap_or_else(|e| panic!("{sql}: {e}"));
        let v: Option<String> = r.rows()[0].get(0).unwrap();
        c.close();
        v
    }

    /// The admin as the enroll `--sql-admin-user` credential.
    pub fn admin_credential(&self) -> dl_agent_pda_sync::bootstrap::admin::SqlAdminCredential {
        dl_agent_pda_sync::bootstrap::admin::SqlAdminCredential::sql(
            self.admin.user(),
            self.admin.password(),
        )
    }

    /// The server's major version (14 = 2017, 16 = 2022).
    pub fn major(&self) -> i64 {
        let mut c = dl_tds::Client::connect(&self.admin).unwrap();
        let r = c
            .query(
                "SELECT CAST(SERVERPROPERTY('ProductMajorVersion') AS int)",
                &[],
            )
            .unwrap();
        let n: i32 = r.rows()[0].get(0).unwrap();
        c.close();
        i64::from(n)
    }
}

impl Drop for SmilePos {
    fn drop(&mut self) {
        if let Ok(mut c) = dl_tds::Client::connect(&self.admin) {
            let mut sql = format!(
                "IF DB_ID(N'{db}') IS NOT NULL BEGIN ALTER DATABASE [{db}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{db}]; END;",
                db = self.database
            );
            if self.created_login {
                sql.push_str("\nIF SUSER_ID(N'pda_sync') IS NOT NULL DROP LOGIN [pda_sync];");
            }
            if let Err(e) = c.batch(&sql) {
                eprintln!("cleanup of {} failed: {e}", self.database);
            }
            c.close();
        }
    }
}

/// `GO`-separated batches.
pub fn batches(sql: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut pos = 0;
    for line in sql.split_inclusive('\n') {
        if line.trim().eq_ignore_ascii_case("GO") {
            let b = sql[start..pos].trim();
            if !b.is_empty() {
                out.push(b);
            }
            start = pos + line.len();
        }
        pos += line.len();
    }
    let b = sql[start..].trim();
    if !b.is_empty() {
        out.push(b);
    }
    out
}

fn strings(v: Option<&dl_json::Value>) -> Vec<String> {
    v.and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// `conventions.never_sync` as `TABLE -> [columns]` (`A/B.C/D` names two tables).
fn never_sync(catalog: &dl_json::Value) -> BTreeMap<String, Vec<String>> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let list = strings(catalog.get("conventions").and_then(|c| c.get("never_sync")));
    for e in list {
        let Some(dot) = e.rfind('.') else { continue };
        if dot == 0 {
            continue;
        }
        for t in e[..dot].split('/') {
            map.entry(t.to_uppercase())
                .or_default()
                .extend(e[dot + 1..].split('/').map(str::to_owned));
        }
    }
    map
}

fn logical_keys() -> BTreeMap<String, Vec<String>> {
    let doc = dl_json::parse(LOGICAL_KEYS).unwrap();
    let mut map = BTreeMap::new();
    if let Some(o) = doc.get("logical_keys").and_then(|v| v.as_object()) {
        for (k, v) in o.iter() {
            map.insert(k.to_uppercase(), strings(Some(v)));
        }
    }
    map
}

/// `e2etool`'s `Columns`: the catalog's columns of `t` with the tool's types.
fn columns(
    t: &dl_json::Value,
    never: &[String],
    logical: &[String],
) -> Vec<(String, &'static str)> {
    let lower =
        |v: Vec<String>| -> HashSet<String> { v.into_iter().map(|s| s.to_lowercase()).collect() };
    let transforms = t.get("transforms");
    let tr = |k: &str| strings(transforms.and_then(|x| x.get(k)));
    let mut dt_list = strings(t.get("datetime_columns"));
    dt_list.extend(tr("local_time_to_utc"));
    let dt = lower(dt_list.clone());
    let money_list = tr("money_decimal_string");
    let money = lower(money_list.clone());
    let tcvn_list = tr("tcvn3_decode");
    let tcvn = lower(tcvn_list.clone());
    let blob_list = strings(t.get("hash_exclude"));
    let blobs = lower(blob_list.clone());
    let mut secret_list = strings(t.get("exclude_columns"));
    secret_list.extend(never.iter().cloned());
    let secret = lower(secret_list.clone());
    let mut names = strings(t.get("source_keys"));
    names.extend(strings(t.get("pk")));
    names.extend(logical.iter().cloned());
    if let Some(mk) = t.get("monotonic_key").and_then(|v| v.as_str()) {
        names.push(mk.to_owned());
    }
    if let Some(pc) = t.get("pg_columns").and_then(|v| v.as_object()) {
        names.extend(pc.keys().cloned());
    }
    names.extend(strings(t.get("watermark_candidates")));
    names.extend(dt_list);
    names.extend(money_list);
    names.extend(tcvn_list);
    names.extend(secret_list);
    names.extend(blob_list);
    names.extend(strings(t.get("unmapped_vendor_columns")));
    let mut seen = HashSet::new();
    let mut cols = Vec::new();
    for n in names {
        if n.trim().is_empty() || !seen.insert(n.to_lowercase()) {
            continue;
        }
        let l = n.to_lowercase();
        let ty = if dt.contains(&l) {
            "datetime"
        } else if money.contains(&l) {
            "money"
        } else if blobs.contains(&l) {
            "image"
        } else if tcvn.contains(&l) {
            "varchar(100)"
        } else if secret.contains(&l) {
            "varchar(50)"
        } else if int_like(&n) {
            "int"
        } else {
            "nvarchar(100)"
        };
        cols.push((n, ty));
    }
    cols
}

/// `(No|Seq|Year|Month|Day|Qty|Status|Period|RVC|Level|Type|Mode|Count|Num)$|^(Is|is|Omit|N[A-Z])`.
fn int_like(n: &str) -> bool {
    const ENDS: [&str; 14] = [
        "No", "Seq", "Year", "Month", "Day", "Qty", "Status", "Period", "RVC", "Level", "Type",
        "Mode", "Count", "Num",
    ];
    if ENDS.iter().any(|e| n.ends_with(e)) {
        return true;
    }
    if n.starts_with("Is") || n.starts_with("is") || n.starts_with("Omit") {
        return true;
    }
    let b = n.as_bytes();
    b.len() >= 2 && b[0] == b'N' && b[1].is_ascii_uppercase()
}
