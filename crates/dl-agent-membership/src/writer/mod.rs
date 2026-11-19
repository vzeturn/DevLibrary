//! The SMILE writers behind [`crate::jobs::dispatch::PmsWriter`].
pub mod additional_name;
pub mod applock;
pub mod folio;
pub mod link;
pub mod lookup;
pub mod post;
pub mod profile;
#[cfg(test)]
pub mod recorder;
pub mod schema;
pub mod session;
pub mod smile_writer;
pub mod tcvn3;
pub mod travel_agent;

/// `SmileClerk.App`: the clerk id stamped on folios and rates the agent writes.
pub const CLERK_APP: &str = "APP";
/// `FolioWriter.MembershipClerkId`: `FolioTransaction.PostingClerkID` is `varchar(3)`.
pub const MEMBERSHIP_CLERK_ID: &str = "MEM";
/// `FolioRefNumbers.MaxLength`: `PostOneTransaction`'s `@RefNumber` is `varchar(30)`.
pub const REF_MAX_LENGTH: usize = 30;

/// `FormatUserInfo2`: `MEM:{member|-};BKG:{booking}`.
pub fn format_user_info2(member_id: Option<dl_uuid::Uuid>, booking_id: dl_uuid::Uuid) -> String {
    let member = member_id.map_or_else(|| "-".to_owned(), |m| m.to_string());
    format!("MEM:{member};BKG:{booking_id}")
}

/// Dapper's expansion of `IN @Name` for a list: `(@Name1,@Name2,…)` and the parameters.
pub(crate) fn dapper_in(
    name: &str,
    values: impl IntoIterator<Item = dl_tds::ParamValue>,
) -> (String, Vec<dl_tds::Param>) {
    let mut names = Vec::new();
    let mut params = Vec::new();
    for (i, v) in values.into_iter().enumerate() {
        let n = format!("@{name}{}", i + 1);
        params.push(dl_tds::Param::new(&n, v));
        names.push(n);
    }
    (format!("({})", names.join(",")), params)
}
