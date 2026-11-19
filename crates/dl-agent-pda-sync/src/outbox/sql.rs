//! Microsoft.Data.Sqlite-style named parameters over dl-sqlite, so the stores keep the C#
//! statements verbatim (`$s`, `$q`, ...). SQLite numbers named parameters in order of
//! their first appearance, which is the order [`ordered`] binds them in.

use std::path::Path;
use std::time::Duration;

use dl_sqlite::{Connection, OpenFlags, Row, ToSql, Value};

use super::error::{OutboxError, Result};

/// One named parameter: `("$s", &stream)`.
pub(crate) type Param<'a> = (&'a str, &'a dyn ToSql);

/// Microsoft.Data.Sqlite retries a busy database for the command timeout (30 s).
const BUSY_TIMEOUT: Duration = Duration::from_secs(30);

/// Opens a store like the C# constructors: creates the directory, opens (creating) the
/// file, then runs `PRAGMA journal_mode=WAL;`, `PRAGMA synchronous=FULL;` and `schema`.
pub(crate) fn open_store(path: &Path, schema: &[&str]) -> Result<Connection> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)?;
    }
    let db = Connection::open(path, OpenFlags::READ_WRITE_CREATE)?;
    db.busy_timeout(BUSY_TIMEOUT)?;
    db.execute_batch("PRAGMA journal_mode=WAL;")?;
    db.execute_batch("PRAGMA synchronous=FULL;")?;
    for script in schema {
        db.execute_batch(script)?;
    }
    Ok(db)
}

/// The distinct `$name` parameters of `sql` in order of first appearance.
fn names(sql: &str) -> Vec<&str> {
    let bytes = sql.as_bytes();
    let mut out: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            // `$` and the ASCII name are char boundaries.
            let name = &sql[start..i];
            if name.len() > 1 && !out.contains(&name) {
                out.push(name);
            }
        } else {
            i += 1;
        }
    }
    out
}

/// The parameter values in the positional order of `sql`.
fn ordered<'a>(sql: &str, params: &[Param<'a>]) -> Result<Vec<&'a dyn ToSql>> {
    names(sql)
        .into_iter()
        .map(|n| {
            params
                .iter()
                .find(|(name, _)| *name == n)
                .map(|(_, v)| *v)
                .ok_or_else(|| {
                    OutboxError::InvalidArgument(format!("Must add values for the parameter {n}."))
                })
        })
        .collect()
}

/// `ExecuteNonQuery`: rows changed.
pub(crate) fn exec(db: &Connection, sql: &str, params: &[Param<'_>]) -> Result<usize> {
    Ok(db.execute(sql, &ordered(sql, params)?)?)
}

/// `ExecuteReader` mapped row by row.
pub(crate) fn query<T>(
    db: &Connection,
    sql: &str,
    params: &[Param<'_>],
    f: impl FnMut(&Row<'_>) -> std::result::Result<T, dl_sqlite::Error>,
) -> Result<Vec<T>> {
    Ok(db.query(sql, &ordered(sql, params)?, f)?)
}

/// `ExecuteScalar`: the first column of the first row, `None` without rows.
pub(crate) fn scalar(db: &Connection, sql: &str, params: &[Param<'_>]) -> Result<Option<Value>> {
    let mut rows = query(db, sql, params, |r| r.get::<Value>(0))?;
    Ok(if rows.is_empty() {
        None
    } else {
        Some(rows.swap_remove(0))
    })
}

/// `Convert.ToInt64(ExecuteScalar())` for the counts and sums the stores read (NULL and
/// no row are 0).
pub(crate) fn scalar_i64(db: &Connection, sql: &str, params: &[Param<'_>]) -> Result<i64> {
    match scalar(db, sql, params)? {
        None | Some(Value::Null) => Ok(0),
        Some(Value::Integer(n)) => Ok(n),
        Some(Value::Real(f)) => Ok(f.round_ties_even() as i64),
        Some(Value::Text(s)) => s
            .trim()
            .parse::<i64>()
            .map_err(|_| OutboxError::Corrupt(format!("'{s}' is not an integer."))),
        Some(Value::Blob(_)) => Err(OutboxError::Corrupt(
            "A blob value is not an integer.".to_owned(),
        )),
    }
}

/// `ExecuteScalar() is long l ? l : 0`.
pub(crate) fn scalar_long_or_zero(db: &Connection, sql: &str, params: &[Param<'_>]) -> Result<i64> {
    Ok(match scalar(db, sql, params)? {
        Some(Value::Integer(n)) => n,
        _ => 0,
    })
}

/// `ExecuteScalar() as string`.
pub(crate) fn scalar_text(
    db: &Connection,
    sql: &str,
    params: &[Param<'_>],
) -> Result<Option<String>> {
    Ok(match scalar(db, sql, params)? {
        Some(Value::Text(s)) => Some(s),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::names;

    #[test]
    fn parameters_are_numbered_by_first_appearance() {
        assert_eq!(
            names("UPDATE t SET a=$t, b=$q WHERE s=$s AND x=$q"),
            ["$t", "$q", "$s"]
        );
        assert_eq!(names("SELECT 1"), Vec::<&str>::new());
        assert_eq!(names("VALUES($k0,$k1) LIMIT $n"), ["$k0", "$k1", "$n"]);
    }
}
