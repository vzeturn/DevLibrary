//! `PmsRequestValidator`: the agent checks every request against the catalog before it
//! touches the PMS, and converts parameter text to SQL values the way .NET did.

use dl_time::{Date, DateTime};

use super::catalog::{Catalog, Operation, OperationKind, ParamType, Parameter};
use super::messages::PmsRequest;
use super::{projection, values, xml};
use crate::netdec::{self, NetDecimal};

const DEFAULT_STRING_MAX: usize = 256;

/// A parameter converted for SQL (`ConvertValue`).
#[derive(Debug, Clone, PartialEq)]
pub enum ParamValue {
    /// `int`.
    Int(i32),
    /// `decimal`.
    Decimal(NetDecimal),
    /// `date` (a `DateTime` at midnight in .NET).
    Date(Date),
    /// `datetime`.
    DateTime(DateTime),
    /// `bigint`.
    Long(i64),
    /// `varbinary`.
    Binary(Vec<u8>),
    /// `bit`.
    Bool(bool),
    /// `nvarchar` (String and Xml).
    Text(String),
}

/// `TryValidate`: the operation, or the .NET message for the first problem.
///
/// # Errors
/// The validation message sent back to the cloud as `Rejected`.
pub fn validate<'c>(catalog: &'c Catalog, request: &PmsRequest) -> Result<&'c Operation, String> {
    let op = catalog
        .get(&request.operation)
        .ok_or_else(|| format!("Operation '{}' is not in the catalog.", request.operation))?;
    if op.kind == OperationKind::Write
        && request
            .idempotency_key
            .as_deref()
            .is_none_or(|k| k.trim().is_empty())
    {
        return Err("Write operations require an idempotency key.".into());
    }
    if let Some(p) = &request.projection {
        projection::try_validate(op, p)?;
    }
    for (name, _) in request.parameters.iter() {
        if op.find_parameter(name).is_none() {
            return Err(format!(
                "Parameter '{name}' is not allowed for '{}'.",
                op.name
            ));
        }
    }
    for p in &op.parameters {
        match request.parameters.get(&p.name).flatten() {
            None if p.required => return Err(format!("Parameter '{}' is required.", p.name)),
            None => {}
            Some(v) if !is_valid_value(p, v) => {
                return Err(format!(
                    "Parameter '{}' is not a valid {}.",
                    p.name,
                    p.ty.name()
                ));
            }
            Some(_) => {}
        }
    }
    Ok(op)
}

/// `IsValidValue`: length (UTF-16 units) and format.
pub fn is_valid_value(p: &Parameter, value: &str) -> bool {
    let max = if p.max_length > 0 {
        p.max_length
    } else {
        DEFAULT_STRING_MAX
    };
    if value.encode_utf16().count() > max {
        return false;
    }
    match p.ty {
        ParamType::Xml => xml::is_well_formed(value),
        _ => convert_value(p, value).is_some(),
    }
}

/// `ConvertValue` for a validated value (`None` when it does not parse).
pub fn convert_value(p: &Parameter, value: &str) -> Option<ParamValue> {
    Some(match p.ty {
        ParamType::Int => ParamValue::Int(
            i32::try_from(netdec::parse_integer(
                value,
                i32::MIN.into(),
                i32::MAX.into(),
            )?)
            .ok()?,
        ),
        ParamType::Decimal => ParamValue::Decimal(netdec::parse_number_style(value)?),
        ParamType::Date => ParamValue::Date(values::parse_date(value)?),
        ParamType::DateTime => ParamValue::DateTime(values::parse_datetime(value)?),
        ParamType::Long => ParamValue::Long(
            i64::try_from(netdec::parse_integer(
                value,
                i64::MIN.into(),
                i64::MAX.into(),
            )?)
            .ok()?,
        ),
        ParamType::Binary => ParamValue::Binary(values::parse_base64(value)?),
        ParamType::Bool => ParamValue::Bool(values::parse_bool(value)?),
        ParamType::String | ParamType::Xml => ParamValue::Text(value.to_string()),
    })
}
