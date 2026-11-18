//! Splitting a script into batches on `GO` lines, and the static checks on the code of
//! each batch (forbidden objects, statements that cannot run in a transaction).

use std::sync::OnceLock;

use crate::bundle::{Bundle, Entry};
use crate::minire::Regex;

/// One client batch: `text` is sent as is; `code` is the same text with comments blanked
/// out (strings kept), used for the scans; `line` is the 1-based line it starts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    /// The batch as sent.
    pub text: String,
    /// The batch with comments blanked.
    pub code: String,
    /// First line in the script.
    pub line: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum St {
    Code,
    LineComment,
    BlockComment,
    Str,
    Bracket,
    Quoted,
}

fn closer(st: St) -> u8 {
    match st {
        St::Bracket => b']',
        St::Quoted => b'"',
        _ => b'\'',
    }
}

/// Whether the line starting at `i` is a GO separator, and where its content ends (the
/// index of its newline, or the end).
fn go_line(s: &str, i: usize) -> (usize, bool) {
    let end = s[i..].find('\n').map_or(s.len(), |e| e + i);
    let mut l = s[i..end].trim_end_matches('\r');
    if let Some(k) = l.find("--") {
        l = &l[..k];
    }
    (end, l.trim().eq_ignore_ascii_case("GO"))
}

/// Splits on GO lines the way sqlcmd does: a line holding only GO (any case, optional
/// blanks, optional trailing `--` comment) that starts outside a string, a quoted or
/// bracketed identifier and a comment. Batches whose code is blank are dropped.
pub fn split_batches(script: &str) -> Vec<Batch> {
    let b = script.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut text: Vec<u8> = Vec::new();
    let mut code: Vec<u8> = Vec::new();
    let (mut st, mut depth) = (St::Code, 0usize);
    let (mut line, mut batch_line) = (1usize, 1usize);
    let flush = |text: &mut Vec<u8>, code: &mut Vec<u8>, out: &mut Vec<Batch>, at: usize| {
        let c = String::from_utf8_lossy(code).into_owned();
        if !c.trim().is_empty() {
            out.push(Batch {
                text: String::from_utf8_lossy(text).trim().to_owned(),
                code: c,
                line: at,
            });
        }
        text.clear();
        code.clear();
    };
    let mut i = 0;
    while i < n {
        let at_line_start = i == 0 || b[i - 1] == b'\n';
        if st == St::Code && at_line_start {
            let (mut end, is_go) = go_line(script, i);
            if is_go {
                flush(&mut text, &mut code, &mut out, batch_line);
                if end < n && b[end] == b'\n' {
                    end += 1;
                }
                line += 1;
                i = end;
                batch_line = line;
                continue;
            }
        }
        let c = b[i];
        let next = if i + 1 < n { b[i + 1] } else { 0 };
        match st {
            St::Code => {
                if c == b'-' && next == b'-' {
                    st = St::LineComment;
                    text.extend_from_slice(b"--");
                    code.extend_from_slice(b"  ");
                    i += 2;
                    continue;
                }
                if c == b'/' && next == b'*' {
                    st = St::BlockComment;
                    depth = 1;
                    text.extend_from_slice(b"/*");
                    code.extend_from_slice(b"  ");
                    i += 2;
                    continue;
                }
                match c {
                    b'\'' => st = St::Str,
                    b'[' => st = St::Bracket,
                    b'"' => st = St::Quoted,
                    _ => {}
                }
                text.push(c);
                code.push(c);
            }
            St::LineComment => {
                if c == b'\n' {
                    st = St::Code;
                    code.push(b'\n');
                } else {
                    code.push(b' ');
                }
                text.push(c);
            }
            St::BlockComment => {
                if c == b'/' && next == b'*' {
                    depth += 1;
                    text.extend_from_slice(b"/*");
                    code.extend_from_slice(b"  ");
                    i += 2;
                    continue;
                }
                if c == b'*' && next == b'/' {
                    depth -= 1;
                    text.extend_from_slice(b"*/");
                    code.extend_from_slice(b"  ");
                    i += 2;
                    if depth == 0 {
                        st = St::Code;
                    }
                    continue;
                }
                text.push(c);
                code.push(if c == b'\n' { b'\n' } else { b' ' });
            }
            St::Str | St::Bracket | St::Quoted => {
                let cl = closer(st);
                text.push(c);
                code.push(c);
                if c == cl {
                    if next == cl {
                        // Doubled: an escaped closer, still inside.
                        text.push(next);
                        code.push(next);
                        i += 2;
                        continue;
                    }
                    st = St::Code;
                }
            }
        }
        if c == b'\n' {
            line += 1;
        }
        i += 1;
    }
    flush(&mut text, &mut code, &mut out, batch_line);
    out
}

struct Rule {
    name: &'static str,
    re: Regex,
}

/// Objects and statements no migration may reach, matched on code with comments removed
/// (string literals kept, so dynamic SQL naming them is caught too).
fn forbidden() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        [
            ("SMLExecRsSql", r"(?i)\bSMLExecRsSql\b"),
            ("ClearDatabase", r"(?i)\bClearDatabase\b"),
            ("extended procedure", r"(?i)\bxp_\w+"),
            ("OLE automation", r"(?i)\bsp_OA\w+"),
            ("sp_configure", r"(?i)\bsp_configure\b|\bRECONFIGURE\b"),
            ("ad hoc remote source", r"(?i)\bOPEN(ROWSET|DATASOURCE)\b"),
            ("linked server", r"(?i)\bsp_addlinkedserver\b"),
            (
                "backup or restore",
                r"(?i)\b(BACKUP|RESTORE)\s+(DATABASE|LOG)\b",
            ),
            ("database DDL", r"(?i)\b(CREATE|DROP|ALTER)\s+DATABASE\b"),
            (
                "login DDL",
                r"(?i)\b(CREATE|ALTER|DROP)\s+LOGIN\b|\bALTER\s+SERVER\b",
            ),
            ("SHUTDOWN", r"(?i)(^|;|\n)\s*SHUTDOWN\b"),
        ]
        .into_iter()
        .map(|(name, p)| Rule {
            name,
            // The patterns are constants covered by the tests below.
            re: Regex::new(p).unwrap_or_else(|e| panic!("{name}: {e}")),
        })
        .collect()
    })
}

fn no_tx() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(CREATE|ALTER|DROP)\s+FULLTEXT\b").unwrap_or_else(|e| panic!("{e}"))
    })
}

/// One forbidden reference: the migration and the rule, never the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Version.
    pub seq: i64,
    /// File name.
    pub name: String,
    /// The rule.
    pub rule: &'static str,
    /// Line in the file.
    pub line: usize,
}

/// The forbidden references in a verified bundle (empty: it may run).
pub fn scan(b: &Bundle) -> Vec<Finding> {
    let mut out = Vec::new();
    for m in &b.migrations {
        for batch in split_batches(&String::from_utf8_lossy(&m.sql)) {
            for f in forbidden() {
                if let Some(at) = f.re.find_start(&batch.code) {
                    out.push(Finding {
                        seq: m.entry.seq,
                        name: m.entry.name.clone(),
                        rule: f.name,
                        line: batch.line + batch.code[..at].matches('\n').count(),
                    });
                }
            }
        }
    }
    out
}

/// Whether a migration runs inside one transaction: not when the signed `online_index`
/// flag is set, nor when a batch holds a statement SQL Server refuses in a transaction.
pub fn runs_in_tx(e: &Entry, batches: &[Batch]) -> bool {
    !e.online_index && !batches.iter().any(|b| no_tx().is_match(&b.code))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(s: &str) -> Vec<(String, usize)> {
        split_batches(s)
            .into_iter()
            .map(|b| (b.text, b.line))
            .collect()
    }

    #[test]
    fn splits_like_sqlcmd() {
        assert_eq!(
            texts("CREATE TABLE a (x int);\nGO\n\nCREATE PROC p AS SELECT 1\n go -- end\nSELECT 2"),
            vec![
                ("CREATE TABLE a (x int);".to_owned(), 1),
                ("CREATE PROC p AS SELECT 1".to_owned(), 3),
                ("SELECT 2".to_owned(), 6),
            ]
        );
        // GO inside a block comment or a multi-line string does not split.
        assert_eq!(texts("/* x\nGO\n*/ SELECT 1").len(), 1);
        assert_eq!(texts("SELECT 'a\nGO\nb'").len(), 1);
        assert_eq!(texts("SELECT [a\nGO\n]").len(), 1);
        assert_eq!(texts("SELECT 'it''s'\nGO\nSELECT 2").len(), 2);
        // Comment-only batches are dropped; GOTO is not GO.
        assert_eq!(
            texts("-- only a comment\nGO\nGOTO x"),
            vec![("GOTO x".to_owned(), 3)]
        );
        assert_eq!(texts("SELECT 1\r\nGO\r\nSELECT 2\r\n").len(), 2);
        let b = &split_batches("SELECT 1 -- note\n/* a */ SELECT 'x--y'")[0];
        assert_eq!(b.code, "SELECT 1        \n        SELECT 'x--y'");
    }

    #[test]
    fn rules_compile_and_find_lines() {
        assert_eq!(forbidden().len(), 11);
        let rule = |code: &str| {
            forbidden()
                .iter()
                .find(|r| r.re.is_match(code))
                .map(|r| r.name)
        };
        assert_eq!(
            rule(concat!("EXEC master..xp_", "cmdshell 'dir'")),
            Some("extended procedure")
        );
        assert_eq!(rule("EXEC sp_OACreate 'x'"), Some("OLE automation"));
        assert_eq!(
            rule("BACKUP  DATABASE x TO DISK='y'"),
            Some("backup or restore")
        );
        assert_eq!(rule("ALTER DATABASE CURRENT SET x"), Some("database DDL"));
        assert_eq!(rule("ALTER SERVER CONFIGURATION"), Some("login DDL"));
        assert_eq!(rule("select 1;shutdown"), Some("SHUTDOWN"));
        assert_eq!(rule("CREATE TABLE BackupLog (x int)"), None);
        assert_eq!(rule("SELECT xp FROM t"), None);
        let e = Entry::default();
        assert!(!runs_in_tx(
            &e,
            &split_batches("CREATE FULLTEXT INDEX ON t(x) KEY INDEX pk")
        ));
        assert!(runs_in_tx(&e, &split_batches("CREATE INDEX ix ON t(x)")));
    }
}
