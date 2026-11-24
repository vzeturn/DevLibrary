//! The op manifest the agent executes from: the procedure allow-list, the call plan and the
//! SMILE database of every op, generated from the TypeScript OpRegistry
//! (packages/shared/src/ops) into `packages/shared/src/ops/ops-manifest.json` (manifest v2)
//! and embedded here as a byte copy of that file. An op or procedure the manifest does not
//! name never runs.

use std::collections::{HashMap, HashSet};

use dl_json::{Map, Value};

use crate::json;
use crate::minire::Regex;
use crate::pat;

mod select;
#[cfg(test)]
mod select_tests;

pub use select::{Filter, SELECT_OPS, Select, Table};

/// The manifest this build carries.
pub const EMBEDDED: &str = include_str!("../assets/ops-manifest.json");

/// The manifest format this build reads.
pub const MANIFEST_VERSION: i64 = 2;

/// The SMILE databases an op may run on, in FO, POS, BO order (`AGENT_TARGET_DB`).
pub const TARGET_DBS: [&str; 3] = ["SMILE_FO", "SMILE_POS", "SMILE_BO"];

/// A read op.
pub const KIND_READ: &str = "read";
/// A write op.
pub const KIND_WRITE: &str = "write";

const SQL_TYPES: [&str; 12] = [
    "bit",
    "tinyint",
    "smallint",
    "int",
    "bigint",
    "decimal",
    "varchar",
    "nvarchar",
    "date",
    "datetime2",
    "datetimeoffset",
    "varbinary",
];

/// Excluded whatever the manifest says: dynamic SQL, OS access, wipes.
const AGENT_FLOOR: [&str; 5] = [
    "^SMLExecRsSql$",
    "^ClearDatabase$",
    "^sp_executesql$",
    "^sp_sqlexec$",
    r"^xp_\w+$",
];

/// One bound procedure parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// Parameter name (without `@`).
    pub name: String,
    /// SQL type (one of the bindable types).
    pub typ: String,
    /// The op argument it takes ("" for none).
    pub arg: String,
    /// A constant value (raw JSON, `None` when absent).
    pub value: Option<Value>,
    /// Whether null is accepted.
    pub nullable: bool,
    /// The raw `size` (characters or bytes; absent or `"max"` means max).
    pub size: Option<Value>,
    /// An OUTPUT parameter.
    pub output: bool,
}

impl Param {
    /// The declared size, or -1 for (max). Like Go's `json.Unmarshal` into an int: a JSON
    /// `null` size leaves 0.
    pub fn max_size(&self) -> i64 {
        match &self.size {
            None => -1,
            Some(Value::Null) => 0,
            Some(v) => json::as_int(v).unwrap_or(-1),
        }
    }

    /// A constant bind.
    pub fn has_value(&self) -> bool {
        self.value.is_some()
    }
}

/// One call of an op's plan: a procedure call, or a direct read of a declared table.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    /// Procedure name ("" for a select call).
    pub proc_name: String,
    /// Parameters in order (empty for a select call).
    pub params: Vec<Param>,
    /// The table read of a select call (`None` for a procedure call).
    pub select: Option<Select>,
}

/// One manifest entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Op {
    /// `<domain>.<area>.<verb>`.
    pub name: String,
    /// Contract version.
    pub contract_ver: i64,
    /// read or write.
    pub kind: String,
    /// FO, POS, BO or PMS.
    pub target: String,
    /// The SMILE database the op runs on (`None` for an op the cloud runs in process).
    pub target_db: Option<String>,
    /// Statement timeout.
    pub timeout_ms: i64,
    /// Safe to retry.
    pub idempotent: bool,
    /// Empty for an in-process-only op.
    pub calls: Vec<Call>,
}

/// The decoded, validated manifest.
#[derive(Debug, Clone)]
pub struct Manifest {
    /// Format version (2).
    pub manifest_version: i64,
    /// Hash of the allow-list (logged at start).
    pub allowlist_hash: String,
    /// The allow-listed procedures.
    pub procs: Vec<String>,
    /// The tables select calls may read.
    pub tables: Vec<Table>,
    /// Ops in manifest order.
    pub ops: Vec<Op>,
    by_name: HashMap<String, usize>,
    allowed: HashSet<String>,
}

fn e(m: impl Into<String>) -> String {
    format!("ops manifest: {}", m.into())
}

/// Go struct decoding: case-insensitive keys, unknown keys refused.
fn fields<'a>(obj: &'a Map, known: &[&str], what: &str) -> Result<&'a Map, String> {
    for (k, _) in obj.iter() {
        if !known.iter().any(|f| k.eq_ignore_ascii_case(f)) {
            return Err(e(format!(
                "json: unknown field {} in {what}",
                json::quote(k)
            )));
        }
    }
    Ok(obj)
}

fn s(obj: &Map, k: &str) -> Result<String, String> {
    match json::field_ci(obj, k) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(v)) => Ok(v.clone()),
        Some(_) => Err(e(format!("{k}: not a string"))),
    }
}

fn i(obj: &Map, k: &str) -> Result<i64, String> {
    match json::field_ci(obj, k) {
        None | Some(Value::Null) => Ok(0),
        Some(v) => json::as_int(v).ok_or_else(|| e(format!("{k}: not an integer"))),
    }
}

fn b(obj: &Map, k: &str) -> Result<bool, String> {
    match json::field_ci(obj, k) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(v)) => Ok(*v),
        Some(_) => Err(e(format!("{k}: not a bool"))),
    }
}

fn arr<'a>(obj: &'a Map, k: &str) -> Result<&'a [Value], String> {
    match json::field_ci(obj, k) {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(v)) => Ok(v),
        Some(_) => Err(e(format!("{k}: not an array"))),
    }
}

fn obj_of<'a>(v: &'a Value, what: &str) -> Result<&'a Map, String> {
    match v {
        Value::Object(m) => Ok(m),
        _ => Err(e(format!("{what}: not an object"))),
    }
}

fn parse_param(v: &Value) -> Result<Param, String> {
    let o = fields(
        obj_of(v, "param")?,
        &["name", "type", "arg", "value", "nullable", "size", "output"],
        "param",
    )?;
    Ok(Param {
        name: s(o, "name")?,
        typ: s(o, "type")?,
        arg: s(o, "arg")?,
        value: json::field_ci(o, "value").cloned(),
        nullable: b(o, "nullable")?,
        size: json::field_ci(o, "size").cloned(),
        output: b(o, "output")?,
    })
}

fn parse_op(v: &Value) -> Result<Op, String> {
    let o = fields(
        obj_of(v, "op")?,
        &[
            "name",
            "contract_ver",
            "kind",
            "target",
            "target_db",
            "timeout_ms",
            "idempotent",
            "calls",
        ],
        "op",
    )?;
    let mut calls = Vec::new();
    for c in arr(o, "calls")? {
        let co = fields(obj_of(c, "call")?, &["proc", "params", "select"], "call")?;
        let select = match json::field_ci(co, "select") {
            None | Some(Value::Null) => None,
            Some(v) => Some(select::parse_select(v)?),
        };
        let proc_name = s(co, "proc")?;
        let params: Vec<Param> = arr(co, "params")?
            .iter()
            .map(parse_param)
            .collect::<Result<_, _>>()?;
        if select.is_some() && (!proc_name.is_empty() || !params.is_empty()) {
            return Err(e("call: exactly one of proc (with params) or select"));
        }
        calls.push(Call {
            proc_name,
            params,
            select,
        });
    }
    Ok(Op {
        name: s(o, "name")?,
        contract_ver: i(o, "contract_ver")?,
        kind: s(o, "kind")?,
        target: s(o, "target")?,
        target_db: Some(s(o, "target_db")?).filter(|t| !t.is_empty()),
        timeout_ms: i(o, "timeout_ms")?,
        idempotent: b(o, "idempotent")?,
        calls,
    })
}

/// The SMILE database of a manifest target (`None` for PMS, which has none on the site).
pub fn target_db_of(target: &str) -> Option<&'static str> {
    match target {
        "FO" => Some(TARGET_DBS[0]),
        "POS" => Some(TARGET_DBS[1]),
        "BO" => Some(TARGET_DBS[2]),
        _ => None,
    }
}

fn validate_op(op: &Op, allowed: &HashSet<String>, tables: &select::Tables) -> Result<(), String> {
    if !pat::is_op_name(&op.name) {
        return Err("name is not <domain>.<area>.<verb>".into());
    }
    if op.contract_ver < 1 || op.timeout_ms < 1 {
        return Err("contract_ver and timeout_ms must be positive".into());
    }
    if op.kind != KIND_READ && op.kind != KIND_WRITE {
        return Err(format!("kind {} is not read|write", json::quote(&op.kind)));
    }
    if !["FO", "POS", "BO", "PMS"].contains(&op.target.as_str()) {
        return Err(format!(
            "target {} is not FO|POS|BO|PMS",
            json::quote(&op.target)
        ));
    }
    match (&op.target_db, op.calls.is_empty()) {
        (Some(db), _) if target_db_of(&op.target) != Some(db.as_str()) => {
            return Err(format!(
                "target_db {} does not match target {}",
                json::quote(db),
                op.target
            ));
        }
        (None, false) => return Err("an op with agent calls needs a target_db".into()),
        _ => {}
    }
    let mut outputs = HashSet::new();
    for c in &op.calls {
        if let Some(sel) = &c.select {
            if op.kind != KIND_READ {
                return Err("a select call is allowed only in a read op".into());
            }
            select::validate_select(sel, tables)?;
            continue;
        }
        if c.proc_name.is_empty() {
            return Err("a call needs exactly one of proc or select".into());
        }
        if !allowed.contains(&c.proc_name.to_lowercase()) {
            return Err(format!("proc {} is not allow-listed", c.proc_name));
        }
        let mut seen = HashSet::new();
        for p in &c.params {
            let key = p.name.to_lowercase();
            if !pat::is_bare_name(&p.name) || !seen.insert(key.clone()) {
                return Err(format!(
                    "{}: parameter {} is not a unique bare name",
                    c.proc_name,
                    json::quote(&p.name)
                ));
            }
            if !SQL_TYPES.contains(&p.typ.as_str()) {
                return Err(format!(
                    "{}.{}: type {} is not bindable",
                    c.proc_name,
                    p.name,
                    json::quote(&p.typ)
                ));
            }
            let sources = [!p.arg.is_empty(), p.has_value(), p.output]
                .iter()
                .filter(|x| **x)
                .count();
            if sources != 1 {
                return Err(format!(
                    "{}.{}: exactly one of arg, value or output",
                    c.proc_name, p.name
                ));
            }
            if p.output && (key == "returnvalue" || !outputs.insert(key)) {
                return Err(format!(
                    "{}.{}: output name is not unique in the op",
                    c.proc_name, p.name
                ));
            }
        }
    }
    Ok(())
}

impl Manifest {
    /// Decodes and validates the embedded manifest.
    pub fn load() -> Result<Manifest, String> {
        Manifest::parse(EMBEDDED.as_bytes())
    }

    /// Decodes and validates a manifest: any proc outside the allow-list, any allow-listed
    /// proc matching an excluded pattern (or the agent's floor), a select call on a table
    /// or column the `tables` list does not declare, unknown bind types and duplicate names
    /// are refused, so a bad generator output fails at boot.
    pub fn parse(raw: &[u8]) -> Result<Manifest, String> {
        let v = json::parse(raw).map_err(|x| e(x.to_string()))?;
        let o = fields(
            obj_of(&v, "manifest")?,
            &[
                "manifest_version",
                "allowlist_hash",
                "procs",
                "tables",
                "excluded_proc_patterns",
                "ops",
            ],
            "manifest",
        )?;
        let version = i(o, "manifest_version")?;
        if version != MANIFEST_VERSION {
            return Err(e(format!(
                "manifest_version {version} is not {MANIFEST_VERSION}"
            )));
        }
        let strings = |k: &str| -> Result<Vec<String>, String> {
            arr(o, k)?
                .iter()
                .map(|v| match v {
                    Value::String(s) => Ok(s.clone()),
                    Value::Null => Ok(String::new()),
                    _ => Err(e(format!("{k}: not a string"))),
                })
                .collect()
        };
        let procs = strings("procs")?;
        let mut excluded = Vec::new();
        for p in AGENT_FLOOR
            .iter()
            .map(|s| (*s).to_owned())
            .chain(strings("excluded_proc_patterns")?)
        {
            let re = Regex::new(&format!("(?i){p}"))
                .map_err(|x| e(format!("excluded pattern {}: {x}", json::quote(&p))))?;
            excluded.push(re);
        }
        let mut allowed = HashSet::new();
        for p in &procs {
            if !pat::is_bare_name(p) {
                return Err(e(format!("proc {} is not a bare name", json::quote(p))));
            }
            if excluded.iter().any(|re| re.is_match(p)) {
                return Err(e(format!("proc {} is excluded", json::quote(p))));
            }
            allowed.insert(p.to_lowercase());
        }
        let (tables, table_set) = select::parse_tables(o)?;
        let ops = arr(o, "ops")?
            .iter()
            .map(parse_op)
            .collect::<Result<Vec<_>, _>>()?;
        let mut by_name = HashMap::new();
        for (idx, op) in ops.iter().enumerate() {
            validate_op(op, &allowed, &table_set).map_err(|x| e(format!("op {}: {x}", op.name)))?;
            if by_name.insert(op.name.clone(), idx).is_some() {
                return Err(e(format!("op {} is listed twice", op.name)));
            }
        }
        Ok(Manifest {
            manifest_version: version,
            allowlist_hash: s(o, "allowlist_hash")?,
            procs,
            tables,
            ops,
            by_name,
            allowed,
        })
    }

    /// The op named exactly `name`.
    pub fn lookup(&self, name: &str) -> Option<&Op> {
        self.by_name.get(name).map(|&i| &self.ops[i])
    }

    /// Whether `proc_name` is allow-listed (case-insensitive, as SQL Server).
    pub fn allowed(&self, proc_name: &str) -> bool {
        self.allowed.contains(&proc_name.to_lowercase())
    }

    /// The highest contract_ver of any op the agent can execute (at least 1).
    pub fn contract_max(&self) -> i64 {
        self.ops
            .iter()
            .filter(|op| !op.calls.is_empty())
            .map(|op| op.contract_ver)
            .fold(1, i64::max)
    }

    /// `name@contract_ver` of every op this build executes (the ones with agent calls), in
    /// manifest order: the `ops` list of the hello frame.
    pub fn implemented_ops(&self) -> Vec<String> {
        self.ops
            .iter()
            .filter(|op| !op.calls.is_empty())
            .map(|op| format!("{}@{}", op.name, op.contract_ver))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_manifest_loads() {
        let m = Manifest::load().unwrap();
        assert_eq!(m.manifest_version, 2);
        let op = m.lookup("fo.hotelDate.get").unwrap();
        assert_eq!(op.calls.len(), 1);
        assert_eq!(op.calls[0].proc_name, "GetHotelDate1");
        assert_eq!(op.target_db.as_deref(), Some("SMILE_FO"));
        assert_eq!(m.lookup("pos.checkVersion.get").unwrap().target_db, None);
        assert!(m.allowed("gethoteldate1"));
        assert!(!m.allowed("MM_Op_CreateLog"));
        assert_eq!(m.contract_max(), 1);
        assert_eq!(
            m.implemented_ops(),
            [
                "fo.arrivals.expected@1",
                "fo.availability.chart@1",
                "fo.availability.contractStatus@1",
                "fo.availability.source@1",
                "fo.availability.sourceV2@1",
                "fo.clerk.login@1",
                "fo.clerk.rights.get@1",
                "fo.dashboard.houseMix@1",
                "fo.dashboard.rooms@1",
                "fo.dashboard.today@1",
                "fo.departures.expected@1",
                "fo.departures.onDate@1",
                "fo.hotelDate.get@1",
                "fo.inhouse.list@1",
                "fo.pos.post@1",
                "fo.reservation.list@1",
                "fo.reservation.listFigures@1",
                "pos.cashier.byInitial@1",
                "pos.cashier.login@1",
                "pos.check.close@1",
                "pos.check.detail@1",
                "pos.check.list@1",
                "pos.check.open@1",
                "pos.check.pay@1",
                "pos.check.postItem@1",
                "pos.check.send@1",
                "pos.clerk.permissions@1",
                "pos.menu.items@1",
                "pos.menu.tree@1",
                "pos.outbox.claim@1",
                "pos.outbox.complete@1",
                "pos.outbox.source@1",
                "pos.outlets.list@1",
                "pos.shift.payments@1",
                "pos.tender.modes@1",
            ]
        );
        // RT-0528: the clerk login is a keyless, non-idempotent write on vendor LogIn; its
        // ComputerName is a manifest constant, the rest are caller args.
        let login = m.lookup("fo.clerk.login").unwrap();
        assert_eq!((login.kind.as_str(), login.idempotent), ("write", false));
        assert_eq!(login.target_db.as_deref(), Some("SMILE_FO"));
        assert_eq!(login.calls[0].proc_name, "LogIn");
        let args: Vec<&str> = login.calls[0]
            .params
            .iter()
            .map(|p| p.arg.as_str())
            .collect();
        assert_eq!(args, ["clerkId", "passwordHash", "todayPassword", ""]);
        assert!(login.calls[0].params[3].has_value());
        let rights = m.lookup("fo.clerk.rights.get").unwrap();
        assert_eq!(rights.kind, "read");
        assert_eq!(rights.calls[0].proc_name, "UGetUserPermission");
        for p in [
            "login",
            "ugetuserpermission",
            "availabilitychartnew",
            "rptreservmasterlist",
        ] {
            assert!(m.allowed(p), "{p}");
        }
        let hash = crate::sha256_hex(m.procs.join("\n").as_bytes());
        assert_eq!(m.allowlist_hash, hash);
    }

    fn with(procs: &str, ops: &str) -> Result<Manifest, String> {
        Manifest::parse(
            format!(
                r#"{{"manifest_version":2,"allowlist_hash":"x","procs":{procs},"excluded_proc_patterns":["^MM_Danger"],"ops":{ops}}}"#
            )
            .as_bytes(),
        )
    }

    #[test]
    fn refuses_bad_manifests() {
        assert!(with(concat!(r#"["xp_"#, r#"cmdshell"]"#), "[]").is_err());
        assert!(with(r#"["mm_danger_x"]"#, "[]").is_err());
        assert!(with(r#"["dbo.X"]"#, "[]").is_err());
        let op = |params: &str| {
            format!(
                r#"[{{"name":"a.b.c","contract_ver":1,"kind":"read","target":"FO","target_db":"SMILE_FO","timeout_ms":1,"idempotent":true,"calls":[{{"proc":"P","params":{params}}}]}}]"#
            )
        };
        assert!(with(r#"["P"]"#, &op("[]")).is_ok());
        assert!(with(r#"["Q"]"#, &op("[]")).is_err());
        let routed = |target: &str, db: &str| {
            with(
                r#"["P"]"#,
                &format!(
                    r#"[{{"name":"a.b.c","contract_ver":1,"kind":"read","target":"{target}","target_db":{db},"timeout_ms":1,"idempotent":true,"calls":[{{"proc":"P","params":[]}}]}}]"#
                ),
            )
        };
        assert!(routed("POS", r#""SMILE_POS""#).is_ok());
        assert!(routed("BO", r#""SMILE_BO""#).is_ok());
        assert!(
            routed("FO", r#""SMILE_POS""#).is_err(),
            "target_db must match target"
        );
        assert!(
            routed("PMS", r#""MM_PMS""#).is_err(),
            "MM_PMS is never a target"
        );
        assert!(
            routed("FO", "null").is_err(),
            "an op with calls needs a target_db"
        );
        assert!(
            Manifest::parse(
                EMBEDDED
                    .replace("\"manifest_version\": 2", "\"manifest_version\": 1")
                    .as_bytes()
            )
            .is_err(),
            "a v1 manifest is refused"
        );
        assert!(
            with(
                r#"["P"]"#,
                &op(r#"[{"name":"A","type":"money","arg":"a"}]"#)
            )
            .is_err()
        );
        assert!(
            with(
                r#"["P"]"#,
                &op(r#"[{"name":"A","type":"int","arg":"a","value":1}]"#)
            )
            .is_err()
        );
        assert!(
            with(
                r#"["P"]"#,
                &op(r#"[{"name":"A","type":"int","arg":"a"},{"name":"a","type":"int","arg":"b"}]"#)
            )
            .is_err()
        );
        assert!(
            with(
                r#"["P"]"#,
                &op(r#"[{"name":"ReturnValue","type":"int","output":true}]"#)
            )
            .is_err()
        );
        assert!(
            with(
                r#"["P"]"#,
                &op(r#"[{"name":"A","type":"int","arg":"a","extra":1}]"#)
            )
            .is_err()
        );
    }
}
