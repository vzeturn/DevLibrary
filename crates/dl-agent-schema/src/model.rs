//! The schema snapshot: structure only. No type here can hold a table row.
#![allow(missing_docs)] // plain data fields, named as the catalog names them

use dl_encoding::hex;
use dl_hash::{Digest, Sha256};

/// One column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub sql_type: String,
    pub max_length: i32,
    pub nullable: bool,
    pub identity: bool,
}

/// One index or key (`primary` marks the primary key).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    pub name: String,
    pub primary: bool,
    pub unique: bool,
    pub columns: Vec<String>,
}

/// One table with its estimated row count (a statistic, not data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub schema: String,
    pub name: String,
    pub est_rows: u64,
    pub columns: Vec<Column>,
    pub indexes: Vec<Index>,
}

/// One foreign-key column pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKey {
    pub name: String,
    pub from: [String; 3],
    pub to: [String; 3],
}

/// A view, procedure, function or trigger, by name and kind (`V`, `P`, `FN`, `IF`, `TF`, `TR`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routine {
    pub schema: String,
    pub name: String,
    pub kind: String,
}

/// One database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Database {
    pub name: String,
    pub tables: Vec<Table>,
    pub foreign_keys: Vec<ForeignKey>,
    pub routines: Vec<Routine>,
}

/// Every database collected on one site.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub server_version: String,
    pub databases: Vec<Database>,
}

impl Database {
    /// SHA-256 over the structure (names, types, keys, relations); estimated row counts are
    /// left out so a busy day does not look like a schema change.
    pub fn schema_hash(&self) -> String {
        let mut h = Hasher::default();
        h.field(&["db", &self.name]);
        for t in &self.tables {
            h.field(&["t", &t.schema, &t.name]);
            for c in &t.columns {
                h.field(&[
                    "c",
                    &c.name,
                    &c.sql_type,
                    &c.max_length.to_string(),
                    &c.nullable.to_string(),
                    &c.identity.to_string(),
                ]);
            }
            for i in &t.indexes {
                let mut f = vec!["i", &i.name];
                let flags = format!("{}{}", i.primary, i.unique);
                f.push(&flags);
                f.extend(i.columns.iter().map(String::as_str));
                h.field(&f);
            }
        }
        for f in &self.foreign_keys {
            h.field(&[
                "f", &f.name, &f.from[0], &f.from[1], &f.from[2], &f.to[0], &f.to[1], &f.to[2],
            ]);
        }
        for r in &self.routines {
            h.field(&["r", &r.schema, &r.name, &r.kind]);
        }
        h.finish()
    }
}

impl Snapshot {
    /// SHA-256 over every database's hash, in name order.
    pub fn schema_hash(&self) -> String {
        let mut dbs: Vec<(&str, String)> = self
            .databases
            .iter()
            .map(|d| (d.name.as_str(), d.schema_hash()))
            .collect();
        dbs.sort();
        let mut h = Hasher::default();
        for (n, x) in &dbs {
            h.field(&[n, x]);
        }
        h.finish()
    }
}

/// Length-prefixed hashing: no separator character can make two layouts collide.
#[derive(Default)]
struct Hasher(Sha256);

impl Hasher {
    fn field(&mut self, parts: &[&str]) {
        self.0.update(&(parts.len() as u64).to_le_bytes());
        for p in parts {
            self.0.update(&(p.len() as u64).to_le_bytes());
            self.0.update(p.as_bytes());
        }
    }

    fn finish(self) -> String {
        hex::encode(self.0.finalize())
    }
}
