//! The only statements the collector may send. Each reads `sys.*` catalog views and returns
//! text columns, so the transport needs no type handling.

use crate::collect::Error;

/// `schema, table, estimated rows`.
pub const TABLES: &str = "SELECT CAST(s.name AS nvarchar(128)), CAST(t.name AS nvarchar(128)), \
    CAST(ISNULL(SUM(p.rows), 0) AS nvarchar(32)) FROM sys.tables t \
    JOIN sys.schemas s ON s.schema_id = t.schema_id \
    LEFT JOIN sys.partitions p ON p.object_id = t.object_id AND p.index_id IN (0, 1) \
    WHERE t.is_ms_shipped = 0 GROUP BY s.name, t.name ORDER BY s.name, t.name";

/// `schema, table, column, type, max_length, nullable, identity, ordinal`.
pub const COLUMNS: &str = "SELECT CAST(s.name AS nvarchar(128)), CAST(t.name AS nvarchar(128)), \
    CAST(c.name AS nvarchar(128)), CAST(ty.name AS nvarchar(128)) + '(' + CAST(c.precision AS nvarchar(4)) + ',' + CAST(c.scale AS nvarchar(4)) + ')', CAST(c.max_length AS nvarchar(8)), \
    CAST(c.is_nullable AS nvarchar(1)), CAST(c.is_identity AS nvarchar(1)), CAST(c.column_id AS nvarchar(8)) \
    FROM sys.columns c JOIN sys.tables t ON t.object_id = c.object_id \
    JOIN sys.schemas s ON s.schema_id = t.schema_id \
    JOIN sys.types ty ON ty.user_type_id = c.user_type_id \
    WHERE t.is_ms_shipped = 0 ORDER BY s.name, t.name, c.column_id";

/// `schema, table, index, primary key, unique, columns (comma list in key order)`.
pub const INDEXES: &str = "SELECT CAST(s.name AS nvarchar(128)), CAST(t.name AS nvarchar(128)), \
    CAST(i.name AS nvarchar(128)), CAST(i.is_primary_key AS nvarchar(1)), CAST(i.is_unique AS nvarchar(1)), \
    CAST(c.name AS nvarchar(128)) + CASE WHEN ic.is_descending_key = 1 THEN ' DESC' ELSE '' END FROM sys.indexes i \
    JOIN sys.tables t ON t.object_id = i.object_id JOIN sys.schemas s ON s.schema_id = t.schema_id \
    JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id \
    JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
    WHERE t.is_ms_shipped = 0 AND i.name IS NOT NULL AND ic.is_included_column = 0 \
    ORDER BY s.name, t.name, i.name, ic.key_ordinal";

/// `fk name, schema, table, column, referenced schema, referenced table, referenced column`.
pub const FOREIGN_KEYS: &str = "SELECT CAST(f.name AS nvarchar(128)), CAST(ps.name AS nvarchar(128)), \
    CAST(pt.name AS nvarchar(128)), CAST(pc.name AS nvarchar(128)), CAST(rs.name AS nvarchar(128)), \
    CAST(rt.name AS nvarchar(128)), CAST(rc.name AS nvarchar(128)) FROM sys.foreign_keys f \
    JOIN sys.foreign_key_columns fc ON fc.constraint_object_id = f.object_id \
    JOIN sys.tables pt ON pt.object_id = f.parent_object_id JOIN sys.schemas ps ON ps.schema_id = pt.schema_id \
    JOIN sys.columns pc ON pc.object_id = fc.parent_object_id AND pc.column_id = fc.parent_column_id \
    JOIN sys.tables rt ON rt.object_id = f.referenced_object_id JOIN sys.schemas rs ON rs.schema_id = rt.schema_id \
    JOIN sys.columns rc ON rc.object_id = fc.referenced_object_id AND rc.column_id = fc.referenced_column_id \
    ORDER BY ps.name, f.name, fc.constraint_column_id";

/// `schema, name, kind` for views (`V`), procedures (`P`), functions (`FN`, `IF`, `TF`) and
/// triggers (`TR`). Names only: no definition text is read.
pub const ROUTINES: &str = "SELECT CAST(s.name AS nvarchar(128)), CAST(o.name AS nvarchar(128)), \
    CAST(RTRIM(o.type) AS nvarchar(4)) FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id \
    WHERE o.is_ms_shipped = 0 AND o.type IN ('V', 'P', 'FN', 'IF', 'TF', 'TR') ORDER BY s.name, o.name";

/// Every statement the collector sends.
pub const ALL: [&str; 5] = [TABLES, COLUMNS, INDEXES, FOREIGN_KEYS, ROUTINES];

/// Refuses any statement that is not byte-for-byte one of [`ALL`]. An allowlist of the
/// fixed statements cannot be bypassed by comments, commas, brackets, functions or chaining.
pub fn ensure_catalog_only(sql: &str) -> Result<(), Error> {
    if ALL.contains(&sql) {
        Ok(())
    } else {
        Err(Error::Refused(
            "statement is not one of the fixed catalog queries".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_collector_query_is_catalog_only() {
        for q in ALL {
            ensure_catalog_only(q).unwrap_or_else(|e| panic!("{e}: {q}"));
        }
    }

    #[test]
    fn user_table_reads_are_refused() {
        for q in [
            "SELECT * FROM dbo.Guest",
            "SELECT * FROM sys.tables t JOIN dbo.Guest g ON g.id = t.object_id",
            "SELECT name FROM sys.tables; SELECT * FROM sys.tables",
            "DELETE FROM sys.tables",
            "SELECT 1",
            "SELECT * FROM sys.tables, dbo.Guest",
            "SELECT 1 FROM sys.objects UPDATE dbo.Guest SET a = 1",
            "SELECT * FROM sys.fn_dblog(NULL, NULL)",
        ] {
            assert!(ensure_catalog_only(q).is_err(), "{q}");
        }
    }
}
