//! `fo.inhouse_search` (`Wire/FoInHouseSearch.cs`, RT-1296, ADR 0172 §7): find the guests in
//! house in the Front Office database of one RVC by room, folio number or name. Read on
//! demand only: the cloud never keeps the answer.

use dl_uuid::Uuid;

use crate::contracts::json::{self, WireJson};
use crate::contracts::utf16_len;
use crate::util::stj::{Obj, ReadError, Writer};

/// Rejection reasons of the room-charge family (`TxnReasons`) this read shares.
pub mod txn_reasons {
    /// The RVC's Front Office database (`dbo.Para InterfaceDB`) is not a usable database
    /// for the agent's login.
    pub const FOLIO_DATABASE_INVALID: &str = "txn_folio_database_invalid";
}

/// Payload of [`site_command_types::FO_INHOUSE_SEARCH`](super::site_command_types::FO_INHOUSE_SEARCH):
/// at most `limit` in-house folios of RVC `rvc` whose room, folio number or name matches
/// `query`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FoInHouseSearchPayload {
    /// RVC.
    pub rvc: i32,
    /// The user's text (trimmed before use); `None` when the JSON has none.
    pub query: Option<String>,
    /// Most rows returned.
    pub limit: i32,
}

impl FoInHouseSearchPayload {
    /// Longest query (UTF-16 units, after trimming).
    pub const MAX_QUERY: usize = 40;
    /// Largest limit.
    pub const MAX_LIMIT: i32 = 20;

    /// One aggregate per search (`search_id`): a slow read never holds the next search back.
    pub fn aggregate_key(rvc: i32, search_id: Uuid) -> String {
        format!("fo-inhouse:{rvc}:{}", search_id.simple())
    }

    /// The first rule the payload breaks, or `None` when it is valid.
    pub fn problem(&self) -> Option<&'static str> {
        if self.rvc <= 0 {
            return Some("rvc must be positive");
        }
        let q = self.query.as_deref().unwrap_or("").trim();
        let len = utf16_len(q);
        if !(1..=Self::MAX_QUERY).contains(&len) {
            return Some("query must be 1..40 characters");
        }
        if !(1..=Self::MAX_LIMIT).contains(&self.limit) {
            return Some("limit must be 1..20");
        }
        None
    }
}

impl WireJson for FoInHouseSearchPayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc").int(self.rvc.into());
        json::opt_str(w, "query", &self.query);
        w.key("limit").int(self.limit.into());
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(FoInHouseSearchPayload {
            rvc: o.int("rvc")?,
            query: o.opt_string("query")?,
            limit: o.int("limit")?,
        })
    }
}

/// One in-house folio: room, folio number, the display name `FirstName LastName` and the
/// stay dates (`yyyy-MM-dd`, `None` when the folio has none). Nothing else of the guest is
/// read.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FoInHouseGuest {
    /// Room code (trimmed).
    pub room_no: String,
    /// Folio number.
    pub folio_num: i64,
    /// `FirstName LastName`, TCVN3-decoded.
    pub guest_name: String,
    /// Arrival day.
    pub arrival_date: Option<String>,
    /// Departure day.
    pub departure_date: Option<String>,
}

impl WireJson for FoInHouseGuest {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("room_no").string(&self.room_no);
        w.key("folio_num").int(self.folio_num);
        w.key("guest_name").string(&self.guest_name);
        json::opt_str(w, "arrival_date", &self.arrival_date);
        json::opt_str(w, "departure_date", &self.departure_date);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(FoInHouseGuest {
            room_no: o.string("room_no")?,
            folio_num: o.int("folio_num")?,
            guest_name: o.string("guest_name")?,
            arrival_date: o.opt_string("arrival_date")?,
            departure_date: o.opt_string("departure_date")?,
        })
    }
}

/// Result of [`site_command_types::FO_INHOUSE_SEARCH`](super::site_command_types::FO_INHOUSE_SEARCH):
/// the matching in-house folios, by room.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FoInHouseSearchResult {
    /// RVC.
    pub rvc: i32,
    /// Matches.
    pub guests: Vec<FoInHouseGuest>,
}

impl WireJson for FoInHouseSearchResult {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc").int(self.rvc.into());
        w.key("guests");
        json::write_items(w, &self.guests);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(FoInHouseSearchResult {
            rvc: o.int("rvc")?,
            guests: o.list("guests", json::item)?,
        })
    }
}
