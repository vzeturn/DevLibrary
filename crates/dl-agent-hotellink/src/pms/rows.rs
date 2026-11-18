//! The other SMILE FO rows written with a folio, and the travel-agent lookups.

use dl_time::DateTime;

use super::entities::{Val, zero_time};

/// `FolioRate` row (Go `pms.FolioRate`).
#[derive(Debug, Clone, PartialEq)]
pub struct FolioRate {
    /// `FolioNum`.
    pub folio_num: f64,
    /// `hDate`.
    pub h_date: DateTime,
    /// `RateAmount`.
    pub rate_amount: f64,
    /// `ToDate`.
    pub to_date: DateTime,
    /// `isCancel`.
    pub is_cancel: bool,
    /// `RateOverride`.
    pub rate_override: i32,
}

impl Default for FolioRate {
    fn default() -> Self {
        FolioRate {
            folio_num: 0.0,
            h_date: zero_time(),
            rate_amount: 0.0,
            to_date: zero_time(),
            is_cancel: false,
            rate_override: 0,
        }
    }
}

impl FolioRate {
    /// The insertable columns with their values, in struct order.
    pub(crate) fn fields(&self) -> Vec<(&'static str, Val)> {
        vec![
            ("FolioNum", Val::F64(self.folio_num)),
            ("hDate", Val::Time(self.h_date)),
            ("RateAmount", Val::F64(self.rate_amount)),
            ("ToDate", Val::Time(self.to_date)),
            ("isCancel", Val::Bool(self.is_cancel)),
            ("RateOverride", Val::I32(self.rate_override)),
        ]
    }
}

/// `AdditionalName` row (Go `pms.AdditionalName`).
#[derive(Debug, Clone, PartialEq)]
pub struct AdditionalName {
    /// `FolioNum`.
    pub folio_num: f64,
    /// `LastName`.
    pub last_name: String,
    /// `CompanyName`.
    pub company_name: String,
    /// `Address1`.
    pub address1: String,
    /// `Address2`.
    pub address2: String,
    /// `Address3`.
    pub address3: String,
    /// `Address4`.
    pub address4: String,
    /// `PostalCode`.
    pub postal_code: String,
    /// `Country`.
    pub country: String,
    /// `TeleNum`.
    pub tele_num: String,
    /// `FaxNum`.
    pub fax_num: String,
    /// `LanguageCode`.
    pub language_code: String,
    /// `PassportNum`.
    pub passport_num: String,
    /// `VisaNum`.
    pub visa_num: String,
    /// `VIPLevel`.
    pub vip_level: i32,
    /// `NationalityCode`.
    pub nationality_code: String,
    /// `ArrivalTime`.
    pub arrival_time: DateTime,
    /// `DepartureTime`.
    pub departure_time: DateTime,
    /// `ConfirmNum`.
    pub confirm_num: f64,
    /// `FirstName`.
    pub first_name: String,
    /// `Gender`.
    pub gender: bool,
    /// `Title`.
    pub title: String,
    /// `Geographic1Code`.
    pub geographic1_code: String,
    /// `Geographic2Code`.
    pub geographic2_code: String,
    /// `Occupation`.
    pub occupation: String,
    /// `ResidenceCard`.
    pub residence_card: String,
    /// `Purpose`.
    pub purpose: String,
    /// `AOriginCode`.
    pub a_origin_code: String,
    /// `EntryFrom`.
    pub entry_from: String,
    /// `EntryPort`.
    pub entry_port: String,
    /// `IsSuedBy`.
    pub is_sued_by: String,
    /// `AdtStatus`.
    pub adt_status: i32,
    /// `AProfileNum`.
    pub a_profile_num: f64,
    /// `Email`.
    pub email: String,
    /// `IsChild`.
    pub is_child: bool,
    /// `MobileNum`.
    pub mobile_num: String,
    /// `WebSite`.
    pub web_site: String,
    /// `Remark`.
    pub remark: String,
    /// `DocType`.
    pub doc_type: String,
    /// `PassportType`.
    pub passport_type: String,
    /// `ACancelNum`.
    pub a_cancel_num: f64,
}

impl Default for AdditionalName {
    fn default() -> Self {
        AdditionalName {
            folio_num: 0.0,
            last_name: String::new(),
            company_name: String::new(),
            address1: String::new(),
            address2: String::new(),
            address3: String::new(),
            address4: String::new(),
            postal_code: String::new(),
            country: String::new(),
            tele_num: String::new(),
            fax_num: String::new(),
            language_code: String::new(),
            passport_num: String::new(),
            visa_num: String::new(),
            vip_level: 0,
            nationality_code: String::new(),
            arrival_time: zero_time(),
            departure_time: zero_time(),
            confirm_num: 0.0,
            first_name: String::new(),
            gender: false,
            title: String::new(),
            geographic1_code: String::new(),
            geographic2_code: String::new(),
            occupation: String::new(),
            residence_card: String::new(),
            purpose: String::new(),
            a_origin_code: String::new(),
            entry_from: String::new(),
            entry_port: String::new(),
            is_sued_by: String::new(),
            adt_status: 0,
            a_profile_num: 0.0,
            email: String::new(),
            is_child: false,
            mobile_num: String::new(),
            web_site: String::new(),
            remark: String::new(),
            doc_type: String::new(),
            passport_type: String::new(),
            a_cancel_num: 0.0,
        }
    }
}

impl AdditionalName {
    /// The insertable columns with their values, in struct order.
    pub(crate) fn fields(&self) -> Vec<(&'static str, Val)> {
        vec![
            ("FolioNum", Val::F64(self.folio_num)),
            ("LastName", Val::Str(self.last_name.clone())),
            ("CompanyName", Val::Str(self.company_name.clone())),
            ("Address1", Val::Str(self.address1.clone())),
            ("Address2", Val::Str(self.address2.clone())),
            ("Address3", Val::Str(self.address3.clone())),
            ("Address4", Val::Str(self.address4.clone())),
            ("PostalCode", Val::Str(self.postal_code.clone())),
            ("Country", Val::Str(self.country.clone())),
            ("TeleNum", Val::Str(self.tele_num.clone())),
            ("FaxNum", Val::Str(self.fax_num.clone())),
            ("LanguageCode", Val::Str(self.language_code.clone())),
            ("PassportNum", Val::Str(self.passport_num.clone())),
            ("VisaNum", Val::Str(self.visa_num.clone())),
            ("VIPLevel", Val::I32(self.vip_level)),
            ("NationalityCode", Val::Str(self.nationality_code.clone())),
            ("ArrivalTime", Val::Time(self.arrival_time)),
            ("DepartureTime", Val::Time(self.departure_time)),
            ("ConfirmNum", Val::F64(self.confirm_num)),
            ("FirstName", Val::Str(self.first_name.clone())),
            ("Gender", Val::Bool(self.gender)),
            ("Title", Val::Str(self.title.clone())),
            ("Geographic1Code", Val::Str(self.geographic1_code.clone())),
            ("Geographic2Code", Val::Str(self.geographic2_code.clone())),
            ("Occupation", Val::Str(self.occupation.clone())),
            ("ResidenceCard", Val::Str(self.residence_card.clone())),
            ("Purpose", Val::Str(self.purpose.clone())),
            ("AOriginCode", Val::Str(self.a_origin_code.clone())),
            ("EntryFrom", Val::Str(self.entry_from.clone())),
            ("EntryPort", Val::Str(self.entry_port.clone())),
            ("IsSuedBy", Val::Str(self.is_sued_by.clone())),
            ("AdtStatus", Val::I32(self.adt_status)),
            ("AProfileNum", Val::F64(self.a_profile_num)),
            ("Email", Val::Str(self.email.clone())),
            ("IsChild", Val::Bool(self.is_child)),
            ("MobileNum", Val::Str(self.mobile_num.clone())),
            ("WebSite", Val::Str(self.web_site.clone())),
            ("Remark", Val::Str(self.remark.clone())),
            ("DocType", Val::Str(self.doc_type.clone())),
            ("PassportType", Val::Str(self.passport_type.clone())),
            ("ACancelNum", Val::F64(self.a_cancel_num)),
        ]
    }
}
impl AdditionalName {
    /// `NewAdditionalName`.
    pub fn new() -> AdditionalName {
        AdditionalName {
            adt_status: 1,
            ..AdditionalName::default()
        }
    }
}

/// A `CLIENT` row (travel agent).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SmileClient {
    /// `ClientFolioNum`.
    pub client_folio_num: f64,
    /// `ClientName`.
    pub client_name: String,
}

/// A `ClientSts` row: the travel agent's default codes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClientSts {
    /// `ClientCode`.
    pub client_code: f64,
    /// `Origin`.
    pub origin: String,
    /// `MSegment`.
    pub m_segment: String,
    /// `UDef1`..`UDef8`.
    pub u_def: [String; 8],
}

/// A `ClientContacts` row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClientContact {
    /// `ClientFolioNum`.
    pub client_folio_num: f64,
    /// `ContactName`.
    pub contact_name: String,
}
