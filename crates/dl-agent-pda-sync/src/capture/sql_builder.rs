//! Pure SQL text generation for a [`SourceQuery`] (`Source/SqlBuilder.cs`): SELECT only,
//! identifiers validated, values parameterised.

use super::error::{CaptureError, Result};
use super::source::SourceQuery;
use super::util::contains_ignore_case;
use crate::contracts::mapping::RawValue;

/// How reads avoid blocking POS Win.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReadHintPolicy {
    /// NOLOCK unless the database already runs read-committed snapshot.
    #[default]
    Auto,
    /// Always NOLOCK / read uncommitted.
    NoLock,
    /// Never hint (tests, or a site where RCSI is known to be on).
    None,
}

/// A parameterised SELECT.
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltQuery {
    /// The SQL text.
    pub sql: String,
    /// Parameters (`@w0`, `@rf`, `@a1`, ...) in order of appearance.
    pub parameters: Vec<(String, RawValue)>,
}

/// `[identifier]` for a plain identifier (`^[A-Za-z_][A-Za-z0-9_]*$`), else an
/// `ArgumentException`.
pub fn quote(identifier: &str) -> Result<String> {
    let mut chars = identifier.chars();
    let ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !ok {
        return Err(CaptureError::argument(format!(
            "Unsafe SQL identifier '{identifier}'. (Parameter 'identifier')"
        )));
    }
    Ok(format!("[{identifier}]"))
}

/// Builds the SELECT of `q` over `columns` (the resolved select list); `bytes_columns`
/// (matched ignoring case) are fetched via `CONVERT(varbinary(max), ...)`.
pub fn build(
    q: &SourceQuery,
    columns: &[String],
    bytes_columns: &[String],
    no_lock_hint: bool,
) -> Result<BuiltQuery> {
    let mut args: Vec<(String, RawValue)> = Vec::new();
    let mut sb = String::from("SELECT ");
    if let Some(limit) = q.limit {
        sb.push_str(&format!("TOP ({limit}) "));
    }
    let mut list = Vec::with_capacity(columns.len());
    for c in columns {
        let quoted = quote(c)?;
        if contains_ignore_case(bytes_columns, c) {
            list.push(format!("CONVERT(varbinary(max), {quoted}) AS {quoted}"));
        } else {
            list.push(quoted);
        }
    }
    sb.push_str(&list.join(", "));
    sb.push_str(" FROM dbo.");
    sb.push_str(&quote(&q.table)?);
    if no_lock_hint {
        sb.push_str(" WITH (NOLOCK)");
    }

    let mut predicates: Vec<String> = Vec::new();
    add_where(q, &mut predicates, &mut args)?;
    add_range(q, &mut predicates, &mut args)?;
    add_keyset(q, &mut predicates, &mut args)?;
    if !predicates.is_empty() {
        sb.push_str(" WHERE ");
        sb.push_str(&predicates.join(" AND "));
    }

    if !q.order_by.is_empty() {
        let dir = if q.descending { " DESC" } else { "" };
        let order = q
            .order_by
            .iter()
            .map(|c| quote(c).map(|x| x + dir))
            .collect::<Result<Vec<_>>>()?;
        sb.push_str(" ORDER BY ");
        sb.push_str(&order.join(", "));
    }
    Ok(BuiltQuery {
        sql: sb,
        parameters: args,
    })
}

fn add_where(
    q: &SourceQuery,
    predicates: &mut Vec<String>,
    args: &mut Vec<(String, RawValue)>,
) -> Result<()> {
    let Some(w) = &q.where_ else { return Ok(()) };
    for (col, value) in w {
        if *value == RawValue::Null {
            predicates.push(format!("{} IS NULL", quote(col)?));
            continue;
        }
        let p = format!("@w{}", args.len());
        predicates.push(format!("{} = {p}", quote(col)?));
        args.push((p, value.clone()));
    }
    Ok(())
}

fn add_range(
    q: &SourceQuery,
    predicates: &mut Vec<String>,
    args: &mut Vec<(String, RawValue)>,
) -> Result<()> {
    let Some(rc) = &q.range_column else {
        return Ok(());
    };
    let col = quote(rc)?;
    let mut parts = Vec::new();
    if let Some(from) = q.range_from {
        args.push(("@rf".into(), RawValue::DateTime(from)));
        parts.push(format!("{col} >= @rf"));
    }
    if let Some(to) = q.range_to {
        args.push(("@rt".into(), RawValue::DateTime(to)));
        parts.push(format!("{col} < @rt"));
    }
    if parts.is_empty() {
        return Ok(());
    }
    let range = parts.join(" AND ");
    predicates.push(if q.range_includes_null {
        format!("(({range}) OR {col} IS NULL)")
    } else {
        range
    });
    Ok(())
}

/// Lexicographic "after this row" predicate over `order_by`. A NULL position follows SQL
/// Server's order: NULL sorts first ascending, last descending. So after NULL ascending
/// means IS NOT NULL, after NULL descending is nothing, and descending after a value also
/// takes the NULLs; equality on a NULL position is IS NULL. Only non-NULL values become
/// parameters.
fn add_keyset(
    q: &SourceQuery,
    predicates: &mut Vec<String>,
    args: &mut Vec<(String, RawValue)>,
) -> Result<()> {
    let Some(after) = &q.after else {
        return Ok(());
    };
    if after.len() != q.order_by.len() {
        return Err(CaptureError::argument("After must align with OrderBy."));
    }
    let mut names: Vec<Option<String>> = Vec::new();
    for (i, v) in after.iter().enumerate() {
        if *v == RawValue::Null {
            names.push(None);
            continue;
        }
        let p = format!("@a{i}");
        args.push((p.clone(), v.clone()));
        names.push(Some(p));
    }
    let mut branches = Vec::new();
    for i in 0..names.len() {
        let col = quote(&q.order_by[i])?;
        let past = match (q.descending, &names[i]) {
            (false, None) => Some(format!("{col} IS NOT NULL")),
            (false, Some(p)) => Some(format!("{col} > {p}")),
            (true, None) => None,
            (true, Some(p)) => Some(format!("({col} < {p} OR {col} IS NULL)")),
        };
        let Some(past) = past else { continue };
        let mut parts = Vec::new();
        for j in 0..i {
            let c = quote(&q.order_by[j])?;
            parts.push(match &names[j] {
                None => format!("{c} IS NULL"),
                Some(p) => format!("{c} = {p}"),
            });
        }
        parts.push(past);
        branches.push(format!("({})", parts.join(" AND ")));
    }
    predicates.push(if branches.is_empty() {
        "(1 = 0)".to_owned()
    } else {
        format!("({})", branches.join(" OR "))
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    //! `SqlBuilderTests.cs`.
    use super::*;
    use crate::contracts::time::DateTime;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    fn q(table: &str) -> SourceQuery {
        SourceQuery::new(table)
    }

    #[test]
    fn select_is_bounded_hinted_and_ordered() {
        let mut query = q("Invoice");
        query.order_by = s(&["CheckNo"]);
        query.limit = Some(500);
        let built = build(&query, &s(&["CheckNo", "SubTotal"]), &[], true).unwrap();
        assert_eq!(
            built.sql,
            "SELECT TOP (500) [CheckNo], [SubTotal] FROM dbo.[Invoice] WITH (NOLOCK) ORDER BY [CheckNo]"
        );
        assert!(built.parameters.is_empty());
    }

    #[test]
    fn hint_is_omitted_when_not_requested() {
        let built = build(&q("Invoice"), &s(&["CheckNo"]), &[], false).unwrap();
        assert!(!built.sql.contains("NOLOCK"));
    }

    #[test]
    fn tcvn3_columns_are_fetched_as_raw_bytes() {
        let built = build(
            &q("Trn"),
            &s(&["TrnSeq", "TrnDesc"]),
            &s(&["trndesc"]),
            true,
        )
        .unwrap();
        assert!(
            built
                .sql
                .contains("CONVERT(varbinary(max), [TrnDesc]) AS [TrnDesc]")
        );
        assert!(built.sql.contains("[TrnSeq],"));
    }

    #[test]
    fn keyset_over_two_columns_expands_to_lexicographic_predicate_with_parameters() {
        let mut query = q("Card");
        query.order_by = s(&["A", "B"]);
        query.after = Some(vec![RawValue::I32(1), RawValue::from("x")]);
        let built = build(&query, &s(&["A", "B"]), &[], false).unwrap();
        assert!(
            built
                .sql
                .contains("WHERE (([A] > @a0) OR ([A] = @a0 AND [B] > @a1))")
        );
        let values: Vec<RawValue> = built.parameters.into_iter().map(|p| p.1).collect();
        assert_eq!(values, [RawValue::I32(1), RawValue::from("x")]);
    }

    #[test]
    fn descending_keyset_flips_the_comparison() {
        let mut query = q("T");
        query.order_by = s(&["A"]);
        query.after = Some(vec![RawValue::I32(5)]);
        query.descending = true;
        let built = build(&query, &s(&["A"]), &[], false).unwrap();
        assert!(built.sql.contains("[A] < @a0"));
        assert!(built.sql.ends_with("ORDER BY [A] DESC"));
    }

    #[test]
    fn range_can_include_null_dates() {
        let mut query = q("Invoice");
        query.range_column = Some("CloseTime".into());
        query.range_from = DateTime::new(2026, 1, 1, 0, 0, 0);
        query.range_to = DateTime::new(2026, 2, 1, 0, 0, 0);
        query.range_includes_null = true;
        let built = build(&query, &s(&["CheckNo"]), &[], false).unwrap();
        assert!(
            built
                .sql
                .contains("(([CloseTime] >= @rf AND [CloseTime] < @rt) OR [CloseTime] IS NULL)")
        );
    }

    #[test]
    fn equality_filters_use_parameters_and_null_becomes_is_null() {
        let mut query = q("T");
        query.where_ = Some(vec![
            ("CheckNo".into(), RawValue::I32(7)),
            ("Note".into(), RawValue::Null),
        ]);
        let built = build(&query, &s(&["CheckNo"]), &[], false).unwrap();
        assert!(built.sql.contains("[CheckNo] = @w0"));
        assert!(built.sql.contains("[Note] IS NULL"));
        assert_eq!(built.parameters, [("@w0".to_owned(), RawValue::I32(7))]);
    }

    #[test]
    fn unsafe_identifiers_are_rejected() {
        for name in ["Invoice; DROP TABLE x", "a b", "[x]", ""] {
            let e = build(&q(name), &s(&["CheckNo"]), &[], false).unwrap_err();
            assert_eq!(e.kind, crate::capture::ErrorKind::Argument, "{name}");
        }
    }

    #[test]
    fn keyset_must_align_with_order() {
        let mut query = q("T");
        query.order_by = s(&["A", "B"]);
        query.after = Some(vec![RawValue::I32(1)]);
        let e = build(&query, &s(&["A"]), &[], false).unwrap_err();
        assert_eq!(e.kind, crate::capture::ErrorKind::Argument);
    }

    #[test]
    fn null_keyset_positions_follow_sql_server_null_ordering() {
        let mut asc = q("T");
        asc.order_by = s(&["A", "B", "C"]);
        asc.after = Some(vec![RawValue::I32(1), RawValue::Null, RawValue::I32(2)]);
        let built = build(&asc, &s(&["A"]), &[], false).unwrap();
        assert!(built.sql.contains(
            "WHERE (([A] > @a0) OR ([A] = @a0 AND [B] IS NOT NULL) OR ([A] = @a0 AND [B] IS NULL AND [C] > @a2))"
        ));
        let values: Vec<RawValue> = built.parameters.into_iter().map(|p| p.1).collect();
        assert_eq!(values, [RawValue::I32(1), RawValue::I32(2)]);

        let mut desc = q("T");
        desc.order_by = s(&["A", "B"]);
        desc.after = Some(vec![RawValue::I32(1), RawValue::Null]);
        desc.descending = true;
        let built = build(&desc, &s(&["A"]), &[], false).unwrap();
        assert!(built.sql.contains("WHERE ((([A] < @a0 OR [A] IS NULL)))"));

        let mut none = q("T");
        none.order_by = s(&["A"]);
        none.after = Some(vec![RawValue::Null]);
        none.descending = true;
        let built = build(&none, &s(&["A"]), &[], false).unwrap();
        assert!(built.sql.contains("WHERE (1 = 0)"));
    }

    #[test]
    fn generated_sql_is_select_only() {
        let mut query = q("Invoice");
        query.order_by = s(&["CheckNo"]);
        query.limit = Some(1);
        query.after = Some(vec![RawValue::I32(3)]);
        let sql = build(&query, &s(&["CheckNo"]), &[], true).unwrap().sql;
        assert!(sql.starts_with("SELECT "));
        for bad in ["INSERT", "UPDATE ", "DELETE", "DROP", "EXEC"] {
            assert!(!sql.contains(bad), "{bad}");
        }
    }
}
