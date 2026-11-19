//! `FolioWriter.PostDiscountAsync` (`PostOneTransaction`, 20 parameters) and
//! `HasPostedRefAsync`, the duplicate guard on the agent's own postings.

use dl_decimal::Decimal;
use dl_time::DateTime;

use super::folio::FolioWriter;
use super::session::{Session, decimal, int, nvarchar, p, varchar};
use super::{MEMBERSHIP_CLERK_ID, REF_MAX_LENGTH, dapper_in};
use crate::folio::read::folio_num_as_varchar;
use crate::jobs::plan::WriteError;

const HAS_POSTED_SQL_HEAD: &str = "SELECT CASE WHEN EXISTS (
    SELECT 1
    FROM FolioTransaction t
    WHERE t.FolioNum IN ";
const HAS_POSTED_SQL_TAIL: &str = "
      AND t.RefNumber = @Ref
      AND t.PostingClerkID = @Clerk
) THEN 1 ELSE 0 END";

fn fmt(now: DateTime, pattern: &str) -> Result<String, WriteError> {
    now.format(pattern).map_err(|e| WriteError::Coded {
        code: "CLOCK_FORMAT".into(),
        message: e.to_string(),
    })
}

impl FolioWriter {
    /// `PostDiscountAsync`: one negative line in hotel time.
    pub fn post_discount(
        &self,
        s: &mut dyn Session,
        now: DateTime,
        folio_num: Decimal,
        amount: Decimal,
        code: &str,
        reference: &str,
    ) -> Result<(), WriteError> {
        let folio_text = folio_num.to_string();
        let neg = -amount.abs();
        let params = [
            p("@FolioNum", nvarchar(Some(&folio_text))),
            p("@BillAmount", decimal(neg)),
            p("@BillExRate", decimal(Decimal::ONE)),
            p("@TransactionCode", nvarchar(Some(code))),
            p("@TransactionSubCode", nvarchar(Some(""))),
            p(
                "@sTransactionDate",
                nvarchar(Some(&fmt(now, "yyyy-MM-dd")?)),
            ),
            p("@RoomCode", nvarchar(Some(""))),
            p("@RefNumber", nvarchar(Some(reference))),
            p("@TransactionAmount", decimal(neg)),
            p("@FolioBalanceCode", nvarchar(Some("A"))),
            p("@MealPeriodNumber", int(0)),
            p("@Comment", nvarchar(Some("Membership voucher discount"))),
            p("@sSystemTime", nvarchar(Some(&fmt(now, "HH:mm:ss")?))),
            p("@OriginAmount", decimal(neg)),
            p("@OriginCurrency", nvarchar(Some("VND"))),
            p("@OriginFolioNumber", nvarchar(Some(&folio_text))),
            p("@PostingClerkID", nvarchar(Some(MEMBERSHIP_CLERK_ID))),
            p("@NumberCover", int(0)),
            p("@OriginRoom", nvarchar(Some(""))),
            p("@Discount", decimal(amount.abs())),
        ];
        s.call("PostOneTransaction", &params)?;
        Ok(())
    }

    /// `HasPostedRefAsync`: whether the agent already posted `reference` (cut to the 30
    /// characters SMILE stores) on one of the folios. Every parameter is `varchar`.
    pub fn has_posted_ref(
        &self,
        s: &mut dyn Session,
        folios: &[Decimal],
        reference: &str,
    ) -> Result<bool, WriteError> {
        if folios.is_empty() || reference.is_empty() {
            return Ok(false);
        }
        let stored: String = reference.chars().take(REF_MAX_LENGTH).collect();
        let (list, mut params) = dapper_in(
            "Folios",
            folios
                .iter()
                .map(|f| varchar(Some(&folio_num_as_varchar(*f)))),
        );
        params.push(p("@Ref", varchar(Some(&stored))));
        params.push(p("@Clerk", varchar(Some(MEMBERSHIP_CLERK_ID))));
        let rows = s.rows(
            &format!("{HAS_POSTED_SQL_HEAD}{list}{HAS_POSTED_SQL_TAIL}"),
            &params,
        )?;
        let found = match rows.first() {
            Some(r) => first_int(r)?,
            None => 0,
        };
        Ok(found == 1)
    }
}

/// The unnamed scalar of the `EXISTS` query.
fn first_int(r: &super::session::Rec) -> Result<i64, WriteError> {
    Ok(r.scalar_int()?.unwrap_or(0))
}
