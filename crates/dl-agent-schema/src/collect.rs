//! Runs the catalog queries through a [`MetaSource`] and assembles a [`Database`].

use std::fmt;

use crate::model::{Column, Database, ForeignKey, Index, Routine, Table};
use crate::queries::{self, COLUMNS, FOREIGN_KEYS, INDEXES, ROUTINES, TABLES};

/// What went wrong while collecting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The SQL transport failed (`schema_sql_failed`).
    Sql(String),
    /// A statement was not a catalog-only SELECT (`schema_query_refused`).
    Refused(String),
    /// A catalog row did not have the expected shape (`schema_row_invalid`).
    Row(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Sql(m) => write!(f, "schema sql failed: {m}"),
            Error::Refused(m) => write!(f, "schema query refused: {m}"),
            Error::Row(m) => write!(f, "schema row invalid: {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// A connection to one database that can run a catalog query and return text rows.
pub trait MetaSource {
    /// Runs `sql` (already checked) and returns every row as text columns.
    fn rows(&mut self, sql: &str) -> Result<Vec<Vec<String>>, Error>;
}

fn run(src: &mut dyn MetaSource, sql: &str, width: usize) -> Result<Vec<Vec<String>>, Error> {
    queries::ensure_catalog_only(sql)?;
    let rows = src.rows(sql)?;
    match rows.iter().find(|r| r.len() != width) {
        Some(r) => Err(Error::Row(format!(
            "expected {width} columns, got {}",
            r.len()
        ))),
        None => Ok(rows),
    }
}

fn num<T: std::str::FromStr>(s: &str, what: &str) -> Result<T, Error> {
    s.trim()
        .parse()
        .map_err(|_| Error::Row(format!("{what} is not a number")))
}

/// Collects the structure of the database `src` is connected to.
pub fn collect_database(name: &str, src: &mut dyn MetaSource) -> Result<Database, Error> {
    let mut tables: Vec<Table> = Vec::new();
    for r in run(src, TABLES, 3)? {
        tables.push(Table {
            schema: r[0].clone(),
            name: r[1].clone(),
            est_rows: num(&r[2], "row count")?,
            columns: vec![],
            indexes: vec![],
        });
    }
    let find = |tables: &mut Vec<Table>, s: &str, n: &str| {
        tables.iter().position(|t| t.schema == s && t.name == n)
    };
    for r in run(src, COLUMNS, 8)? {
        let Some(i) = find(&mut tables, &r[0], &r[1]) else {
            return Err(Error::Row(format!(
                "column of unknown table {}.{}",
                r[0], r[1]
            )));
        };
        {
            tables[i].columns.push(Column {
                name: r[2].clone(),
                sql_type: r[3].clone(),
                max_length: num(&r[4], "max length")?,
                nullable: r[5] == "1",
                identity: r[6] == "1",
            });
        }
    }
    for r in run(src, INDEXES, 6)? {
        let Some(i) = find(&mut tables, &r[0], &r[1]) else {
            continue;
        };
        match tables[i].indexes.iter_mut().find(|x| x.name == r[2]) {
            Some(ix) => ix.columns.push(r[5].clone()),
            None => tables[i].indexes.push(Index {
                name: r[2].clone(),
                primary: r[3] == "1",
                unique: r[4] == "1",
                columns: vec![r[5].clone()],
            }),
        }
    }
    let foreign_keys = run(src, FOREIGN_KEYS, 7)?
        .into_iter()
        .map(|r| ForeignKey {
            name: r[0].clone(),
            from: [r[1].clone(), r[2].clone(), r[3].clone()],
            to: [r[4].clone(), r[5].clone(), r[6].clone()],
        })
        .collect();
    let routines = run(src, ROUTINES, 3)?
        .into_iter()
        .map(|r| Routine {
            schema: r[0].clone(),
            name: r[1].clone(),
            kind: r[2].clone(),
        })
        .collect();
    Ok(Database {
        name: name.to_owned(),
        tables,
        foreign_keys,
        routines,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        seen: Vec<String>,
        busy: bool,
    }

    impl MetaSource for Fake {
        fn rows(&mut self, sql: &str) -> Result<Vec<Vec<String>>, Error> {
            self.seen.push(sql.to_owned());
            let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
            Ok(match sql {
                TABLES => vec![s(&["dbo", "Guest", if self.busy { "9000" } else { "5" }])],
                COLUMNS => vec![
                    s(&["dbo", "Guest", "Id", "int", "4", "0", "1", "1"]),
                    s(&["dbo", "Guest", "Name", "nvarchar", "100", "1", "0", "2"]),
                ],
                INDEXES => vec![s(&["dbo", "Guest", "PK_Guest", "1", "1", "Id"])],
                FOREIGN_KEYS => vec![s(&["FK_a", "dbo", "Stay", "GuestId", "dbo", "Guest", "Id"])],
                ROUTINES => vec![s(&["dbo", "usp_CheckIn", "P"])],
                _ => vec![],
            })
        }
    }

    #[test]
    fn collects_structure_and_sends_only_catalog_queries() {
        let mut f = Fake {
            seen: vec![],
            busy: false,
        };
        let db = collect_database("FO", &mut f).unwrap();
        assert_eq!(db.tables[0].columns.len(), 2);
        assert_eq!(db.tables[0].indexes[0].columns, ["Id"]);
        assert_eq!(db.foreign_keys[0].to, ["dbo", "Guest", "Id"]);
        assert_eq!(db.routines[0].kind, "P");
        assert_eq!(f.seen.len(), 5);
        assert!(
            f.seen
                .iter()
                .all(|q| queries::ensure_catalog_only(q).is_ok())
        );
    }

    #[test]
    fn hash_ignores_row_counts_but_sees_structure() {
        let a = collect_database(
            "FO",
            &mut Fake {
                seen: vec![],
                busy: false,
            },
        )
        .unwrap();
        let mut b = collect_database(
            "FO",
            &mut Fake {
                seen: vec![],
                busy: true,
            },
        )
        .unwrap();
        assert_eq!(a.schema_hash(), b.schema_hash());
        b.tables[0].columns.pop();
        assert_ne!(a.schema_hash(), b.schema_hash());
        assert_eq!(a.schema_hash().len(), 64);
    }

    #[test]
    fn hash_has_no_delimiter_collisions() {
        let base = collect_database(
            "FO",
            &mut Fake {
                seen: vec![],
                busy: false,
            },
        )
        .unwrap();
        let mut one = base.clone();
        one.tables[0].indexes[0].columns = vec!["a,b".into()];
        let mut two = base.clone();
        two.tables[0].indexes[0].columns = vec!["a".into(), "b".into()];
        assert_ne!(one.schema_hash(), two.schema_hash());
        let mut t1 = base.clone();
        (t1.tables[0].schema, t1.tables[0].name) = ("a.b".into(), "c".into());
        let mut t2 = base;
        (t2.tables[0].schema, t2.tables[0].name) = ("a".into(), "b.c".into());
        assert_ne!(t1.schema_hash(), t2.schema_hash());
    }

    #[test]
    fn refuses_a_source_that_is_asked_for_data() {
        struct Never;
        impl MetaSource for Never {
            fn rows(&mut self, _: &str) -> Result<Vec<Vec<String>>, Error> {
                Ok(vec![vec!["x".into()]])
            }
        }
        assert!(matches!(
            collect_database("FO", &mut Never),
            Err(Error::Row(_))
        ));
        assert!(queries::ensure_catalog_only("SELECT * FROM dbo.Guest").is_err());
    }
}
