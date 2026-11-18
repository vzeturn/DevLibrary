//! SQL Server 2008 / 2008 R2 support: a port of `installsql_legacy.go`.
//!
//! The embedded collect scripts are written for SQL Server 2012+: they use `TRY_CONVERT`,
//! `CONCAT` and `HASHBYTES('SHA2_256')`. None of those exist on 2008, and `TRY_CONVERT` is
//! also rejected by a newer server whose database runs at compatibility level 100. For
//! such a database the installer rewrites the scripts into equivalent 2008 T-SQL before
//! executing them. A 2012+ database never takes this path, so its installed text (and
//! every stored `MMEI_Collected.SourceHash`) stays exactly as before.
//!
//! The rewrite must be byte-identical to Go's: the rewritten text decides the stored
//! fingerprints, so a site that switches binaries and re-enrolls would otherwise
//! re-collect every bill. `tests/legacy_golden.rs` compares it with vectors exported from
//! the Go code. The Go patterns are matched by hand here (no regex crate); RE2's `\s` is
//! ASCII space, `\t`, `\n`, `\f`, `\r`, `\w` is ASCII `[0-9A-Za-z_]`, `\b` is an ASCII word
//! boundary, and `(?i)` folds ASCII case (the scripts are ASCII).

use dl_tds::Client;

/// SQL Server 2012 (11.x), the first version that has every function the scripts use.
pub const MIN_MODERN_SQL_MAJOR: i32 = 11;
/// SQL Server 2012's compatibility level.
pub const MODERN_COMPAT_LEVEL: i32 = 110;

const VERSION_SQL: &str = "SELECT CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(128)),
		CAST((SELECT compatibility_level FROM sys.databases WHERE database_id = DB_ID()) AS int)";

/// `needsLegacySQL`: an engine older than 2012, or a database at a pre-2012 compatibility
/// level. An unparsable version is an error, not "modern".
pub fn needs_legacy_sql(c: &mut Client) -> Result<bool, String> {
    let ctx = |e: &dyn std::fmt::Display| format!("agent: read SQL Server version: {e}");
    let rs = c.query(VERSION_SQL, &[]).map_err(|e| ctx(&e))?;
    let row = rs.rows().first().ok_or_else(|| ctx(&"no row"))?;
    let version: String = row.get(0).map_err(|e| ctx(&e))?;
    let compat: i32 = row.get(1).map_err(|e| ctx(&e))?;
    let major = sql_major_version(&version)?;
    Ok(major < MIN_MODERN_SQL_MAJOR || compat < MODERN_COMPAT_LEVEL)
}

/// `sqlMajorVersion`: the major number of `SERVERPROPERTY('ProductVersion')`
/// (`"10.50.1600.1"` is 10).
pub fn sql_major_version(version: &str) -> Result<i32, String> {
    let t = version.trim();
    let head = t.split_once('.').map_or(t, |(h, _)| h);
    // strconv.Atoi parses a 64-bit int with an optional sign, like i64::from_str.
    let n = head.parse::<i64>().map_err(|e| {
        let why = match e.kind() {
            std::num::IntErrorKind::PosOverflow | std::num::IntErrorKind::NegOverflow => {
                "value out of range"
            }
            _ => "invalid syntax",
        };
        format!(
            "agent: unrecognised SQL Server version {}: strconv.Atoi: parsing {}: {why}",
            go_quote(version),
            go_quote(head),
        )
    })?;
    // Any real major version fits; a huge one is still "modern".
    Ok(i32::try_from(n).unwrap_or(if n < 0 { i32::MIN } else { i32::MAX }))
}

/// Go's `%q` for the ASCII text this module quotes in errors.
pub fn go_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{7}' => out.push_str("\\a"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{b}' => out.push_str("\\v"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0c | b'\r')
}

fn is_word(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphanumeric()
}

fn skip_spaces(b: &[u8], mut p: usize) -> usize {
    while p < b.len() && is_space(b[p]) {
        p += 1;
    }
    p
}

fn starts_ci(b: &[u8], p: usize, lit: &str) -> bool {
    b.len() >= p + lit.len() && b[p..p + lit.len()].eq_ignore_ascii_case(lit.as_bytes())
}

/// `(@\w+)` at `p`: the end of the variable.
fn variable(b: &[u8], p: usize) -> Option<usize> {
    if b.get(p) != Some(&b'@') {
        return None;
    }
    let mut q = p + 1;
    while q < b.len() && is_word(b[q]) {
        q += 1;
    }
    (q > p + 1).then_some(q)
}

/// `NAME\(\s*<inner>\s*,\s*(@\w+)\s*\)` at `p`, where `inner` matches at the position after
/// the spaces: returns (end of match, variable range).
fn call_with_variable(
    b: &[u8],
    p: usize,
    name: &str,
    inner: impl Fn(&[u8], usize) -> Option<usize>,
) -> Option<(usize, (usize, usize))> {
    if !starts_ci(b, p, name) {
        return None;
    }
    let mut q = skip_spaces(b, p + name.len());
    q = inner(b, q)?;
    q = skip_spaces(b, q);
    if b.get(q) != Some(&b',') {
        return None;
    }
    let vs = skip_spaces(b, q + 1);
    let ve = variable(b, vs)?;
    let q = skip_spaces(b, ve);
    if b.get(q) != Some(&b')') {
        return None;
    }
    Some((q + 1, (vs, ve)))
}

/// `(?i)TRY_CONVERT\(\s*numeric\s*,\s*(@\w+)\s*\)`.
fn try_convert_numeric(b: &[u8], p: usize) -> Option<(usize, (usize, usize))> {
    call_with_variable(b, p, "TRY_CONVERT(", |b, q| {
        starts_ci(b, q, "numeric").then_some(q + 7)
    })
}

/// `(?i)HASHBYTES\(\s*'SHA2_256'\s*,\s*(@\w+)\s*\)`.
fn sha256_of_variable(b: &[u8], p: usize) -> Option<(usize, (usize, usize))> {
    call_with_variable(b, p, "HASHBYTES(", |b, q| {
        starts_ci(b, q, "'SHA2_256'").then_some(q + 10)
    })
}

/// `regexp.ReplaceAllString` for a one-capture pattern whose replacement template uses
/// only `$1`.
fn replace_all(
    s: &str,
    find: impl Fn(&[u8], usize) -> Option<(usize, (usize, usize))>,
    template: &str,
) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    let mut p = 0;
    while p < b.len() {
        // Every pattern starts with an ASCII letter, so a match starts on a char boundary.
        if let Some((end, (vs, ve))) = find(b, p) {
            out.push_str(&s[last..p]);
            out.push_str(&template.replace("$1", &s[vs..ve]));
            last = end;
            p = end;
        } else {
            p += 1;
        }
    }
    out.push_str(&s[last..]);
    out
}

const POST_2008_WORDS: &[&str] = &[
    "TRY_CONVERT",
    "TRY_CAST",
    "TRY_PARSE",
    "CONCAT",
    "CONCAT_WS",
    "IIF",
    "CHOOSE",
    "FORMAT",
    "EOMONTH",
    "STRING_AGG",
    "STRING_SPLIT",
    "DATEFROMPARTS",
    "DATETIMEFROMPARTS",
    "THROW",
    "OFFSET",
];

/// `post2008Feature.FindString`: the leftmost match of
/// `(?i)\b(TRY_CONVERT|...|OFFSET)\b\s*\(|SHA2_(256|512)|\bOFFSET\s+\d+\s+ROWS\b`, checked
/// on the comment-stripped text INCLUDING string literals, so 2012+ syntax hidden in
/// dynamic SQL fails the install loudly instead of at run time.
pub fn post2008_feature(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    for p in 0..b.len() {
        let boundary = p == 0 || !is_word(b[p - 1]);
        if boundary && is_word(b[p]) {
            let mut w = p;
            while w < b.len() && is_word(b[w]) {
                w += 1;
            }
            if POST_2008_WORDS
                .iter()
                .any(|k| k.len() == w - p && b[p..w].eq_ignore_ascii_case(k.as_bytes()))
            {
                let q = skip_spaces(b, w);
                if b.get(q) == Some(&b'(') {
                    return Some(&s[p..q + 1]);
                }
            }
        }
        if starts_ci(b, p, "SHA2_")
            && (b[p + 5..].starts_with(b"256") || b[p + 5..].starts_with(b"512"))
        {
            return Some(&s[p..p + 8]);
        }
        if boundary && starts_ci(b, p, "OFFSET") {
            let q = p + 6;
            let d = skip_spaces(b, q);
            if d > q {
                let mut e = d;
                while e < b.len() && b[e].is_ascii_digit() {
                    e += 1;
                }
                if e > d {
                    let r = skip_spaces(b, e);
                    if r > e && starts_ci(b, r, "ROWS") && b.get(r + 4).is_none_or(|&c| !is_word(c))
                    {
                        return Some(&s[p..r + 4]);
                    }
                }
            }
        }
    }
    None
}

/// `legacySQL`: rewrites one collect script into SQL Server 2008 T-SQL.
///
/// - `TRY_CONVERT(numeric, @x)` becomes a digits-only guarded `CONVERT` (NULL otherwise);
/// - `CONCAT(a, b, ...)` becomes `ISNULL(CAST(a AS nvarchar(4000)), N'') + ...`;
/// - `HASHBYTES('SHA2_256', @v)` becomes 32 bytes built from two SHA-1 digests.
///
/// `--` comments are dropped first (several sit between CONCAT arguments and would
/// swallow the text appended after an argument); newlines are kept, so the line numbers
/// SQL Server reports still point into the embedded file. Anything 2012+ the rewrite does
/// not know is an error rather than a broken install.
pub fn legacy_sql(name: &str, body: &str) -> Result<String, String> {
    let out = strip_line_comments(body);
    let out = replace_all(
        &out,
        try_convert_numeric,
        "CASE WHEN LEN($1) BETWEEN 1 AND 18 AND $1 NOT LIKE '%[^0-9]%' THEN CONVERT(numeric, $1) END",
    );
    let out =
        rewrite_concat(&out).map_err(|e| format!("agent: {name}: SQL Server 2008 rewrite: {e}"))?;
    // HASHBYTES takes at most 8000 bytes before SQL Server 2016, i.e. 4000 nvarchar
    // characters. The signature is one <=4000-character header plus short aggregate
    // suffixes, so two 4000-character windows cover it: SHA-1 of the first (20 bytes) +
    // 12 bytes of SHA-1 of the second = varbinary(32).
    let out = replace_all(
        &out,
        sha256_of_variable,
        "(HASHBYTES('SHA1', CAST(SUBSTRING($1, 1, 4000) AS nvarchar(4000))) + SUBSTRING(HASHBYTES('SHA1', CAST(SUBSTRING($1, 4001, 4000) AS nvarchar(4000))), 1, 12))",
    );
    if let Some(m) = post2008_feature(&out) {
        return Err(format!(
            "agent: {name}: uses {}, which SQL Server 2008 lacks and the installer cannot rewrite",
            go_quote(m.trim())
        ));
    }
    Ok(out)
}

/// `sqlSpanEnd`: the end of the string literal, `--` comment or `/* */` comment that
/// starts at `b[i]`, or `i` when none starts there, and whether it is a line comment. A
/// line comment ends before its newline; unterminated spans run to the end.
fn span_end(b: &[u8], i: usize) -> (usize, bool) {
    if b[i] == b'\'' {
        let mut j = i + 1;
        while j < b.len() {
            if b[j] == b'\'' {
                if j + 1 < b.len() && b[j + 1] == b'\'' {
                    j += 2; // doubled quote = escaped quote
                    continue;
                }
                return (j + 1, false);
            }
            j += 1;
        }
        return (b.len(), false);
    }
    if b[i..].starts_with(b"--") {
        return match b[i..].iter().position(|&c| c == b'\n') {
            Some(j) => (i + j, true),
            None => (b.len(), true),
        };
    }
    if b[i..].starts_with(b"/*") {
        return match b[i + 2..].windows(2).position(|w| w == b"*/") {
            Some(j) => (i + 2 + j + 2, false),
            None => (b.len(), false),
        };
    }
    (i, false)
}

/// `stripLineComments`: removes `--` comments outside string literals and block comments,
/// keeping every newline.
pub fn strip_line_comments(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    let mut i = 0;
    while i < b.len() {
        let (end, line_comment) = span_end(b, i);
        if end == i {
            i += 1;
        } else if line_comment {
            // Spans start and end on ASCII bytes, so these are char boundaries.
            out.push_str(&s[last..i]);
            last = end;
            i = end;
        } else {
            i = end;
        }
    }
    out.push_str(&s[last..]);
    out
}

fn is_ident_byte(c: u8) -> bool {
    matches!(c, b'_' | b'@' | b'#' | b'$') || c.is_ascii_alphanumeric()
}

const CONCAT_CALL: &str = "CONCAT(";

/// `rewriteConcat`: replaces every `CONCAT(...)` call outside literals and comments,
/// innermost first, with `+` concatenation. `s` must carry no `--` comments.
pub fn rewrite_concat(s: &str) -> Result<String, String> {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 4);
    let mut last = 0;
    let mut i = 0;
    while i < b.len() {
        let (end, _) = span_end(b, i);
        if end > i {
            i = end;
            continue;
        }
        let is_call = starts_ci(b, i, CONCAT_CALL) && (i == 0 || !is_ident_byte(b[i - 1]));
        if !is_call {
            i += 1;
            continue;
        }
        let (args, end) = split_call_args(s, i + CONCAT_CALL.len())?;
        out.push_str(&s[last..i]);
        out.push('(');
        for (k, arg) in args.iter().enumerate() {
            let inner = rewrite_concat(arg)?;
            if k > 0 {
                out.push_str(" +");
            }
            out.push_str(&legacy_concat_arg(&inner));
        }
        out.push(')');
        last = end;
        i = end;
    }
    out.push_str(&s[last..]);
    Ok(out)
}

/// `splitCallArgs`: the top-level arguments of the call whose argument list starts at
/// `start`, and the index just past the matching closing parenthesis.
fn split_call_args(s: &str, start: usize) -> Result<(Vec<&str>, usize), String> {
    let b = s.as_bytes();
    let mut args = Vec::new();
    let (mut depth, mut arg_start) = (0usize, start);
    let mut i = start;
    while i < b.len() {
        let (span, _) = span_end(b, i);
        if span > i {
            i = span;
            continue;
        }
        match b[i] {
            b'(' => depth += 1,
            b')' => {
                if depth == 0 {
                    args.push(&s[arg_start..i]);
                    return Ok((args, i + 1));
                }
                depth -= 1;
            }
            b',' if depth == 0 => {
                args.push(&s[arg_start..i]);
                arg_start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    Err("unterminated CONCAT( call".to_owned())
}

/// `legacyConcatArg`: wraps one CONCAT argument so `+` treats it the way CONCAT does
/// (converted to a string, NULL as empty). Arguments are cast to nvarchar(4000) so a
/// per-row HASHBYTES input never exceeds the 8000-byte limit; a bare variable (the
/// running `@sig`) keeps nvarchar(max) so appending to it never truncates. Surrounding
/// whitespace is kept so line numbers stay stable.
fn legacy_concat_arg(arg: &str) -> String {
    // Go: TrimSpace (Unicode White_Space, as Rust's trim), then the first occurrence of
    // the trimmed text; for an all-space argument that is index 0.
    let trimmed = arg.trim();
    let lead_len = if trimmed.is_empty() {
        0
    } else {
        arg.len() - arg.trim_start().len()
    };
    let lead = &arg[..lead_len];
    let trail = &arg[lead_len + trimmed.len()..];
    let tb = trimmed.as_bytes();
    let is_variable = tb.len() >= 2 && tb[0] == b'@' && tb[1..].iter().all(|&c| is_word(c));
    let typ = if is_variable {
        "nvarchar(max)"
    } else {
        "nvarchar(4000)"
    };
    format!("{lead}ISNULL(CAST({trimmed} AS {typ}), N''){trail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::{FoProbe, SCRIPTS, substitute_fo_tokens};

    #[test]
    fn major_version() {
        for (v, want) in [
            ("10.0.1600.22", 10),
            ("10.50.6000.34", 10),
            ("11.0.2100.60", 11),
            (" 16.0.4135.4 ", 16),
        ] {
            assert_eq!(sql_major_version(v), Ok(want), "{v}");
        }
        assert_eq!(
            sql_major_version("unknown").unwrap_err(),
            "agent: unrecognised SQL Server version \"unknown\": strconv.Atoi: parsing \"unknown\": invalid syntax"
        );
    }

    #[test]
    fn rewrites_each_construct() {
        let input = "DECLARE @n numeric = TRY_CONVERT(numeric, @ID); -- it's (a, comment\n\
            SELECT @sig = CONCAT(@sig, N'#X:', -- note, with ) paren\n    \
            HASHBYTES('MD5', CONCAT(a.Name, '|', ISNULL(a.Note, 'x,)y'))), /*{{T}}*/'')\n\
            RETURN HASHBYTES('SHA2_256', @sig);\n";
        let got = legacy_sql("t.sql", input).unwrap();
        for want in [
            "CASE WHEN LEN(@ID) BETWEEN 1 AND 18 AND @ID NOT LIKE '%[^0-9]%' THEN CONVERT(numeric, @ID) END",
            "(ISNULL(CAST(@sig AS nvarchar(max)), N'') + ISNULL(CAST(N'#X:' AS nvarchar(4000)), N'') +",
            "HASHBYTES('MD5', (ISNULL(CAST(a.Name AS nvarchar(4000)), N'') + ISNULL(CAST('|' AS nvarchar(4000)), N'') + ISNULL(CAST(ISNULL(a.Note, 'x,)y') AS nvarchar(4000)), N'')))",
            "ISNULL(CAST(/*{{T}}*/'' AS nvarchar(4000)), N''))",
            "HASHBYTES('SHA1', CAST(SUBSTRING(@sig, 1, 4000) AS nvarchar(4000)))",
            "SUBSTRING(HASHBYTES('SHA1', CAST(SUBSTRING(@sig, 4001, 4000) AS nvarchar(4000))), 1, 12)",
        ] {
            assert!(got.contains(want), "rewrite lacks {want:?}\n---\n{got}");
        }
        assert!(!got.contains("comment") && !got.contains("note"), "{got}");
        assert_eq!(got.matches('\n').count(), input.matches('\n').count());
    }

    #[test]
    fn keeps_literals_and_block_comments() {
        let input = "SET @p = N'a -- not a comment, CONCAT(x) stays'; /*MMEI_FO_PREDICATE*/ SELECT 1 /*END_MMEI_FO_PREDICATE*/";
        assert!(
            legacy_sql("t.sql", input).is_err(),
            "2012+ syntax inside a string literal (dynamic SQL) must fail the install"
        );
        let input = "SET @p = N'a -- not a comment, it''s (kept'; /*MMEI_FO_PREDICATE*/ SELECT 1 /*END_MMEI_FO_PREDICATE*/";
        assert_eq!(legacy_sql("t.sql", input).unwrap(), input);
    }

    #[test]
    fn rejects_what_it_cannot_rewrite() {
        for (input, msg) in [
            ("SELECT IIF(1 = 1, 'a', 'b')", "uses \"IIF(\""),
            ("SELECT TRY_CONVERT(int, @x)", "uses \"TRY_CONVERT(\""),
            ("SELECT HASHBYTES('SHA2_512', @x)", "uses \"SHA2_512\""),
            (
                "SELECT CONCAT(a, b",
                "SQL Server 2008 rewrite: unterminated CONCAT( call",
            ),
            (
                "SELECT * FROM t ORDER BY a OFFSET 10 ROWS",
                "uses \"OFFSET 10 ROWS\"",
            ),
            ("SELECT FORMAT\n (x)", "uses \"FORMAT\\n (\""),
        ] {
            let e = legacy_sql("t.sql", input).unwrap_err();
            assert!(
                e.starts_with("agent: t.sql: ") && e.contains(msg),
                "{input}: {e}"
            );
        }
        // Word boundaries: CONCAT_WS is its own feature, FORMATTED is not FORMAT.
        assert_eq!(post2008_feature("x CONCAT_WS ("), Some("CONCAT_WS ("));
        assert_eq!(post2008_feature("FORMATTED(1)"), None);
        assert_eq!(post2008_feature("x.OFFSET  5\tROWSX"), None);
        assert_eq!(post2008_feature("xSHA2_256"), Some("SHA2_256"));
    }

    #[test]
    fn concat_argument_whitespace() {
        assert_eq!(
            legacy_concat_arg("\n  @sig "),
            "\n  ISNULL(CAST(@sig AS nvarchar(max)), N'') "
        );
        assert_eq!(
            legacy_concat_arg(" @ "),
            " ISNULL(CAST(@ AS nvarchar(4000)), N'') "
        );
        assert_eq!(
            legacy_concat_arg("  "),
            "ISNULL(CAST( AS nvarchar(4000)), N'')  "
        );
        // CONCAT inside an identifier is not a call.
        assert_eq!(rewrite_concat("x_CONCAT(a)").unwrap(), "x_CONCAT(a)");
        assert_eq!(
            rewrite_concat("concat(a,b)").unwrap(),
            "(ISNULL(CAST(a AS nvarchar(4000)), N'') +ISNULL(CAST(b AS nvarchar(4000)), N''))"
        );
    }

    /// Every embedded collect script survives the rewrite with its line count, balanced
    /// parentheses, the fenced FO predicate parity and the POS `TrnCode` parity intact.
    #[test]
    fn covers_every_embedded_script() {
        let p = FoProbe {
            has_trn_group: true,
            has_folio_balance: true,
            has_is_hk_post: true,
            has_tax_rate: true,
        };
        let mut defs = std::collections::HashMap::new();
        for (name, body) in SCRIPTS {
            let body = substitute_fo_tokens(body, p);
            let got = legacy_sql(name, &body).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(
                got.matches('\n').count(),
                body.matches('\n').count(),
                "{name}: line count changed"
            );
            assert_eq!(got.matches('(').count(), got.matches(')').count(), "{name}");
            assert!(post2008_feature(&got).is_none(), "{name}");
            defs.insert(*name, got);
        }
        crate::sql::drift::verify(
            "dbo.sp_FetchFO",
            &defs["sp_FetchFO.sql"],
            "dbo.sp_FetchPOS",
            &defs["sp_FetchPOS.sql"],
            &defs["01_source_hash.sql"],
            &crate::agent_config::AgentConfig::default().charge_codes(),
        )
        .unwrap();
    }
}
