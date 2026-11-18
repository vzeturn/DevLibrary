//! `buildInsert` and the truncation diagnosis (Go `insert.go`, `truncation.go`).

use std::collections::HashMap;

use dl_tds::Param;

use super::entities::Val;
use crate::gofmt::quote;

/// `INSERT INTO <table> (<cols>) VALUES (@p1..@pN)` with the matching parameters.
pub fn build_insert(table: &str, fields: &[(&'static str, Val)]) -> (String, Vec<Param>) {
    let cols: Vec<&str> = fields.iter().map(|(c, _)| *c).collect();
    let names: Vec<String> = (1..=fields.len()).map(|i| format!("@p{i}")).collect();
    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        table,
        cols.join(", "),
        names.join(", ")
    );
    let params = fields
        .iter()
        .enumerate()
        .map(|(i, (_, v))| v.param(&format!("p{}", i + 1)))
        .collect();
    (sql, params)
}

/// `isTruncationErr`.
pub fn is_truncation_err(msg: &str) -> bool {
    msg.contains("String or binary data would be truncated")
}

/// `overflowNote`: the string columns whose value is longer than the column allows
/// (`limits` maps the lower-case column name to its length, below 0 for MAX), sorted by
/// column, or "" when none overflow.
pub fn overflow_note(fields: &[(&'static str, Val)], limits: &HashMap<String, i64>) -> String {
    let mut hits: Vec<(&str, usize, i64, String)> = Vec::new();
    for (col, v) in fields {
        let Some(&max) = limits.get(&col.to_lowercase()) else {
            continue;
        };
        if max < 0 {
            continue;
        }
        let Some(s) = v.as_str() else {
            continue;
        };
        let got = s.chars().count();
        if got as i64 > max {
            hits.push((col, got, max, clip(s, 40)));
        }
    }
    hits.sort_by(|a, b| a.0.cmp(b.0));
    hits.iter()
        .map(|(col, got, max, sample)| {
            format!(
                "{col} vượt {got}>{max} ký tự (giá trị={})",
                quote(sample)
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(n).collect();
    out.push('…');
    out
}

/// `diagnoseTruncation`'s message: `msg` unchanged unless it is a truncation error and the
/// limits are known.
pub fn truncation_message(
    msg: String,
    fields: &[(&'static str, Val)],
    limits: Option<&HashMap<String, i64>>,
) -> String {
    if !is_truncation_err(&msg) {
        return msg;
    }
    let Some(limits) = limits.filter(|l| !l.is_empty()) else {
        return msg;
    };
    let note = overflow_note(fields, limits);
    if note.is_empty() {
        format!("{msg} — không khớp cột chuỗi nào vượt giới hạn (kiểm tra cột số/định dạng)")
    } else {
        format!("{msg} — cột tràn: {note}")
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Folio, FolioRate};
    use super::*;
    use dl_time::DateTime;

    fn t(s: &str) -> DateTime {
        DateTime::parse_iso(s).unwrap()
    }

    #[test]
    fn build_insert_folio() {
        let f = Folio::new(t("2026-06-10T08:00:00"));
        let (sql, params) = build_insert("Folio", &f.fields());
        assert!(!sql.contains("(Id,"), "Id (identity) must not be inserted");
        for col in [
            "FolioNum",
            "ArrivalDate",
            "ExternalConfirmNumber",
            "BreakfastCode",
            "RateOverideFlag",
            "UserDefineCode8",
            "ShareNum",
            "Booker",
        ] {
            assert!(sql.contains(col), "INSERT missing {col}");
        }
        assert_eq!(params.len(), sql.matches("@p").count());
        assert_eq!(params.len(), 109, "every Go `db` column except identity Id");
    }

    #[test]
    fn build_insert_folio_rate_uses_db_names() {
        let (sql, _) = build_insert("FolioRate", &FolioRate::default().fields());
        for col in ["hDate", "isCancel", "RateOverride"] {
            assert!(sql.contains(col), "{sql}");
        }
        assert!(!sql.contains("HDate") && !sql.contains("IsCancel"), "{sql}");
    }

    #[test]
    fn new_folio_defaults() {
        let now = t("2026-06-10T08:00:00");
        let f = Folio::new(now);
        assert_eq!(f.num_room, 1);
        assert!(f.confirm_print_flag);
        assert_eq!(f.folio_status, 1);
        assert_eq!(f.payment_code, "7");
        assert!(f.rate_overide_flag);
        assert_eq!(f.book_status, 1);
        assert!(f.net_rate && f.include_bf && f.update_ex_rate_at_ci);
        assert_eq!(f.breakfast_code, "ABU");
        assert_eq!(f.folio_currency_code, "VND");
        assert_eq!(f.folio_ex_rate, 1.0);
        assert_eq!(f.reservation_time, now);
        assert_eq!(f.last_change_time, now);
        assert_eq!(f.external_confirm_number.as_deref(), Some(""));
        assert_eq!(super::super::AdditionalName::new().adt_status, 1);
    }

    fn limits(pairs: &[(&str, i64)]) -> HashMap<String, i64> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect()
    }

    #[test]
    fn overflow_note_identifies_column() {
        let f = Folio {
            room_type_code: "SUPERDELUXE".into(),
            room_code: "101".into(),
            booker: "x".repeat(100),
            ..Folio::default()
        };
        let l = limits(&[("roomtypecode", 6), ("roomcode", 4), ("booker", -1)]);
        let note = overflow_note(&f.fields(), &l);
        assert!(note.contains("RoomTypeCode"), "{note}");
        assert!(!note.contains("RoomCode vượt"), "{note}");
        assert!(!note.contains("Booker"), "{note}");
        assert!(note.contains("11>6"), "{note}");
    }

    #[test]
    fn overflow_note_empty_when_all_fit() {
        let f = Folio {
            room_type_code: "DLX01".into(),
            room_code: "101".into(),
            ..Folio::default()
        };
        let l = limits(&[("roomtypecode", 20), ("roomcode", 10)]);
        assert_eq!(overflow_note(&f.fields(), &l), "");
    }

    #[test]
    fn overflow_note_optional_string() {
        let f = Folio {
            external_confirm_number: Some("ABCDEFGH".into()),
            ..Folio::default()
        };
        let note = overflow_note(&f.fields(), &limits(&[("externalconfirmnumber", 5)]));
        assert!(note.contains("ExternalConfirmNumber") && note.contains("8>5"), "{note}");
    }

    #[test]
    fn truncation_detection() {
        assert!(is_truncation_err(
            "mssql: String or binary data would be truncated."
        ));
        assert!(!is_truncation_err("some other error"));
        let f = Folio::default();
        let msg = truncation_message("other".into(), &f.fields(), None);
        assert_eq!(msg, "other");
        let l = limits(&[("roomcode", 10)]);
        let msg = truncation_message(
            "String or binary data would be truncated".into(),
            &f.fields(),
            Some(&l),
        );
        assert!(msg.ends_with("(kiểm tra cột số/định dạng)"), "{msg}");
    }
}
