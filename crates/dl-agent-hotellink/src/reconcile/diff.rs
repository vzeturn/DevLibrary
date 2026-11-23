//! The modify path: create missing rooms, update changed fields and rates, cancel what left
//! the booking (Go `createRooms`, `applyDiff`, `updateFolioDiff`, `diffRates`).

use std::collections::{BTreeMap, HashMap, HashSet};

use dl_time::DateTime;

use super::build::{rates_equal, room_rates};
use super::{
    Reconciler, build_folio, item_key, manual_conflict_err, merge_rooms_by_item, next_seq,
    pick_to_cancel, room_item_key,
};
use crate::error::{Error, Result};
use crate::gofmt::f0;
use crate::jobmodel::{Job, Room};
use crate::log::{self, V};
use crate::pms::{ClientSts, Folio, FolioRate, SmileClient, Val, ymd};

fn protected_reason(f: &Folio) -> String {
    format!(
        "folio {} đã check-in/đã xếp phòng → bỏ qua, giữ nguyên",
        f0(f.folio_num)
    )
}

fn skip_message(reasons: &[String]) -> String {
    if reasons.is_empty() {
        String::new()
    } else {
        format!("đã check-in/đã xếp phòng → bỏ qua: {}", reasons.join("; "))
    }
}

impl Reconciler {
    /// `createRooms`. With `guard_create` it first re-reads the booking and creates nothing
    /// when a folio has appeared meanwhile (no double booking on a retry).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn create_rooms(
        &self,
        job: &Job,
        rooms: &[Room],
        confirm_num: f64,
        check_in: DateTime,
        check_out: DateTime,
        ta: Option<&SmileClient>,
        sts: Option<&ClientSts>,
        seq_start_by_item: Option<&HashMap<String, i64>>,
        guard_create: bool,
    ) -> Result<()> {
        if guard_create {
            let live = self.find_existing_folios(job)?;
            if !live.is_empty() {
                log::info(
                    "createRooms: bỏ qua tạo phòng, folio đã tồn tại (chống đúp)",
                    &[
                        ("bookingId", V::S(&job.booking_id)),
                        ("count", V::I(live.len() as i64)),
                    ],
                );
                return Ok(());
            }
        }
        for room in rooms {
            if room.rates().is_empty() {
                return Err(Error::other(format!(
                    "booking {} room {}: thiếu rates",
                    job.booking_id, room.room_type_code
                )));
            }
            let key = room_item_key(&room.room_type_code, &room.rate_plan_id);
            let seq_start = seq_start_by_item
                .and_then(|m| m.get(&key).copied())
                .unwrap_or(0);
            for i in 0..room.quantity.max(0) {
                let folio_num = self.repo.get_next_folio_num()?;
                let (mut f, rates, an) = build_folio(
                    job,
                    room,
                    folio_num,
                    confirm_num,
                    check_in,
                    check_out,
                    ta,
                    sts,
                    self.now_local(),
                )?;
                f.user_info1 = format!("{key}#{}", seq_start + i);
                self.repo.insert_folio_with_rates(&f, &rates, &an)?;
                log::info(
                    "đã tạo folio",
                    &[
                        ("folioNum", V::S(&f0(folio_num))),
                        ("confirmNum", V::S(&f0(confirm_num))),
                        ("bookingId", V::S(&job.booking_id)),
                        ("roomType", V::S(&room.room_type_code)),
                        ("itemKey", V::S(&f.user_info1)),
                    ],
                );
            }
        }
        Ok(())
    }

    /// `applyDiff`. Returns the skip reason ("" when nothing was skipped). Order: check every
    /// conflict first, then update and create, and cancel last, so a failed create never
    /// leaves the guest without a room.
    pub(super) fn apply_diff(
        &self,
        job: &Job,
        existing: &[Folio],
        check_in: DateTime,
        check_out: DateTime,
        ta: Option<&SmileClient>,
        sts: Option<&ClientSts>,
    ) -> Result<String> {
        let confirm_num = existing[0].confirm_num;
        // BTreeMap: Go ranges a map here (random order); sorted keys make it deterministic.
        let mut cur_by_item: BTreeMap<String, Vec<Folio>> = BTreeMap::new();
        for f in existing {
            cur_by_item.entry(item_key(f)).or_default().push(f.clone());
        }
        self.log_merge_conflicts(job);
        let want_rooms = merge_rooms_by_item(job.rooms());
        let job_items: HashSet<String> = want_rooms
            .iter()
            .map(|r| room_item_key(&r.room_type_code, &r.rate_plan_id))
            .collect();

        let op = self.op_modify();
        let in_house = || {
            manual_conflict_err(format!(
                "booking {}: folio đang ở (in-house) cần hủy để đổi loại phòng — từ chối hủy tự động, không tạo lại",
                job.booking_id
            ))
        };
        let mut skipped: Vec<String> = Vec::new();
        let mut has_disappeared_skip = false;
        let mut cancel_by_item: HashMap<String, Vec<Folio>> = HashMap::new();
        let mut reduce_cancel_by_item: HashMap<String, Vec<Folio>> = HashMap::new();

        // PHASE 0: classify and check conflicts; nothing is written.
        for (key, cur) in &cur_by_item {
            if job_items.contains(key) {
                for f in cur {
                    if self.is_protected_folio(f, op) {
                        skipped.push(protected_reason(f));
                    }
                }
                for room in &want_rooms {
                    if room_item_key(&room.room_type_code, &room.rate_plan_id) != *key {
                        continue;
                    }
                    let have = cur.len() as i64;
                    if room.quantity < have {
                        let to_cancel = pick_to_cancel(cur, (have - room.quantity) as usize);
                        let mut cancellable = Vec::new();
                        for f in to_cancel {
                            if self.is_protected_folio(&f, op) {
                                skipped.push(protected_reason(&f));
                            } else {
                                cancellable.push(f);
                            }
                        }
                        let conflict = !cancellable.is_empty()
                            && self.has_conflicting_folios(&cancellable, op)?;
                        reduce_cancel_by_item.insert(key.clone(), cancellable);
                        if conflict {
                            return Err(in_house());
                        }
                    }
                }
            } else {
                let mut cancellable = Vec::new();
                for f in cur {
                    if self.is_protected_folio(f, op) {
                        skipped.push(protected_reason(f));
                        has_disappeared_skip = true;
                    } else {
                        cancellable.push(f.clone());
                    }
                }
                let conflict =
                    !cancellable.is_empty() && self.has_conflicting_folios(&cancellable, op)?;
                cancel_by_item.insert(key.clone(), cancellable);
                if conflict {
                    return Err(in_house());
                }
            }
        }
        // An item left the booking but a front-desk-owned folio of it stays: change nothing.
        if has_disappeared_skip {
            return Ok(skip_message(&skipped));
        }

        // PHASE A: update survivors, create what is missing.
        let mut seq_start_by_item: HashMap<String, i64> = HashMap::new();
        for room in &want_rooms {
            let key = room_item_key(&room.room_type_code, &room.rate_plan_id);
            let empty = Vec::new();
            let cur = cur_by_item.get(&key).unwrap_or(&empty);
            let cancel_ids: HashSet<i32> = reduce_cancel_by_item
                .get(&key)
                .map(|v| v.iter().map(|f| f.id).collect())
                .unwrap_or_default();
            for f in cur {
                if cancel_ids.contains(&f.id) || self.is_protected_folio(f, op) {
                    continue; // about to be cancelled, or owned by the front desk
                }
                self.update_folio_diff(job, room, f, check_in, check_out)?;
            }
            let have = cur.len() as i64;
            if room.quantity > have {
                seq_start_by_item.insert(key.clone(), next_seq(cur));
                let mut add = room.clone();
                add.quantity = room.quantity - have;
                // A failed create has cancelled nothing yet, so a retry is safe.
                self.create_rooms(
                    job,
                    std::slice::from_ref(&add),
                    confirm_num,
                    check_in,
                    check_out,
                    ta,
                    sts,
                    Some(&seq_start_by_item),
                    false,
                )?;
            }
        }

        // PHASE B: cancel the surplus and the items that left the booking.
        for key in cur_by_item.keys() {
            if job_items.contains(key) {
                if let Some(cancellable) = reduce_cancel_by_item.get(key)
                    && !cancellable.is_empty()
                {
                    self.repo.cancel_folios(cancellable).map_err(|e| {
                        e.context(format!(
                            "booking {}: hủy folio dư thất bại: ",
                            job.booking_id
                        ))
                    })?;
                }
            } else if let Some(cancellable) = cancel_by_item.get(key)
                && !cancellable.is_empty()
            {
                self.repo.cancel_folios(cancellable).map_err(|e| {
                    e.context(format!(
                        "booking {}: hủy folio item rời khỏi booking thất bại: ",
                        job.booking_id
                    ))
                })?;
                log::info(
                    "hủy folio item rời khỏi booking",
                    &[
                        ("bookingId", V::S(&job.booking_id)),
                        ("itemKey", V::S(key)),
                        ("count", V::I(cancellable.len() as i64)),
                    ],
                );
            }
        }
        Ok(skip_message(&skipped))
    }

    /// `updateFolioDiff`. The folio query reads `LastName`, `TelNumM` and `MailM`, so the
    /// guest fields are only written when they changed (RT-0651).
    fn update_folio_diff(
        &self,
        job: &Job,
        room: &Room,
        f: &Folio,
        check_in: DateTime,
        check_out: DateTime,
    ) -> Result<()> {
        let mut set: BTreeMap<&'static str, Val> = BTreeMap::new();
        if !room.room_type_code.is_empty() && f.room_type_code != room.room_type_code {
            set.insert("RoomTypeCode", Val::Str(room.room_type_code.clone()));
            set.insert("RoomTypeBooked", Val::Str(room.room_type_code.clone()));
            let folio_num = f0(f.folio_num);
            if !f.room_code.is_empty() {
                set.insert("RoomCode", Val::Str(String::new()));
                log::warn(
                    "HL đổi loại phòng — đã GỠ xếp phòng cũ, cần lễ tân xếp lại",
                    &[
                        ("bookingId", V::S(&job.booking_id)),
                        ("folioNum", V::S(&folio_num)),
                        ("loaiCu", V::S(&f.room_type_code)),
                        ("loaiMoi", V::S(&room.room_type_code)),
                        ("phongCuDaGo", V::S(&f.room_code)),
                    ],
                );
            } else {
                log::info(
                    "HL đổi loại phòng (chưa xếp phòng)",
                    &[
                        ("bookingId", V::S(&job.booking_id)),
                        ("folioNum", V::S(&folio_num)),
                        ("loaiCu", V::S(&f.room_type_code)),
                        ("loaiMoi", V::S(&room.room_type_code)),
                    ],
                );
            }
        }
        if f.arrival_date != check_in {
            set.insert("ArrivalDate", Val::Time(check_in));
        }
        if f.departure_date != check_out {
            set.insert("DepartureDate", Val::Time(check_out));
        }
        if i64::from(f.num_adult) != room.adults {
            set.insert("NumAdult", Val::I32(room.adults as i32));
        }
        if i64::from(f.num_child) != room.children {
            set.insert("NumChild", Val::I32(room.children as i32));
        }
        if f.last_name != job.guest_name {
            set.insert("LastName", Val::Str(job.guest_name.clone()));
        }
        if f.tel_num_m != job.guest_phone {
            set.insert("TelNumM", Val::Str(job.guest_phone.clone()));
        }
        if f.mail_m != job.guest_email {
            set.insert("MailM", Val::Str(job.guest_email.clone()));
        }
        let Some(first_rate) = room.rates().first() else {
            return Err(Error::other(format!(
                "booking {} room {}: thiếu rates (modify path)",
                job.booking_id, room.room_type_code
            )));
        };
        if f.rate_amount != first_rate.rate {
            set.insert("RateAmount", Val::F64(first_rate.rate));
        }
        if f.include_bf != room.include_bf {
            set.insert("IncludeBF", Val::Bool(room.include_bf));
        }
        if f.breakfast_code != room.breakfast_code {
            set.insert("BreakfastCode", Val::Str(room.breakfast_code.clone()));
        }
        let (gbf, cbf) = if room.include_bf {
            (room.adults, room.children)
        } else {
            (0, 0)
        };
        if i64::from(f.num_guest_bf) != gbf {
            set.insert("NumGuestBF", Val::I32(gbf as i32));
        }
        if i64::from(f.num_child_bf) != cbf {
            set.insert("NumChildBF", Val::I32(cbf as i32));
        }
        if !set.is_empty() {
            self.repo.update_folio_fields(f.folio_num, &set)?;
            let fields: Vec<&str> = set.keys().copied().collect();
            log::info(
                "cập nhật folio (field đổi)",
                &[
                    ("folioNum", V::S(&f0(f.folio_num))),
                    ("bookingId", V::S(&job.booking_id)),
                    ("fields", V::S(&fields.join(","))),
                ],
            );
        }
        let rates_changed = self.diff_rates(room, f.folio_num)?;
        // AdditionalName: name, e-mail or dates changed. The dates are written explicitly
        // because SMILE's Folio trigger only narrows them, never extends (RT-0651).
        let name_changed = ["LastName", "MailM", "ArrivalDate", "DepartureDate"]
            .iter()
            .any(|k| set.contains_key(k));
        if name_changed {
            self.repo.update_additional_name(
                f.folio_num,
                &job.guest_name,
                &job.guest_email,
                check_in,
                check_out,
            )?;
        }
        // LAST CHANGE: update_folio_fields stamped it when a Folio column changed; when only
        // a child row did, stamp it separately so the folio still records the change.
        if set.is_empty() && (rates_changed || name_changed) {
            self.repo.stamp_folio_change(f.folio_num)?;
        }
        Ok(())
    }

    /// `diffRates`: replace the folio's rates only when a date or amount differs; true when
    /// they were replaced. Nightly rates live outside the Folio table, so SMILE's FolioLog
    /// trigger cannot see them: one "FolioRate" FolioLog row (old → new) records it. A
    /// failed log write does not stop the booking sync (the rates are right; only one
    /// history row is missing), it is logged as a warning.
    fn diff_rates(&self, room: &Room, folio_num: f64) -> Result<bool> {
        let cur = self.repo.get_folio_rates(folio_num)?;
        let want = room_rates(room, folio_num)?;
        if rates_equal(&cur, &want) {
            return Ok(false);
        }
        self.repo.replace_folio_rates(folio_num, &want)?;
        if let Err(e) = self.repo.append_folio_log(
            folio_num,
            "FolioRate",
            &rates_summary(&cur),
            &rates_summary(&want),
        ) {
            log::warn(
                "không ghi được FolioLog cho thay đổi giá đêm (giá đã cập nhật)",
                &[
                    ("folioNum", V::S(&f0(folio_num))),
                    ("error", V::S(e.message())),
                ],
            );
        }
        Ok(true)
    }
}

/// `ratesSummary`: "yyyy-mm-dd=amount;..." in date order, for FolioLog. The amount is the
/// shortest decimal form, like Go's `strconv.FormatFloat(x, 'f', -1, 64)`.
pub(super) fn rates_summary(rs: &[FolioRate]) -> String {
    rs.iter()
        .map(|x| format!("{}={}", ymd(x.h_date), x.rate_amount))
        .collect::<Vec<_>>()
        .join(";")
}
