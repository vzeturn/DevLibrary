//! `PmsOperationCatalog`: the closed list of stored procedures the cloud may ask for.
//!
//! The .NET catalog is C# code shared by agent and cloud; here it is `assets/catalog.json`,
//! exported from that code and compiled into the binary. A catalog change therefore ships
//! with a dl-agent release, in step with the cloud's `CatalogVersion` (the cloud answers
//! 409 to an agent whose version it does not accept).

use std::collections::HashMap;
use std::sync::OnceLock;

use dl_json::Value;

use crate::{Error, stj};

const CATALOG_JSON: &str = include_str!("../../assets/catalog.json");

/// Read or write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind {
    /// Cached, delta-capable read.
    Read,
    /// Idempotent write.
    Write,
}

/// Parameter types (`PmsParamType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamType {
    /// `nvarchar`.
    String,
    /// `int`.
    Int,
    /// `decimal`.
    Decimal,
    /// `date` (`yyyy-MM-dd`).
    Date,
    /// `bit` (`true` / `false`).
    Bool,
    /// Well-formed XML text.
    Xml,
    /// `datetime` (`yyyy-MM-ddTHH:mm:ss`).
    DateTime,
    /// `bigint`.
    Long,
    /// `varbinary` (base64).
    Binary,
}

impl ParamType {
    /// The .NET enum name, used in validation messages.
    pub fn name(self) -> &'static str {
        match self {
            ParamType::String => "String",
            ParamType::Int => "Int",
            ParamType::Decimal => "Decimal",
            ParamType::Date => "Date",
            ParamType::Bool => "Bool",
            ParamType::Xml => "Xml",
            ParamType::DateTime => "DateTime",
            ParamType::Long => "Long",
            ParamType::Binary => "Binary",
        }
    }

    fn parse(s: &str) -> Option<ParamType> {
        Some(match s {
            "String" => ParamType::String,
            "Int" => ParamType::Int,
            "Decimal" => ParamType::Decimal,
            "Date" => ParamType::Date,
            "Bool" => ParamType::Bool,
            "Xml" => ParamType::Xml,
            "DateTime" => ParamType::DateTime,
            "Long" => ParamType::Long,
            "Binary" => ParamType::Binary,
            _ => return None,
        })
    }
}

/// One procedure parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    /// Name without `@`.
    pub name: String,
    /// Type.
    pub ty: ParamType,
    /// Must be present and non-null.
    pub required: bool,
    /// Maximum length in UTF-16 units (0 = the default of 256).
    pub max_length: usize,
}

/// What one result set may carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultSetSpec {
    /// Columns a projection may name (`None` = any).
    pub columns: Option<Vec<String>>,
    /// Columns that identify a row for deltas (empty = the set is sent whole).
    pub key_columns: Vec<String>,
}

/// One catalog entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation {
    /// Name the cloud uses (`booking.list-checkin`).
    pub name: String,
    /// Read or write.
    pub kind: OperationKind,
    /// Stored procedure in the PMS database.
    pub procedure: String,
    /// Allowed parameters.
    pub parameters: Vec<Parameter>,
    /// Result sets, in order.
    pub result_sets: Vec<ResultSetSpec>,
    /// Cloud cache lifetime (informational on the agent).
    pub cache_ttl_seconds: u64,
    /// Rows kept per result set.
    pub max_rows: usize,
    /// Reads a write invalidates in the cloud cache.
    pub invalidates: Option<Vec<String>>,
    /// The cloud may queue this write while the PMS is down.
    pub deferrable: bool,
}

impl Operation {
    /// The parameter named `name`, ignoring case (`FindParameter`).
    pub fn find_parameter(&self, name: &str) -> Option<&Parameter> {
        self.parameters.iter().find(|p| stj::ci_eq(&p.name, name))
    }

    /// Key columns of result set `index` (empty past the declared sets).
    pub fn key_columns(&self, index: usize) -> &[String] {
        self.result_sets
            .get(index)
            .map_or(&[][..], |s| s.key_columns.as_slice())
    }
}

/// The catalog.
#[derive(Debug)]
pub struct Catalog {
    /// `PmsOperationCatalog.Version`, sent in the hello.
    pub version: i32,
    operations: Vec<Operation>,
    by_name: HashMap<String, usize>,
}

impl Catalog {
    /// The operation named exactly `name` (`TryGet`, ordinal).
    pub fn get(&self, name: &str) -> Option<&Operation> {
        self.by_name.get(name).map(|&i| &self.operations[i])
    }

    /// Every operation, in catalog order.
    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }

    /// Parses catalog JSON.
    ///
    /// # Errors
    /// [`Error::Config`] naming the first malformed entry.
    pub fn parse(text: &str) -> Result<Catalog, Error> {
        let bad = |m: String| Error::Config(format!("catalog: {m}"));
        let root = stj::parse(text).map_err(bad)?;
        let version = root
            .get("version")
            .and_then(Value::as_i64)
            .and_then(|v| i32::try_from(v).ok())
            .ok_or_else(|| bad("version missing".into()))?;
        let ops = root
            .get("operations")
            .and_then(Value::as_array)
            .ok_or_else(|| bad("operations missing".into()))?;
        let mut operations = Vec::with_capacity(ops.len());
        let mut by_name = HashMap::new();
        for (i, op) in ops.iter().enumerate() {
            let op = parse_operation(op).map_err(|m| bad(format!("operation {i}: {m}")))?;
            if by_name.insert(op.name.clone(), operations.len()).is_some() {
                return Err(bad(format!("duplicate operation '{}'", op.name)));
            }
            operations.push(op);
        }
        Ok(Catalog {
            version,
            operations,
            by_name,
        })
    }
}

fn string(v: &Value, key: &str) -> Result<String, String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("'{key}' missing"))
}

fn strings(v: &Value) -> Result<Vec<String>, String> {
    v.as_array()
        .ok_or("expected a list")?
        .iter()
        .map(|s| s.as_str().map(str::to_string).ok_or("expected a string"))
        .collect::<Result<_, _>>()
        .map_err(str::to_string)
}

fn opt_strings(v: Option<&Value>) -> Result<Option<Vec<String>>, String> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(v) => strings(v).map(Some),
    }
}

fn parse_operation(v: &Value) -> Result<Operation, String> {
    let kind = match string(v, "kind")?.as_str() {
        "read" => OperationKind::Read,
        "write" => OperationKind::Write,
        k => return Err(format!("unknown kind '{k}'")),
    };
    let mut parameters = Vec::new();
    for p in v
        .get("parameters")
        .and_then(Value::as_array)
        .ok_or("'parameters' missing")?
    {
        let ty = string(p, "type")?;
        parameters.push(Parameter {
            name: string(p, "name")?,
            ty: ParamType::parse(&ty).ok_or_else(|| format!("unknown type '{ty}'"))?,
            required: p.get("required").and_then(Value::as_bool).unwrap_or(false),
            max_length: p
                .get("maxLength")
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(0),
        });
    }
    let mut result_sets = Vec::new();
    for s in v
        .get("resultSets")
        .and_then(Value::as_array)
        .ok_or("'resultSets' missing")?
    {
        result_sets.push(ResultSetSpec {
            columns: opt_strings(s.get("columns"))?,
            key_columns: strings(s.get("keyColumns").ok_or("'keyColumns' missing")?)?,
        });
    }
    Ok(Operation {
        name: string(v, "name")?,
        kind,
        procedure: string(v, "procedure")?,
        parameters,
        result_sets,
        cache_ttl_seconds: v
            .get("cacheTtlSeconds")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        max_rows: v
            .get("maxRows")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(500),
        invalidates: opt_strings(v.get("invalidates"))?,
        deferrable: v
            .get("deferrable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// The catalog compiled into this build.
///
/// # Errors
/// [`Error::Config`] when the embedded JSON is malformed (a build defect the catalog load
/// test catches).
pub fn catalog() -> Result<&'static Catalog, Error> {
    static CATALOG: OnceLock<Result<Catalog, String>> = OnceLock::new();
    CATALOG
        .get_or_init(|| Catalog::parse(CATALOG_JSON).map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| Error::Config(e.clone()))
}
