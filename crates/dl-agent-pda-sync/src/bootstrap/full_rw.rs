//! Script 210's full read/write (RT-1425 owner decision, temporary): `pda_sync` is a
//! member of `db_datareader` and `db_datawriter` and holds the database-wide EXECUTE in
//! every SMILE database the store's server has (the POS database the agent runs in, and
//! `SMILE_FO` and `SMILE_BO` when present). Both checks walk the database list of the
//! script itself, so a grant missing in the Front or Back Office is seen as well as one in
//! POS. Tightening again is the revert of the step, [`EXISTS_SQL`] and [`PROBE_SQL`].

use super::admin::{COMMAND_TIMEOUT, run};
use super::error::{Error, Result};
use super::models::BootstrapStep;
use crate::util::ctx::Ctx;
use dl_tds::{Client, Param, Value};

/// The databases of `210_full_rw_grants.sql`, filled into `@dbs` (kept identical to the
/// script's line).
pub const DATABASES_SQL: &str = "INSERT INTO @dbs (name)
SELECT DISTINCT d.name FROM (VALUES (DB_NAME()), (N'SMILE_FO'), (N'SMILE_BO')) d(name) WHERE DB_ID(d.name) IS NOT NULL;";

/// The existence check of the step, run by the setup identity in the POS database: 1 once
/// `@name` holds both roles and the database-wide EXECUTE in every database of the script.
/// A database the identity cannot open counts as not in place, so the step runs and the
/// script's failure there names the database (the self-repair then tries the next
/// identity). Each database is read in its own context through
/// `[db].sys.sp_executesql`: `IS_ROLEMEMBER` and `sys.database_permissions` only see the
/// current database.
pub const EXISTS_SQL: &str = "SET NOCOUNT ON;
DECLARE @db sysname, @proc nvarchar(300), @held int, @all int = 1;
DECLARE @dbs TABLE (name sysname NOT NULL PRIMARY KEY);
INSERT INTO @dbs (name)
SELECT DISTINCT d.name FROM (VALUES (DB_NAME()), (N'SMILE_FO'), (N'SMILE_BO')) d(name) WHERE DB_ID(d.name) IS NOT NULL;
WHILE EXISTS (SELECT 1 FROM @dbs)
BEGIN
    SELECT TOP (1) @db = name FROM @dbs ORDER BY name;
    SET @held = 0;
    IF HAS_DBACCESS(@db) = 1
    BEGIN
        SET @proc = QUOTENAME(@db) + N'.sys.sp_executesql';
        EXECUTE @proc N'SELECT @held = CASE WHEN IS_ROLEMEMBER(N''db_datareader'', @name) = 1
            AND IS_ROLEMEMBER(N''db_datawriter'', @name) = 1
            AND EXISTS (SELECT 1 FROM sys.database_permissions p
                JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
                WHERE p.class = 0 AND p.state = ''G'' AND p.permission_name = ''EXECUTE'')
            THEN 1 ELSE 0 END;', N'@name sysname, @held int OUTPUT', @name, @held OUTPUT;
    END
    IF @held = 0 SET @all = 0;
    DELETE FROM @dbs WHERE name = @db;
END
SELECT @all;";

/// The runtime probe, read as `pda_sync` in the POS database: one row per database of the
/// script with `db`, `HAS_DBACCESS`, the reader and writer memberships and the
/// database-wide EXECUTE. A database `pda_sync` cannot open has NULL memberships.
pub const PROBE_SQL: &str = "SET NOCOUNT ON;
DECLARE @db sysname, @proc nvarchar(300), @reader int, @writer int, @exec int;
DECLARE @dbs TABLE (name sysname NOT NULL PRIMARY KEY);
DECLARE @r TABLE (db sysname NOT NULL, access int NULL, reader int NULL, writer int NULL, exec_db int NULL);
INSERT INTO @dbs (name)
SELECT DISTINCT d.name FROM (VALUES (DB_NAME()), (N'SMILE_FO'), (N'SMILE_BO')) d(name) WHERE DB_ID(d.name) IS NOT NULL;
WHILE EXISTS (SELECT 1 FROM @dbs)
BEGIN
    SELECT TOP (1) @db = name FROM @dbs ORDER BY name;
    SELECT @reader = NULL, @writer = NULL, @exec = NULL;
    IF HAS_DBACCESS(@db) = 1
    BEGIN
        SET @proc = QUOTENAME(@db) + N'.sys.sp_executesql';
        EXECUTE @proc N'SELECT @reader = IS_ROLEMEMBER(N''db_datareader''), @writer = IS_ROLEMEMBER(N''db_datawriter''),
            @exec = HAS_PERMS_BY_NAME(NULL, N''DATABASE'', N''EXECUTE'');',
            N'@reader int OUTPUT, @writer int OUTPUT, @exec int OUTPUT', @reader OUTPUT, @writer OUTPUT, @exec OUTPUT;
    END
    INSERT INTO @r (db, access, reader, writer, exec_db) VALUES (@db, HAS_DBACCESS(@db), @reader, @writer, @exec);
    DELETE FROM @dbs WHERE name = @db;
END
SELECT db, access, reader, writer, exec_db FROM @r ORDER BY db;";

/// The missing-permission entries of one [`PROBE_SQL`] row: `held` is `HAS_DBACCESS`,
/// the reader role, the writer role and the EXECUTE. `0` is missing; NULL (unknown, or
/// not readable without access) is not reported.
pub fn missing(db: &str, held: [Option<&Value>; 4]) -> Vec<String> {
    let [access, reader, writer, execute] = held;
    let no = Some(&Value::I32(0));
    let mut out = Vec::new();
    if access == no {
        out.push(format!("CONNECT DATABASE::{db}"));
    }
    if reader == no {
        out.push(format!("ROLE db_datareader DATABASE::{db}"));
    }
    if writer == no {
        out.push(format!("ROLE db_datawriter DATABASE::{db}"));
    }
    if execute == no {
        out.push(format!("EXECUTE DATABASE::{db}"));
    }
    out
}

/// Runs [`PROBE_SQL`] on the probe's connection and lists what is missing, database by
/// database.
pub(crate) fn probe(
    c: &mut Client,
    timeout: std::time::Duration,
    ctx: &Ctx,
) -> Result<Vec<String>> {
    let r = run(c, PROBE_SQL, &[], timeout, ctx)?;
    let mut out = Vec::new();
    for row in r.rows() {
        let Some(Value::String(db)) = row.value(0) else {
            return Err(Error::unexpected(
                "System.InvalidCastException",
                format!(
                    "the full read/write probe returned no database name: {:?}",
                    row.value(0)
                ),
            ));
        };
        out.extend(missing(
            db,
            [row.value(1), row.value(2), row.value(3), row.value(4)],
        ));
    }
    Ok(out)
}

/// The step's existence check ([`EXISTS_SQL`]) on the setup identity's connection.
pub(crate) fn grants_exist(c: &mut Client, step: &BootstrapStep, ctx: &Ctx) -> Result<bool> {
    let r = run(
        c,
        EXISTS_SQL,
        &[Param::new("name", step.object_name)],
        COMMAND_TIMEOUT,
        ctx,
    )?;
    Ok(matches!(
        r.rows().first().and_then(|row| row.value(0)),
        Some(Value::I32(1))
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script() -> &'static str {
        crate::bootstrap::read_script("Sql.210_full_rw_grants.sql").unwrap()
    }

    #[test]
    fn both_checks_walk_the_databases_of_script_210() {
        assert!(script().contains(DATABASES_SQL));
        for sql in [EXISTS_SQL, PROBE_SQL] {
            assert!(sql.contains(DATABASES_SQL));
            assert!(sql.contains("IF HAS_DBACCESS(@db) = 1"));
            assert!(sql.contains("SET @proc = QUOTENAME(@db) + N'.sys.sp_executesql';"));
            assert!(sql.contains("IS_ROLEMEMBER(N''db_datareader''"));
            assert!(sql.contains("IS_ROLEMEMBER(N''db_datawriter''"));
            // No three-part name: a database the server lacks would be error 911.
            assert!(!sql.contains("SMILE_FO]."));
            assert!(!sql.contains("SMILE_BO]."));
        }
    }

    #[test]
    fn the_existence_check_needs_both_roles_and_execute_in_every_database() {
        assert!(
            EXISTS_SQL
                .contains("p.class = 0 AND p.state = ''G'' AND p.permission_name = ''EXECUTE''")
        );
        assert!(EXISTS_SQL.contains("N'@name sysname, @held int OUTPUT', @name, @held OUTPUT;"));
        assert!(EXISTS_SQL.contains("SET @held = 0;"));
        assert!(EXISTS_SQL.contains("IF @held = 0 SET @all = 0;"));
        assert!(EXISTS_SQL.ends_with("SELECT @all;"));
        // The generic per-kind query no longer answers for this step.
        assert!(!crate::bootstrap::admin::exists_query().contains("FullRwGrants"));
    }

    #[test]
    fn the_probe_reports_each_refused_grant_with_its_database() {
        assert!(PROBE_SQL.contains("HAS_PERMS_BY_NAME(NULL, N''DATABASE'', N''EXECUTE'')"));
        assert!(
            PROBE_SQL.ends_with("SELECT db, access, reader, writer, exec_db FROM @r ORDER BY db;")
        );
        let (no, yes, null) = (Value::I32(0), Value::I32(1), Value::Null);
        assert!(
            missing(
                "SMILE_POS",
                [Some(&yes), Some(&yes), Some(&yes), Some(&yes)]
            )
            .is_empty()
        );
        assert_eq!(
            missing("SMILE_FO", [Some(&yes), Some(&no), Some(&no), Some(&no)]),
            [
                "ROLE db_datareader DATABASE::SMILE_FO",
                "ROLE db_datawriter DATABASE::SMILE_FO",
                "EXECUTE DATABASE::SMILE_FO",
            ]
        );
        // No user in the database: only the access is reported, the rest is unknown.
        assert_eq!(
            missing(
                "SMILE_BO",
                [Some(&no), Some(&null), Some(&null), Some(&null)]
            ),
            ["CONNECT DATABASE::SMILE_BO"]
        );
        assert!(missing("SMILE_BO", [None, None, None, None]).is_empty());
    }
}
