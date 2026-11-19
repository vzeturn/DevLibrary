//! `FolioWriter.Lookup` (the folios of a booking, with guest and bed fields),
//! `ReadSpecialBedRowsAsync` and `FolioWriter.DuplicateGuard`.

use std::collections::HashMap;

use dl_decimal::Decimal;
use dl_tds::Param;
use dl_uuid::Uuid;

use super::dapper_in;
use super::folio::{FolioWriter, NOTE_COLUMN};
use super::schema::TableSchema;
use super::session::{Session, day, decimal, nvarchar, p};
use crate::jobs::beds::{ExistingBeds, SpecialBedRow, note_bed_count};
use crate::jobs::payload::Room;
use crate::jobs::plan::WriteError;
use crate::jobs::result::POSSIBLE_DUPLICATE;
use crate::jobs::room_diff::{ExistingFolio, ExistingGuest};
use crate::log::{self, V};
use crate::smile::SmileError;

const HAND_ENTERED_SQL: &str = "SELECT FolioNum, LastName
FROM Folio
WHERE ISNULL(CancelNum, 0) = 0
  AND ISNULL(ExternalConfirmNumber, '') = ''
  AND RoomTypeBooked = @RoomType
  AND ArrivalDate = @Arrival
  AND DepartureDate = @Departure";

/// `SpecialRowMarkerPattern`: `MM:{room} %`.
pub fn special_row_marker_pattern(room_item_id: Uuid) -> String {
    format!("MM:{room_item_id} %")
}

/// `Col`: `alias.column` when the install has it, else `NULL`.
fn col(schema: &TableSchema, alias: &str, column: &str) -> Result<String, WriteError> {
    if !schema.is_whitelisted(column) {
        return Err(WriteError::Coded {
            code: "SCHEMA_COLUMN_NOT_WHITELISTED".into(),
            message: format!(
                "Cột '{column}' không thuộc whitelist bảng {}.",
                schema.table()
            ),
        });
    }
    Ok(if schema.has(column) {
        format!("{alias}.{column}")
    } else {
        "NULL".into()
    })
}

/// `FoldName`: no tones, `đ` as `D`, upper case, single spaces.
pub fn fold_name(name: Option<&str>) -> String {
    let Some(name) = name.filter(|n| !n.trim().is_empty()) else {
        return String::new();
    };
    let plain = dl_vn::text::remove_diacritics(name);
    let mut out = String::with_capacity(plain.len());
    for ch in plain.chars() {
        let c = if matches!(ch, 'đ' | 'Đ') { 'D' } else { ch };
        if c.is_whitespace() {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            continue;
        }
        out.extend(c.to_uppercase());
    }
    out.trim_end().to_owned()
}

/// `RefuseLikelyHandEnteredDuplicateAsync`: a live hand-entered folio (no booking code)
/// with the room type, dates and folded last name refuses the create.
pub fn refuse_hand_entered_duplicate(s: &mut dyn Session, room: &Room) -> Result<(), WriteError> {
    let wanted = fold_name(Some(&room.guest_last_name));
    if wanted.is_empty() {
        return Ok(());
    }
    let rows = s.rows(
        HAND_ENTERED_SQL,
        &[
            p("@RoomType", nvarchar(Some(&room.room_type_code))),
            p("@Arrival", day(room.arrival)),
            p("@Departure", day(room.departure)),
        ],
    )?;
    for r in &rows {
        let last = r.opt_str("LastName")?;
        let decoded = last
            .as_deref()
            .map(|l| dl_vn::tcvn3::decode(l, dl_vn::tcvn3::CodePage::Cp1252));
        if fold_name(decoded.as_deref()) != wanted {
            continue;
        }
        let folio = r.dec("FolioNum")?.round(0);
        log::warn(
            "Live hand-entered folio matches room type/dates/last name; refusing to create a possible duplicate",
            &[
                ("room_item", V::S(&room.room_item_id.to_string())),
                ("folio", V::S(&folio.to_string())),
            ],
        );
        return Err(WriteError::Refused {
            code: POSSIBLE_DUPLICATE.into(),
            message: format!(
                "Folio {folio} trên SMILE (lễ tân nhập tay, không có mã đặt phòng) trùng loại phòng, ngày đến/đi và họ khách — có thể là cùng lượt đặt. Kiểm tra trên PMS; nếu không phải, thử lại job với force."
            ),
        });
    }
    Ok(())
}

impl FolioWriter {
    /// `LookupByConfirmationAsync`.
    pub fn lookup_by_confirmation(
        &self,
        s: &mut dyn Session,
        ext: &str,
    ) -> Result<Vec<ExistingFolio>, WriteError> {
        self.lookup(
            s,
            "f.ExternalConfirmNumber = @Ext".into(),
            vec![p("@Ext", nvarchar(Some(ext)))],
        )
    }

    /// `LookupByFolioNumsAsync`: hand-entered folios only.
    pub fn lookup_by_folio_nums(
        &self,
        s: &mut dyn Session,
        nums: &[Decimal],
    ) -> Result<Vec<ExistingFolio>, WriteError> {
        if nums.is_empty() {
            return Ok(Vec::new());
        }
        let (list, params) = dapper_in("Nums", nums.iter().map(|n| decimal(*n)));
        self.lookup(
            s,
            format!("f.FolioNum IN {list} AND ISNULL(f.ExternalConfirmNumber, '') = ''"),
            params,
        )
    }

    fn lookup(
        &self,
        s: &mut dyn Session,
        where_clause: String,
        params: Vec<Param>,
    ) -> Result<Vec<ExistingFolio>, WriteError> {
        let folio = self.probe.get(s, "Folio")?;
        let an = self.probe.get(s, "AdditionalName")?;
        let sql = format!(
            "SELECT f.FolioNum,
       f.UserInfo1        AS RoomItemId,
       f.RoomTypeBooked,
       f.ArrivalDate,
       f.DepartureDate,
       ISNULL((SELECT SUM(fr.RateAmount) FROM FolioRate fr
               WHERE fr.FolioNum = f.FolioNum AND ISNULL(fr.isCancel, 0) = 0),
              f.RateAmount) AS RateAmount,
       ISNULL(f.RoomCode, '')   AS RoomCode,
       ISNULL(f.FolioStatus, 1) AS FolioStatus,
       ISNULL(f.CancelNum, 0)   AS CancelNum,
       ISNULL(f.NumAdult, 0)    AS NumAdult,
       ISNULL(f.NumChild, 0)    AS NumChild,
       f.LastName               AS LastName,
       {}  AS ExtraBed,
       {}  AS Note,
       {} AS FirstName,
       {}   AS Phone,
       {}     AS Email,
       an.BirthDate, an.Gender, an.Nationality
FROM Folio f
OUTER APPLY (SELECT TOP 1 {}       AS BirthDate,
                          {}          AS Gender,
                          {} AS Nationality
             FROM AdditionalName a
             WHERE a.FolioNum = f.FolioNum AND ISNULL(a.ACancelNum, 0) = 0
             ORDER BY a.IdAddition) an
WHERE {where_clause}",
            col(&folio, "f", "ExtraBed")?,
            col(&folio, "f", NOTE_COLUMN)?,
            col(&folio, "f", "FirstName")?,
            col(&folio, "f", "TelNumM")?,
            col(&folio, "f", "MailM")?,
            col(&an, "a", "BirthDate")?,
            col(&an, "a", "Gender")?,
            col(&an, "a", "NationalityCode")?,
        );
        let rows = s.rows(&sql, &params)?;
        let mut keyed = Vec::with_capacity(rows.len());
        for r in &rows {
            let room = r
                .opt_str("RoomItemId")?
                .and_then(|s| s.trim().parse::<Uuid>().ok())
                .unwrap_or(Uuid::NIL);
            keyed.push((r.dec("FolioNum")?, room));
        }
        let special_known = self.has_special_rows_table(s)?;
        let special = self.read_special_bed_rows(s, &keyed, special_known)?;
        let mut out = Vec::with_capacity(rows.len());
        for (r, (folio_num, room)) in rows.iter().zip(keyed) {
            let (mm, foreign) = special.get(&folio_num).cloned().unwrap_or_default();
            let note = r.opt_str("Note")?;
            out.push(ExistingFolio {
                folio_num,
                room_item_id: room,
                room_type_booked: r.opt_str("RoomTypeBooked")?.unwrap_or_default(),
                arrival: r
                    .opt_date("ArrivalDate")?
                    .ok_or(SmileError::Decode("ArrivalDate"))?,
                departure: r
                    .opt_date("DepartureDate")?
                    .ok_or(SmileError::Decode("DepartureDate"))?,
                rate_amount: r.opt_dec("RateAmount")?.unwrap_or(Decimal::ZERO),
                room_code: r.opt_str("RoomCode")?.unwrap_or_default(),
                folio_status: to_i32(r.int("FolioStatus")?, "FolioStatus")?,
                cancel_num: r.dec("CancelNum")?,
                guest: Some(ExistingGuest {
                    num_adult: to_i32(r.int("NumAdult")?, "NumAdult")?,
                    num_child: to_i32(r.int("NumChild")?, "NumChild")?,
                    last_name: r.opt_str("LastName")?,
                    first_name: r.opt_str("FirstName")?,
                    phone: r.opt_str("Phone")?,
                    email: r.opt_str("Email")?,
                    birth_date: r.opt_date("BirthDate")?,
                    gender: r.opt_bool("Gender")?,
                    nationality: r.opt_str("Nationality")?,
                }),
                beds: Some(ExistingBeds {
                    extra_bed: r
                        .opt_int("ExtraBed")?
                        .map(|n| to_i32(n, "ExtraBed"))
                        .transpose()?,
                    mm_rows: mm,
                    foreign_qty: foreign,
                    note_beds: note_bed_count(note.as_deref()),
                    special_known,
                }),
            });
        }
        Ok(out)
    }

    /// `HasSpecialRowsTableAsync`.
    fn has_special_rows_table(&self, s: &mut dyn Session) -> Result<bool, WriteError> {
        let special = self.probe.get(s, "Folio_Special")?;
        Ok(special.has("SPNotes") && special.has("Qty"))
    }

    /// `ReadSpecialBedRowsAsync`: per folio, the MM rows of its room and the quantity of
    /// other rows with the hotel's extra-bed code.
    fn read_special_bed_rows(
        &self,
        s: &mut dyn Session,
        folios: &[(Decimal, Uuid)],
        known: bool,
    ) -> Result<HashMap<Decimal, (Vec<SpecialBedRow>, i32)>, WriteError> {
        let mut result: HashMap<Decimal, (Vec<SpecialBedRow>, i32)> = HashMap::new();
        if folios.is_empty() || !known {
            return Ok(result);
        }
        let special = self.probe.get(s, "Folio_Special")?;
        let num = |column: &str| {
            if special.has(column) {
                let ty = if column == "Price" {
                    "decimal(18,2)"
                } else {
                    "int"
                };
                format!("CAST(ISNULL(s.{column}, 0) AS {ty})")
            } else {
                "0".into()
            }
        };
        let cancel = if special.has("CancelNum") {
            "AND ISNULL(s.CancelNum, 0) = 0"
        } else {
            ""
        };
        let mut distinct: Vec<Decimal> = Vec::new();
        for (f, _) in folios {
            if !distinct.contains(f) {
                distinct.push(*f);
            }
        }
        let (list, params) = dapper_in("Nums", distinct.iter().map(|n| decimal(*n)));
        let sql = format!(
            "SELECT s.FolioNum, s.SPNotes, LTRIM(RTRIM(ISNULL(s.SpecialCode, ''))) AS SpecialCode,
       {} AS Qty, {} AS Price, {} AS FromDay, {} AS ToDay
FROM Folio_Special s
WHERE s.FolioNum IN {list} {cancel}",
            num("Qty"),
            num("Price"),
            num("FromDay"),
            num("ToDay"),
        );
        let rows = s.rows(&sql, &params)?;
        let code = self.effective_special_code();
        for r in &rows {
            let folio = r.dec("FolioNum")?;
            let Some((_, room)) = folios.iter().find(|(f, _)| *f == folio) else {
                continue;
            };
            let prefix = format!("MM:{room} ");
            let notes = r.opt_str("SPNotes")?;
            let qty = to_i32(r.int("Qty")?, "Qty")?;
            let entry = result.entry(folio).or_default();
            let is_mm = notes.as_deref().is_some_and(|n| {
                n.get(..prefix.len())
                    .is_some_and(|h| h.eq_ignore_ascii_case(&prefix))
            });
            if is_mm {
                entry.0.push(SpecialBedRow {
                    qty,
                    price: r.opt_dec("Price")?.unwrap_or(Decimal::ZERO),
                    from_day: to_i32(r.int("FromDay")?, "FromDay")?,
                    to_day: to_i32(r.int("ToDay")?, "ToDay")?,
                });
            } else if r
                .opt_str("SpecialCode")?
                .is_some_and(|c| c.eq_ignore_ascii_case(&code))
            {
                entry.1 += qty.max(0);
            }
        }
        Ok(result)
    }
}

fn to_i32(n: i64, col: &'static str) -> Result<i32, SmileError> {
    i32::try_from(n).map_err(|_| SmileError::Decode(col))
}
