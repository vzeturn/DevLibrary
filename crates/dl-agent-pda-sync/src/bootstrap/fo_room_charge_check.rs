//! The existence check of script 160 (`160_fo_room_charge_grants.sql`, ADR 0162; RT-1427 for
//! this agent): the Front Office user and the `PostInterfaceTransactionToFolio` grant of
//! `txn.room_charge`, in the database `Para InterfaceDB` names (else `SMILE_FO`).

use super::admin::{COMMAND_TIMEOUT, FO_DATABASE_SQL, run};
use super::error::Result;
use super::models::BootstrapStep;
use crate::util::ctx::Ctx;
use dl_tds::{Client, Param};

/// 160: 1 when there is nothing to grant (no usable Front Office database or no posting
/// procedure) or `@name` already holds EXECUTE on it. `fo` is validated by the caller.
pub fn fo_room_charge_grants_exists_query(fo: &str) -> String {
    let proc = format!("N'[{fo}].dbo.PostInterfaceTransactionToFolio', N'P'");
    [
        "SELECT CASE WHEN DB_ID(@fo) IS NULL OR DB_ID(@fo) = DB_ID() THEN 1".to_owned(),
        format!("            WHEN OBJECT_ID({proc}) IS NULL THEN 1"),
        format!("            WHEN EXISTS (SELECT 1 FROM [{fo}].sys.database_permissions p"),
        format!(
            "                   JOIN [{fo}].sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name"
        ),
        "                  WHERE p.class = 1 AND p.state = 'G' AND p.permission_name = 'EXECUTE'"
            .to_owned(),
        format!("                    AND p.major_id = OBJECT_ID({proc})) THEN 1"),
        "            ELSE 0 END".to_owned(),
    ]
    .join("\r\n")
}

/// 160: done once `pda_sync` may run the posting procedure in the Front Office database; an
/// invalid name has nothing to grant, as in the script.
pub(super) fn fo_room_charge_grants_exist(
    c: &mut Client,
    step: &BootstrapStep,
    ctx: &Ctx,
) -> Result<bool> {
    let r = run(c, FO_DATABASE_SQL, &[], COMMAND_TIMEOUT, ctx)?;
    let fo = match r.rows().first().and_then(|row| row.value(0)) {
        Some(dl_tds::Value::String(s)) if !s.trim().is_empty() => s.trim().to_owned(),
        _ => "SMILE_FO".to_owned(),
    };
    if !fo.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return Ok(true);
    }
    let r = run(
        c,
        &fo_room_charge_grants_exists_query(&fo),
        &[
            Param::new("fo", fo.as_str()),
            Param::new("name", step.object_name),
        ],
        COMMAND_TIMEOUT,
        ctx,
    )?;
    Ok(matches!(
        r.rows().first().and_then(|row| row.value(0)),
        Some(dl_tds::Value::I32(n)) if *n > 0
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fo_room_charge_query_checks_execute_on_the_posting_procedure() {
        let q = fo_room_charge_grants_exists_query("SMILE_FO");
        assert!(q.contains("OBJECT_ID(N'[SMILE_FO].dbo.PostInterfaceTransactionToFolio', N'P')"));
        assert!(q.contains("p.permission_name = 'EXECUTE'"));
        assert!(q.contains("DB_ID(@fo) = DB_ID() THEN 1"));
        assert!(q.ends_with("ELSE 0 END"));
        let sql = crate::bootstrap::read_script("Sql.160_fo_room_charge_grants.sql").unwrap();
        assert!(sql.contains("PostInterfaceTransactionToFolio"));
        assert!(!sql.contains("ALTER PROC") && !sql.contains("DROP "));
    }
}
