//! `FolioWriter.ExtraBedSpecial`: the `SMILE_SPECIAL` preflight inputs read right before a
//! booking write (`ReadExtraBedInputsAsync`), and the agent-marked `Folio_Special` rows
//! brought to the decision inside the folio transaction (`WriteMmBedRowsInTxAsync`). Agent
//! rows are recognised by the `SPNotes` marker, never by `nSeq`; front-desk rows are never
//! touched. The agent never writes `GlobalParameter` or the `SpecialService` catalogue.

use std::sync::{Arc, Mutex, PoisonError};

use dl_decimal::Decimal;
use dl_uuid::Uuid;

use super::bed_terms_reader;
use super::folio::FolioWriter;
use super::lookup;
use super::schema::{ColumnList, TableSchema};
use super::session::{Session, decimal, int, nvarchar, p, varchar};
use super::tcvn3;
use crate::jobs::beds::{BedDecision, MODE_SMILE_SPECIAL, SpecialBedRow};
use crate::jobs::plan::WriteError;
use crate::jobs::preflight::PreflightInputs;
use crate::jobs::result::UNKNOWN;
use crate::log::{self, V};

/// `ExtraBedWire.FoldFingerprintDrift`, logged when `Folio_Special` cannot carry a row.
const FOLD_FINGERPRINT_DRIFT: &str = crate::jobs::beds::FOLD_FINGERPRINT_DRIFT;

/// `SpecialWriteColumns`.
const SPECIAL_WRITE_COLUMNS: [&str; 6] =
    ["SpecialCode", "Qty", "Price", "FromDay", "ToDay", "SPNotes"];

const NSEQ_IDENTITY_SQL: &str =
    "SELECT COLUMNPROPERTY(OBJECT_ID(N'dbo.Folio_Special'), 'nSeq', 'IsIdentity')";

const NSEQ_NEXT: &str = "(SELECT ISNULL(MAX(s.nSeq), 0) + 1 FROM Folio_Special s WITH (UPDLOCK, HOLDLOCK) WHERE s.FolioNum = @FolioNum)";

/// `SmileWriteOptions.HubExtraBedSpecialCode`: the extra-bed special the hotel's admin
/// picked on the hub (ADR-0106), delivered with `api/agent/sync-config`. Runtime state,
/// shared between the sync worker that reads sync-config and the writer.
#[derive(Debug, Clone, Default)]
pub struct HubSpecialCode(Arc<Mutex<Option<String>>>);

impl HubSpecialCode {
    /// The hub's choice (`None`: none picked, or not heard from the hub yet).
    pub fn get(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// `SetHubExtraBedSpecialCode`: records the hub's choice (blank: none); `true` when it
    /// changed.
    pub fn set(&self, code: Option<&str>) -> bool {
        let next = code
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_owned);
        let mut cur = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let changed = *cur != next;
        *cur = next;
        changed
    }
}

impl PartialEq for HubSpecialCode {
    fn eq(&self, other: &HubSpecialCode) -> bool {
        self.get() == other.get()
    }
}

impl Eq for HubSpecialCode {}

/// `SpecialRowMarker`: `MM:<bookingRoomId> giường phụ (MM)`.
pub fn special_row_marker(room_item_id: Uuid) -> String {
    format!("MM:{room_item_id} giường phụ (MM)")
}

impl FolioWriter {
    /// `EffectiveExtraBedSpecialCode`: the hub's choice, else `Smile:ExtraBedSpecialCode`.
    pub fn effective_special_code(&self) -> String {
        self.opts
            .hub_special_code
            .get()
            .unwrap_or_else(|| self.opts.extra_bed_special_code.clone())
    }

    /// `ReadExtraBedInputsAsync`: `None` in `FOLD_LEGACY` mode. `special_code` is the
    /// admin's pick from the job payload (ADR-0106): the terms are then read for that code
    /// on the PMS-priced basis; `None` means the hub's sync-config code (also PMS-priced),
    /// else the configured code (agent-priced).
    pub fn read_extra_bed_inputs(
        &self,
        s: &mut dyn Session,
        verified_fingerprint: Option<&str>,
        special_code: Option<&str>,
    ) -> Result<Option<PreflightInputs>, WriteError> {
        if self.opts.extra_bed_mode != MODE_SMILE_SPECIAL {
            return Ok(None);
        }
        let inputs = |terms| PreflightInputs {
            configured_mode: self.opts.extra_bed_mode.clone(),
            verified_fingerprint: verified_fingerprint.map(str::to_owned),
            terms,
            configured_smile_hotel_id: self.opts.smile_hotel_id.clone(),
        };
        // Without a writable Folio_Special the bed would vanish from the folio: fold.
        let special = self.probe.get(s, "Folio_Special")?;
        if !SPECIAL_WRITE_COLUMNS.iter().all(|c| special.has(c)) {
            log::warn(
                "SMILE Folio_Special lacks the extra-bed columns; SMILE_SPECIAL folds this write",
                &[("reason", V::S(FOLD_FINGERPRINT_DRIFT))],
            );
            return Ok(Some(inputs(None)));
        }
        let chosen = match special_code.map(str::trim).filter(|c| !c.is_empty()) {
            Some(c) => Some(c.to_owned()),
            None => self.opts.hub_special_code.get(),
        };
        let code = chosen
            .clone()
            .unwrap_or_else(|| self.opts.extra_bed_special_code.clone());
        let read = bed_terms_reader::read(s, self.opts.smile_hotel_id.as_deref(), &code)?;
        if read.terms.is_none() {
            log::warn(
                "SMILE extra-bed terms unreadable; SMILE_SPECIAL folds this write",
                &[(
                    "skip_reason",
                    V::S(read.skip_reason.as_deref().unwrap_or("")),
                )],
            );
        }
        let terms = if chosen.is_some() {
            read.terms.map(|t| t.as_pms_priced())
        } else {
            read.terms
        };
        Ok(Some(inputs(terms)))
    }

    /// `WriteMmBedRowsInTxAsync`: this room's agent rows brought to `bed.special_rows`
    /// inside the folio transaction. Nothing when they already match; else the marked rows
    /// are deleted and the wanted ones inserted. A fold wants none, so a switch back to
    /// `FOLD_LEGACY` deletes them.
    pub(crate) fn write_mm_bed_rows_in_tx(
        &self,
        s: &mut dyn Session,
        schema: &TableSchema,
        folio_num: Decimal,
        room_item_id: Uuid,
        bed: &BedDecision,
    ) -> Result<(), WriteError> {
        if !schema.has("SPNotes") {
            if !bed.special_rows.is_empty() {
                return Err(invalid(
                    "Folio_Special.SPNotes missing for a SMILE_SPECIAL write.",
                ));
            }
            return Ok(());
        }
        // A cancelled agent row counts as "not as wanted", so it is replaced by a live one.
        let cancelled = if schema.has("CancelNum") {
            "CAST(CASE WHEN ISNULL(CancelNum, 0) = 0 THEN 0 ELSE 1 END AS bit)"
        } else {
            "CAST(0 AS bit)"
        };
        let pattern = lookup::special_row_marker_pattern(room_item_id);
        let params = [
            p("@FolioNum", decimal(folio_num)),
            p("@Marker", nvarchar(Some(&pattern))),
        ];
        let current = s.rows(
            &format!(
                "SELECT LTRIM(RTRIM(ISNULL(SpecialCode, ''))) AS SpecialCode, CAST(ISNULL(Qty, 0) AS int) AS Qty,
       CAST(ISNULL(Price, 0) AS decimal(18,2)) AS Price, CAST(ISNULL(FromDay, 0) AS int) AS FromDay,
       CAST(ISNULL(ToDay, 0) AS int) AS ToDay, {cancelled} AS Cancelled
FROM Folio_Special WITH (UPDLOCK)
WHERE FolioNum = @FolioNum AND SPNotes LIKE @Marker"
            ),
            &params,
        )?;
        let mut wanted = bed.special_rows.clone();
        wanted.sort_by_key(|r| r.from_day);
        let code = bed
            .special_code
            .clone()
            .unwrap_or_else(|| self.effective_special_code());
        let mut have = Vec::with_capacity(current.len());
        let mut same = true;
        for r in &current {
            let row_code = r.opt_str("SpecialCode")?.unwrap_or_default();
            same &=
                !r.opt_bool("Cancelled")?.unwrap_or(false) && row_code.eq_ignore_ascii_case(&code);
            have.push(SpecialBedRow {
                qty: to_i32(r.int("Qty")?)?,
                price: r.opt_dec("Price")?.unwrap_or(Decimal::ZERO),
                from_day: to_i32(r.int("FromDay")?)?,
                to_day: to_i32(r.int("ToDay")?)?,
            });
        }
        have.sort_by_key(|r| r.from_day);
        if same && have == wanted {
            return Ok(());
        }
        if !current.is_empty() {
            s.execute(
                "DELETE FROM Folio_Special WHERE FolioNum = @FolioNum AND SPNotes LIKE @Marker",
                &params,
            )?;
        }
        if wanted.is_empty() {
            return Ok(());
        }
        let nseq = nseq_literal(s)?;
        let marker = tcvn3::encode(&special_row_marker(room_item_id));
        for row in &wanted {
            let mut c = ColumnList::new(schema).core("FolioNum", decimal(folio_num));
            if let Some(n) = nseq {
                c = c.core_lit("nSeq", n);
            }
            let c = c
                .opt("SpecialCode", varchar(Some(&code)))
                .opt("Qty", int(row.qty))
                .opt("Price", decimal(row.price))
                .opt("FromDay", int(row.from_day))
                .opt("ToDay", int(row.to_day))
                .opt("SPNotes", varchar(Some(&marker)))
                .opt_lit("CancelNum", "0");
            if !c.has_param("SPNotes") {
                return Err(invalid(
                    "Folio_Special.SPNotes too narrow for the MM row marker.",
                ));
            }
            // SalesPerson is left out on purpose: NULL, so SMILE books no commission.
            s.execute(&c.insert_sql()?, c.params())?;
        }
        Ok(())
    }
}

/// `NSeqLiteralAsync`: none when `nSeq` is an IDENTITY (or unknown), else the folio's next
/// number under the transaction's update lock.
fn nseq_literal(s: &mut dyn Session) -> Result<Option<&'static str>, WriteError> {
    let rows = s.rows(NSEQ_IDENTITY_SQL, &[])?;
    let identity = match rows.first() {
        Some(r) => r.scalar_int()?,
        None => None,
    };
    Ok(match identity {
        Some(1) | None => None,
        Some(_) => Some(NSEQ_NEXT),
    })
}

/// .NET `InvalidOperationException`: an unclassified failure (`UNKNOWN`).
fn invalid(message: &str) -> WriteError {
    WriteError::Coded {
        code: UNKNOWN.into(),
        message: message.into(),
    }
}

fn to_i32(n: i64) -> Result<i32, WriteError> {
    i32::try_from(n)
        .map_err(|_| WriteError::Smile(crate::smile::SmileError::Decode("Folio_Special")))
}

#[cfg(test)]
#[path = "special_rows_tests.rs"]
mod tests;
