//! `FolioWriter.AdditionalName`: the guest placeholder rows of a folio (one per adult,
//! one per child when the install has `IsChild`), inserted on create and reconciled on
//! update without ever deleting a row whose name differs from the main guest.

use std::collections::BTreeMap;

use dl_decimal::Decimal;

use super::schema::{ColumnList, Mode, TableSchema};
use super::session::{Session, bit, day, decimal, int, nvarchar, p, varchar};
use super::tcvn3;
use crate::jobs::beds::plan_guests;
use crate::jobs::guest::smile_gender;
use crate::jobs::payload::Room;
use crate::jobs::plan::WriteError;
use crate::smile::SmileError;

/// `FolioGuestKeys`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestKeys {
    /// `ConfirmNum`.
    pub confirm_num: Option<Decimal>,
    /// `OriginCode`.
    pub origin_code: Option<String>,
}

const ROWS_SQL_HEAD: &str = "SELECT a.IdAddition,
       CASE WHEN a.IdAddition <> m.IdAddition
             AND ISNULL(a.FirstName, '') = ISNULL(m.FirstName, '')
             AND ISNULL(a.LastName, '') = ISNULL(m.LastName, '')
            THEN 1 ELSE 0 END AS IsPlaceholder,
       ";
const ROWS_SQL_TAIL: &str = " AS IsChild
FROM AdditionalName a
CROSS APPLY (SELECT TOP 1 x.IdAddition, x.FirstName, x.LastName FROM AdditionalName x
             WHERE x.FolioNum = @FolioNum AND ISNULL(x.ACancelNum, 0) = 0
             ORDER BY x.IdAddition) m
WHERE a.FolioNum = @FolioNum AND ISNULL(a.ACancelNum, 0) = 0
ORDER BY a.IdAddition";

/// `AdditionalNameColumns`.
fn columns<'a>(schema: &'a TableSchema, room: &Room, contact: Mode) -> ColumnList<'a> {
    let guest = room.guest.as_ref();
    let gender = smile_gender(guest.and_then(|g| g.gender.as_deref())).map(|g| g == 1);
    ColumnList::new(schema)
        .core(
            "FirstName",
            varchar(Some(&tcvn3::encode(&room.guest_first_name))),
        )
        .core(
            "LastName",
            varchar(Some(&tcvn3::encode(&room.guest_last_name))),
        )
        .opt("ArrivalTime", day(room.arrival))
        .opt("DepartureTime", day(room.departure))
        .opt_mode(
            "MobileNum",
            nvarchar(guest.and_then(|g| g.phone.as_deref())),
            contact,
        )
        .opt_mode(
            "Email",
            nvarchar(guest.and_then(|g| g.email.as_deref())),
            contact,
        )
        .opt_mode(
            "BirthDate",
            dl_tds::ParamValue::DateTime(guest.and_then(|g| g.date_of_birth).map(|d| d.midnight())),
            contact,
        )
        .opt_mode("Gender", dl_tds::ParamValue::Bool(gender), contact)
        .opt_mode(
            "NationalityCode",
            nvarchar(guest.and_then(|g| g.nationality.as_deref())),
            contact,
        )
}

/// `InsertAdditionalNameRowsAsync`: the same insert `count` times.
pub fn insert_rows(
    s: &mut dyn Session,
    schema: &TableSchema,
    folio_num: Decimal,
    room: &Room,
    keys: &GuestKeys,
    count: i32,
    is_child: bool,
) -> Result<(), WriteError> {
    let origin = keys
        .origin_code
        .as_deref()
        .map(str::trim)
        .filter(|o| !o.is_empty());
    let c = columns(schema, room, Mode::Overwrite)
        .core("FolioNum", decimal(folio_num))
        .core_lit("ACancelNum", "0")
        .opt("AdtStatus", int(1))
        .opt("IsChild", bit(is_child))
        .opt("VIPLevel", int(0))
        .opt("FolioSubNum", decimal(Decimal::ZERO))
        .opt("ConfirmNum", dl_tds::ParamValue::Decimal(keys.confirm_num))
        .opt("AOriginCode", nvarchar(origin));
    let sql = c.insert_sql()?;
    for _ in 0..count {
        s.execute(&sql, c.params())?;
    }
    Ok(())
}

struct GuestRow {
    id: Decimal,
    placeholder: bool,
    child: bool,
}

/// `UpdateAdditionalNameAsync`.
pub fn update(
    s: &mut dyn Session,
    schema: &TableSchema,
    folio_num: Decimal,
    room: &Room,
) -> Result<(), WriteError> {
    let targets = plan_guests(room, schema.has("IsChild"));
    let key = [p("@FolioNum", decimal(folio_num))];
    let k = s
        .rows(
            "SELECT ConfirmNum, OriginCode FROM Folio WHERE FolioNum = @FolioNum",
            &key,
        )?
        .into_iter()
        .next()
        .ok_or(WriteError::Smile(SmileError::Decode("ConfirmNum")))?;
    let keys = GuestKeys {
        confirm_num: k.opt_dec("ConfirmNum")?,
        origin_code: k.opt_str("OriginCode")?,
    };
    let is_child_sql = if schema.has("IsChild") {
        "CAST(ISNULL(a.IsChild, 0) AS int)"
    } else {
        "0"
    };
    let rows = s
        .rows(
            &format!("{ROWS_SQL_HEAD}{is_child_sql}{ROWS_SQL_TAIL}"),
            &key,
        )?
        .iter()
        .map(|r| {
            Ok(GuestRow {
                id: r.dec("IdAddition")?,
                placeholder: r.int("IsPlaceholder")? == 1,
                child: r.int("IsChild")? == 1,
            })
        })
        .collect::<Result<Vec<_>, SmileError>>()?;

    if rows.is_empty() {
        insert_rows(
            s,
            schema,
            folio_num,
            room,
            &keys,
            targets.placeholder_adults,
            false,
        )?;
        if targets.placeholder_children > 0 {
            insert_rows(
                s,
                schema,
                folio_num,
                room,
                &keys,
                targets.placeholder_children,
                true,
            )?;
        }
        return Ok(());
    }

    let mut to_delete: Vec<Decimal> = Vec::new();
    let mut missing: BTreeMap<bool, i32> = BTreeMap::new();
    for child in [false, true] {
        let of_kind: Vec<&GuestRow> = rows.iter().filter(|r| r.child == child).collect();
        let desired = if child {
            targets.placeholder_children
        } else {
            targets.placeholder_adults
        };
        let mut placeholders: Vec<Decimal> = of_kind
            .iter()
            .filter(|r| r.placeholder)
            .map(|r| r.id)
            .collect();
        if child && !schema.has("IsChild") {
            continue;
        }
        let count = of_kind.len() as i32;
        let excess = (count - desired).max(0).min(placeholders.len() as i32);
        placeholders.sort_by(|a, b| b.cmp(a));
        for id in placeholders.iter().take(excess as usize) {
            if !to_delete.contains(id) {
                to_delete.push(*id);
            }
        }
        missing.insert(child, desired - (count - excess));
    }
    for id in &to_delete {
        s.execute(
            "DELETE FROM AdditionalName WHERE IdAddition = @Id AND FolioNum = @FolioNum",
            &[p("@Id", decimal(*id)), p("@FolioNum", decimal(folio_num))],
        )?;
    }

    let c = columns(schema, room, Mode::KeepWhenNull).param("@FolioNum", decimal(folio_num));
    let sql = format!(
        "UPDATE AdditionalName SET {} WHERE IdAddition = @IdAddition AND FolioNum = @FolioNum",
        c.set_clause()?
    );
    let survivors = std::iter::once(rows[0].id).chain(
        rows.iter()
            .filter(|r| r.placeholder && !to_delete.contains(&r.id))
            .map(|r| r.id),
    );
    for id in survivors {
        let mut params = c.params().to_vec();
        params.push(p("@IdAddition", decimal(id)));
        s.execute(&sql, &params)?;
    }

    if let Some(&n) = missing.get(&false)
        && n > 0
    {
        insert_rows(s, schema, folio_num, room, &keys, n, false)?;
    }
    if let Some(&n) = missing.get(&true)
        && n > 0
    {
        insert_rows(s, schema, folio_num, room, &keys, n, true)?;
    }
    Ok(())
}
