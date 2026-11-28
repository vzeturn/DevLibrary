//! The existence checks of the SMILE_BO grant scripts (port of `SqlServerAdmin.BoStockGrantsExistAsync`,
//! `BoCountGrantsExistAsync` and `BoReceiptGrantsExistAsync`, RT-1427): scripts 175, 176 and 177
//! (ADR 0164, 0167, 0169) are done once `pda_sync` holds every grant they give in every usable SMILE_BO
//! they cover: the default `SMILE_BO` and each `Pda_TxnStockMap.BoDatabase`. A database that is not
//! there (or is the POS database itself) or an object it lacks has nothing to grant.

use super::admin::{COMMAND_TIMEOUT, run};
use super::error::{Error, Result};
use super::models::StepKind;
use crate::util::ctx::Ctx;
use dl_tds::{Client, Param, Results};

/// The default stock database.
pub const DEFAULT_BO: &str = "SMILE_BO";

/// What script 176 grants: (object, permission, column), the column only for the column-level
/// UPDATE (`SqlServerAdmin.CountGrants`).
pub const COUNT_GRANTS: [(&str, &str, Option<&str>); 9] = [
    ("InvSession", "SELECT", None),
    ("InvSession", "INSERT", None),
    ("InvSSDtl", "SELECT", None),
    ("InvSSDtl", "INSERT", None),
    ("InvSession", "UPDATE", Some("InvStatus")),
    ("InvSession", "UPDATE", Some("VNo")),
    ("Inventory", "SELECT", None),
    ("Department", "SELECT", None),
    ("IUnitDef", "SELECT", None),
];

/// What script 177 grants: (object, type, permission); the vendor post is a procedure (type
/// `P`), everything else a table (`U`) (`SqlServerAdmin.ReceiptGrants`).
pub const RECEIPT_GRANTS: [(&str, &str, &str); 11] = [
    ("tmpReceivingHeader", "U", "SELECT"),
    ("tmpReceivingHeader", "U", "INSERT"),
    ("tmpReceivingDetail", "U", "SELECT"),
    ("tmpReceivingDetail", "U", "INSERT"),
    ("ReceivingHeader", "U", "SELECT"),
    ("ReceivingDetail", "U", "SELECT"),
    ("CLIENT", "U", "SELECT"),
    ("APAccGroup", "U", "SELECT"),
    ("JVoucher", "U", "SELECT"),
    ("tmpJVoucher", "U", "SELECT"),
    ("RCVUpdateGL", "P", "EXECUTE"),
];

/// The script 175 check in database `bo` (`@bo`, `@name`): 1 when `bo` is missing or the POS
/// database itself, or has no `INVUpdateGLAvg`, or `pda_sync` may execute it.
pub fn stock_query(bo: &str) -> String {
    format!(
        "SELECT CASE WHEN DB_ID(@bo) IS NULL OR DB_ID(@bo) = DB_ID() THEN 1\r\n\
         \x20           WHEN OBJECT_ID(N'[{bo}].dbo.INVUpdateGLAvg', N'P') IS NULL THEN 1\r\n\
         \x20           WHEN EXISTS (SELECT 1 FROM [{bo}].sys.database_permissions p\r\n\
         \x20                  JOIN [{bo}].sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name\r\n\
         \x20                 WHERE p.class = 1 AND p.state = 'G' AND p.permission_name = 'EXECUTE'\r\n\
         \x20                   AND p.major_id = OBJECT_ID(N'[{bo}].dbo.INVUpdateGLAvg', N'P')) THEN 1\r\n\
         \x20           ELSE 0 END"
    )
}

/// The script 176 check in database `bo`.
pub fn count_query(bo: &str) -> String {
    let checks: Vec<String> = COUNT_GRANTS
        .iter()
        .map(|(object, permission, column)| {
            let scope = match column {
                None => "AND p.minor_id = 0".to_owned(),
                Some(c) => format!(
                    "AND (p.minor_id = 0 OR EXISTS (SELECT 1 FROM [{bo}].sys.columns k WHERE k.object_id = p.major_id AND k.column_id = p.minor_id AND k.name = N'{c}'))"
                ),
            };
            format!(
                "(OBJECT_ID(N'[{bo}].dbo.{object}', N'U') IS NULL OR EXISTS (SELECT 1 FROM [{bo}].sys.database_permissions p \
                 JOIN [{bo}].sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name \
                 WHERE p.class = 1 AND p.state = 'G' AND p.permission_name = '{permission}' AND p.major_id = OBJECT_ID(N'[{bo}].dbo.{object}', N'U') {scope}))"
            )
        })
        .collect();
    format!(
        "SELECT CASE WHEN DB_ID(@bo) IS NULL OR DB_ID(@bo) = DB_ID() THEN 1 WHEN {} THEN 1 ELSE 0 END",
        checks.join(" AND ")
    )
}

/// The script 177 check in database `bo`.
pub fn receipt_query(bo: &str) -> String {
    let checks: Vec<String> = RECEIPT_GRANTS
        .iter()
        .map(|(object, kind, permission)| {
            format!(
                "(OBJECT_ID(N'[{bo}].dbo.{object}', N'{kind}') IS NULL OR EXISTS (SELECT 1 FROM [{bo}].sys.database_permissions p \
                 JOIN [{bo}].sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name \
                 WHERE p.class = 1 AND p.state = 'G' AND p.permission_name = '{permission}' AND p.major_id = OBJECT_ID(N'[{bo}].dbo.{object}', N'{kind}') AND p.minor_id = 0))"
            )
        })
        .collect();
    format!(
        "SELECT CASE WHEN DB_ID(@bo) IS NULL OR DB_ID(@bo) = DB_ID() THEN 1 WHEN {} THEN 1 ELSE 0 END",
        checks.join(" AND ")
    )
}

/// The mapped databases (`BoDatabase`), when the map table exists.
pub const MAP_SQL: &str = "IF OBJECT_ID(N'dbo.Pda_TxnStockMap', N'U') IS NOT NULL EXECUTE sys.sp_executesql N'SELECT DISTINCT LTRIM(RTRIM(BoDatabase)) FROM dbo.Pda_TxnStockMap WHERE BoDatabase IS NOT NULL';";

/// Whether the database exists and is not the POS database (`@bo`).
pub const USABLE_SQL: &str =
    "SELECT CASE WHEN DB_ID(@bo) IS NULL OR DB_ID(@bo) = DB_ID() THEN 0 ELSE 1 END";

/// `^[A-Za-z0-9_]+$` as .NET evaluates it (`$` also matches before a final newline).
pub fn is_plain_identifier(name: &str) -> bool {
    let core = name.strip_suffix('\n').unwrap_or(name);
    !core.is_empty() && core.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The names to check: the default plus every mapped database, case-insensitively distinct, in
/// first-seen order, only plain identifiers (`BoDatabaseNamesAsync` before the existence probe).
pub fn candidate_names(mapped: &[String]) -> Vec<String> {
    let mut names: Vec<String> = vec![DEFAULT_BO.to_owned()];
    for m in mapped {
        if !m.is_empty() && !names.iter().any(|n| n.eq_ignore_ascii_case(m)) {
            names.push(m.clone());
        }
    }
    names.retain(|n| is_plain_identifier(n));
    names
}

/// The check of `kind` in database `bo`.
fn query(kind: StepKind, bo: &str) -> String {
    match kind {
        StepKind::BoStockGrants => stock_query(bo),
        StepKind::BoCountGrants => count_query(bo),
        _ => receipt_query(bo),
    }
}

/// `BoStockGrantsExistAsync` / `BoCountGrantsExistAsync` / `BoReceiptGrantsExistAsync`: done
/// when every usable SMILE_BO answers non-zero.
pub(crate) fn bo_grants_exist(
    c: &mut Client,
    kind: StepKind,
    name: &str,
    ctx: &Ctx,
) -> Result<bool> {
    let r = run(c, MAP_SQL, &[], COMMAND_TIMEOUT, ctx)?;
    let mut mapped = Vec::new();
    for row in r.rows() {
        if let Some(n) = row.get::<Option<String>>(0).map_err(cast)? {
            mapped.push(n);
        }
    }
    for bo in candidate_names(&mapped) {
        let r = run(
            c,
            USABLE_SQL,
            &[Param::new("bo", bo.as_str())],
            COMMAND_TIMEOUT,
            ctx,
        )?;
        if scalar_int(&r)? != 1 {
            continue;
        }
        let r = run(
            c,
            &query(kind, &bo),
            &[Param::new("bo", bo.as_str()), Param::new("name", name)],
            COMMAND_TIMEOUT,
            ctx,
        )?;
        if scalar_int(&r)? == 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

fn cast(e: impl std::fmt::Display) -> Error {
    Error::unexpected("System.InvalidCastException", e.to_string())
}

/// `Convert.ToInt32(ExecuteScalar())`: no row is 0.
fn scalar_int(r: &Results) -> Result<i64> {
    let n = match r.rows().first() {
        Some(row) => row.get::<Option<i64>>(0).map_err(cast)?,
        None => None,
    };
    Ok(n.unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_count_and_receipt_checks_name_every_grant() {
        let count = count_query("BO_2");
        assert!(count.contains("[BO_2].dbo.InvSession', N'U') IS NULL OR EXISTS"));
        assert!(count.contains("k.name = N'InvStatus'") && count.contains("k.name = N'VNo'"));
        assert_eq!(
            count.matches("OBJECT_ID(N'[BO_2]").count() / 2,
            COUNT_GRANTS.len()
        );
        let receipt = receipt_query("BO_2");
        assert!(receipt.contains("[BO_2].dbo.RCVUpdateGL', N'P'"));
        assert!(receipt.ends_with("THEN 1 ELSE 0 END"));
        assert_eq!(
            receipt.matches("p.minor_id = 0").count(),
            RECEIPT_GRANTS.len()
        );
    }

    #[test]
    fn the_names_are_the_default_plus_the_map_and_plain_identifiers_only() {
        let mapped: Vec<String> = ["smile_bo", "BO_2", "BO]; DROP", "BO_2", ""]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(candidate_names(&mapped), ["SMILE_BO", "BO_2"]);
        assert_eq!(candidate_names(&[]), ["SMILE_BO"]);
        assert!(is_plain_identifier("A_1\n") && !is_plain_identifier("A_1\n\n"));
    }
}
