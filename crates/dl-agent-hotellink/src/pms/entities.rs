//! SMILE FO table rows (Go `internal/pms/entities.go`). Field order is the struct order,
//! which is also the column order of the generated `INSERT`.

use dl_tds::Param;
use dl_time::{Date, DateTime};

/// Go's zero `time.Time` (0001-01-01 00:00:00).
pub fn zero_time() -> DateTime {
    match Date::new(1, 1, 1) {
        Ok(d) => d.midnight(),
        Err(_) => DateTime::UNIX_EPOCH,
    }
}

/// `datetime` parameter text: ISO 8601 with milliseconds, which every SQL Server since
/// 2005 converts to `datetime` regardless of the session language (a `datetime2`
/// parameter would fail on 2005).
pub fn sql_time(t: DateTime) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}",
        t.year(),
        t.month().number(),
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
        t.nanosecond() / 1_000_000
    )
}

/// `time.Format("2006-01-02")`.
pub fn ymd(t: DateTime) -> String {
    format!("{:04}-{:02}-{:02}", t.year(), t.month().number(), t.day())
}

/// A column value (Go's `any` in `buildInsert` and `UpdateFolioFields`).
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    /// `float64`.
    F64(f64),
    /// `int`.
    I32(i32),
    /// `int64`.
    I64(i64),
    /// `string`.
    Str(String),
    /// `*string` (`None` is NULL).
    OptStr(Option<String>),
    /// `*float64` (`None` is NULL).
    OptF64(Option<f64>),
    /// `bool`.
    Bool(bool),
    /// `time.Time` (sent as ISO text, see [`sql_time`]).
    Time(DateTime),
}

impl Val {
    /// The value as a named parameter.
    pub fn param(&self, name: &str) -> Param {
        match self {
            Val::F64(v) => Param::new(name, *v),
            Val::I32(v) => Param::new(name, *v),
            Val::I64(v) => Param::new(name, *v),
            Val::Str(s) => Param::new(name, s.as_str()),
            Val::OptStr(s) => Param::new(name, s.clone()),
            Val::OptF64(v) => Param::new(name, *v),
            Val::Bool(v) => Param::new(name, *v),
            Val::Time(t) => Param::new(name, sql_time(*t)),
        }
    }

    /// The text of a string value (`stringFieldValue`).
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Val::Str(s) => Some(s),
            Val::OptStr(Some(s)) => Some(s),
            _ => None,
        }
    }
}

/// `Folio` row (Go `pms.Folio`).
#[derive(Debug, Clone, PartialEq)]
pub struct Folio {
    /// `Id`.
    pub id: i32,
    /// `FolioNum`.
    pub folio_num: f64,
    /// `ArrivalDate`.
    pub arrival_date: DateTime,
    /// `DepartureDate`.
    pub departure_date: DateTime,
    /// `NumRoom`.
    pub num_room: i32,
    /// `RoomTypeCode`.
    pub room_type_code: String,
    /// `RoomCode`.
    pub room_code: String,
    /// `FolioSubNum`.
    pub folio_sub_num: f64,
    /// `GroupCode`.
    pub group_code: String,
    /// `CompanyName`.
    pub company_name: String,
    /// `NumAdult`.
    pub num_adult: i32,
    /// `NumChild`.
    pub num_child: i32,
    /// `RateReasonCode`.
    pub rate_reason_code: String,
    /// `TravelAgent1Code`.
    pub travel_agent1_code: f64,
    /// `TravelAgent2Code`.
    pub travel_agent2_code: i64,
    /// `ArrivalAirCode`.
    pub arrival_air_code: String,
    /// `ArrivalFlight`.
    pub arrival_flight: String,
    /// `DepartureAirCode`.
    pub departure_air_code: String,
    /// `DepartureFlight`.
    pub departure_flight: String,
    /// `Geographic1Code`.
    pub geographic1_code: String,
    /// `Geographic2Code`.
    pub geographic2_code: String,
    /// `OriginCode`.
    pub origin_code: String,
    /// `MarketSegmentCode`.
    pub market_segment_code: String,
    /// `MarketBusinessCode`.
    pub market_business_code: String,
    /// `MarketChannelCode`.
    pub market_channel_code: String,
    /// `BuildingCode`.
    pub building_code: String,
    /// `WingCode`.
    pub wing_code: String,
    /// `LowFloor`.
    pub low_floor: i32,
    /// `HighFloor`.
    pub high_floor: i32,
    /// `RoomExposureCode`.
    pub room_exposure_code: String,
    /// `RoomAttributeCode`.
    pub room_attribute_code: String,
    /// `Notice`.
    pub notice: String,
    /// `ConfirmNum`.
    pub confirm_num: f64,
    /// `ExternalConfirmNumber`.
    pub external_confirm_number: Option<String>,
    /// `ConfirmPrintFlag`.
    pub confirm_print_flag: bool,
    /// `FolioStatus`.
    pub folio_status: i32,
    /// `NoShowFlag`.
    pub no_show_flag: bool,
    /// `WalkInFlag`.
    pub walk_in_flag: bool,
    /// `ReservationTime`.
    pub reservation_time: DateTime,
    /// `ReservationClerkID`.
    pub reservation_clerk_id: String,
    /// `CancelNum`.
    pub cancel_num: f64,
    /// `LastChangeClerkID`.
    pub last_change_clerk_id: String,
    /// `LastChangeTime`.
    pub last_change_time: DateTime,
    /// `UserDefineCode1`.
    pub user_define_code1: String,
    /// `UserDefineCode2`.
    pub user_define_code2: String,
    /// `UserDefineCode3`.
    pub user_define_code3: String,
    /// `UserDefineCode4`.
    pub user_define_code4: String,
    /// `UserDefineCode5`.
    pub user_define_code5: String,
    /// `UserDefineCode6`.
    pub user_define_code6: String,
    /// `UserDefineCode7`.
    pub user_define_code7: String,
    /// `UserDefineCode8`.
    pub user_define_code8: String,
    /// `RoomRateCode`.
    pub room_rate_code: String,
    /// `PaymentCode`.
    pub payment_code: String,
    /// `PartyCode`.
    pub party_code: String,
    /// `CreditCardExpireTime`.
    pub credit_card_expire_time: String,
    /// `PackageCode`.
    pub package_code: String,
    /// `TransferHoldFlag`.
    pub transfer_hold_flag: bool,
    /// `RateAmount`.
    pub rate_amount: f64,
    /// `RateOverideFlag`.
    pub rate_overide_flag: bool,
    /// `ConfidentialFlag`.
    pub confidential_flag: bool,
    /// `NoPostFlag`.
    pub no_post_flag: bool,
    /// `HouseUseFlag`.
    pub house_use_flag: bool,
    /// `CompFlag`.
    pub comp_flag: bool,
    /// `CreditCardTypeCode`.
    pub credit_card_type_code: String,
    /// `Booker`.
    pub booker: String,
    /// `ARAccountNumber`.
    pub ar_account_number: Option<f64>,
    /// `BookStatus`.
    pub book_status: i32,
    /// `Allotment`.
    pub allotment: bool,
    /// `AllotmentTA`.
    pub allotment_ta: i64,
    /// `AllotmentContract`.
    pub allotment_contract: String,
    /// `NetRate`.
    pub net_rate: bool,
    /// `SubGroupCode`.
    pub sub_group_code: String,
    /// `IncludeBF`.
    pub include_bf: bool,
    /// `BreakfastCode`.
    pub breakfast_code: String,
    /// `FolioCurrencyCode`.
    pub folio_currency_code: String,
    /// `ExtraBed`.
    pub extra_bed: i32,
    /// `RoomChargeToMasterFlag`.
    pub room_charge_to_master_flag: bool,
    /// `FolioExRate`.
    pub folio_ex_rate: f64,
    /// `NumGuestBF`.
    pub num_guest_bf: i32,
    /// `LastName`.
    pub last_name: String,
    /// `FirstName`.
    pub first_name: String,
    /// `TitleCodeM`.
    pub title_code_m: String,
    /// `TelNumM`.
    pub tel_num_m: String,
    /// `FaxNumM`.
    pub fax_num_m: String,
    /// `MailM`.
    pub mail_m: String,
    /// `ProfileNumber`.
    pub profile_number: f64,
    /// `CreditLimit`.
    pub credit_limit: Option<f64>,
    /// `BlockFlag`.
    pub block_flag: bool,
    /// `BlockClerkID`.
    pub block_clerk_id: String,
    /// `RoomTypeBooked`.
    pub room_type_booked: String,
    /// `CreditCardHolder`.
    pub credit_card_holder: String,
    /// `MemberID`.
    pub member_id: String,
    /// `CreditCardBlockAmount`.
    pub credit_card_block_amount: f64,
    /// `UserInfo1`.
    pub user_info1: String,
    /// `UserInfo2`.
    pub user_info2: String,
    /// `UserInfo3`.
    pub user_info3: String,
    /// `UserInfo4`.
    pub user_info4: String,
    /// `DscPcnt`.
    pub dsc_pcnt: f64,
    /// `DscAmount`.
    pub dsc_amount: f64,
    /// `UpdateExRateAtCI`.
    pub update_ex_rate_at_ci: bool,
    /// `NumChildBF`.
    pub num_child_bf: i32,
    /// `PrvOwnerID`.
    pub prv_owner_id: String,
    /// `MonthlyRate`.
    pub monthly_rate: i32,
    /// `TraceNote`.
    pub trace_note: String,
    /// `CntRoom`.
    pub cnt_room: i32,
    /// `RmChgtrn`.
    pub rm_chgtrn: i32,
    /// `ChargeNights`.
    pub charge_nights: i32,
    /// `CommRate`.
    pub comm_rate: f64,
    /// `CommAmount`.
    pub comm_amount: f64,
    /// `ShareNum`.
    pub share_num: f64,
}

impl Default for Folio {
    fn default() -> Self {
        Folio {
            id: 0,
            folio_num: 0.0,
            arrival_date: zero_time(),
            departure_date: zero_time(),
            num_room: 0,
            room_type_code: String::new(),
            room_code: String::new(),
            folio_sub_num: 0.0,
            group_code: String::new(),
            company_name: String::new(),
            num_adult: 0,
            num_child: 0,
            rate_reason_code: String::new(),
            travel_agent1_code: 0.0,
            travel_agent2_code: 0,
            arrival_air_code: String::new(),
            arrival_flight: String::new(),
            departure_air_code: String::new(),
            departure_flight: String::new(),
            geographic1_code: String::new(),
            geographic2_code: String::new(),
            origin_code: String::new(),
            market_segment_code: String::new(),
            market_business_code: String::new(),
            market_channel_code: String::new(),
            building_code: String::new(),
            wing_code: String::new(),
            low_floor: 0,
            high_floor: 0,
            room_exposure_code: String::new(),
            room_attribute_code: String::new(),
            notice: String::new(),
            confirm_num: 0.0,
            external_confirm_number: None,
            confirm_print_flag: false,
            folio_status: 0,
            no_show_flag: false,
            walk_in_flag: false,
            reservation_time: zero_time(),
            reservation_clerk_id: String::new(),
            cancel_num: 0.0,
            last_change_clerk_id: String::new(),
            last_change_time: zero_time(),
            user_define_code1: String::new(),
            user_define_code2: String::new(),
            user_define_code3: String::new(),
            user_define_code4: String::new(),
            user_define_code5: String::new(),
            user_define_code6: String::new(),
            user_define_code7: String::new(),
            user_define_code8: String::new(),
            room_rate_code: String::new(),
            payment_code: String::new(),
            party_code: String::new(),
            credit_card_expire_time: String::new(),
            package_code: String::new(),
            transfer_hold_flag: false,
            rate_amount: 0.0,
            rate_overide_flag: false,
            confidential_flag: false,
            no_post_flag: false,
            house_use_flag: false,
            comp_flag: false,
            credit_card_type_code: String::new(),
            booker: String::new(),
            ar_account_number: None,
            book_status: 0,
            allotment: false,
            allotment_ta: 0,
            allotment_contract: String::new(),
            net_rate: false,
            sub_group_code: String::new(),
            include_bf: false,
            breakfast_code: String::new(),
            folio_currency_code: String::new(),
            extra_bed: 0,
            room_charge_to_master_flag: false,
            folio_ex_rate: 0.0,
            num_guest_bf: 0,
            last_name: String::new(),
            first_name: String::new(),
            title_code_m: String::new(),
            tel_num_m: String::new(),
            fax_num_m: String::new(),
            mail_m: String::new(),
            profile_number: 0.0,
            credit_limit: None,
            block_flag: false,
            block_clerk_id: String::new(),
            room_type_booked: String::new(),
            credit_card_holder: String::new(),
            member_id: String::new(),
            credit_card_block_amount: 0.0,
            user_info1: String::new(),
            user_info2: String::new(),
            user_info3: String::new(),
            user_info4: String::new(),
            dsc_pcnt: 0.0,
            dsc_amount: 0.0,
            update_ex_rate_at_ci: false,
            num_child_bf: 0,
            prv_owner_id: String::new(),
            monthly_rate: 0,
            trace_note: String::new(),
            cnt_room: 0,
            rm_chgtrn: 0,
            charge_nights: 0,
            comm_rate: 0.0,
            comm_amount: 0.0,
            share_num: 0.0,
        }
    }
}

impl Folio {
    /// The insertable columns with their values, in struct order.
    pub(crate) fn fields(&self) -> Vec<(&'static str, Val)> {
        vec![
            ("FolioNum", Val::F64(self.folio_num)),
            ("ArrivalDate", Val::Time(self.arrival_date)),
            ("DepartureDate", Val::Time(self.departure_date)),
            ("NumRoom", Val::I32(self.num_room)),
            ("RoomTypeCode", Val::Str(self.room_type_code.clone())),
            ("RoomCode", Val::Str(self.room_code.clone())),
            ("FolioSubNum", Val::F64(self.folio_sub_num)),
            ("GroupCode", Val::Str(self.group_code.clone())),
            ("CompanyName", Val::Str(self.company_name.clone())),
            ("NumAdult", Val::I32(self.num_adult)),
            ("NumChild", Val::I32(self.num_child)),
            ("RateReasonCode", Val::Str(self.rate_reason_code.clone())),
            ("TravelAgent1Code", Val::F64(self.travel_agent1_code)),
            ("TravelAgent2Code", Val::I64(self.travel_agent2_code)),
            ("ArrivalAirCode", Val::Str(self.arrival_air_code.clone())),
            ("ArrivalFlight", Val::Str(self.arrival_flight.clone())),
            (
                "DepartureAirCode",
                Val::Str(self.departure_air_code.clone()),
            ),
            ("DepartureFlight", Val::Str(self.departure_flight.clone())),
            ("Geographic1Code", Val::Str(self.geographic1_code.clone())),
            ("Geographic2Code", Val::Str(self.geographic2_code.clone())),
            ("OriginCode", Val::Str(self.origin_code.clone())),
            (
                "MarketSegmentCode",
                Val::Str(self.market_segment_code.clone()),
            ),
            (
                "MarketBusinessCode",
                Val::Str(self.market_business_code.clone()),
            ),
            (
                "MarketChannelCode",
                Val::Str(self.market_channel_code.clone()),
            ),
            ("BuildingCode", Val::Str(self.building_code.clone())),
            ("WingCode", Val::Str(self.wing_code.clone())),
            ("LowFloor", Val::I32(self.low_floor)),
            ("HighFloor", Val::I32(self.high_floor)),
            (
                "RoomExposureCode",
                Val::Str(self.room_exposure_code.clone()),
            ),
            (
                "RoomAttributeCode",
                Val::Str(self.room_attribute_code.clone()),
            ),
            ("Notice", Val::Str(self.notice.clone())),
            ("ConfirmNum", Val::F64(self.confirm_num)),
            (
                "ExternalConfirmNumber",
                Val::OptStr(self.external_confirm_number.clone()),
            ),
            ("ConfirmPrintFlag", Val::Bool(self.confirm_print_flag)),
            ("FolioStatus", Val::I32(self.folio_status)),
            ("NoShowFlag", Val::Bool(self.no_show_flag)),
            ("WalkInFlag", Val::Bool(self.walk_in_flag)),
            ("ReservationTime", Val::Time(self.reservation_time)),
            (
                "ReservationClerkID",
                Val::Str(self.reservation_clerk_id.clone()),
            ),
            ("CancelNum", Val::F64(self.cancel_num)),
            (
                "LastChangeClerkID",
                Val::Str(self.last_change_clerk_id.clone()),
            ),
            ("LastChangeTime", Val::Time(self.last_change_time)),
            ("UserDefineCode1", Val::Str(self.user_define_code1.clone())),
            ("UserDefineCode2", Val::Str(self.user_define_code2.clone())),
            ("UserDefineCode3", Val::Str(self.user_define_code3.clone())),
            ("UserDefineCode4", Val::Str(self.user_define_code4.clone())),
            ("UserDefineCode5", Val::Str(self.user_define_code5.clone())),
            ("UserDefineCode6", Val::Str(self.user_define_code6.clone())),
            ("UserDefineCode7", Val::Str(self.user_define_code7.clone())),
            ("UserDefineCode8", Val::Str(self.user_define_code8.clone())),
            ("RoomRateCode", Val::Str(self.room_rate_code.clone())),
            ("PaymentCode", Val::Str(self.payment_code.clone())),
            ("PartyCode", Val::Str(self.party_code.clone())),
            (
                "CreditCardExpireTime",
                Val::Str(self.credit_card_expire_time.clone()),
            ),
            ("PackageCode", Val::Str(self.package_code.clone())),
            ("TransferHoldFlag", Val::Bool(self.transfer_hold_flag)),
            ("RateAmount", Val::F64(self.rate_amount)),
            ("RateOverideFlag", Val::Bool(self.rate_overide_flag)),
            ("ConfidentialFlag", Val::Bool(self.confidential_flag)),
            ("NoPostFlag", Val::Bool(self.no_post_flag)),
            ("HouseUseFlag", Val::Bool(self.house_use_flag)),
            ("CompFlag", Val::Bool(self.comp_flag)),
            (
                "CreditCardTypeCode",
                Val::Str(self.credit_card_type_code.clone()),
            ),
            ("Booker", Val::Str(self.booker.clone())),
            ("ARAccountNumber", Val::OptF64(self.ar_account_number)),
            ("BookStatus", Val::I32(self.book_status)),
            ("Allotment", Val::Bool(self.allotment)),
            ("AllotmentTA", Val::I64(self.allotment_ta)),
            (
                "AllotmentContract",
                Val::Str(self.allotment_contract.clone()),
            ),
            ("NetRate", Val::Bool(self.net_rate)),
            ("SubGroupCode", Val::Str(self.sub_group_code.clone())),
            ("IncludeBF", Val::Bool(self.include_bf)),
            ("BreakfastCode", Val::Str(self.breakfast_code.clone())),
            (
                "FolioCurrencyCode",
                Val::Str(self.folio_currency_code.clone()),
            ),
            ("ExtraBed", Val::I32(self.extra_bed)),
            (
                "RoomChargeToMasterFlag",
                Val::Bool(self.room_charge_to_master_flag),
            ),
            ("FolioExRate", Val::F64(self.folio_ex_rate)),
            ("NumGuestBF", Val::I32(self.num_guest_bf)),
            ("LastName", Val::Str(self.last_name.clone())),
            ("FirstName", Val::Str(self.first_name.clone())),
            ("TitleCodeM", Val::Str(self.title_code_m.clone())),
            ("TelNumM", Val::Str(self.tel_num_m.clone())),
            ("FaxNumM", Val::Str(self.fax_num_m.clone())),
            ("MailM", Val::Str(self.mail_m.clone())),
            ("ProfileNumber", Val::F64(self.profile_number)),
            ("CreditLimit", Val::OptF64(self.credit_limit)),
            ("BlockFlag", Val::Bool(self.block_flag)),
            ("BlockClerkID", Val::Str(self.block_clerk_id.clone())),
            ("RoomTypeBooked", Val::Str(self.room_type_booked.clone())),
            (
                "CreditCardHolder",
                Val::Str(self.credit_card_holder.clone()),
            ),
            ("MemberID", Val::Str(self.member_id.clone())),
            (
                "CreditCardBlockAmount",
                Val::F64(self.credit_card_block_amount),
            ),
            ("UserInfo1", Val::Str(self.user_info1.clone())),
            ("UserInfo2", Val::Str(self.user_info2.clone())),
            ("UserInfo3", Val::Str(self.user_info3.clone())),
            ("UserInfo4", Val::Str(self.user_info4.clone())),
            ("DscPcnt", Val::F64(self.dsc_pcnt)),
            ("DscAmount", Val::F64(self.dsc_amount)),
            ("UpdateExRateAtCI", Val::Bool(self.update_ex_rate_at_ci)),
            ("NumChildBF", Val::I32(self.num_child_bf)),
            ("PrvOwnerID", Val::Str(self.prv_owner_id.clone())),
            ("MonthlyRate", Val::I32(self.monthly_rate)),
            ("TraceNote", Val::Str(self.trace_note.clone())),
            ("CntRoom", Val::I32(self.cnt_room)),
            ("RmChgtrn", Val::I32(self.rm_chgtrn)),
            ("ChargeNights", Val::I32(self.charge_nights)),
            ("CommRate", Val::F64(self.comm_rate)),
            ("CommAmount", Val::F64(self.comm_amount)),
            ("ShareNum", Val::F64(self.share_num)),
        ]
    }
}

impl Folio {
    /// `NewFolio`: the defaults SMILE FO expects on a new reservation.
    pub fn new(now: DateTime) -> Folio {
        Folio {
            num_room: 1,
            confirm_print_flag: true,
            folio_status: 1,
            reservation_time: now,
            last_change_time: now,
            payment_code: "7".into(),
            rate_overide_flag: true,
            book_status: 1,
            net_rate: true,
            include_bf: true,
            breakfast_code: "ABU".into(),
            folio_currency_code: "VND".into(),
            folio_ex_rate: 1.0,
            update_ex_rate_at_ci: true,
            external_confirm_number: Some(String::new()),
            ..Folio::default()
        }
    }
}
