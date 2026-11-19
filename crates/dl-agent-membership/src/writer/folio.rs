//! `FolioWriter`: the booking write (`WriteBookingAsync`), one room's create and update
//! (`Reservation`), cancel (`Cancel`) and the guest note (`Note`, `ExtraBed`). The
//! agent-marked `Folio_Special` rows (`ExtraBedSpecial`) are in
//! [`super::special_rows`]. Every statement and parameter mirrors the .NET agent.

use std::sync::Arc;

use dl_decimal::Decimal;

use super::additional_name;
use super::applock::{self, CANCEL_NUMBER_LOCK, DEFAULT_TIMEOUT_MS, FOLIO_NUMBER_LOCK};
use super::lookup;
use super::schema::{ColumnList, Mode, SchemaProbe, TableSchema};
use super::session::{Rec, Session, bit, day, decimal, int, nvarchar, p, varchar};
use super::special_rows::HubSpecialCode;
use super::{CLERK_APP, format_user_info2, tcvn3};
use crate::jobs::beds::{
    BedDecision, GUEST_COUNT_MISMATCH, GuestPlan, MODE_SMILE_SPECIAL, compose_note,
    extract_bed_marker, night_rate, plan_guests,
};
use crate::jobs::payload::Room;
use crate::jobs::plan::{BookingPlan, WriteContext, WriteError, Written};
use crate::jobs::preflight::decide_extra_bed;
use crate::log::{self, V};
use crate::smile::SmileError;

/// `FolioWriter.NoteColumn`.
pub const NOTE_COLUMN: &str = "UserInfo3";

const DUPE_SQL: &str = "SELECT TOP 1 FolioNum, ISNULL(ConfirmNum, 0) AS ConfirmNum
FROM Folio
WHERE ExternalConfirmNumber = @Ext AND UserInfo1 = @Room AND ISNULL(CancelNum, 0) = 0";

/// `SmileWriteOptions`: the write-side settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteOptions {
    /// `WriteLockTimeoutMs` (`SET LOCK_TIMEOUT` and the applock wait of a booking write).
    pub write_lock_timeout_ms: i32,
    /// `DefaultPaymentCode` (`None`: the column is not written).
    pub default_payment_code: Option<String>,
    /// `DefaultBreakfastCode` (`None`: breakfast is not written).
    pub default_breakfast_code: Option<String>,
    /// `ExtraBedMode`.
    pub extra_bed_mode: String,
    /// `ExtraBedSpecialCode`: the configured code (the hub's choice wins, see
    /// [`FolioWriter::effective_special_code`]).
    pub extra_bed_special_code: String,
    /// `HubExtraBedSpecialCode`: the hub's sync-config choice (runtime state).
    pub hub_special_code: HubSpecialCode,
    /// `SmileHotelId` (`None`: the only `HotelID` SMILE has).
    pub smile_hotel_id: Option<String>,
}

impl Default for WriteOptions {
    fn default() -> WriteOptions {
        WriteOptions {
            write_lock_timeout_ms: 10_000,
            default_payment_code: Some("1".into()),
            default_breakfast_code: Some("BFF".into()),
            extra_bed_mode: MODE_SMILE_SPECIAL.into(),
            extra_bed_special_code: "EB".into(),
            hub_special_code: HubSpecialCode::default(),
            smile_hotel_id: None,
        }
    }
}

/// The four tables a reservation write touches, probed before the transaction.
pub(crate) struct Schemas {
    pub folio: Arc<TableSchema>,
    pub rate: Arc<TableSchema>,
    pub add_name: Arc<TableSchema>,
    pub special: Arc<TableSchema>,
}

/// `FolioWriter`.
#[derive(Debug, Default)]
pub struct FolioWriter {
    pub(crate) opts: WriteOptions,
    pub(crate) probe: SchemaProbe,
}

fn first(rows: Vec<Rec>, col: &'static str) -> Result<Rec, WriteError> {
    rows.into_iter()
        .next()
        .ok_or(WriteError::Smile(SmileError::Decode(col)))
}

fn null_if_blank(s: &str) -> Option<&str> {
    let t = s.trim();
    (!t.is_empty()).then_some(t)
}

/// `FirstNightRate`: the rate of the earliest night (0 without nights).
pub fn first_night_rate(room: &Room, use_net_excl_bed: bool) -> Decimal {
    room.nights
        .iter()
        .min_by_key(|n| n.date)
        .map_or(Decimal::ZERO, |n| night_rate(n, use_net_excl_bed))
}

/// `AddLastChange`.
pub(crate) fn add_last_change(c: ColumnList<'_>) -> ColumnList<'_> {
    c.opt_lit("LastChangeTime", "GETDATE()")
        .opt("LastChangeClerkID", varchar(Some(CLERK_APP)))
}

impl FolioWriter {
    /// A writer with these options.
    pub fn new(opts: WriteOptions) -> FolioWriter {
        FolioWriter {
            opts,
            probe: SchemaProbe::default(),
        }
    }

    /// `ProbeReservationTablesAsync` (outside any transaction).
    pub(crate) fn schemas(&self, s: &mut dyn Session) -> Result<Schemas, WriteError> {
        Ok(Schemas {
            folio: self.probe.get(s, "Folio")?,
            rate: self.probe.get(s, "FolioRate")?,
            add_name: self.probe.get(s, "AdditionalName")?,
            special: self.probe.get(s, "Folio_Special")?,
        })
    }

    /// `WriteBookingAsync`: every room of the booking in one transaction under the counter
    /// applocks (folio number, then cancel number), with `SET LOCK_TIMEOUT`.
    pub fn write_booking(
        &self,
        s: &mut dyn Session,
        plan: &BookingPlan,
    ) -> Result<Written, WriteError> {
        let note_folios: &[Decimal] = if plan.ctx.note.is_none() {
            &[]
        } else {
            plan.note_folios.as_deref().unwrap_or(&[])
        };
        if plan.creates.is_empty()
            && plan.updates.is_empty()
            && plan.cancels.is_empty()
            && note_folios.is_empty()
        {
            return Ok(Written {
                created: Vec::new(),
                confirm_num: plan.shared_confirm_num,
            });
        }
        let sc = self.schemas(s)?;
        let locks: &[&str] = if plan.cancels.is_empty() {
            &[FOLIO_NUMBER_LOCK]
        } else {
            &[FOLIO_NUMBER_LOCK, CANCEL_NUMBER_LOCK]
        };
        let timeout = self.opts.write_lock_timeout_ms;
        let decide = |room: &Room| {
            decide_extra_bed(room, plan.extra_bed.as_ref(), &self.opts.extra_bed_mode)
        };
        applock::in_lock(s, locks, timeout, Some(timeout), |s| {
            let mut confirm = plan.shared_confirm_num;
            let mut created = Vec::with_capacity(plan.creates.len());
            for room in &plan.creates {
                let (folio, conf) = self.create_in_tx(
                    s,
                    &sc,
                    &plan.ctx,
                    room,
                    &decide(room),
                    confirm,
                    plan.profile_number,
                )?;
                confirm.get_or_insert(conf);
                created.push((room.room_item_id, folio));
            }
            for (folio, room) in &plan.updates {
                self.update_in_tx(s, &sc, *folio, room, &decide(room))?;
            }
            for folio in &plan.cancels {
                self.cancel_in_tx(s, &sc.folio, *folio)?;
            }
            for folio in note_folios {
                self.write_note_in_tx(s, &sc.folio, *folio, plan.ctx.note.as_deref(), None, true)?;
            }
            Ok(Written {
                created,
                confirm_num: confirm,
            })
        })
    }

    /// `CreateFolioInTxAsync`: (folio number, confirm number).
    #[allow(clippy::too_many_arguments)]
    fn create_in_tx(
        &self,
        s: &mut dyn Session,
        sc: &Schemas,
        ctx: &WriteContext,
        room: &Room,
        bed: &BedDecision,
        shared_confirm: Option<Decimal>,
        profile_number: Decimal,
    ) -> Result<(Decimal, Decimal), WriteError> {
        let room_id = room.room_item_id.to_string();
        let dupe = s.rows(
            DUPE_SQL,
            &[
                p("@Ext", nvarchar(Some(&ctx.confirmation_code))),
                p("@Room", nvarchar(Some(&room_id))),
            ],
        )?;
        if let Some(r) = dupe.first() {
            let folio = r.dec("FolioNum")?;
            let existing_confirm = r.dec("ConfirmNum")?;
            self.update_in_tx(s, sc, folio, room, bed)?;
            return Ok((folio, shared_confirm.unwrap_or(existing_confirm)));
        }
        if !ctx.force_create {
            lookup::refuse_hand_entered_duplicate(s, room)?;
        }
        let counter = first(s.call("GetFolioNumber", &[])?.rows, "FolioNumber")?;
        let folio_num = counter.dec("FolioNumber")?;
        let sub = i32::try_from(counter.int("FolioSubNumber")?)
            .map_err(|_| WriteError::Smile(SmileError::Decode("FolioSubNumber")))?;
        let confirm = match shared_confirm {
            Some(c) => c,
            None => first(s.call("GetConfirmNum", &[])?.rows, "ConfNumber")?.dec("ConfNumber")?,
        };
        let origin = ctx
            .travel_agent
            .as_ref()
            .map_or(ctx.origin_code.as_str(), |ta| {
                ta.effective_origin(&ctx.origin_code)
            });
        let plan = plan_guests(room, sc.add_name.has("IsChild"));
        let insert = self.folio_insert(
            &sc.folio,
            ctx,
            room,
            (folio_num, sub, confirm, profile_number),
            origin,
            bed,
            &plan,
        );
        s.execute(&insert.insert_sql()?, insert.params())?;
        self.insert_rates(s, &sc.rate, folio_num, room, bed.use_net_excl_bed)?;
        if !bed.special_rows.is_empty() {
            self.write_mm_bed_rows_in_tx(s, &sc.special, folio_num, room.room_item_id, bed)?;
        }
        log_guest_plan(&plan, room, Some(folio_num));
        let keys = additional_name::GuestKeys {
            confirm_num: Some(confirm),
            origin_code: Some(origin.to_owned()),
        };
        additional_name::insert_rows(
            s,
            &sc.add_name,
            folio_num,
            room,
            &keys,
            plan.placeholder_adults,
            false,
        )?;
        if plan.placeholder_children > 0 {
            additional_name::insert_rows(
                s,
                &sc.add_name,
                folio_num,
                room,
                &keys,
                plan.placeholder_children,
                true,
            )?;
        }
        Ok((folio_num, confirm))
    }

    /// `BuildFolioInsert`. `nums` is (folio, sub folio, confirm, profile).
    #[allow(clippy::too_many_arguments)]
    fn folio_insert<'a>(
        &self,
        schema: &'a TableSchema,
        ctx: &WriteContext,
        room: &Room,
        nums: (Decimal, i32, Decimal, Decimal),
        origin: &str,
        bed: &BedDecision,
        plan: &GuestPlan,
    ) -> ColumnList<'a> {
        let (folio_num, sub, confirm, profile) = nums;
        let ext = Some(ctx.confirmation_code.as_str());
        let mut c = ColumnList::new(schema)
            .core("FolioNum", decimal(folio_num))
            .core("FolioSubNum", int(sub))
            .core("ConfirmNum", decimal(confirm))
            .core("ExternalConfirmNumber", nvarchar(ext))
            .core("ExternalFolioNumber", nvarchar(ext))
            .core("UserInfo1", nvarchar(Some(&room.room_item_id.to_string())))
            .core(
                "UserInfo2",
                nvarchar(Some(&format_user_info2(ctx.member_id, ctx.booking_id))),
            )
            .core("RoomTypeBooked", nvarchar(Some(&room.room_type_code)))
            .core("ArrivalDate", day(room.arrival))
            .core("DepartureDate", day(room.departure))
            .core("NumAdult", int(plan.num_adult))
            .core("NumChild", int(plan.num_child))
            .core(
                "RateAmount",
                decimal(first_night_rate(room, bed.use_net_excl_bed)),
            )
            .core(
                "LastName",
                varchar(Some(&tcvn3::encode(&room.guest_last_name))),
            )
            .core("OriginCode", nvarchar(Some(origin)))
            .core("ProfileNumber", decimal(profile))
            .core_lit("FolioStatus", "1")
            .core_lit("CancelNum", "0")
            .core_lit("RoomCode", "''");
        if let Some(ta) = &ctx.travel_agent {
            c = c
                .core("TravelAgent1Code", dl_tds::ParamValue::I64(Some(ta.code)))
                .core_lit("TravelAgent2Code", "0")
                .core("ContactName", nvarchar(ta.contact_name().as_deref()))
                .core("MarketSegmentCode", nvarchar(ta.market_segment.as_deref()))
                .core(
                    "MarketBusinessCode",
                    nvarchar(ta.market_business.as_deref()),
                )
                .core("MarketChannelCode", nvarchar(ta.market_channel.as_deref()))
                .core("Geographic1Code", nvarchar(ta.geographic1.as_deref()))
                .core("Geographic2Code", nvarchar(ta.geographic2.as_deref()))
                .core_lit("SourceGeographic1Code", "@Geographic1Code")
                .core_lit("SourceGeographic2Code", "@Geographic2Code");
            for i in 0..crate::jobs::travel_agent::USER_DEFINE_COUNT {
                let v = ta.user_defines.get(i).cloned().flatten();
                c = c.core(&format!("UserDefineCode{}", i + 1), nvarchar(v.as_deref()));
            }
        }
        let note = self.encode_create_note(schema, ctx.note.as_deref(), bed.bed_note.as_deref());
        c = c
            .opt_lit("ReservationTime", "GETDATE()")
            .opt("ReservationClerkID", varchar(Some(CLERK_APP)))
            .opt("NetRate", bit(true))
            .opt("Booker", nvarchar(ext))
            .opt("ConfirmPrintFlag", bit(false))
            .opt("DscPcnt", decimal(Decimal::ZERO))
            .opt("DscAmount", decimal(Decimal::ZERO))
            .opt("ExtraBed", int(bed.extra_bed_to_write))
            .opt("NumEnf", int(plan.num_enf))
            .opt(NOTE_COLUMN, varchar(note.as_deref()));
        c = add_last_change(c);
        c = self.add_breakfast(c, schema);
        let guest = room.guest.as_ref();
        c.opt("BookStatus", dl_tds::ParamValue::U8(Some(1)))
            .opt("RateOverideFlag", bit(true))
            .opt(
                "PaymentCode",
                nvarchar(self.opts.default_payment_code.as_deref()),
            )
            .opt("FolioCurrencyCode", nvarchar(Some("VND")))
            .opt("FolioExRate", decimal(Decimal::ONE))
            .opt("UpdateExRateAtCI", bit(true))
            .opt("NumRoom", int(1))
            .opt("WalkInFlag", bit(false))
            .opt(
                "FirstName",
                varchar(
                    null_if_blank(&room.guest_first_name)
                        .map(tcvn3::encode)
                        .as_deref(),
                ),
            )
            .opt("RoomTypeCode", nvarchar(Some(&room.room_type_code)))
            .opt("TelNumM", nvarchar(guest.and_then(|g| g.phone.as_deref())))
            .opt("MailM", nvarchar(guest.and_then(|g| g.email.as_deref())))
    }

    /// `AddBreakfast`.
    fn add_breakfast<'a>(&self, c: ColumnList<'a>, schema: &TableSchema) -> ColumnList<'a> {
        let Some(code) = self.opts.default_breakfast_code.as_deref() else {
            return c;
        };
        if !schema.has("BreakfastCode") {
            return c;
        }
        if let Some(max) = schema.max_length("BreakfastCode")
            && code.chars().count() > max
        {
            log::warn(
                "SMILE_FO column Folio.BreakfastCode is too narrow; breakfast not written",
                &[
                    ("length", V::I(code.chars().count() as i64)),
                    ("max_length", V::I(max as i64)),
                ],
            );
            return c;
        }
        c.opt("IncludeBF", bit(true))
            .opt("BreakfastCode", varchar(Some(code)))
    }

    /// `UpdateFolioInTxAsync`.
    pub(crate) fn update_in_tx(
        &self,
        s: &mut dyn Session,
        sc: &Schemas,
        folio_num: Decimal,
        room: &Room,
        bed: &BedDecision,
    ) -> Result<(), WriteError> {
        let guest = room.guest.as_ref();
        let counts = plan_guests(room, sc.add_name.has("IsChild"));
        log_guest_plan(&counts, room, Some(folio_num));
        let c = ColumnList::new(&sc.folio)
            .core("RoomTypeBooked", nvarchar(Some(&room.room_type_code)))
            .core("ArrivalDate", day(room.arrival))
            .core("DepartureDate", day(room.departure))
            .core("NumAdult", int(counts.num_adult))
            .core("NumChild", int(counts.num_child))
            .opt("NumEnf", dl_tds::ParamValue::I32(counts.num_enf_update))
            .core(
                "RateAmount",
                decimal(first_night_rate(room, bed.use_net_excl_bed)),
            )
            .core(
                "LastName",
                varchar(Some(&tcvn3::encode(&room.guest_last_name))),
            )
            .opt("ExtraBed", int(bed.extra_bed_to_write))
            .opt_mode(
                "FirstName",
                varchar(
                    null_if_blank(&room.guest_first_name)
                        .map(tcvn3::encode)
                        .as_deref(),
                ),
                Mode::KeepWhenNull,
            )
            .opt("RoomTypeCode", nvarchar(Some(&room.room_type_code)))
            .opt("RateOverideFlag", bit(true))
            .opt_mode(
                "TelNumM",
                nvarchar(guest.and_then(|g| g.phone.as_deref())),
                Mode::KeepWhenNull,
            )
            .opt_mode(
                "MailM",
                nvarchar(guest.and_then(|g| g.email.as_deref())),
                Mode::KeepWhenNull,
            );
        let c = add_last_change(c).param("@FolioNum", decimal(folio_num));
        s.execute(
            &format!(
                "UPDATE Folio SET {} WHERE FolioNum = @FolioNum",
                c.set_clause()?
            ),
            c.params(),
        )?;
        s.execute(
            "DELETE FROM FolioRate WHERE FolioNum = @FolioNum",
            &[p("@FolioNum", decimal(folio_num))],
        )?;
        self.insert_rates(s, &sc.rate, folio_num, room, bed.use_net_excl_bed)?;
        additional_name::update(s, &sc.add_name, folio_num, room)?;
        if sc.special.has("SPNotes") {
            self.write_mm_bed_rows_in_tx(s, &sc.special, folio_num, room.room_item_id, bed)?;
        }
        self.write_note_in_tx(
            s,
            &sc.folio,
            folio_num,
            None,
            bed.bed_note.as_deref(),
            false,
        )
    }

    /// `InsertRatesAsync`: one `FolioRate` row per night, in payload order.
    fn insert_rates(
        &self,
        s: &mut dyn Session,
        schema: &TableSchema,
        folio_num: Decimal,
        room: &Room,
        use_net_excl_bed: bool,
    ) -> Result<(), WriteError> {
        warn_if_nights_mismatch(folio_num, room);
        for night in &room.nights {
            let c = ColumnList::new(schema)
                .core("FolioNum", decimal(folio_num))
                .core("hDate", day(night.date))
                .core("ToDate", day(night.date))
                .core("RateAmount", decimal(night_rate(night, use_net_excl_bed)))
                .core_lit("isCancel", "0")
                .opt("RateOverride", int(1))
                .opt("CreateBy", varchar(Some(CLERK_APP)))
                .opt_lit("CreateTime", "GETDATE()")
                .opt("DscPcnt", decimal(Decimal::ZERO))
                .opt("DscAmount", decimal(Decimal::ZERO));
            s.execute(&c.insert_sql()?, c.params())?;
        }
        Ok(())
    }

    /// `CancelFolioAsync`: its own transaction under the cancel-number applock.
    pub fn cancel_folio(
        &self,
        s: &mut dyn Session,
        folio_num: Decimal,
    ) -> Result<Decimal, WriteError> {
        let schema = self.probe.get(s, "Folio")?;
        applock::in_lock(s, &[CANCEL_NUMBER_LOCK], DEFAULT_TIMEOUT_MS, None, |s| {
            self.cancel_in_tx(s, &schema, folio_num)
        })
    }

    /// `CancelFolioInTxAsync`: the new cancel number.
    fn cancel_in_tx(
        &self,
        s: &mut dyn Session,
        schema: &TableSchema,
        folio_num: Decimal,
    ) -> Result<Decimal, WriteError> {
        let cancel =
            first(s.call("GetCancelNum", &[])?.rows, "CancelNumber")?.dec("CancelNumber")?;
        let c = ColumnList::new(schema)
            .core("CancelNum", decimal(cancel))
            .opt_lit("CancelTime", "GETDATE()")
            .opt("CancelClerkID", varchar(Some(CLERK_APP)));
        let c = add_last_change(c).param("@FolioNum", decimal(folio_num));
        s.execute(
            &format!(
                "UPDATE Folio SET {} WHERE FolioNum = @FolioNum",
                c.set_clause()?
            ),
            c.params(),
        )?;
        s.execute(
            "UPDATE AdditionalName SET ACancelNum = @CancelNum WHERE FolioNum = @FolioNum",
            &[
                p("@FolioNum", decimal(folio_num)),
                p("@CancelNum", decimal(cancel)),
            ],
        )?;
        Ok(cancel)
    }

    /// `EncodeNote`: TCVN3 of the trimmed note, cut to the column width.
    fn encode_note(&self, folio: &TableSchema, note: Option<&str>) -> Option<String> {
        let note = note.map(str::trim).filter(|n| !n.is_empty())?;
        if !folio.has(NOTE_COLUMN) {
            return None;
        }
        let encoded = tcvn3::encode(note);
        let len = encoded.chars().count();
        match folio.max_length(NOTE_COLUMN) {
            Some(max) if len > max => {
                log::info(
                    "Booking note is wider than Folio.UserInfo3; truncated",
                    &[
                        ("length", V::I(len as i64)),
                        ("max_length", V::I(max as i64)),
                    ],
                );
                let cut: String = encoded.chars().take(max).collect();
                Some(cut.trim_end().to_owned())
            }
            _ => Some(encoded),
        }
    }

    /// `EncodeCreateNote`.
    fn encode_create_note(
        &self,
        folio: &TableSchema,
        guest_note: Option<&str>,
        bed_note: Option<&str>,
    ) -> Option<String> {
        let Some(bed) = bed_note else {
            return self.encode_note(folio, guest_note);
        };
        if !folio.has(NOTE_COLUMN) {
            return None;
        }
        compose_note(
            self.encode_note(folio, guest_note).as_deref(),
            Some(bed),
            folio.max_length(NOTE_COLUMN),
            None,
        )
    }

    /// `WriteFolioNoteInTxAsync`: base (the hub's note, else what is there) plus the bed
    /// marker; nothing is written when the value is already there.
    fn write_note_in_tx(
        &self,
        s: &mut dyn Session,
        folio: &TableSchema,
        folio_num: Decimal,
        guest_note: Option<&str>,
        bed_note: Option<&str>,
        keep_existing_marker: bool,
    ) -> Result<(), WriteError> {
        if !folio.has(NOTE_COLUMN) {
            return Ok(());
        }
        let rows = s.rows(
            "SELECT UserInfo3 FROM Folio WHERE FolioNum = @FolioNum",
            &[p("@FolioNum", decimal(folio_num))],
        )?;
        let current = match rows.first() {
            Some(r) => r.opt_str("UserInfo3")?,
            None => None,
        };
        let marker = extract_bed_marker(current.as_deref());
        if guest_note.is_none() && bed_note.is_none() && !keep_existing_marker && marker.is_none() {
            return Ok(());
        }
        let base = match guest_note {
            None => current.clone(),
            Some(_) => self.encode_note(folio, guest_note),
        };
        let wanted = compose_note(
            base.as_deref(),
            if keep_existing_marker { None } else { bed_note },
            folio.max_length(NOTE_COLUMN),
            if keep_existing_marker {
                marker.as_deref()
            } else {
                None
            },
        );
        if wanted == current || (wanted.is_none() && current.as_deref().unwrap_or("").is_empty()) {
            return Ok(());
        }
        let c = ColumnList::new(folio).opt(NOTE_COLUMN, varchar(wanted.as_deref()));
        let c = add_last_change(c).param("@FolioNum", decimal(folio_num));
        s.execute(
            &format!(
                "UPDATE Folio SET {} WHERE FolioNum = @FolioNum",
                c.set_clause()?
            ),
            c.params(),
        )?;
        Ok(())
    }
}

fn warn_if_nights_mismatch(folio_num: Decimal, room: &Room) {
    let stay = i64::from(room.arrival.days_until(room.departure));
    if !room.nights.is_empty() && room.nights.len() as i64 == stay {
        return;
    }
    log::warn(
        "Folio nightly rates do not cover the stay; FolioRate/RateAmount may be incomplete",
        &[
            ("folio", V::S(&folio_num.to_string())),
            ("room_item", V::S(&room.room_item_id.to_string())),
            ("nights", V::I(room.nights.len() as i64)),
            ("stay_nights", V::I(stay)),
        ],
    );
}

/// `LogGuestPlan`: counts only, never names.
fn log_guest_plan(plan: &GuestPlan, room: &Room, folio_num: Option<Decimal>) {
    let Some(m) = &plan.mismatch else { return };
    log::error(
        "Guest counts do not match the placeholders; wrote today's guest counts instead",
        &[
            ("code", V::S(GUEST_COUNT_MISMATCH)),
            ("room_item", V::S(&room.room_item_id.to_string())),
            (
                "folio",
                V::S(&folio_num.map(|f| f.to_string()).unwrap_or_default()),
            ),
            ("mismatch", V::S(m)),
        ],
    );
}

#[cfg(test)]
#[path = "folio_tests.rs"]
pub(crate) mod tests;
