//! `PmsProjection`: the columns the cloud asks for per result set. The agent validates
//! them, trims rows to them (key columns always kept) and keys its snapshots by their hash.

use super::catalog::Operation;
use super::hasher;
use crate::stj;

/// Requested columns per result set (`None` = the whole set).
pub type Projection = Vec<Option<Vec<String>>>;

const MAX_COLUMNS: usize = 256;

/// `^[A-Za-z_][A-Za-z0-9_]{0,127}$`, including .NET's `$` also matching before a final `\n`.
fn is_identifier(c: &str) -> bool {
    let c = c.strip_suffix('\n').unwrap_or(c);
    let b = c.as_bytes();
    !b.is_empty()
        && b.len() <= 128
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b[1..]
            .iter()
            .all(|&x| x.is_ascii_alphanumeric() || x == b'_')
}

fn contains_ci(list: &[String], c: &str) -> bool {
    list.iter().any(|a| stj::ci_eq(a, c))
}

/// `TryValidate`.
///
/// # Errors
/// The .NET message for the first problem.
pub fn try_validate(op: &Operation, projection: &Projection) -> Result<(), String> {
    if projection.len() > op.result_sets.len() {
        return Err(format!(
            "Projection lists {} result sets; '{}' has {}.",
            projection.len(),
            op.name,
            op.result_sets.len()
        ));
    }
    for (i, columns) in projection.iter().enumerate() {
        let Some(columns) = columns else { continue };
        if columns.is_empty() || columns.len() > MAX_COLUMNS {
            return Err(format!(
                "Projection for set {i} must list 1-{MAX_COLUMNS} columns."
            ));
        }
        let allowed = op.result_sets[i].columns.as_deref();
        for c in columns {
            if !is_identifier(c) {
                return Err(format!("Projection column '{c}' is not a valid name."));
            }
            if let Some(allowed) = allowed
                && !contains_ci(allowed, c)
            {
                return Err(format!(
                    "Column '{c}' is not allowed for '{}' set {i}.",
                    op.name
                ));
            }
        }
    }
    Ok(())
}

/// `Effective`: the columns to keep for set `index` (`None` = all).
pub fn effective(
    op: &Operation,
    projection: Option<&Projection>,
    index: usize,
) -> Option<Vec<String>> {
    let spec = op.result_sets.get(index);
    let requested = projection
        .and_then(|p| p.get(index))
        .and_then(Option::as_ref);
    let Some(requested) = requested else {
        return spec.and_then(|s| s.columns.clone());
    };
    let allowed = spec.and_then(|s| s.columns.as_deref());
    let mut out: Vec<String> = Vec::new();
    let keys = spec.map_or(&[][..], |s| s.key_columns.as_slice());
    let candidates = requested
        .iter()
        .filter(|c| allowed.is_none_or(|a| contains_ci(a, c)))
        .chain(keys.iter());
    for c in candidates {
        if !contains_ci(&out, c) {
            out.push(c.clone());
        }
    }
    Some(out)
}

/// `Hash`: empty without a projection, else the hash of `[COLS,]` per set.
pub fn hash(projection: Option<&Projection>) -> String {
    let Some(projection) = projection else {
        return String::new();
    };
    let mut sb = String::new();
    for set in projection {
        sb.push('[');
        if let Some(cols) = set {
            let mut up: Vec<String> = Vec::new();
            for c in cols {
                let u = stj::upper(c);
                if !up.contains(&u) {
                    up.push(u);
                }
            }
            up.sort_by(|a, b| stj::ordinal_cmp(a, b));
            for c in up {
                sb.push_str(&c);
                sb.push(',');
            }
        }
        sb.push(']');
    }
    hasher::hash_parameters([("projection", Some(sb.as_str()))])
}
