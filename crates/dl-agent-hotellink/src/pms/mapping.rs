//! `RoomTypeMapped` validation and CRUD (Go `mapping.go`). Rows have no key: a row is
//! identified by all of its columns, compared NULL-safely.

use dl_tds::{Client, Param};

use super::repo::{SqlRepo, count, exec};
use crate::error::{Error, Kind, Result};
use crate::jobmodel::MappingRow;

/// `ErrMappingDuplicate`.
pub const DUPLICATE: &str = "cặp RoomId+RoomTypeCode đã tồn tại";
/// `ErrMappingNotFound`.
pub const NOT_FOUND: &str = "không tìm thấy hàng mapping (RoomId+RoomTypeCode)";
/// `ErrMappingAmbiguous`.
pub const AMBIGUOUS: &str =
    "nhiều hàng trùng khớp y hệt — không thể sửa/xóa an toàn, hãy dọn trùng trước";

fn duplicate() -> Error {
    Error::new(Kind::MappingDuplicate, DUPLICATE)
}
fn not_found() -> Error {
    Error::new(Kind::MappingNotFound, NOT_FOUND)
}
fn ambiguous() -> Error {
    Error::new(Kind::MappingAmbiguous, AMBIGUOUS)
}

/// `ValidateMapping`. Lengths are bytes, like Go's `len`.
pub fn validate_mapping(m: &MappingRow) -> Result<()> {
    if m.room_type_code.trim().is_empty() {
        return Err(Error::other("RoomTypeCode bắt buộc"));
    }
    if m.room_type_code.len() > 50 {
        return Err(Error::other("RoomTypeCode tối đa 50 ký tự"));
    }
    if m.room_id.len() > 50 {
        return Err(Error::other("RoomId tối đa 50 ký tự"));
    }
    if m.split_part.is_some() != m.total_part.is_some() {
        return Err(Error::other(
            "SplitPart và TotalPart phải cùng có hoặc cùng trống",
        ));
    }
    if let (Some(split), Some(total)) = (m.split_part, m.total_part) {
        if total < 1 {
            return Err(Error::other("TotalPart phải >= 1"));
        }
        if split < 1 || split > total {
            return Err(Error::other("SplitPart phải trong khoảng 1..TotalPart"));
        }
    }
    Ok(())
}

const MATCH_ORIG: &str = "RoomId=@p1 AND RoomTypeCode=@p2 AND IsBooking=@p3
  AND ((SplitPart=@p4) OR (SplitPart IS NULL AND @p4 IS NULL))
  AND ((TotalPart=@p5) OR (TotalPart IS NULL AND @p5 IS NULL))";

/// The five identity parameters of `m`, numbered from `first`.
fn ident(m: &MappingRow, first: usize) -> Vec<Param> {
    let n = |i: usize| format!("p{}", first + i);
    vec![
        Param::new(&n(0), m.room_id.as_str()),
        Param::new(&n(1), m.room_type_code.as_str()),
        Param::new(&n(2), m.is_booking),
        Param::new(&n(3), m.split_part),
        Param::new(&n(4), m.total_part),
    ]
}

fn row_params(m: &MappingRow) -> Vec<Param> {
    vec![
        Param::new("p1", m.room_id.as_str()),
        Param::new("p2", m.room_type_code.as_str()),
        Param::new("p3", m.split_part),
        Param::new("p4", m.total_part),
        Param::new("p5", m.is_booking),
    ]
}

/// `SELECT COUNT(1)` of the rows equal to `orig`, mapped to not-found / ambiguous.
fn require_single(c: &mut Client, orig: &MappingRow, what: &str) -> Result<()> {
    let n = count(
        c,
        &format!("SELECT COUNT(1) FROM RoomTypeMapped WHERE {MATCH_ORIG}"),
        &ident(orig, 1),
    )
    .map_err(|e| e.context(format!("{what}: ")))?;
    match n {
        0 => Err(not_found()),
        1 => Ok(()),
        _ => Err(ambiguous()),
    }
}

fn affected_one(n: u64) -> Result<()> {
    match n {
        0 => Err(not_found()),
        1 => Ok(()),
        _ => Err(ambiguous()),
    }
}

impl SqlRepo {
    pub(super) fn insert_mapping_sql(&self, m: &MappingRow) -> Result<()> {
        validate_mapping(m)?;
        self.with_tx(|c| {
            let n = count(
                c,
                "SELECT COUNT(1) FROM RoomTypeMapped WHERE RoomId=@p1 AND RoomTypeCode=@p2",
                &[
                    Param::new("p1", m.room_id.as_str()),
                    Param::new("p2", m.room_type_code.as_str()),
                ],
            )
            .map_err(|e| e.context("kiểm tra (RoomId,RoomTypeCode) trùng: "))?;
            if n > 0 {
                return Err(duplicate());
            }
            if m.is_booking {
                let n = count(
                    c,
                    "SELECT COUNT(1) FROM RoomTypeMapped WHERE RoomId=@p1 AND IsBooking=1",
                    &[Param::new("p1", m.room_id.as_str())],
                )
                .map_err(|e| e.context("kiểm tra booking row trùng: "))?;
                if n > 0 {
                    return Err(duplicate());
                }
            }
            exec(
                c,
                "INSERT INTO RoomTypeMapped (RoomId, RoomTypeCode, SplitPart, TotalPart, IsBooking) VALUES (@p1, @p2, @p3, @p4, @p5)",
                &row_params(m),
            )
            .map_err(|e| {
                e.context(format!(
                    "insert mapping {}/{}: ",
                    m.room_id, m.room_type_code
                ))
            })?;
            Ok(())
        })
    }

    pub(super) fn update_mapping_sql(&self, orig: &MappingRow, m: &MappingRow) -> Result<()> {
        validate_mapping(m)?;
        self.with_tx(|c| {
            require_single(c, orig, "kiểm tra orig row")?;
            // Another row (not orig) already holds the new (RoomId, RoomTypeCode) pair.
            let mut p = vec![
                Param::new("p1", m.room_id.as_str()),
                Param::new("p2", m.room_type_code.as_str()),
            ];
            p.extend(ident(orig, 3));
            let n = count(
                c,
                "SELECT COUNT(1) FROM RoomTypeMapped
WHERE RoomId=@p1 AND RoomTypeCode=@p2
  AND NOT (
    RoomId=@p3 AND RoomTypeCode=@p4 AND IsBooking=@p5
    AND ((SplitPart=@p6) OR (SplitPart IS NULL AND @p6 IS NULL))
    AND ((TotalPart=@p7) OR (TotalPart IS NULL AND @p7 IS NULL))
  )",
                &p,
            )
            .map_err(|e| e.context("kiểm tra (RoomId,RoomTypeCode) conflict: "))?;
            if n > 0 {
                return Err(duplicate());
            }
            if m.is_booking {
                let mut p = vec![Param::new("p1", m.room_id.as_str())];
                p.extend(ident(orig, 2));
                let n = count(
                    c,
                    "SELECT COUNT(1) FROM RoomTypeMapped
WHERE RoomId=@p1 AND IsBooking=1
  AND NOT (
    RoomId=@p2 AND RoomTypeCode=@p3 AND IsBooking=@p4
    AND ((SplitPart=@p5) OR (SplitPart IS NULL AND @p5 IS NULL))
    AND ((TotalPart=@p6) OR (TotalPart IS NULL AND @p6 IS NULL))
  )",
                    &p,
                )
                .map_err(|e| e.context("kiểm tra booking conflict: "))?;
                if n > 0 {
                    return Err(duplicate());
                }
            }
            let mut p = row_params(m);
            p.extend(ident(orig, 6));
            let n = exec(
                c,
                "UPDATE RoomTypeMapped
SET RoomId=@p1, RoomTypeCode=@p2, SplitPart=@p3, TotalPart=@p4, IsBooking=@p5
WHERE RoomId=@p6 AND RoomTypeCode=@p7 AND IsBooking=@p8
  AND ((SplitPart=@p9) OR (SplitPart IS NULL AND @p9 IS NULL))
  AND ((TotalPart=@p10) OR (TotalPart IS NULL AND @p10 IS NULL))",
                &p,
            )
            .map_err(|e| e.context("update mapping: "))?;
            affected_one(n)
        })
    }

    pub(super) fn delete_mapping_sql(&self, orig: &MappingRow) -> Result<()> {
        self.with_tx(|c| {
            require_single(c, orig, "kiểm tra orig row (delete)")?;
            let n = exec(
                c,
                &format!("DELETE FROM RoomTypeMapped WHERE {MATCH_ORIG}"),
                &ident(orig, 1),
            )
            .map_err(|e| e.context("delete mapping: "))?;
            affected_one(n)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(rt: &str, split: Option<i64>, total: Option<i64>) -> MappingRow {
        MappingRow {
            room_id: "R1".into(),
            room_type_code: rt.into(),
            split_part: split,
            total_part: total,
            is_booking: false,
        }
    }

    #[test]
    fn validate_mapping_cases() {
        // mapping_test.go TestValidateMapping
        let long = "x".repeat(51);
        let cases: Vec<(MappingRow, Option<&str>)> = vec![
            (row("DLX", None, None), None),
            (row("  ", None, None), Some("RoomTypeCode bắt buộc")),
            (row(&long, None, None), Some("RoomTypeCode tối đa 50 ký tự")),
            (
                MappingRow {
                    room_id: long.clone(),
                    ..row("DLX", None, None)
                },
                Some("RoomId tối đa 50 ký tự"),
            ),
            (
                row("DLX", Some(1), None),
                Some("SplitPart và TotalPart phải cùng có hoặc cùng trống"),
            ),
            (row("DLX", Some(1), Some(0)), Some("TotalPart phải >= 1")),
            (
                row("DLX", Some(3), Some(2)),
                Some("SplitPart phải trong khoảng 1..TotalPart"),
            ),
            (
                row("DLX", Some(0), Some(2)),
                Some("SplitPart phải trong khoảng 1..TotalPart"),
            ),
            (row("DLX", Some(2), Some(2)), None),
        ];
        for (m, want) in cases {
            let got = validate_mapping(&m).err().map(|e| e.message().to_owned());
            assert_eq!(got.as_deref(), want, "{m:?}");
        }
    }
}
