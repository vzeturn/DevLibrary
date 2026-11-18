//! The MM_SchemaVersion ledger: its table, the reads, the chain check against itself and
//! against a bundle, and adoption of a site that `migrate.mjs` migrated before.

use std::collections::{BTreeMap, HashMap};

use dl_tds::Value as V;

use super::{
    CODE_LEDGER_BEHIND, CODE_LEDGER_CORRUPT, CODE_LEDGER_DIVERGED, CODE_NEEDS_OPERATOR, MErr,
    db_err,
};
use crate::bundle::{self, Bundle, Manifest};
use crate::ctx::Ctx;
use crate::db::{Conn, Pool};
use crate::exec::SchemaVersion;
use crate::wire;

/// A row whose file finished.
pub const STATUS_APPLIED: &str = "applied";
/// A row committed before a file that runs outside a transaction; it becomes applied
/// when the file finishes.
pub const STATUS_RUNNING: &str = "running";
/// Written by the agent.
pub const SOURCE_AGENT: &str = "agent";
/// Seeded from MM_MigrationHistory (migrate.mjs) on the first run.
pub const SOURCE_ADOPTED: &str = "adopted";

/// Creates the ledger; idempotent.
pub const ENSURE_LEDGER_SQL: &str = "IF OBJECT_ID(N'dbo.MM_SchemaVersion', N'U') IS NULL
BEGIN
  CREATE TABLE dbo.MM_SchemaVersion (
    Version      bigint       NOT NULL CONSTRAINT PK_MM_SchemaVersion PRIMARY KEY,
    Name         varchar(200) NOT NULL,
    Sha256       char(64)     NOT NULL,
    Chain        char(64)     NOT NULL,
    Status       varchar(16)  NOT NULL
                   CONSTRAINT CK_MM_SchemaVersion_Status CHECK (Status IN ('applied', 'running')),
    InTx         bit          NOT NULL,
    Source       varchar(16)  NOT NULL
                   CONSTRAINT CK_MM_SchemaVersion_Source CHECK (Source IN ('agent', 'adopted')),
    BundleId     varchar(100) NOT NULL,
    AgentVersion varchar(32)  NOT NULL,
    AppliedAt    datetime2(3) NOT NULL
                   CONSTRAINT DF_MM_SchemaVersion_AppliedAt DEFAULT SYSUTCDATETIME(),
    DurationMs   int          NULL
  );
END;";

const READ_LEDGER_SQL: &str =
    "SELECT Version, Name, Sha256, Chain, Status FROM dbo.MM_SchemaVersion ORDER BY Version;";

/// The head for `hello.schema_version`: the highest applied version, 0 without a ledger.
pub const HEAD_SQL: &str = "IF OBJECT_ID(N'dbo.MM_SchemaVersion', N'U') IS NULL
    SELECT CAST(0 AS bigint);
ELSE
    SELECT ISNULL(MAX(Version), 0) FROM dbo.MM_SchemaVersion WHERE Status = 'applied';";

/// Inserts a ledger row.
pub const INSERT_ROW_SQL: &str = "INSERT INTO dbo.MM_SchemaVersion
  (Version, Name, Sha256, Chain, Status, InTx, Source, BundleId, AgentVersion, DurationMs)
VALUES (@Version, @Name, @Sha256, @Chain, @Status, @InTx, @Source, @BundleId, @AgentVersion, @DurationMs);";

/// Marks a running row applied.
pub const FINISH_ROW_SQL: &str = "UPDATE dbo.MM_SchemaVersion
SET Status = 'applied', AppliedAt = SYSUTCDATETIME(), DurationMs = @DurationMs, AgentVersion = @AgentVersion, BundleId = @BundleId
WHERE Version = @Version AND Status = 'running';";

/// Keeps MM_MigrationHistory (migrate.mjs) in step. The table is named only inside the
/// inner IF: SQL Server defers resolving a missing table to the statement's execution,
/// which never comes when the table is absent. Naming it in the outer IF's condition
/// fails with error 208 on a database without it.
pub const HISTORY_SQL: &str = "IF OBJECT_ID(N'dbo.MM_MigrationHistory', N'U') IS NOT NULL
  IF NOT EXISTS (SELECT 1 FROM dbo.MM_MigrationHistory WHERE FileName = @Name)
    INSERT INTO dbo.MM_MigrationHistory (FileName, Checksum, DurationMs) VALUES (@Name, @Sha256, @DurationMs);";

const READ_HISTORY_SQL: &str = "IF OBJECT_ID(N'dbo.MM_MigrationHistory', N'U') IS NULL
    SELECT CAST(NULL AS varchar(200)), CAST(NULL AS char(64)) WHERE 1 = 0;
ELSE
    SELECT FileName, Checksum FROM dbo.MM_MigrationHistory;";

const LEDGER_EXISTS_SQL: &str = "SELECT CAST(OBJECT_ID(N'dbo.MM_SchemaVersion', N'U') AS bigint);";

/// One ledger row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Row {
    /// Version.
    pub version: i64,
    /// File name.
    pub name: String,
    /// File sha256.
    pub sha256: String,
    /// Chain link.
    pub chain: String,
    /// applied or running.
    pub status: String,
}

fn text(v: Option<&V>) -> Result<String, MErr> {
    match v {
        Some(v) => v
            .convert::<String>()
            .map(|s| s.trim().to_owned())
            .map_err(|e| db_err(e.into())),
        None => Err(db_err(
            dl_tds::Error::ColumnNotFound("column".into()).into(),
        )),
    }
}

pub(super) fn read_ledger(ctx: &Ctx, c: &mut Conn<'_>) -> Result<Vec<Row>, MErr> {
    let r = c.batch(ctx, READ_LEDGER_SQL).map_err(db_err)?;
    let mut out = Vec::new();
    for row in r.rows() {
        let vals = row.values();
        out.push(Row {
            version: vals
                .first()
                .ok_or_else(|| db_err(dl_tds::Error::ColumnNotFound("Version".into()).into()))?
                .convert::<i64>()
                .map_err(|e| db_err(e.into()))?,
            name: text(vals.get(1))?,
            sha256: text(vals.get(2))?,
            chain: text(vals.get(3))?,
            status: text(vals.get(4))?,
        });
    }
    Ok(out)
}

pub(super) fn read_ledger_if_exists(ctx: &Ctx, c: &mut Conn<'_>) -> Result<(Vec<Row>, bool), MErr> {
    let r = c.batch(ctx, LEDGER_EXISTS_SQL).map_err(db_err)?;
    let exists = r
        .rows()
        .first()
        .and_then(|row| row.values().first())
        .is_some_and(|v| !v.is_null());
    if !exists {
        return Ok((Vec::new(), false));
    }
    Ok((read_ledger(ctx, c)?, true))
}

pub(super) fn read_history(ctx: &Ctx, c: &mut Conn<'_>) -> Result<HashMap<String, String>, MErr> {
    let r = c.batch(ctx, READ_HISTORY_SQL).map_err(db_err)?;
    let mut out = HashMap::new();
    for row in r.rows() {
        let vals = row.values();
        out.insert(text(vals.first())?, text(vals.get(1))?);
    }
    Ok(out)
}

/// The checked ledger: the last applied version, its chain value, every row by version,
/// and the "running" row after the head.
#[derive(Debug, Clone, Default)]
pub struct LedgerState {
    /// Last applied version.
    pub head: i64,
    /// Its chain value.
    pub head_chain: String,
    /// Rows by version.
    pub chains: BTreeMap<i64, Row>,
    /// The half-applied row.
    pub running: Option<Row>,
}

/// Recomputes the ledger's own chain from genesis: versions 1..n without gaps, every
/// row's chain following from the previous one, only the last row "running".
pub fn check_ledger(rows: &[Row]) -> Result<LedgerState, MErr> {
    let mut st = LedgerState {
        head_chain: bundle::GENESIS_CHAIN.to_owned(),
        ..LedgerState::default()
    };
    let mut prev = bundle::GENESIS_CHAIN.to_owned();
    for (i, r) in rows.iter().enumerate() {
        if r.version != i as i64 + 1 {
            return Err(MErr::new(
                CODE_LEDGER_CORRUPT,
                format!(
                    "ledger version {} follows {i} (gap or duplicate)",
                    r.version
                ),
            ));
        }
        if r.chain != bundle::chain_step(&prev, r.version, &r.name, &r.sha256) {
            return Err(MErr::new(
                CODE_LEDGER_CORRUPT,
                format!("ledger chain breaks at version {}", r.version),
            ));
        }
        match r.status.as_str() {
            STATUS_APPLIED => {
                if st.running.is_some() {
                    return Err(MErr::new(
                        CODE_LEDGER_CORRUPT,
                        format!(
                            "ledger version {} is applied after a running one",
                            r.version
                        ),
                    ));
                }
                st.head = r.version;
                st.head_chain = r.chain.clone();
            }
            STATUS_RUNNING => {
                if i != rows.len() - 1 {
                    return Err(MErr::new(
                        CODE_LEDGER_CORRUPT,
                        format!(
                            "ledger version {} is running but not the last row",
                            r.version
                        ),
                    ));
                }
                st.running = Some(r.clone());
            }
            _ => {
                return Err(MErr::new(
                    CODE_LEDGER_CORRUPT,
                    format!("ledger version {} has an unknown status", r.version),
                ));
            }
        }
        st.chains.insert(r.version, r.clone());
        prev = r.chain.clone();
    }
    Ok(st)
}

/// The ledger against the bundle: the chain at the bundle's start must be its prev_chain
/// and every version both hold must carry the same link.
pub fn compare(st: &LedgerState, m: &Manifest) -> Result<(), MErr> {
    if st.head < m.from_version {
        return Err(MErr::new(
            CODE_LEDGER_BEHIND,
            format!(
                "ledger head {} is before the bundle start {}: an earlier bundle must run first",
                st.head, m.from_version
            ),
        ));
    }
    let at = if m.from_version > 0 {
        st.chains
            .get(&m.from_version)
            .map(|r| r.chain.clone())
            .unwrap_or_default()
    } else {
        bundle::GENESIS_CHAIN.to_owned()
    };
    if at != m.prev_chain {
        return Err(MErr::new(
            CODE_LEDGER_DIVERGED,
            format!(
                "ledger chain at version {} differs from the bundle's prev_chain",
                m.from_version
            ),
        ));
    }
    for e in &m.migrations {
        let Some(r) = st.chains.get(&e.seq) else {
            break;
        };
        if r.chain != e.chain || r.name != e.name || r.sha256 != e.sha256 {
            return Err(MErr::new(
                CODE_LEDGER_DIVERGED,
                format!(
                    "ledger version {} differs from the bundle ({})",
                    e.seq, e.name
                ),
            ));
        }
    }
    Ok(())
}

/// The checksum migrate.mjs accepted for rows written from a CRLF checkout.
fn crlf_sha256(lf: &[u8]) -> String {
    let s = String::from_utf8_lossy(lf).replace('\n', "\r\n");
    crate::sha256_hex(s.as_bytes())
}

/// The bundle prefix migrate.mjs already applied (by MM_MigrationHistory) when the ledger
/// is empty. The history must be exactly a prefix of the bundle.
pub fn adoptable(history: &HashMap<String, String>, b: &Bundle) -> Result<usize, MErr> {
    if history.is_empty() {
        return Ok(0);
    }
    if b.manifest.from_version != 0 {
        return Err(MErr::new(
            CODE_NEEDS_OPERATOR,
            "the site was migrated by migrate.mjs; adopting it needs a full bundle (from_version 0)",
        ));
    }
    let mut n = 0;
    for m in &b.migrations {
        let Some(sum) = history.get(&m.entry.name) else {
            break;
        };
        if *sum != m.entry.sha256 && *sum != crlf_sha256(&m.sql) {
            return Err(MErr::new(
                CODE_LEDGER_DIVERGED,
                format!(
                    "MM_MigrationHistory checksum of {} differs from the bundle",
                    m.entry.name
                ),
            ));
        }
        n += 1;
    }
    if n != history.len() {
        return Err(MErr::new(
            CODE_LEDGER_DIVERGED,
            format!(
                "MM_MigrationHistory holds {} files but only the first {n} bundle files form a prefix",
                history.len()
            ),
        ));
    }
    Ok(n)
}

/// The [`SchemaVersion`] over the ledger head.
pub struct LedgerVersion {
    /// The MM_PMS pool (`None`: unknown).
    pub pool: Option<std::sync::Arc<Pool>>,
}

impl SchemaVersion for LedgerVersion {
    fn schema_version(&self, ctx: &Ctx) -> Result<i64, String> {
        let Some(pool) = &self.pool else {
            return Ok(wire::SCHEMA_VERSION_UNKNOWN);
        };
        let read = || -> Result<i64, String> {
            let mut c = pool.get(ctx).map_err(|e| e.to_string())?;
            let r = c.batch(ctx, HEAD_SQL).map_err(|e| e.to_string())?;
            r.rows()
                .first()
                .ok_or("no row")?
                .get::<i64>(0)
                .map_err(|e| e.to_string())
        };
        match read() {
            Ok(v) if v >= 0 => Ok(v),
            Ok(_) => Ok(wire::SCHEMA_VERSION_UNKNOWN),
            Err(e) => Err(format!("read MM_SchemaVersion: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(n: i64, running_last: bool) -> Vec<Row> {
        let mut prev = bundle::GENESIS_CHAIN.to_owned();
        (1..=n)
            .map(|v| {
                let name = format!("{v:03}_x.sql");
                let sha = crate::sha256_hex(name.as_bytes());
                let chain = bundle::chain_step(&prev, v, &name, &sha);
                prev = chain.clone();
                Row {
                    version: v,
                    name,
                    sha256: sha,
                    chain,
                    status: if running_last && v == n {
                        STATUS_RUNNING
                    } else {
                        STATUS_APPLIED
                    }
                    .into(),
                }
            })
            .collect()
    }

    /// The Go raw string `const <name> = `...`` in `go` (CRs dropped, as Go does).
    fn go_const(go: &str, name: &str) -> String {
        let start = go
            .find(&format!("const {name} = `"))
            .unwrap_or_else(|| panic!("{name} not in ledger.go"))
            + name.len()
            + 10;
        let len = go[start..].find('`').unwrap();
        go[start..start + len].replace('\r', "")
    }

    #[test]
    fn ledger_sql_is_the_go_agents() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/go/ledger.go");
        let go = std::fs::read_to_string(path).unwrap();
        for (name, sql) in [
            ("ensureLedgerSQL", ENSURE_LEDGER_SQL),
            ("readLedgerSQL", READ_LEDGER_SQL),
            ("headSQL", HEAD_SQL),
            ("insertRowSQL", INSERT_ROW_SQL),
            ("finishRowSQL", FINISH_ROW_SQL),
            ("historySQL", HISTORY_SQL),
            ("readHistorySQL", READ_HISTORY_SQL),
        ] {
            assert_eq!(sql, go_const(&go, name), "{name}");
        }
    }

    #[test]
    fn ledger_rules() {
        let st = check_ledger(&rows(3, false)).unwrap();
        assert_eq!(st.head, 3);
        let st = check_ledger(&rows(3, true)).unwrap();
        assert_eq!((st.head, st.running.unwrap().version), (2, 3));
        let mut gap = rows(3, false);
        gap.remove(1);
        assert_eq!(check_ledger(&gap).unwrap_err().code, CODE_LEDGER_CORRUPT);
        let mut bad = rows(2, false);
        bad[1].sha256 = "0".repeat(64);
        assert_eq!(check_ledger(&bad).unwrap_err().code, CODE_LEDGER_CORRUPT);
        let mut mid = rows(3, false);
        mid[1].status = STATUS_RUNNING.into();
        assert!(check_ledger(&mid).is_err());
    }
}
