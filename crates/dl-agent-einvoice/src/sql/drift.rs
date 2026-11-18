//! The charge-code drift guard (`driftcheck.go`), pure text checks.

/// The T-SQL fingerprint function that re-embeds the fetch filters.
pub const SOURCE_HASH_FUNC: &str = "dbo.MMEI_SourceHash";
const OPEN: &str = "/*MMEI_FO_PREDICATE*/";
const CLOSE: &str = "/*END_MMEI_FO_PREDICATE*/";

/// RE2 `\s`: ASCII space, \t, \n, \f, \r.
fn is_re2_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{c}' | '\r')
}

/// Collapses whitespace runs to one space and trims (`normalizePredicate`).
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_space = false;
    for c in s.chars() {
        if is_re2_space(c) {
            in_space = true;
        } else {
            if in_space {
                out.push(' ');
            }
            in_space = false;
            out.push(c);
        }
    }
    if in_space {
        out.push(' ');
    }
    out.trim().to_owned()
}

fn fenced(obj: &str, def: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut rest = def;
    while let Some(i) = rest.find(OPEN) {
        let after = &rest[i + OPEN.len()..];
        let Some(j) = after.find(CLOSE) else {
            return Err(format!(
                "agent: {obj} has an unterminated {OPEN} block (missing {CLOSE})"
            ));
        };
        out.push(normalize(&after[..j]));
        rest = &after[j + CLOSE.len()..];
    }
    Ok(out)
}

fn all_identical(obj: &str, blocks: &[String]) -> Result<String, String> {
    let Some(first) = blocks.first() else {
        return Err(format!(
            "agent: {obj} is on the fenced FO-predicate contract but has no {OPEN} block"
        ));
    };
    for (i, b) in blocks.iter().enumerate().skip(1) {
        if b != first {
            return Err(format!(
                "agent: {obj} has internally divergent fenced FO predicates: block #{} differs from block #1",
                i + 1
            ));
        }
    }
    Ok(first.clone())
}

/// Bodies of every `<col>\s+IN\s*\(([^)]*)\)` clause, case-insensitive.
fn in_clauses(def: &str, col: &str) -> Vec<String> {
    let lower = def.to_ascii_lowercase();
    let col_l = col.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = lower[from..].find(&col_l) {
        let start = from + rel;
        from = start + 1;
        let mut p = start + col_l.len();
        let ws = |b: u8| matches!(b, b' ' | b'\t' | b'\n' | 0x0c | b'\r');
        let s0 = p;
        while p < bytes.len() && ws(bytes[p]) {
            p += 1;
        }
        if p == s0 || !lower[p..].starts_with("in") {
            continue;
        }
        p += 2;
        while p < bytes.len() && ws(bytes[p]) {
            p += 1;
        }
        if p >= bytes.len() || bytes[p] != b'(' {
            continue;
        }
        let body_start = p + 1;
        let Some(close) = def[body_start..].find(')') else {
            continue;
        };
        out.push(def[body_start..body_start + close].to_owned());
        from = body_start + close + 1;
    }
    out
}

/// Code tokens of an IN-list body: `'quoted'` literals or bare digit runs.
fn codes(body: &str) -> Vec<String> {
    let b = body.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\'' {
            if let Some(end) = body[i + 1..].find('\'') {
                out.push(body[i + 1..i + 1 + end].to_owned());
                i += end + 2;
                continue;
            }
            i += 1;
        } else if b[i].is_ascii_digit() {
            let s = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            out.push(body[s..i].to_owned());
        } else {
            i += 1;
        }
    }
    out
}

fn same_set(a: &[String], b: &[String]) -> (bool, String, String) {
    let set = |x: &[String]| {
        let mut v: Vec<String> = x.iter().map(|s| s.trim().to_owned()).collect();
        v.sort();
        v.dedup();
        v
    };
    let render = |x: &[String]| {
        let mut v = x.to_vec();
        v.sort();
        v.join(",")
    };
    (set(a) == set(b), render(a), render(b))
}

fn in_list_codes(obj: &str, def: &str, col: &str) -> Result<Vec<String>, String> {
    let clauses = in_clauses(def, col);
    if clauses.is_empty() {
        return Err(format!(
            "agent: {obj} definition has no `{col} IN (...)` clause"
        ));
    }
    let mut first = Vec::new();
    for (i, c) in clauses.iter().enumerate() {
        let cs = codes(c);
        if cs.is_empty() {
            return Err(format!(
                "agent: {obj} `{col} IN (...)` clause #{} is empty",
                i + 1
            ));
        }
        if i == 0 {
            first = cs;
            continue;
        }
        let (eq, got, want) = same_set(&cs, &first);
        if !eq {
            return Err(format!(
                "agent: {obj} has internally divergent `{col} IN (...)` clauses: [{got}] vs [{want}]"
            ));
        }
    }
    Ok(first)
}

/// `VerifyChargeCodeSync` on the three module definitions.
pub fn verify(
    fo_sp: &str,
    fo_def: &str,
    pos_sp: &str,
    pos_def: &str,
    hash_def: &str,
    charge_codes: &[String],
) -> Result<(), String> {
    let h = SOURCE_HASH_FUNC;
    if fo_def.contains(OPEN) || hash_def.contains(OPEN) {
        if !fo_def.contains(OPEN) {
            return Err(format!(
                "agent: FO predicate drift: {h} carries the fenced predicate but {fo_sp} does not — mixed contract"
            ));
        }
        if !hash_def.contains(OPEN) {
            return Err(format!(
                "agent: FO predicate drift: {fo_sp} carries the fenced predicate but {h} does not — mixed contract"
            ));
        }
        let fo_pred = all_identical(fo_sp, &fenced(fo_sp, fo_def)?)?;
        let hash_pred = all_identical(h, &fenced(h, hash_def)?)?;
        if fo_pred != hash_pred {
            return Err(format!(
                "agent: FO predicate drift: {fo_sp} and {h} fenced predicates differ — hash and fetch would cover different rows:\n  {fo_pred}\n  {hash_pred}"
            ));
        }
    } else {
        let from_sp = in_list_codes(fo_sp, fo_def, "TransactionCode")?;
        let from_hash = in_list_codes(h, hash_def, "TransactionCode")?;
        let (eq, got, want) = same_set(&from_sp, charge_codes);
        if !eq {
            return Err(format!(
                "agent: FO charge-code drift: installed {fo_sp} has [{got}] but config FOChargeCodes is [{want}]"
            ));
        }
        let (eq, got, want) = same_set(&from_hash, charge_codes);
        if !eq {
            return Err(format!(
                "agent: FO charge-code drift: {h} fingerprints [{got}] but config FOChargeCodes is [{want}] — hash and fetch would cover different rows"
            ));
        }
    }
    let pos_sp_codes = in_list_codes(pos_sp, pos_def, "TrnCode")?;
    let pos_hash_codes = in_list_codes(h, hash_def, "TrnCode")?;
    let (eq, got, want) = same_set(&pos_sp_codes, &pos_hash_codes);
    if !eq {
        return Err(format!(
            "agent: POS TrnCode drift: installed {pos_sp} filters [{got}] but {h} fingerprints [{want}] — hash and fetch would cover different rows"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_lists_and_codes() {
        let def = "WHERE ft.TransactionCode IN ('110', '150')\n AND t.TrnCode  in(100,110) and x.transactioncode\tIN ( '150','110' )";
        assert_eq!(
            in_list_codes("x", def, "TransactionCode").unwrap(),
            vec!["110", "150"]
        );
        assert_eq!(
            in_list_codes("x", def, "TrnCode").unwrap(),
            vec!["100", "110"]
        );
        assert!(in_list_codes("x", "TrnCode IN ()", "TrnCode").is_err());
        assert!(in_list_codes("x", "nothing", "TrnCode").is_err());
        // A column name glued to IN without whitespace is not a clause (\s+ needs one).
        assert!(in_list_codes("x", "TrnCodeIN (1)", "TrnCode").is_err());
    }

    #[test]
    fn fenced_parity() {
        let f = |p: &str| format!("a {OPEN} {p} {CLOSE} b {OPEN}{p}{CLOSE}");
        let fo = f("x  =\n 1");
        let hash = format!("{} TrnCode IN (100,110)", f("x = 1"));
        let pos = "TrnCode IN (110, 100)";
        verify("sp_FetchFO", &fo, "sp_FetchPOS", pos, &hash, &[]).unwrap();
        let bad = format!("{} TrnCode IN (100)", f("x = 2"));
        assert!(verify("sp_FetchFO", &fo, "sp_FetchPOS", pos, &bad, &[]).is_err());
        assert!(verify("sp_FetchFO", "no fence", "sp_FetchPOS", pos, &hash, &[]).is_err());
    }
}
