//! Port of `Commands/SqlCashierCommandUnit.cs` (ADR 0150): [`CashierCommandUnit`] on the
//! command transaction. Reads use the staff sync projection ([`source::to_clerk`]) under
//! `UPDLOCK, HOLDLOCK`, so POS WIN cannot change the clerk between the hash check and the
//! write. Writes are the constants below, the statements `120_cashier_master_grants.sql`
//! allows; every value is a parameter. Optional vendor columns (`IsDisable`, `Security`)
//! are probed as the staff sync does (pos-compat.md). The PIN is never logged.

use dl_tds::{Param, ParamValue, Value};

use super::error::{CommandError, Result};
use super::sql;
use super::sql_store::SqlSiteCommandUnit;
use super::units::{CashierApply, CashierCommandUnit};
use crate::contracts::wire::{StaffClerk, staff_function_ids};
use crate::staff::source::{self, CashierColumns};
use crate::util::ctx::Ctx;
use crate::util::stj::eq_ignore_case;

/// The `CashierDef` columns a cashier insert names; any other NOT NULL column without a
/// default refuses the create.
pub const INSERT_COLUMNS: &[&str] = &["Initial", "FullName", "TouchID", "IsDisable", "Security"];

/// `SchemaSql`: every `CashierDef` column with its type and whether an insert may omit it.
pub const SCHEMA_SQL: &str = "SELECT c.name, t.name, c.is_nullable, c.is_identity, c.is_computed, CAST(CASE WHEN c.default_object_id <> 0 THEN 1 ELSE 0 END AS BIT) FROM sys.columns c JOIN sys.types t ON t.user_type_id = c.user_type_id WHERE c.object_id = OBJECT_ID(N'dbo.CashierDef', N'U');";

/// `RvcSql`.
pub const RVC_SQL: &str = "SELECT RVC FROM dbo.CashierRVC WITH (UPDLOCK, HOLDLOCK) WHERE Cashier = @Key AND RVC IS NOT NULL;";

/// `PermissionSql` (one `@f<i>` per [`staff_function_ids::MAPPED`] id).
pub const PERMISSION_SQL: &str = "SELECT RTRIM(FunctionID) FROM dbo.ClerkPermission WITH (UPDLOCK, HOLDLOCK) WHERE ClerkID = @Key AND ISNULL(CAST(Allowed AS INT), 0) <> 0 AND FunctionID IN (@f0, @f1, @f2, @f3, @f4);";

/// `PinTakenSql`, completed by the `IsDisable` filter when the column exists.
pub const PIN_TAKEN_SQL: &str = "SELECT COUNT(*) FROM dbo.CashierDef c WITH (UPDLOCK, HOLDLOCK) WHERE c.Initial <> @Key AND RTRIM(CAST(c.TouchID AS NVARCHAR(50))) = @Pin";

/// `DeleteRvcsSql` (`@rvcs` becomes `@r0, @r1, ...`).
pub const DELETE_RVCS_SQL: &str =
    "DELETE FROM dbo.CashierRVC WHERE Cashier = @Key AND (RVC IS NULL OR RVC NOT IN (@rvcs));";

/// `DeleteAllRvcsSql`.
pub const DELETE_ALL_RVCS_SQL: &str = "DELETE FROM dbo.CashierRVC WHERE Cashier = @Key;";

/// `InsertRvcSql`.
pub const INSERT_RVC_SQL: &str = "IF NOT EXISTS (SELECT 1 FROM dbo.CashierRVC WHERE Cashier = @Key AND RVC = @Rvc) INSERT INTO dbo.CashierRVC (Cashier, RVC) VALUES (@Key, @Rvc);";

/// `PermissionUpsertSql`.
pub const PERMISSION_UPSERT_SQL: &str = "UPDATE dbo.ClerkPermission SET Allowed = @Allowed WHERE ClerkID = @Key AND FunctionID = @Function; IF @@ROWCOUNT = 0 AND @Allowed = 1 INSERT INTO dbo.ClerkPermission (ClerkID, FunctionID, Allowed) VALUES (@Key, @Function, 1);";

/// The refusal when this store's `CashierDef` cannot hold the write.
pub const SCHEMA_UNSUPPORTED: &str = "cashier_schema_unsupported";

fn name_expr(cols: CashierColumns) -> &'static str {
    if cols.full_name_is_unicode {
        "@Name"
    } else {
        "LEFT(dbo.Unicode2TCVN(@Name), 50)"
    }
}

/// `InsertSql`: a `varchar` FullName goes through the vendor's `dbo.Unicode2TCVN`
/// (granted in 070), as POS WIN stores it.
pub fn insert_sql(cols: CashierColumns) -> String {
    format!(
        "INSERT INTO dbo.CashierDef (Initial, FullName, TouchID{}{}) VALUES (@Key, {}, @Pin{}{});",
        if cols.has_is_disable {
            ", IsDisable"
        } else {
            ""
        },
        if cols.has_security { ", Security" } else { "" },
        name_expr(cols),
        if cols.has_is_disable {
            ", @IsDisable"
        } else {
            ""
        },
        if cols.has_security { ", @Security" } else { "" },
    )
}

/// `UpdateSql`.
pub fn update_sql(cols: CashierColumns, pin: bool) -> String {
    format!(
        "UPDATE dbo.CashierDef SET FullName = {}{}{}{} WHERE Initial = @Key;",
        name_expr(cols),
        if cols.has_is_disable {
            ", IsDisable = @IsDisable"
        } else {
            ""
        },
        if cols.has_security {
            ", Security = @Security"
        } else {
            ""
        },
        if pin { ", TouchID = @Pin" } else { "" },
    )
}

/// The probed `CashierDef` of one unit (`Schema`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct CashierSchema {
    columns: CashierColumns,
    blocks_insert: bool,
}

fn key_param(key: &str) -> Param {
    Param::new("Key", ParamValue::VarChar(Some(key.to_owned())))
}

fn bit(v: Option<&Value>) -> Result<bool> {
    match v {
        Some(Value::Bool(b)) => Ok(*b),
        _ => Err(CommandError::invalid(
            "dbo.CashierDef schema probe returned a non-bit flag.",
        )),
    }
}

fn staff_error(e: crate::staff::StaffError) -> CommandError {
    CommandError::invalid(e.to_string())
}

impl SqlSiteCommandUnit<'_> {
    fn cashier_schema(&mut self, ctx: &Ctx) -> Result<CashierSchema> {
        if let Some(s) = self.cashier_schema {
            return Ok(s);
        }
        let r = sql::query(self.c, SCHEMA_SQL, &[], ctx)?;
        let mut types: Vec<(String, String)> = Vec::new();
        let mut blocks = false;
        if let Some(set) = r.first() {
            for row in set.rows() {
                let (Some(name), Some(ty)) = (sql::text(row, 0), sql::text(row, 1)) else {
                    return Err(CommandError::invalid(
                        "dbo.CashierDef schema probe returned a NULL name.",
                    ));
                };
                let v = row.values();
                let required =
                    !bit(v.get(2))? && !bit(v.get(3))? && !bit(v.get(4))? && !bit(v.get(5))?;
                if required && !INSERT_COLUMNS.iter().any(|c| eq_ignore_case(c, &name)) {
                    blocks = true;
                }
                match types.iter_mut().find(|(n, _)| eq_ignore_case(n, &name)) {
                    Some(e) => e.1 = ty,
                    None => types.push((name, ty)),
                }
            }
        }
        let get = |n: &str| {
            types
                .iter()
                .find(|(k, _)| eq_ignore_case(k, n))
                .map(|(_, t)| t.as_str())
        };
        let (Some(_), Some(_), Some(full_name)) = (get("Initial"), get("TouchID"), get("FullName"))
        else {
            return Err(CommandError::invalid(
                "dbo.CashierDef has no Initial, FullName or TouchID column; a cashier cannot be written.",
            ));
        };
        let schema = CashierSchema {
            columns: CashierColumns {
                has_is_disable: get("IsDisable").is_some(),
                has_security: get("Security").is_some(),
                full_name_is_unicode: full_name.starts_with(['n', 'N']),
            },
            blocks_insert: blocks,
        };
        self.cashier_schema = Some(schema);
        Ok(schema)
    }
}

impl CashierCommandUnit for SqlSiteCommandUnit<'_> {
    fn read(&mut self, key: &str, pin_key: &[u8], ctx: &Ctx) -> Result<Option<StaffClerk>> {
        let schema = self.cashier_schema(ctx)?;
        let select = source::clerk_columns_sql(schema.columns)
            + " FROM dbo.CashierDef c WITH (UPDLOCK, HOLDLOCK) WHERE c.Initial = @Key;";
        let r = sql::query(self.c, &select, &[key_param(key)], ctx)?;
        let Some(row) = r.first().and_then(|s| s.rows().first()) else {
            return Ok(None);
        };
        let clerk = source::clerk_row(row.values(), schema.columns).map_err(staff_error)?;
        drop(r);

        let mut rvcs = Vec::new();
        let r = sql::query(self.c, RVC_SQL, &[key_param(key)], ctx)?;
        if let Some(set) = r.first() {
            for row in set.rows() {
                rvcs.push(sql::to_i32(row.values().first().unwrap_or(&Value::Null))?);
            }
        }

        let mut params = vec![key_param(key)];
        params.extend(staff_function_ids::MAPPED.iter().enumerate().map(|(i, f)| {
            Param::new(&format!("f{i}"), ParamValue::VarChar(Some((*f).to_owned())))
        }));
        let mut functions = Vec::new();
        let r = sql::query(self.c, PERMISSION_SQL, &params, ctx)?;
        if let Some(set) = r.first() {
            for row in set.rows() {
                if let Some(id) = sql::text(row, 0) {
                    functions.push(id.trim().to_owned());
                }
            }
        }
        let store_key = clerk.key.clone().unwrap_or_else(|| key.to_owned());
        Ok(Some(source::to_clerk(
            pin_key,
            &store_key,
            &clerk.name,
            clerk.active,
            clerk.security,
            clerk.touch_id.as_deref(),
            &rvcs,
            &functions,
        )))
    }

    fn pin_taken(&mut self, key: &str, pin: &str, ctx: &Ctx) -> Result<bool> {
        let schema = self.cashier_schema(ctx)?;
        let text = format!(
            "{PIN_TAKEN_SQL}{}",
            if schema.columns.has_is_disable {
                " AND ISNULL(c.IsDisable, 0) = 0;"
            } else {
                ";"
            }
        );
        let r = sql::query(
            self.c,
            &text,
            &[
                key_param(key),
                Param::new("Pin", ParamValue::String(Some(pin.to_owned()))),
            ],
            ctx,
        )?;
        Ok(sql::to_i32(sql::scalar(&r).unwrap_or(&Value::Null))? > 0)
    }

    fn apply(&mut self, apply: &CashierApply<'_>, ctx: &Ctx) -> Result<Option<String>> {
        let schema = self.cashier_schema(ctx)?;
        let cols = schema.columns;
        if !cols.has_is_disable && !apply.active {
            return Ok(Some(SCHEMA_UNSUPPORTED.to_owned()));
        }
        if apply.create && schema.blocks_insert {
            return Ok(Some(SCHEMA_UNSUPPORTED.to_owned()));
        }

        let text = if apply.create {
            insert_sql(cols)
        } else {
            update_sql(cols, apply.pin.is_some())
        };
        let mut params = vec![
            key_param(apply.key),
            Param::new("Name", ParamValue::String(Some(apply.name.to_owned()))),
        ];
        if cols.has_is_disable {
            params.push(Param::new(
                "IsDisable",
                ParamValue::Bool(Some(!apply.active)),
            ));
        }
        if cols.has_security {
            params.push(Param::new(
                "Security",
                ParamValue::I32(Some(apply.security)),
            ));
        }
        if let Some(pin) = apply.pin {
            params.push(Param::new("Pin", ParamValue::VarChar(Some(pin.to_owned()))));
        }
        let changed = sql::execute(self.c, &text, &params, ctx)?;
        drop(params);
        if changed != 1 {
            return Err(CommandError::invalid(format!(
                "Cashier write changed {changed} dbo.CashierDef rows instead of one."
            )));
        }

        if apply.rvcs.is_empty() {
            sql::execute(self.c, DELETE_ALL_RVCS_SQL, &[key_param(apply.key)], ctx)?;
        } else {
            let names: Vec<String> = (0..apply.rvcs.len()).map(|i| format!("@r{i}")).collect();
            let mut params = vec![key_param(apply.key)];
            params.extend(
                apply
                    .rvcs
                    .iter()
                    .enumerate()
                    .map(|(i, r)| Param::new(&format!("r{i}"), ParamValue::I32(Some(*r)))),
            );
            sql::execute(
                self.c,
                &DELETE_RVCS_SQL.replace("@rvcs", &names.join(", ")),
                &params,
                ctx,
            )?;
            for rvc in apply.rvcs {
                sql::execute(
                    self.c,
                    INSERT_RVC_SQL,
                    &[
                        key_param(apply.key),
                        Param::new("Rvc", ParamValue::I32(Some(*rvc))),
                    ],
                    ctx,
                )?;
            }
        }

        for function in staff_function_ids::MAPPED {
            let allowed = apply.allowed.iter().any(|a| a == function);
            sql::execute(
                self.c,
                PERMISSION_UPSERT_SQL,
                &[
                    key_param(apply.key),
                    Param::new(
                        "Function",
                        ParamValue::VarChar(Some((*function).to_owned())),
                    ),
                    Param::new("Allowed", ParamValue::Bool(Some(allowed))),
                ],
                ctx,
            )?;
        }
        Ok(None)
    }
}
