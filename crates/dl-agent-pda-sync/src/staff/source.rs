//! Port of `Staff/SqlStaffSource.cs`: the store's POS clerk list, already hashed; the PIN
//! never comes out of the source.

use super::StaffError;
use super::pin_hasher;
use crate::contracts::hashing::ordinal_cmp;
use crate::contracts::mapping::tcvn3;
use crate::contracts::utf16_len;
use crate::contracts::wire::{StaffClerk, staff_function_ids, staff_sync_limits};
use crate::util::ctx::Ctx;
use crate::util::stj::eq_ignore_case;
use dl_tds::{Client, Param, Value};
use std::collections::HashMap;
use std::time::Duration;

/// Reads the store's POS clerk list, already hashed (`IStaffSource`).
pub trait StaffSource: Send + Sync {
    /// The clerks, their lookups made with the tenant's PIN key `pin_key` (32 bytes).
    fn read(&self, pin_key: &[u8], ctx: &Ctx) -> Result<Vec<StaffClerk>, StaffError>;
}

/// `SqlCommand.CommandTimeout` (SqlClient's default).
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// The probe of the optional vendor columns of `dbo.CashierDef`.
pub const PROBE_SQL: &str = "SELECT c.name, t.name FROM sys.columns c JOIN sys.types t ON t.user_type_id = c.user_type_id WHERE c.object_id = OBJECT_ID(N'dbo.CashierDef');";

/// The RVCs of every clerk.
pub const RVC_SQL: &str =
    "SELECT RTRIM(Cashier), RVC FROM dbo.CashierRVC WHERE Cashier IS NOT NULL AND RVC IS NOT NULL;";

/// Only the mapped function ids ([`staff_function_ids::MAPPED`]), as `@f0, @f1, ...`.
pub fn permission_sql() -> String {
    let params: Vec<String> = (0..staff_function_ids::MAPPED.len())
        .map(|i| format!("@f{i}"))
        .collect();
    format!(
        "SELECT RTRIM(ClerkID), RTRIM(FunctionID) FROM dbo.ClerkPermission WHERE ISNULL(CAST(Allowed AS INT), 0) <> 0 AND RTRIM(FunctionID) IN ({});",
        params.join(", ")
    )
}

/// Which optional columns `dbo.CashierDef` has (`CashierColumns`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CashierColumns {
    /// `IsDisable` exists.
    pub has_is_disable: bool,
    /// `Security` exists.
    pub has_security: bool,
    /// `FullName` is an `n...` type (read as text, else as bytes and TCVN3-decoded).
    pub full_name_is_unicode: bool,
}

/// The clerk SELECT for the probed columns (`ClerkSql`).
pub fn clerk_sql(cols: CashierColumns) -> String {
    let mut s = String::from("SELECT RTRIM(c.Initial), ");
    s.push_str(if cols.full_name_is_unicode {
        "RTRIM(c.FullName), "
    } else {
        "CAST(RTRIM(c.FullName) AS VARBINARY(400)), "
    });
    s.push_str(if cols.has_is_disable {
        "CAST(ISNULL(c.IsDisable, 0) AS BIT), "
    } else {
        "CAST(0 AS BIT), "
    });
    s.push_str(if cols.has_security {
        "c.Security, "
    } else {
        "CAST(NULL AS INT), "
    });
    s.push_str(
        "CAST(c.TouchID AS NVARCHAR(50)) FROM dbo.CashierDef c WHERE c.Initial IS NOT NULL;",
    );
    s
}

/// A read failure that is not a SQL Server error (the C# `InvalidOperationException` /
/// `InvalidCastException` / `OverflowException`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceError {
    /// The .NET exception type.
    pub type_name: &'static str,
    /// The message.
    pub message: String,
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SourceError {}

fn cast_error(what: &str) -> StaffError {
    Box::new(SourceError {
        type_name: "System.InvalidCastException",
        message: format!("Unable to cast object to type '{what}'."),
    })
}

/// [`StaffSource`] over the POS database as `pda_sync`: SELECT only (ADR 0016), on
/// `dbo.CashierDef`, `dbo.CashierRVC` and `dbo.ClerkPermission`. Optional vendor columns
/// are probed first, so an older POS build without `IsDisable` or `Security` still syncs.
/// `TouchID` goes straight to [`pin_hasher::hash`]; `PassWord` and `CardID` are not read.
pub struct SqlStaffSource {
    connection: Box<dyn Fn() -> Result<dl_tds::Config, StaffError> + Send + Sync>,
}

impl std::fmt::Debug for SqlStaffSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqlStaffSource").finish_non_exhaustive()
    }
}

/// An ordinal-ignore-case map key (simple upper-casing of each char, as
/// [`eq_ignore_case`] compares).
fn fold(s: &str) -> String {
    s.chars()
        .map(|c| {
            let mut u = c.to_uppercase();
            match (u.next(), u.next()) {
                (Some(x), None) => x,
                _ => c,
            }
        })
        .collect()
}

/// `string.Trim()` (.NET white space).
fn trim(s: &str) -> &str {
    s.trim()
}

fn text(v: &Value) -> Result<Option<&str>, StaffError> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s)),
        _ => Err(cast_error("System.String")),
    }
}

/// `Convert.ToInt32(object, InvariantCulture)`.
fn to_i32(v: &Value) -> Result<i32, StaffError> {
    let overflow = || -> StaffError {
        Box::new(SourceError {
            type_name: "System.OverflowException",
            message: "Value was either too large or too small for an Int32.".to_owned(),
        })
    };
    let round = |f: f64| -> Result<i32, StaffError> {
        let r = f.round_ties_even();
        if (f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&r) {
            // In range, checked above.
            Ok(r as i32)
        } else {
            Err(overflow())
        }
    };
    match v {
        Value::Bool(b) => Ok(i32::from(*b)),
        Value::I8(n) => Ok(i32::from(*n)),
        Value::I16(n) => Ok(i32::from(*n)),
        Value::I32(n) => Ok(*n),
        Value::I64(n) => i32::try_from(*n).map_err(|_| overflow()),
        Value::F32(f) => round(f64::from(*f)),
        Value::F64(f) => round(*f),
        Value::Decimal(d) => {
            let t = d.to_string();
            t.parse::<f64>().map_err(|_| overflow()).and_then(round)
        }
        Value::String(s) => trim(s).parse::<i32>().map_err(|_| {
            Box::new(SourceError {
                type_name: "System.FormatException",
                message: format!("The input string '{s}' was not in a correct format."),
            }) as StaffError
        }),
        Value::Null => Err(cast_error("System.Int32")),
        _ => Err(cast_error("System.Int32")),
    }
}

/// The contract spelling of a function id the collation matched case-insensitively.
fn canonical(id: &str) -> String {
    staff_function_ids::MAPPED
        .iter()
        .find(|m| eq_ignore_case(m, id))
        .map_or_else(|| id.to_owned(), |m| (*m).to_owned())
}

/// The first `max` UTF-16 code units of `value` (a surrogate pair is not split; the C#
/// `value[..max]` would keep its high half).
fn cut(value: &str, max: usize) -> String {
    if utf16_len(value) <= max {
        return value.to_owned();
    }
    let mut n = 0;
    let mut out = String::new();
    for c in value.chars() {
        n += c.len_utf16();
        if n > max {
            break;
        }
        out.push(c);
    }
    out
}

impl SqlStaffSource {
    /// A source connecting with the config `connection` returns (called on every read,
    /// like the C# `Func<string>`).
    pub fn new(
        connection: impl Fn() -> Result<dl_tds::Config, StaffError> + Send + Sync + 'static,
    ) -> SqlStaffSource {
        SqlStaffSource {
            connection: Box::new(connection),
        }
    }

    fn query(
        c: &mut Client,
        sql: &str,
        params: &[Param],
        ctx: &Ctx,
    ) -> Result<dl_tds::Results, StaffError> {
        if ctx.is_done() {
            return Err(Box::new(crate::transport::Error::Cancelled));
        }
        let timeout = ctx
            .remaining()
            .map_or(COMMAND_TIMEOUT, |r| r.min(COMMAND_TIMEOUT));
        c.set_timeout(Some(timeout))?;
        Ok(c.query(sql, params)?)
    }

    fn probe(c: &mut Client, ctx: &Ctx) -> Result<CashierColumns, StaffError> {
        let rs = Self::query(c, PROBE_SQL, &[], ctx)?;
        let mut types: Vec<(String, String)> = Vec::new();
        for r in rs.rows() {
            let v = r.values();
            let name = v.first().map(text).transpose()?.flatten();
            let ty = v.get(1).map(text).transpose()?.flatten();
            let (Some(name), Some(ty)) = (name, ty) else {
                return Err(cast_error("System.String"));
            };
            match types.iter_mut().find(|(n, _)| eq_ignore_case(n, name)) {
                Some(e) => e.1 = ty.to_owned(),
                None => types.push((name.to_owned(), ty.to_owned())),
            }
        }
        let get = |n: &str| {
            types
                .iter()
                .find(|(k, _)| eq_ignore_case(k, n))
                .map(|(_, t)| t.as_str())
        };
        if get("Initial").is_none() || get("TouchID").is_none() {
            return Err(Box::new(SourceError {
                type_name: "System.InvalidOperationException",
                message: "dbo.CashierDef has no Initial or TouchID column; the staff sync cannot read the clerks.".to_owned(),
            }));
        }
        let full_name = get("FullName").unwrap_or("varchar");
        Ok(CashierColumns {
            has_is_disable: get("IsDisable").is_some(),
            has_security: get("Security").is_some(),
            full_name_is_unicode: full_name.starts_with(['n', 'N']),
        })
    }

    fn read_rvcs(c: &mut Client, ctx: &Ctx) -> Result<HashMap<String, Vec<i32>>, StaffError> {
        let rs = Self::query(c, RVC_SQL, &[], ctx)?;
        let mut map: HashMap<String, Vec<i32>> = HashMap::new();
        for r in rs.rows() {
            let v = r.values();
            let key = v
                .first()
                .map(text)
                .transpose()?
                .flatten()
                .ok_or_else(|| cast_error("System.String"))?;
            let rvc = to_i32(v.get(1).unwrap_or(&Value::Null))?;
            let list = map.entry(fold(trim(key))).or_default();
            if !list.contains(&rvc) {
                list.push(rvc);
            }
        }
        Ok(map)
    }

    fn read_functions(
        c: &mut Client,
        ctx: &Ctx,
    ) -> Result<HashMap<String, Vec<String>>, StaffError> {
        let params: Vec<Param> = staff_function_ids::MAPPED
            .iter()
            .enumerate()
            .map(|(i, f)| Param::new(&format!("f{i}"), *f))
            .collect();
        let rs = Self::query(c, &permission_sql(), &params, ctx)?;
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for r in rs.rows() {
            let v = r.values();
            let (Some(key), Some(id)) = (
                v.first().map(text).transpose()?.flatten(),
                v.get(1).map(text).transpose()?.flatten(),
            ) else {
                continue;
            };
            let set = map.entry(fold(trim(key))).or_default();
            let id = canonical(trim(id));
            if !set.iter().any(|s| eq_ignore_case(s, &id)) {
                set.push(id);
            }
        }
        Ok(map)
    }
}

impl StaffSource for SqlStaffSource {
    fn read(&self, pin_key: &[u8], ctx: &Ctx) -> Result<Vec<StaffClerk>, StaffError> {
        let cfg = (self.connection)()?;
        if ctx.is_done() {
            return Err(Box::new(crate::transport::Error::Cancelled));
        }
        let mut c = Client::connect(&cfg)?;
        let columns = Self::probe(&mut c, ctx)?;
        let rvcs = Self::read_rvcs(&mut c, ctx)?;
        let functions = Self::read_functions(&mut c, ctx)?;
        let rs = Self::query(&mut c, &clerk_sql(columns), &[], ctx)?;
        c.close();

        let mut clerks = Vec::new();
        for r in rs.rows() {
            let v = r.values();
            let at = |i: usize| v.get(i).unwrap_or(&Value::Null);
            let key = match text(at(0))? {
                Some(k) => trim(k).to_owned(),
                None => String::new(),
            };
            if key.is_empty() {
                continue;
            }
            let name = match at(1) {
                Value::Null => String::new(),
                Value::String(s) if columns.full_name_is_unicode => s.clone(),
                Value::Bytes(b) if !columns.full_name_is_unicode => tcvn3::decode(b),
                _ => {
                    return Err(cast_error(if columns.full_name_is_unicode {
                        "System.String"
                    } else {
                        "System.Byte[]"
                    }));
                }
            };
            let active = match at(2) {
                Value::Bool(b) => !*b,
                _ => return Err(cast_error("System.Boolean")),
            };
            let security = match at(3) {
                Value::Null => None,
                other => Some(to_i32(other)?),
            };
            let touch_id = text(at(4))?;
            let (lookup, status) = pin_hasher::hash(pin_key, touch_id, active);
            let folded = fold(&key);
            let mut clerk_rvcs = rvcs.get(&folded).cloned().unwrap_or_default();
            clerk_rvcs.sort_unstable();
            clerk_rvcs.truncate(staff_sync_limits::MAX_RVCS);
            let mut clerk_functions = functions.get(&folded).cloned().unwrap_or_default();
            clerk_functions.sort_by(|a, b| ordinal_cmp(a, b));
            clerks.push(StaffClerk {
                name: cut(trim(&name), staff_sync_limits::MAX_NAME_LENGTH),
                key,
                active,
                pin_lookup: lookup,
                pin_status: status.to_owned(),
                security,
                rvcs: clerk_rvcs,
                functions: clerk_functions,
            });
        }
        Ok(clerks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clerk_select_follows_the_probed_columns() {
        let all = CashierColumns {
            has_is_disable: true,
            has_security: true,
            full_name_is_unicode: true,
        };
        assert_eq!(
            clerk_sql(all),
            "SELECT RTRIM(c.Initial), RTRIM(c.FullName), CAST(ISNULL(c.IsDisable, 0) AS BIT), c.Security, CAST(c.TouchID AS NVARCHAR(50)) FROM dbo.CashierDef c WHERE c.Initial IS NOT NULL;"
        );
        let old = CashierColumns {
            has_is_disable: false,
            has_security: false,
            full_name_is_unicode: false,
        };
        assert_eq!(
            clerk_sql(old),
            "SELECT RTRIM(c.Initial), CAST(RTRIM(c.FullName) AS VARBINARY(400)), CAST(0 AS BIT), CAST(NULL AS INT), CAST(c.TouchID AS NVARCHAR(50)) FROM dbo.CashierDef c WHERE c.Initial IS NOT NULL;"
        );
        assert_eq!(
            permission_sql(),
            "SELECT RTRIM(ClerkID), RTRIM(FunctionID) FROM dbo.ClerkPermission WHERE ISNULL(CAST(Allowed AS INT), 0) <> 0 AND RTRIM(FunctionID) IN (@f0, @f1, @f2, @f3, @f4);"
        );
    }

    #[test]
    fn helpers_follow_dotnet() {
        assert_eq!(canonical("pt_newcheck"), "PT_NewCheck");
        assert_eq!(canonical("Other"), "Other");
        assert_eq!(cut("abcdef", 3), "abc");
        assert_eq!(cut("ab", 3), "ab");
        assert_eq!(to_i32(&Value::I16(5)).unwrap(), 5);
        assert_eq!(to_i32(&Value::F64(2.5)).unwrap(), 2);
        assert!(to_i32(&Value::I64(1 << 40)).is_err());
        assert_eq!(fold("Lan"), fold("LAN"));
    }
}
