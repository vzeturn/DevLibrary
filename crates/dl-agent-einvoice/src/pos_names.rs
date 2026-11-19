//! POS item names from `ItemDef.IName1` (port of MM-EINVOICE `pos_item_names.go`, RT-0204).
//! Opt-in through `collect_profile.pos_item_name = "iname1"`; with the default `till`
//! nothing here runs. The lookup is a separate query on purpose: the collect procedures
//! are installed only at enrolment while the agent updates itself, and the name is NOT
//! part of `MMEI_SourceHash`, so renaming an item never re-collects sent checks.

use std::collections::HashMap;

use crate::pos::PosRawLine;

/// `(ItemCode, IName1)` of the revenue rows of one check, joined like `sp_FetchPOS` joins
/// `ItemDef` (ICode alone, trimmed). Behind a `COL_LENGTH` probe and `sp_executesql`: a
/// batch naming a missing column fails to compile even in a false IF, and an install
/// without `ItemDef.IName1` (or the FO database) must keep till names instead of failing.
/// Without the column the batch returns no result set.
pub const ITEM_NAMES_QUERY: &str = "IF COL_LENGTH('dbo.ItemDef', 'IName1') IS NOT NULL
	EXEC sp_executesql
		N'SELECT RTRIM(t.ItemCode), ISNULL(idf.IName1, N'''')
		  FROM dbo.Trn t
		  JOIN dbo.ItemDef idf ON RTRIM(idf.ICode) = RTRIM(t.ItemCode)
		  WHERE t.CheckNo = @CheckNo AND t.TrnCode = 100',
		N'@CheckNo bigint',
		@CheckNo = @CheckNo";

/// The check number as a whole number; `None` for a non-numeric check (no Trn rows, no lookup).
pub fn check_no_param(check_no: &str) -> Option<i64> {
    check_no.trim().parse().ok()
}

/// Fold `(code, name)` rows into the lookup map: codes and names trimmed, blanks left out,
/// the first non-blank name per code wins.
pub fn collect_names<I: IntoIterator<Item = (String, String)>>(rows: I) -> HashMap<String, String> {
    let mut names = HashMap::new();
    for (code, name) in rows {
        let (code, name) = (code.trim(), name.trim());
        if code.is_empty() || name.is_empty() {
            continue;
        }
        names
            .entry(code.to_owned())
            .or_insert_with(|| name.to_owned());
    }
    names
}

/// Replace each revenue line's till name with the item's `IName1` when one was found.
/// Discount rows (TrnCode 110) and unmatched lines keep `TrnDesc`.
pub fn apply_names(lines: &mut [PosRawLine], names: &HashMap<String, String>) {
    if names.is_empty() {
        return;
    }
    for l in lines.iter_mut().filter(|l| l.trn_code == 100) {
        if let Some(n) = names.get(l.item_code.trim()) {
            l.trn_desc.clone_from(n);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(code: i64, item: &str, desc: &str) -> PosRawLine {
        PosRawLine {
            trn_code: code,
            item_code: item.into(),
            trn_desc: desc.into(),
            ..Default::default()
        }
    }

    #[test]
    fn check_no_numeric_only() {
        assert_eq!(check_no_param(" 42 "), Some(42));
        assert_eq!(check_no_param("A-1"), None);
        assert_eq!(check_no_param(""), None);
    }

    #[test]
    fn first_non_blank_wins_and_blanks_dropped() {
        let m = collect_names([
            ("A1 ".into(), "".into()),
            ("A1".into(), " Tôm ".into()),
            ("A1".into(), "Khác".into()),
            ("B2".into(), "  ".into()),
            ("".into(), "x".into()),
        ]);
        assert_eq!(m.len(), 1);
        assert_eq!(m["A1"], "Tôm");
    }

    #[test]
    fn apply_touches_revenue_rows_only() {
        let m = collect_names([("A1".into(), "Tôm".into()), ("D".into(), "Giảm".into())]);
        let mut ls = vec![
            line(100, "A1 ", "RS Prawn"),
            line(100, "Z9", "Keep me"),
            line(110, "D", "Discount"),
        ];
        apply_names(&mut ls, &m);
        assert_eq!(ls[0].trn_desc, "Tôm");
        assert_eq!(ls[1].trn_desc, "Keep me");
        assert_eq!(ls[2].trn_desc, "Discount");
        let mut before = ls.clone();
        apply_names(&mut before, &HashMap::new());
        assert_eq!(before, ls);
    }
}
