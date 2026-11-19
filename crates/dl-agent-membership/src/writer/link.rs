//! `FolioWriter.Link` (LINK_FOLIO): stamps each operator-linked folio with the booking's
//! `ExternalConfirmNumber`, `UserInfo1` = room id and the `UserInfo2` tag, in one
//! transaction under the folio-number applock.

use dl_decimal::Decimal;
use dl_uuid::Uuid;

use super::applock::{self, FOLIO_NUMBER_LOCK};
use super::folio::{FolioWriter, add_last_change};
use super::format_user_info2;
use super::schema::ColumnList;
use super::session::{Session, decimal, nvarchar, p};
use crate::jobs::payload::{LinkFolio, LinkFolioPayload};
use crate::jobs::plan::WriteError;
use crate::jobs::result::LINK_TARGET_CHANGED;

const LINK_ROW_SQL: &str = "SELECT ISNULL(CancelNum, 0) AS CancelNum, ConfirmNum, RoomTypeBooked, ExternalConfirmNumber, UserInfo1, UserInfo2
FROM Folio WITH (UPDLOCK, ROWLOCK)
WHERE FolioNum = @FolioNum";

/// `LinkRow`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRow {
    /// `CancelNum`.
    pub cancel_num: Decimal,
    /// `ConfirmNum`.
    pub confirm_num: Option<Decimal>,
    /// `RoomTypeBooked`.
    pub room_type_booked: Option<String>,
    /// `ExternalConfirmNumber`.
    pub external_confirm_number: Option<String>,
    /// `UserInfo1`.
    pub user_info1: Option<String>,
    /// `UserInfo2`.
    pub user_info2: Option<String>,
}

/// `ParseUserInfo2`: `(member, booking)`; a malformed tag is `(None, NIL)`.
pub fn parse_user_info2(tag: Option<&str>) -> (Option<Uuid>, Uuid) {
    let mut member = None;
    let mut booking = Uuid::NIL;
    let Some(tag) = tag.filter(|t| !t.trim().is_empty()) else {
        return (member, booking);
    };
    for part in tag.split(';').filter(|p| !p.is_empty()) {
        let span = part.trim();
        if let Some(m) = span.strip_prefix("MEM:")
            && let Ok(id) = m.parse::<Uuid>()
        {
            member = Some(id);
        } else if let Some(b) = span.strip_prefix("BKG:")
            && let Ok(id) = b.parse::<Uuid>()
        {
            booking = id;
        }
    }
    (member, booking)
}

fn blank(s: Option<&str>) -> bool {
    s.is_none_or(|v| v.trim().is_empty())
}

/// `Same`: trimmed, case-insensitive.
fn same(a: Option<&str>, b: Option<&str>) -> bool {
    match (a.map(str::trim), b.map(str::trim)) {
        (None, None) => true,
        (Some(x), Some(y)) => x.to_lowercase() == y.to_lowercase(),
        _ => false,
    }
}

/// `LinkMismatch`: why the folio is no longer the one the operator linked.
pub fn link_mismatch(
    row: Option<&LinkRow>,
    link: &LinkFolioPayload,
    target: &LinkFolio,
    room_id: &str,
) -> Option<&'static str> {
    let Some(row) = row else {
        return Some("no longer exists");
    };
    if !row.cancel_num.is_zero() {
        return Some("was cancelled");
    }
    if row.confirm_num != Some(link.confirm_num) {
        return Some("moved to another ConfirmNum");
    }
    if !blank(target.room_label.as_deref())
        && !same(
            row.room_type_booked.as_deref(),
            target.room_label.as_deref(),
        )
    {
        return Some("room type changed");
    }
    if !blank(row.external_confirm_number.as_deref())
        && !same(
            row.external_confirm_number.as_deref(),
            Some(&link.external_confirm_number),
        )
    {
        return Some("was stamped for another booking");
    }
    if !blank(row.user_info1.as_deref()) && !same(row.user_info1.as_deref(), Some(room_id)) {
        return Some("was stamped for another room");
    }
    if !blank(row.user_info2.as_deref())
        && parse_user_info2(row.user_info2.as_deref()).1 != link.booking_id
    {
        return Some("holds another booking tag or front-desk text in UserInfo2");
    }
    None
}

impl FolioWriter {
    /// `LinkFoliosAsync`.
    pub fn link_folios(
        &self,
        s: &mut dyn Session,
        link: &LinkFolioPayload,
    ) -> Result<(), WriteError> {
        let schema = self.probe.get(s, "Folio")?;
        let timeout = self.opts.write_lock_timeout_ms;
        let tag = format_user_info2(Some(link.member_id), link.booking_id);
        applock::in_lock(s, &[FOLIO_NUMBER_LOCK], timeout, Some(timeout), |s| {
            for target in &link.folios {
                let rows = s.rows(LINK_ROW_SQL, &[p("@FolioNum", decimal(target.folio_num))])?;
                let row = match rows.first() {
                    Some(r) => Some(LinkRow {
                        cancel_num: r.dec("CancelNum")?,
                        confirm_num: r.opt_dec("ConfirmNum")?,
                        room_type_booked: r.opt_str("RoomTypeBooked")?,
                        external_confirm_number: r.opt_str("ExternalConfirmNumber")?,
                        user_info1: r.opt_str("UserInfo1")?,
                        user_info2: r.opt_str("UserInfo2")?,
                    }),
                    None => None,
                };
                let room_id = target.booking_room_id.to_string();
                if let Some(reason) = link_mismatch(row.as_ref(), link, target, &room_id) {
                    return Err(WriteError::Refused {
                        code: LINK_TARGET_CHANGED.into(),
                        message: format!(
                            "Folio {}: {reason} since it was linked.",
                            target.folio_num
                        ),
                    });
                }
                let Some(row) = row else { continue };
                if same(
                    row.external_confirm_number.as_deref(),
                    Some(&link.external_confirm_number),
                ) && same(row.user_info1.as_deref(), Some(&room_id))
                    && same(row.user_info2.as_deref(), Some(&tag))
                {
                    continue;
                }
                let c = add_last_change(
                    ColumnList::new(&schema)
                        .core(
                            "ExternalConfirmNumber",
                            nvarchar(Some(&link.external_confirm_number)),
                        )
                        .core("UserInfo1", nvarchar(Some(&room_id)))
                        .core("UserInfo2", nvarchar(Some(&tag))),
                )
                .param("@FolioNum", decimal(target.folio_num));
                s.execute(
                    &format!(
                        "UPDATE Folio SET {} WHERE FolioNum = @FolioNum",
                        c.set_clause()?
                    ),
                    c.params(),
                )?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use dl_tds::Value;

    use super::*;
    use crate::writer::recorder::Recorder;
    use crate::writer::session::Rec;

    const BOOKING: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    const MEMBER: &str = "12345678-1234-1234-1234-123456789abc";
    const ROOM: &str = "11111111-2222-3333-4444-555555555555";

    fn link() -> LinkFolioPayload {
        LinkFolioPayload {
            booking_id: BOOKING.parse().unwrap(),
            member_id: MEMBER.parse().unwrap(),
            external_confirm_number: "MM-1".into(),
            confirm_num: Decimal::from(900i64),
            folios: vec![LinkFolio {
                folio_num: Decimal::from(5001i64),
                booking_room_id: ROOM.parse().unwrap(),
                room_label: Some("DLX".into()),
            }],
        }
    }

    fn row(ext: Value, ui1: Value, ui2: Value) -> Rec {
        Rec::new(vec![
            ("CancelNum", Value::Decimal(Decimal::ZERO)),
            ("ConfirmNum", Value::Decimal(Decimal::from(900i64))),
            ("RoomTypeBooked", Value::String("dlx ".into())),
            ("ExternalConfirmNumber", ext),
            ("UserInfo1", ui1),
            ("UserInfo2", ui2),
        ])
    }

    #[test]
    fn a_blank_hand_entered_folio_is_stamped_under_the_lock() {
        let mut r = Recorder::default();
        r.rows("UPDLOCK", vec![row(Value::Null, Value::Null, Value::Null)]);
        FolioWriter::default().link_folios(&mut r, &link()).unwrap();
        let upd = r
            .log
            .iter()
            .find(|s| s.text.starts_with("UPDATE Folio SET"))
            .unwrap();
        assert!(upd.text.starts_with(
            "UPDATE Folio SET ExternalConfirmNumber = @ExternalConfirmNumber, UserInfo1 = @UserInfo1, UserInfo2 = @UserInfo2"
        ));
        let tag = upd.params.iter().find(|p| p.0 == "UserInfo2").unwrap();
        assert_eq!(
            tag.2.as_deref(),
            Some(format!("MEM:{MEMBER};BKG:{BOOKING}").as_str())
        );
        assert!(r.texts().iter().any(|t| t.contains("COMMIT")));
    }

    #[test]
    fn an_already_stamped_folio_is_not_written_again() {
        let mut r = Recorder::default();
        r.rows(
            "UPDLOCK",
            vec![row(
                Value::String("mm-1".into()),
                Value::String(ROOM.into()),
                Value::String(format!("MEM:{MEMBER};BKG:{BOOKING}")),
            )],
        );
        FolioWriter::default().link_folios(&mut r, &link()).unwrap();
        assert!(!r.texts().iter().any(|t| t.starts_with("UPDATE Folio")));
    }

    #[test]
    fn a_changed_target_is_refused_and_rolled_back() {
        let mut r = Recorder::default();
        r.rows(
            "UPDLOCK",
            vec![row(
                Value::Null,
                Value::Null,
                Value::String("front desk".into()),
            )],
        );
        let e = FolioWriter::default()
            .link_folios(&mut r, &link())
            .unwrap_err();
        assert!(matches!(e, WriteError::Refused { ref code, ref message }
            if code == LINK_TARGET_CHANGED && message == "Folio 5001: holds another booking tag or front-desk text in UserInfo2 since it was linked."));
        assert!(r.texts().iter().any(|t| t.contains("ROLLBACK")));
        let mut r = Recorder::default();
        let e = FolioWriter::default()
            .link_folios(&mut r, &link())
            .unwrap_err();
        assert!(
            matches!(e, WriteError::Refused { ref message, .. } if message.contains("no longer exists"))
        );
    }

    #[test]
    fn user_info2_parses_like_dotnet() {
        let (m, b) = parse_user_info2(Some(&format!(" MEM:-; BKG:{BOOKING} ")));
        assert_eq!(m, None);
        assert_eq!(b, BOOKING.parse().unwrap());
        assert_eq!(parse_user_info2(Some("garbage")), (None, Uuid::NIL));
    }
}
