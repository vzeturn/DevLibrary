//! Installs the collection procedures into SMILE_FO / SMILE_POS: a port of
//! `installsql.go` and `installsql_fo.go`. The scripts are copies of the files the Go agent
//! embeds (MM-EINVOICE `db/agent-sql/collect/*.sql`), kept here in `sql/collect/`.

use std::time::Duration;

use dl_tds::{Client, Config};

/// The embedded scripts in deploy order (byte-wise sorted names, as Go's `sort.Strings`).
pub const SCRIPTS: &[(&str, &str)] = &[
    (
        "00_marker_table.sql",
        include_str!("../sql/collect/00_marker_table.sql"),
    ),
    (
        "01_source_hash.sql",
        include_str!("../sql/collect/01_source_hash.sql"),
    ),
    (
        "sp_FetchFO.sql",
        include_str!("../sql/collect/sp_FetchFO.sql"),
    ),
    (
        "sp_FetchPOS.sql",
        include_str!("../sql/collect/sp_FetchPOS.sql"),
    ),
    (
        "sp_ListTrnTypes.sql",
        include_str!("../sql/collect/sp_ListTrnTypes.sql"),
    ),
    (
        "sp_MarkCollected.sql",
        include_str!("../sql/collect/sp_MarkCollected.sql"),
    ),
    (
        "sp_ProbeFOSchema.sql",
        include_str!("../sql/collect/sp_ProbeFOSchema.sql"),
    ),
    (
        "sp_SelectPendingFO.sql",
        include_str!("../sql/collect/sp_SelectPendingFO.sql"),
    ),
    (
        "sp_SelectPendingPOS.sql",
        include_str!("../sql/collect/sp_SelectPendingPOS.sql"),
    ),
];

const INSTALL_TIMEOUT: Duration = Duration::from_secs(60);

const FO_STATIC_PRED: &str = "/*{{FO_STATIC_PRED}}*/1 = 1";
const FO_GRP_EXPR: &str = "/*{{FO_GRP_EXPR}}*/''";
const FO_HK_EXPR: &str = "/*{{FO_HK_EXPR}}*/'0'";
const FO_RATE_EXPR: &str = "/*{{FO_RATE_EXPR}}*/''";
const FO_RATE_JOIN: &str = "/*{{FO_RATE_JOIN}}*/";

/// The optional FO columns `sp_FetchFO` probes at run time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FoProbe {
    /// `TransactionCode.TransactionGroup` exists.
    pub has_trn_group: bool,
    /// `FolioTransaction.FolioBalanceCode` exists.
    pub has_folio_balance: bool,
    /// `FolioTransaction.isHKPost` exists.
    pub has_is_hk_post: bool,
    /// `TaxDef.TaxRate` exists.
    pub has_tax_rate: bool,
}

const PROBE_SQL: &str = "SELECT
		CAST(CASE WHEN COL_LENGTH('dbo.TransactionCode', 'TransactionGroup') IS NOT NULL THEN 1 ELSE 0 END AS bit),
		CAST(CASE WHEN COL_LENGTH('dbo.FolioTransaction', 'FolioBalanceCode') IS NOT NULL THEN 1 ELSE 0 END AS bit),
		CAST(CASE WHEN COL_LENGTH('dbo.FolioTransaction', 'isHKPost') IS NOT NULL THEN 1 ELSE 0 END AS bit),
		CAST(CASE WHEN OBJECT_ID('dbo.TaxDef') IS NOT NULL AND COL_LENGTH('dbo.TaxDef', 'TaxRate') IS NOT NULL THEN 1 ELSE 0 END AS bit)";

/// `foStaticPredicate`.
fn static_predicate(p: FoProbe) -> &'static str {
    if p.has_trn_group && p.has_folio_balance {
        "EXISTS (SELECT 1 FROM dbo.TransactionCode tcp WHERE tcp.TransactionCode = ft.TransactionCode AND tcp.TransactionGroup = 1) AND ft.FolioBalanceCode NOT IN ('P', 'V', 'C')"
    } else {
        "ft.TransactionCode IN ('110', '150', '160', '170', '200', '201', '210', '211', '220', '221', '230', '240')"
    }
}

/// `substituteFOTokens`.
pub fn substitute_fo_tokens(body: &str, p: FoProbe) -> String {
    let grp = if p.has_trn_group {
        "ISNULL(RTRIM(CAST(tc.TransactionGroup AS varchar(20))), '')"
    } else {
        "''"
    };
    let hk = if p.has_is_hk_post {
        "CAST(CAST(ISNULL(ft.isHKPost, 0) AS bit) AS varchar(1))"
    } else {
        "'0'"
    };
    let (rate, join) = if p.has_tax_rate {
        (
            "ISNULL(RTRIM(CAST(td.TaxRate AS varchar(20))), '')",
            "LEFT JOIN dbo.TaxDef td ON td.TaxCode = ft.TaxCode",
        )
    } else {
        ("''", "")
    };
    body.replace(FO_STATIC_PRED, static_predicate(p))
        .replace(FO_GRP_EXPR, grp)
        .replace(FO_HK_EXPR, hk)
        .replace(FO_RATE_EXPR, rate)
        .replace(FO_RATE_JOIN, join)
}

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{c}' | '\r')
}

/// Splits a script on lines holding only `GO` (`(?im)^\s*GO\s*$`), dropping batches that
/// are only whitespace.
pub fn split_batches(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in body.split_inclusive('\n') {
        let t = line.trim_matches(is_space);
        if t.eq_ignore_ascii_case("go") {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push_str(line);
        }
    }
    out.push(cur);
    out.into_iter()
        .filter(|b| !b.trim_matches(is_space).is_empty())
        .collect()
}

/// Finds `CREATE\s+OR\s+ALTER\s+(FUNCTION|PROCEDURE)\s+(\S+)` (case-insensitive) at or
/// after `from`: (start, end, object type as written, name).
fn find_create_or_alter(b: &str, from: usize) -> Option<(usize, usize, String, String)> {
    let lower = b.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let ws = |p: usize| p < bytes.len() && matches!(bytes[p], b' ' | b'\t' | b'\n' | 0x0c | b'\r');
    let mut search = from;
    while let Some(rel) = lower[search..].find("create") {
        let start = search + rel;
        search = start + 1;
        let mut p = start + 6;
        let skip_ws = |mut p: usize| -> Option<usize> {
            let s = p;
            while ws(p) {
                p += 1;
            }
            (p > s).then_some(p)
        };
        let Some(q) = skip_ws(p) else { continue };
        p = q;
        if !lower[p..].starts_with("or") {
            continue;
        }
        let Some(q) = skip_ws(p + 2) else { continue };
        p = q;
        if !lower[p..].starts_with("alter") {
            continue;
        }
        let Some(q) = skip_ws(p + 5) else { continue };
        p = q;
        let kind_len = if lower[p..].starts_with("function") {
            8
        } else if lower[p..].starts_with("procedure") {
            9
        } else {
            continue;
        };
        let kind = b[p..p + kind_len].to_owned();
        let Some(q) = skip_ws(p + kind_len) else {
            continue;
        };
        p = q;
        let name_start = p;
        while p < bytes.len() && !ws(p) {
            p += 1;
        }
        if p == name_start {
            continue;
        }
        // \S+ is greedy over non-whitespace characters; the end must sit on a char
        // boundary, which it does because whitespace is ASCII.
        return Some((start, p, kind, b[name_start..p].to_owned()));
    }
    None
}

/// The `CREATE OR ALTER` rewrite for SQL Server 2016 RTM: returns the stub batch to run
/// first and the batch with every header occurrence replaced by `ALTER <type> <name>`.
pub fn rewrite_create_or_alter(batch: &str) -> Option<(String, String)> {
    let (_, _, kind, name) = find_create_or_alter(batch, 0)?;
    let upper = kind.to_ascii_uppercase();
    let is_fn = kind.eq_ignore_ascii_case("FUNCTION");
    let params = if is_fn { "(@x varchar(1))" } else { "" };
    let body = if is_fn {
        "RETURNS varbinary(32) AS BEGIN RETURN NULL END"
    } else {
        "AS BEGIN RETURN END"
    };
    let stub =
        format!("IF OBJECT_ID('{name}') IS NULL EXEC('CREATE {upper} {name} {params} {body}')");
    let mut out = String::with_capacity(batch.len());
    let mut from = 0;
    while let Some((s, e, _, _)) = find_create_or_alter(batch, from) {
        out.push_str(&batch[from..s]);
        out.push_str(&format!("ALTER {kind} {name}"));
        from = e;
    }
    out.push_str(&batch[from..]);
    Some((stub, out))
}

fn run_script(c: &mut Client, name: &str, body: &str) -> Result<(), String> {
    for (i, batch) in split_batches(body).iter().enumerate() {
        let mut batch = batch.clone();
        if let Some((stub, rewritten)) = rewrite_create_or_alter(&batch) {
            c.batch(&stub)
                .map_err(|e| format!("agent: ensure stub for {name}: {e}"))?;
            batch = rewritten;
        }
        if let Err(e) = c.batch(&batch) {
            return Err(match &e {
                dl_tds::Error::Server {
                    number,
                    state,
                    class,
                    line,
                    ..
                } => format!(
                    "agent: {name} batch #{} line {line} (error {number}, state {state}, class {class}): {e}",
                    i + 1
                ),
                _ => format!("agent: {name} batch #{}: {e}", i + 1),
            });
        }
    }
    Ok(())
}

/// `InstallCollectSQLString`: installs every script into the database of `dsn`.
pub fn install(dsn: &str) -> Result<(), String> {
    let cfg: Config = dsn
        .parse()
        .map_err(|e| format!("agent: open for SQL install: {e}"))?;
    let mut c = Client::connect(&cfg.with_connect_timeout(INSTALL_TIMEOUT))
        .map_err(|e| format!("agent: install collect SQL: {e}"))?;
    c.set_timeout(Some(INSTALL_TIMEOUT))
        .map_err(|e| e.to_string())?;
    install_into(&mut c).map_err(|e| format!("agent: install collect SQL: {e}"))?;
    c.close();
    Ok(())
}

/// `installCollectSQLInto`: installs every embedded script into the open connection's
/// database, in deploy order. Idempotent. A database on SQL Server 2008, or at a
/// compatibility level below 110, gets the 2008 rewrite ([`crate::legacy`]), applied
/// after the FO tokens are resolved and before the `CREATE OR ALTER` stub rewrite.
pub fn install_into(c: &mut Client) -> Result<(), String> {
    let legacy = crate::legacy::needs_legacy_sql(c)?;
    if legacy {
        crate::log::info(
            "agent: SQL Server older than 2012 (or compatibility level below 110): installing the 2008-compatible procedures",
            &[],
        );
    }
    for (name, body) in SCRIPTS {
        let mut body = (*body).to_owned();
        if body.contains(FO_STATIC_PRED) {
            let rs = c
                .query(PROBE_SQL, &[])
                .map_err(|e| format!("agent: probe FO schema for hash templating: {e}"))?;
            let r = rs
                .rows()
                .first()
                .ok_or("agent: probe FO schema for hash templating: no row")?;
            let g = |i: usize| {
                r.get::<bool>(i)
                    .map_err(|e| format!("agent: probe FO schema for hash templating: {e}"))
            };
            let p = FoProbe {
                has_trn_group: g(0)?,
                has_folio_balance: g(1)?,
                has_is_hk_post: g(2)?,
                has_tax_rate: g(3)?,
            };
            body = substitute_fo_tokens(&body, p);
        }
        if legacy {
            body = crate::legacy::legacy_sql(name, &body)?;
        }
        run_script(c, name, &body)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_list_matches_the_directory() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/sql/collect");
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.ends_with(".sql"))
            .collect();
        names.sort();
        let ours: Vec<&str> = SCRIPTS.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ours, "a script was added or removed: update SCRIPTS");
    }

    #[test]
    fn go_splitting() {
        let s = "-- c\nSELECT 1\nGO\n  go  \r\nSELECT 2\nGOTO x\nGO";
        assert_eq!(
            split_batches(s),
            vec!["-- c\nSELECT 1\n", "SELECT 2\nGOTO x\n"]
        );
        assert!(split_batches("GO\n\nGO\n").is_empty());
    }

    #[test]
    fn create_or_alter_rewrite() {
        let b =
            "-- CREATE header below\nCREATE  OR\n ALTER PROCEDURE dbo.sp_X\n@ID int AS SELECT 1";
        let (stub, out) = rewrite_create_or_alter(b).unwrap();
        assert_eq!(
            stub,
            "IF OBJECT_ID('dbo.sp_X') IS NULL EXEC('CREATE PROCEDURE dbo.sp_X  AS BEGIN RETURN END')"
        );
        assert_eq!(
            out,
            "-- CREATE header below\nALTER PROCEDURE dbo.sp_X\n@ID int AS SELECT 1"
        );
        let (stub, _) = rewrite_create_or_alter("create or alter function dbo.F(@a int)").unwrap();
        assert_eq!(
            stub,
            "IF OBJECT_ID('dbo.F(@a') IS NULL EXEC('CREATE FUNCTION dbo.F(@a (@x varchar(1)) RETURNS varbinary(32) AS BEGIN RETURN NULL END')"
        );
        assert!(rewrite_create_or_alter("ALTER PROCEDURE x").is_none());
    }

    #[test]
    fn every_embedded_script_with_tokens_resolves() {
        for (name, body) in SCRIPTS {
            for p in [
                FoProbe::default(),
                FoProbe {
                    has_trn_group: true,
                    has_folio_balance: true,
                    has_is_hk_post: true,
                    has_tax_rate: true,
                },
            ] {
                let out = substitute_fo_tokens(body, p);
                // A comment may mention a token name; the token+default pairs must all go.
                for t in [FO_STATIC_PRED, FO_GRP_EXPR, FO_HK_EXPR, FO_RATE_EXPR] {
                    assert!(!out.contains(t), "{name}: unresolved {t}");
                }
                if body.contains(FO_STATIC_PRED) {
                    assert!(
                        !out.contains("/*{{FO_RATE_JOIN}}*/"),
                        "{name}: unresolved join"
                    );
                }
            }
        }
    }
}
