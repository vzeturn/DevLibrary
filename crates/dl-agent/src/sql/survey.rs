//! Site survey of one database (PROTOCOL.md §9.5): engine, compatibility level, collations
//! and the grants the agent's login holds, so the hub can tell a site that cannot run a
//! product before it is moved (legacy site variety, missing grants).

use dl_json::{Map, Value, json};

use super::pool::Connection;
use crate::Error;

/// One row; only built-ins that exist on SQL Server 2005/2008 (no `IS_ROLEMEMBER`,
/// no `TRY_CAST`), every value cast to text so old engines answer the same shape.
pub const SURVEY_SQL: &str = "SELECT \
CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(128)) AS product_version, \
CAST(SERVERPROPERTY('ProductLevel') AS nvarchar(128)) AS product_level, \
CAST(SERVERPROPERTY('Edition') AS nvarchar(128)) AS edition, \
CAST(SERVERPROPERTY('EngineEdition') AS nvarchar(16)) AS engine_edition, \
CAST(SERVERPROPERTY('Collation') AS nvarchar(128)) AS server_collation, \
CAST(@@VERSION AS nvarchar(512)) AS version_text, \
CAST(DB_NAME() AS nvarchar(128)) AS database_name, \
CAST(DATABASEPROPERTYEX(DB_NAME(), 'Collation') AS nvarchar(128)) AS database_collation, \
CAST(COLLATIONPROPERTY(CAST(DATABASEPROPERTYEX(DB_NAME(), 'Collation') AS nvarchar(128)), 'CodePage') AS nvarchar(16)) AS code_page, \
CAST(d.compatibility_level AS nvarchar(8)) AS compatibility_level, \
CAST(d.is_read_only AS nvarchar(2)) AS read_only, \
CAST(SUSER_SNAME() AS nvarchar(128)) AS login_name, \
CAST(USER_NAME() AS nvarchar(128)) AS user_name, \
CAST(IS_SRVROLEMEMBER('sysadmin') AS nvarchar(2)) AS sysadmin, \
CAST(IS_MEMBER('db_owner') AS nvarchar(2)) AS db_owner, \
CAST(HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'CREATE PROCEDURE') AS nvarchar(2)) AS create_procedure, \
CAST(HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'CREATE FUNCTION') AS nvarchar(2)) AS create_function, \
CAST(HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'CREATE TABLE') AS nvarchar(2)) AS create_table, \
CAST(HAS_PERMS_BY_NAME('dbo', 'SCHEMA', 'ALTER') AS nvarchar(2)) AS alter_dbo, \
CAST(HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'EXECUTE') AS nvarchar(2)) AS [execute], \
CAST(HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'VIEW DEFINITION') AS nvarchar(2)) AS view_definition, \
CAST(HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'BACKUP DATABASE') AS nvarchar(2)) AS backup_database \
FROM sys.databases d WHERE d.database_id = DB_ID()";

const GRANTS: [&str; 9] = [
    "sysadmin",
    "db_owner",
    "create_procedure",
    "create_function",
    "create_table",
    "alter_dbo",
    "execute",
    "view_definition",
    "backup_database",
];

/// How this session signed in, server side (`CONNECTIONPROPERTY`, SQL Server 2008 and later,
/// no extra permission): `auth_scheme` is `SQL`, `NTLM` or `KERBEROS`.
pub const SESSION_SQL: &str = "SELECT \
CAST(CONNECTIONPROPERTY('auth_scheme') AS nvarchar(40)) AS auth_scheme, \
CAST(CONNECTIONPROPERTY('net_transport') AS nvarchar(40)) AS net_transport, \
CAST(CONNECTIONPROPERTY('local_tcp_port') AS nvarchar(16)) AS server_port, \
CAST(SERVERPROPERTY('InstanceName') AS nvarchar(128)) AS instance";

/// Surveys the database behind `conn`; `rung` is the transport rung it was reached on.
/// On SQL Server 2008 and later the session facts ([`SESSION_SQL`]) and the TDS version the
/// server accepted are added under `session`, so a site's first install proves its sign-in
/// (NTLM / Kerberos / SQL), transport and protocol without anyone at a test bench.
///
/// # Errors
/// The SQL error when the main query fails; a failed session query only leaves
/// `session.error`.
pub fn survey(conn: &mut impl Connection, rung: Option<&str>) -> Result<Value, Error> {
    let rows = conn.query_json(SURVEY_SQL, &[])?;
    let raw = rows
        .into_iter()
        .next()
        .ok_or_else(|| Error::Sql("survey returned no row".into()))?;
    let mut s = assess(&raw, rung);
    let major = s["engine"]["major"].as_u64().unwrap_or(0);
    let facts = if major >= 10 {
        conn.query_json(SESSION_SQL, &[])
            .map(|r| r.into_iter().next().unwrap_or(Value::Null))
    } else {
        Ok(Value::Null)
    };
    if let Some(o) = s.as_object_mut() {
        let mut sess = session(facts, conn.tds_version());
        add_transport(&mut sess, conn.encrypted(), rung);
        o.insert("session", sess);
    }
    Ok(s)
}

/// `0x73_0B_00_03` → `7.3B`; `0x74_00_00_04` → `7.4`; others as hex.
pub fn tds_name(v: u32) -> String {
    match v {
        0x7000_0000 => "7.0".into(),
        0x7100_0001 => "7.1".into(),
        0x7209_0002 => "7.2".into(),
        0x730A_0003 => "7.3A".into(),
        0x730B_0003 => "7.3B".into(),
        0x7400_0004 => "7.4".into(),
        0x0800_0000 => "8.0".into(),
        other => format!("0x{other:08X}"),
    }
}

/// The `session` block: server-side sign-in facts (or the error that stopped them) and the
/// negotiated TDS version. Never a password: none of these hold one.
pub fn session(facts: Result<Value, Error>, tds: Option<u32>) -> Value {
    let mut m = Map::new();
    match facts {
        Ok(row) => {
            for k in ["auth_scheme", "net_transport", "instance"] {
                m.insert(k, text(&row, k).map_or(Value::Null, Value::from));
            }
            m.insert(
                "server_port",
                number(&row, "server_port").map_or(Value::Null, Value::from),
            );
        }
        Err(e) => {
            m.insert("error", Value::from(e.to_string()));
        }
    }
    m.insert("tds", tds.map_or(Value::Null, |v| Value::from(tds_name(v))));
    Value::Object(m)
}

/// Adds the TLS level to a `session` block: `encryption` (`full`, `login_only`, or `none` on
/// the unencrypted rung) and `tls`, the transport rung that connected (§9.4).
pub fn add_transport(session: &mut Value, encrypted: Option<bool>, rung: Option<&str>) {
    let encryption = match (rung, encrypted) {
        (Some("unencrypted"), _) => Some("none"),
        (_, Some(true)) => Some("full"),
        (_, Some(false)) => Some("login_only"),
        (_, None) => None,
    };
    if let Some(m) = session.as_object_mut() {
        m.insert("encryption", encryption.map_or(Value::Null, Value::from));
        m.insert("tls", rung.map_or(Value::Null, Value::from));
    }
}

/// The marketing name of an engine version (`10.50.1600.1` → `SQL Server 2008 R2`).
pub fn engine_name(version: &str) -> String {
    let mut parts = version.split('.');
    let major: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let year = match (major, minor) {
        (8, _) => "2000",
        (9, _) => "2005",
        (10, m) if m >= 50 => "2008 R2",
        (10, _) => "2008",
        (11, _) => "2012",
        (12, _) => "2014",
        (13, _) => "2016",
        (14, _) => "2017",
        (15, _) => "2019",
        (16, _) => "2022",
        (17, _) => "2025",
        _ => return format!("SQL Server {version}"),
    };
    format!("SQL Server {year}")
}

fn text<'a>(raw: &'a Value, key: &str) -> Option<&'a str> {
    raw.get(key).and_then(Value::as_str).map(str::trim)
}

fn number(raw: &Value, key: &str) -> Option<u64> {
    text(raw, key).and_then(|s| s.parse().ok())
}

/// The survey from the raw row: facts plus `warnings` the hub gate and operators act on.
pub fn assess(raw: &Value, rung: Option<&str>) -> Value {
    let version = text(raw, "product_version").unwrap_or("");
    let major = version
        .split('.')
        .next()
        .and_then(|p| p.parse::<u64>().ok())
        .unwrap_or(0);
    let compat = number(raw, "compatibility_level").unwrap_or(0);
    let server_collation = text(raw, "server_collation").unwrap_or("");
    let db_collation = text(raw, "database_collation").unwrap_or("");
    // "… on Windows NT 6.1 <X64> (Build 7601: Service Pack 1)" — the database host's OS.
    let host_os = text(raw, "version_text")
        .and_then(|t| t.rsplit_once(" on "))
        .map(|(_, os)| os.trim().to_string());

    let mut grants = Map::new();
    for g in GRANTS {
        grants.insert(g, Value::from(number(raw, g) == Some(1)));
    }
    let has = |g: &str| grants.get(g).and_then(Value::as_bool).unwrap_or(false);
    let owner = has("sysadmin") || has("db_owner");

    let mut warnings = Vec::new();
    if major < 11 {
        warnings.push("engine_pre_2012"); // T-SQL 2008 only: no THROW, OFFSET, IIF, CREATE OR ALTER
    }
    if compat < 110 && major >= 11 {
        warnings.push("compat_below_2012"); // engine is newer, but 2012 syntax still refused
    }
    if !db_collation.is_empty() && db_collation != server_collation {
        warnings.push("collation_differs_from_server"); // #temp joins need COLLATE DATABASE_DEFAULT
    }
    if text(raw, "code_page") != Some("1258") && text(raw, "code_page") != Some("0") {
        warnings.push("non_vietnamese_code_page"); // varchar holds TCVN3 / ANSI bytes, not Unicode
    }
    if !owner && !(has("create_procedure") && has("alter_dbo")) {
        warnings.push("cannot_install_procedures");
    }
    if !owner && !has("execute") {
        warnings.push("no_execute_grant");
    }
    if number(raw, "read_only") == Some(1) {
        warnings.push("database_read_only");
    }
    if text(raw, "edition").is_some_and(|e| e.contains("Express")) {
        warnings.push("express_edition"); // 10 GB cap, no SQL Agent
    }
    if rung == Some("unencrypted") {
        warnings.push("unencrypted_transport");
    }

    json!({
        "engine": {
            "version": version,
            "major": major,
            "name": engine_name(version),
            "level": text(raw, "product_level"),
            "edition": text(raw, "edition"),
            "engine_edition": number(raw, "engine_edition"),
            "host_os": host_os,
        },
        "database": {
            "name": text(raw, "database_name"),
            "compatibility_level": compat,
            "collation": db_collation,
            "code_page": number(raw, "code_page"),
            "read_only": number(raw, "read_only") == Some(1),
        },
        "server_collation": server_collation,
        "login": text(raw, "login_name"),
        "user": text(raw, "user_name"),
        "grants": Value::Object(grants),
        "syntax_2012": major >= 11 && compat >= 110,
        "transport": rung,
        "warnings": warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SQL Server 2008 R2 RTM Express, a SMILE_FO database on the DB owner login.
    pub fn smile_2008r2() -> Value {
        json!({
            "product_version": "10.50.1600.1", "product_level": "RTM",
            "edition": "Express Edition", "engine_edition": "4",
            "server_collation": "SQL_Latin1_General_CP1_CI_AS",
            "version_text": "Microsoft SQL Server 2008 R2 (RTM) - 10.50.1600.1 (Intel X86) \n\tApr  2 2010 15:53:02 \n\tCopyright (c) Microsoft Corporation\n\tExpress Edition on Windows NT 6.1 <X86> (Build 7601: Service Pack 1)",
            "database_name": "SMILE_FO", "database_collation": "Vietnamese_CI_AS",
            "code_page": "1258", "compatibility_level": "100", "read_only": "0",
            "login_name": "smile", "user_name": "dbo", "sysadmin": "0", "db_owner": "1",
            "create_procedure": "1", "create_function": "1", "create_table": "1",
            "alter_dbo": "1", "execute": "1", "view_definition": "1", "backup_database": "1"
        })
    }

    #[test]
    fn the_session_block_names_the_tls_level_and_the_named_instance() {
        let row = json!({"auth_scheme": "SQL", "net_transport": "TCP",
                         "server_port": "49172", "instance": "SMILE"});
        let mut s = session(Ok(row), Some(0x730B_0003));
        add_transport(&mut s, Some(false), Some("tls1.0"));
        assert_eq!(s["instance"].as_str(), Some("SMILE"));
        assert_eq!(s["encryption"].as_str(), Some("login_only"));
        assert_eq!(s["tls"].as_str(), Some("tls1.0"));
        let mut clear = session(Ok(json!({})), None);
        add_transport(&mut clear, Some(false), Some("unencrypted"));
        assert_eq!(clear["encryption"].as_str(), Some("none"));
        assert!(clear["instance"].is_null());
        let mut full = session(Ok(json!({})), None);
        add_transport(&mut full, Some(true), Some("as_configured"));
        assert_eq!(full["encryption"].as_str(), Some("full"));
    }

    #[test]
    fn the_session_block_names_the_sign_in_and_the_protocol() {
        let row = json!({"auth_scheme": "NTLM", "net_transport": "TCP", "server_port": "1433"});
        let s = session(Ok(row), Some(0x730B_0003));
        assert_eq!(s["auth_scheme"].as_str(), Some("NTLM"));
        assert_eq!(s["net_transport"].as_str(), Some("TCP"));
        assert_eq!(s["server_port"].as_u64(), Some(1433));
        assert_eq!(s["tds"].as_str(), Some("7.3B"));
        let e = session(Err(Error::Sql("denied".into())), None);
        assert!(e["error"].as_str().unwrap().contains("denied"));
        assert!(e["tds"].is_null());
        assert_eq!(tds_name(0x7400_0004), "7.4");
        assert_eq!(tds_name(0x1234), "0x00001234");
    }

    #[test]
    fn names_engines_by_version() {
        assert_eq!(engine_name("10.50.1600.1"), "SQL Server 2008 R2");
        assert_eq!(engine_name("10.0.5500.0"), "SQL Server 2008");
        assert_eq!(engine_name("11.0.7001.0"), "SQL Server 2012");
        assert_eq!(engine_name("16.0.4135.4"), "SQL Server 2022");
        assert_eq!(engine_name("99.0"), "SQL Server 99.0");
    }

    #[test]
    fn a_2008_r2_rtm_site_is_flagged_for_2008_syntax_and_collations() {
        let s = assess(&smile_2008r2(), Some("unencrypted"));
        assert_eq!(s["engine"]["name"].as_str(), Some("SQL Server 2008 R2"));
        assert_eq!(s["engine"]["major"].as_u64(), Some(10));
        assert_eq!(
            s["engine"]["host_os"].as_str(),
            Some("Windows NT 6.1 <X86> (Build 7601: Service Pack 1)")
        );
        assert_eq!(s["syntax_2012"].as_bool(), Some(false));
        let w: Vec<&str> = s["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(
            w,
            [
                "engine_pre_2012",
                "collation_differs_from_server",
                "express_edition",
                "unencrypted_transport"
            ]
        );
        assert_eq!(s["grants"]["db_owner"].as_bool(), Some(true));
    }

    #[test]
    fn a_reader_login_on_a_new_engine_in_old_compat_cannot_install() {
        let mut raw = smile_2008r2();
        let o = raw.as_object_mut().unwrap();
        for (k, v) in [
            ("product_version", "16.0.4135.4"),
            ("edition", "Standard Edition (64-bit)"),
            ("compatibility_level", "100"),
            ("database_collation", "SQL_Latin1_General_CP1_CI_AS"),
            ("code_page", "1252"),
            ("db_owner", "0"),
            ("create_procedure", "0"),
            ("execute", "0"),
        ] {
            o.insert(k, Value::from(v));
        }
        let s = assess(&raw, Some("as_configured"));
        let w: Vec<&str> = s["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(
            w,
            [
                "compat_below_2012",
                "non_vietnamese_code_page",
                "cannot_install_procedures",
                "no_execute_grant"
            ]
        );
        assert_eq!(s["syntax_2012"].as_bool(), Some(false));
    }
}
