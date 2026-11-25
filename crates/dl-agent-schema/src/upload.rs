//! The hub upload body for `POST /v1/agent/schema-snapshot` (RT-1215). Structure only:
//! every field comes from [`Snapshot`], which cannot hold a table row.

use dl_json::{Value, json};

use crate::model::{Database, Snapshot};

/// Mirrors the hub `MAX_SCHEMA_BYTES` (services/agent-hub/src/routes/agent.ts).
const MAX_BODY_BYTES: usize = 1_900_000;

fn strings(items: &[String]) -> Value {
    Value::Array(items.iter().map(|s| Value::String(s.clone())).collect())
}

fn database(d: &Database) -> Value {
    let tables = d.tables.iter().map(|t| {
        json!({
            "schema": t.schema.as_str(),
            "name": t.name.as_str(),
            "est_rows": t.est_rows,
            "columns": Value::Array(t.columns.iter().map(|c| json!({
                "name": c.name.as_str(),
                "sql_type": c.sql_type.as_str(),
                "max_length": c.max_length,
                "nullable": c.nullable,
                "identity": c.identity,
            })).collect()),
            "indexes": Value::Array(t.indexes.iter().map(|i| json!({
                "name": i.name.as_str(),
                "primary": i.primary,
                "unique": i.unique,
                "columns": strings(&i.columns),
            })).collect()),
        })
    });
    json!({
        "name": d.name.as_str(),
        "schema_hash": d.schema_hash(),
        "tables": Value::Array(tables.collect()),
        "foreign_keys": Value::Array(d.foreign_keys.iter().map(|f| json!({
            "name": f.name.as_str(),
            "from": strings(&f.from),
            "to": strings(&f.to),
        })).collect()),
        "routines": Value::Array(d.routines.iter().map(|r| json!({
            "schema": r.schema.as_str(),
            "name": r.name.as_str(),
            "kind": r.kind.as_str(),
        })).collect()),
    })
}

/// The JSON body the hub's `schemaSnapshotBody` accepts.
///
/// Refuses what the hub would reject anyway: no database, or a body over the hub cap.
pub fn upload_body(agent_version: &str, snapshot: &Snapshot) -> Result<String, String> {
    if snapshot.databases.is_empty() {
        return Err("snapshot has no database".to_string());
    }
    let body = json!({
        "agent_version": agent_version,
        "server_version": snapshot.server_version.as_str(),
        "databases": Value::Array(snapshot.databases.iter().map(database).collect()),
    });
    let text = dl_json::to_string(&body);
    if text.len() > MAX_BODY_BYTES {
        return Err(format!(
            "schema snapshot is {} bytes, hub cap is {MAX_BODY_BYTES}",
            text.len()
        ));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Column, Index, Table};

    #[test]
    fn body_matches_hub_contract() {
        let snap = Snapshot {
            server_version: "15.0".into(),
            databases: vec![Database {
                name: "SMILE".into(),
                tables: vec![Table {
                    schema: "dbo".into(),
                    name: "Room".into(),
                    est_rows: 12,
                    columns: vec![Column {
                        name: "Id".into(),
                        sql_type: "int".into(),
                        max_length: 4,
                        nullable: false,
                        identity: true,
                    }],
                    indexes: vec![Index {
                        name: "PK_Room".into(),
                        primary: true,
                        unique: true,
                        columns: vec!["Id".into()],
                    }],
                }],
                foreign_keys: vec![],
                routines: vec![],
            }],
        };
        let v = dl_json::parse(&upload_body("1.2.3", &snap).unwrap()).unwrap();
        assert_eq!(v["agent_version"], "1.2.3");
        let db = &v["databases"][0];
        assert_eq!(db["schema_hash"].as_str().unwrap().len(), 64);
        assert_eq!(db["tables"][0]["est_rows"].as_u64(), Some(12));
        assert_eq!(db["tables"][0]["indexes"][0]["columns"][0], "Id");
    }

    #[test]
    fn empty_snapshot_is_refused() {
        let snap = Snapshot {
            server_version: "15.0".into(),
            databases: vec![],
        };
        assert!(upload_body("1.2.3", &snap).is_err());
    }
}
