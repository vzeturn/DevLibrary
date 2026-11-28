//! Read-only PMS audit (Go `internal/pms/audit.go`, RT-0702/RT-1480): the live Hotel Link
//! folios of a date window, or every folio of a few PMS keys, with their last-change stamp,
//! newest FolioLog rows and (targeted audits) FolioRate nightly prices. Only SELECTs, every
//! value is a parameter, row counts are capped.

use std::collections::BTreeMap;

use dl_tds::Param;
use dl_time::DateTime;

use super::repo::{SqlRepo, opt_str, rows};
use super::{AGENT_CANCEL_CLERK_ID, AGENT_CLERK_ID, LEGACY_AGENT_CANCEL_CLERK_ID};
use crate::error::Result;
use crate::jobmodel::{AuditFolio, AuditLogRow, AuditRate};

/// Hard cap on folio rows per audit.
pub const AUDIT_MAX_ROWS: i64 = 5000;
/// Hard cap on FolioLog rows per folio.
pub const AUDIT_MAX_LOG_ROWS: i64 = 50;
/// Hard cap on keys of a targeted audit.
pub const AUDIT_MAX_KEYS: usize = 20;
/// Folio numbers per child-table query (SQL Server allows 2100 parameters).
const AUDIT_CHUNK: usize = 400;

/// `AuditQuery`: `from` = first arrival date (window audit); `keys` non-empty = targeted
/// audit (all folios of those ExternalConfirmNumber/Booker values, live and cancelled).
#[derive(Debug, Clone)]
pub struct AuditQuery {
    /// First arrival date of a window audit.
    pub from: DateTime,
    /// Folio row cap.
    pub max_rows: i64,
    /// Newest FolioLog rows per folio.
    pub log_rows: i64,
    /// Targeted keys.
    pub keys: Vec<String>,
}

/// `AuditResult`: `truncated` = more rows than `max_rows` matched; `log_err` = folios were
/// read but their FolioLog/FolioRate rows were not (reported, never silent).
#[derive(Debug, Clone, Default)]
pub struct AuditResult {
    /// The folios read.
    pub folios: Vec<AuditFolio>,
    /// The row cap was reached.
    pub truncated: bool,
    /// Child-table read failure text.
    pub log_err: Option<String>,
}

const AUDIT_COLS: &str = "FolioNum, ConfirmNum, ISNULL(ExternalConfirmNumber, ''), ISNULL(Booker, ''),
       ISNULL(BookStatus, 0), ISNULL(FolioStatus, 0), ISNULL(CancelNum, 0), ISNULL(RoomCode, ''),
       ISNULL(RoomTypeCode, ''), ISNULL(NumRoom, 1), ArrivalDate, DepartureDate, ISNULL(UserInfo1, ''),
       ISNULL(LastName, ''), ISNULL(ReservationClerkID, ''), ISNULL(LastChangeClerkID, ''), LastChangeTime";

fn clamp(v: i64, lo: i64, hi: i64) -> i64 {
    v.clamp(lo, hi)
}

/// `placeholders`: `@p<start>,@p<start+1>,…` (n of them).
fn placeholders(start: usize, n: usize) -> String {
    (start..start + n)
        .map(|i| format!("@p{i}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// `auditFolioQuery`: the folio SELECT and its parameters.
pub(super) fn audit_folio_query(q: &AuditQuery) -> (String, Vec<Param>) {
    let limit = clamp(q.max_rows, 1, AUDIT_MAX_ROWS) + 1; // +1 detects truncation
    if !q.keys.is_empty() {
        let ph = placeholders(2, q.keys.len());
        let mut params = vec![Param::new("p1", limit)];
        for (i, k) in q.keys.iter().enumerate() {
            params.push(Param::new(&format!("p{}", i + 2), k.as_str()));
        }
        let sql = format!(
            "SELECT TOP (@p1) {AUDIT_COLS} FROM Folio
WHERE ExternalConfirmNumber IN ({ph}) OR Booker IN ({ph})
ORDER BY ArrivalDate, FolioNum"
        );
        return (sql, params);
    }
    let sql = format!(
        "SELECT TOP (@p1) {AUDIT_COLS} FROM Folio
WHERE ArrivalDate >= @p2
  AND (ISNULL(CancelNum, 0) = 0 OR ISNULL(LastChangeClerkID, '') NOT IN (@p4, @p5))
  AND (UserInfo1 LIKE 'rt:%' OR UserInfo1 LIKE '#%' OR ReservationClerkID = @p3
       OR ISNULL(ExternalConfirmNumber, '') <> '')
ORDER BY ArrivalDate, FolioNum"
    );
    let params = vec![
        Param::new("p1", limit),
        Param::new("p2", q.from),
        Param::new("p3", AGENT_CLERK_ID),
        Param::new("p4", AGENT_CANCEL_CLERK_ID),
        Param::new("p5", LEGACY_AGENT_CANCEL_CLERK_ID),
    ];
    (sql, params)
}

/// `NormalizeAuditKeys`: trims, drops empties and duplicates, caps the list.
pub fn normalize_audit_keys(keys: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for k in keys {
        let k = k.trim();
        if k.is_empty() || out.iter().any(|x| x == k) {
            continue;
        }
        out.push(k.to_owned());
        if out.len() == AUDIT_MAX_KEYS {
            break;
        }
    }
    out
}

/// `IsHLOrigin`: created by the HL agent (room item key `rt:…` or `#n`, or the agent clerk).
pub fn is_hl_origin(item_key: &str, reservation_clerk: &str) -> bool {
    item_key.starts_with("rt:") || item_key.starts_with('#') || reservation_clerk == AGENT_CLERK_ID
}

/// `cancelledBy`: "" live, "agent" for the agent's cancel clerks, "unknown" for HLA (a
/// front-desk cancel that kept the agent's last-change clerk, or the legacy mark cut to 3
/// characters), "staff" otherwise.
pub fn cancelled_by(cancel_num: f64, clerk: &str) -> &'static str {
    if cancel_num == 0.0 {
        ""
    } else if clerk == AGENT_CANCEL_CLERK_ID || clerk == LEGACY_AGENT_CANCEL_CLERK_ID {
        "agent"
    } else if clerk == AGENT_CLERK_ID {
        "unknown"
    } else {
        "staff"
    }
}

fn ymd(t: DateTime) -> String {
    super::entities::ymd(t)
}

fn timestamp(t: DateTime) -> String {
    format!(
        "{} {:02}:{:02}:{:02}",
        ymd(t),
        t.hour(),
        t.minute(),
        t.second()
    )
}

/// Folio numbers in first-seen order sorted ascending, and folio number → positions.
fn index_folios(folios: &[AuditFolio]) -> (Vec<f64>, BTreeMap<u64, Vec<usize>>) {
    let mut idx: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
    let mut nums: Vec<f64> = Vec::new();
    for (i, f) in folios.iter().enumerate() {
        let e = idx.entry(f.folio_num.to_bits()).or_default();
        if e.is_empty() {
            nums.push(f.folio_num);
        }
        e.push(i);
    }
    nums.sort_by(f64::total_cmp);
    (nums, idx)
}

impl SqlRepo {
    /// `AuditFolios`: a failure reading folios is an error; a failure reading FolioLog or
    /// FolioRate is returned in `log_err`.
    pub(super) fn audit_folios_impl(&self, q: &AuditQuery) -> Result<AuditResult> {
        let keys = normalize_audit_keys(&q.keys);
        let q = AuditQuery { keys, ..q.clone() };
        let (sql, params) = audit_folio_query(&q);
        let mut folios = self.with_conn(|c| {
            rows(c, &sql, &params)
                .map_err(|e| e.context("audit folios: "))?
                .iter()
                .map(|r| {
                    let item_key = opt_str(r, 12)?;
                    let reservation_clerk = opt_str(r, 14)?;
                    let last_change_clerk_id = opt_str(r, 15)?;
                    let cancel_num: f64 = r.get(6)?;
                    Ok(AuditFolio {
                        folio_num: r.get(0)?,
                        confirm_num: r.get(1)?,
                        external_conf: opt_str(r, 2)?,
                        booker: opt_str(r, 3)?,
                        book_status: i64::from(r.get::<i32>(4)?),
                        folio_status: i64::from(r.get::<i32>(5)?),
                        cancel_num,
                        room_code: opt_str(r, 7)?,
                        room_type_code: opt_str(r, 8)?,
                        num_room: i64::from(r.get::<i32>(9)?),
                        arrival_date: ymd(r.get::<DateTime>(10)?),
                        departure_date: ymd(r.get::<DateTime>(11)?),
                        hl_origin: is_hl_origin(&item_key, &reservation_clerk),
                        item_key,
                        guest_name: opt_str(r, 13)?,
                        cancelled_by: cancelled_by(cancel_num, &last_change_clerk_id).to_owned(),
                        reservation_clerk,
                        last_change_time: r
                            .get::<Option<DateTime>>(16)?
                            .map(timestamp)
                            .unwrap_or_default(),
                        last_change_clerk_id,
                        ..AuditFolio::default()
                    })
                })
                .collect::<Result<Vec<_>>>()
        })?;
        let max = clamp(q.max_rows, 1, AUDIT_MAX_ROWS) as usize;
        let truncated = folios.len() > max;
        folios.truncate(max);
        let mut errs: Vec<String> = Vec::new();
        let log_rows = clamp(q.log_rows, 0, AUDIT_MAX_LOG_ROWS);
        if log_rows > 0
            && !folios.is_empty()
            && let Err(e) = self.attach_folio_logs(&mut folios, log_rows)
        {
            errs.push(e.message().to_owned());
        }
        if !q.keys.is_empty() && !folios.is_empty() {
            // Targeted evidence: nightly prices too (RT-1480).
            if let Err(e) = self.attach_folio_rates(&mut folios) {
                errs.push(e.message().to_owned());
            }
        }
        let log_err = (!errs.is_empty()).then(|| errs.join("; "));
        Ok(AuditResult {
            folios,
            truncated,
            log_err,
        })
    }

    fn attach_folio_logs(&self, folios: &mut [AuditFolio], n: i64) -> Result<()> {
        let (nums, idx) = index_folios(folios);
        for chunk in nums.chunks(AUDIT_CHUNK) {
            let mut params = vec![Param::new("p1", n)];
            for (i, x) in chunk.iter().enumerate() {
                params.push(Param::new(&format!("p{}", i + 2), *x));
            }
            let sql = format!(
                "SELECT FolioNum, FieldName, OldValue, NewValue, ChangedClerk, ChangedTime FROM (
  SELECT FolioNum, ISNULL(FieldName, '') AS FieldName, ISNULL(CAST(OldValue AS nvarchar(1000)), '') AS OldValue,
         ISNULL(CAST(NewValue AS nvarchar(1000)), '') AS NewValue, ISNULL(ChangedClerk, '') AS ChangedClerk, ChangedTime,
         ROW_NUMBER() OVER (PARTITION BY FolioNum ORDER BY ChangedTime DESC) AS rn
  FROM FolioLog WHERE FolioNum IN ({})) x
WHERE rn <= @p1 ORDER BY FolioNum, ChangedTime DESC",
                placeholders(2, chunk.len())
            );
            let found = self
                .with_conn(|c| rows(c, &sql, &params).map_err(|e| e.context("audit FolioLog: ")))?;
            for r in &found {
                let num: f64 = r.get(0)?;
                let row = AuditLogRow {
                    field: opt_str(r, 1)?,
                    old_value: opt_str(r, 2)?,
                    new_value: opt_str(r, 3)?,
                    clerk: opt_str(r, 4)?,
                    at: r
                        .get::<Option<DateTime>>(5)?
                        .map(timestamp)
                        .unwrap_or_default(),
                };
                for &i in idx.get(&num.to_bits()).into_iter().flatten() {
                    folios[i].log.push(row.clone());
                }
            }
        }
        Ok(())
    }

    fn attach_folio_rates(&self, folios: &mut [AuditFolio]) -> Result<()> {
        let (nums, idx) = index_folios(folios);
        for chunk in nums.chunks(AUDIT_CHUNK) {
            let params: Vec<Param> = chunk
                .iter()
                .enumerate()
                .map(|(i, x)| Param::new(&format!("p{}", i + 1), *x))
                .collect();
            let sql = format!(
                "SELECT FolioNum, hDate, ISNULL(RateAmount, 0) FROM FolioRate WHERE FolioNum IN ({}) ORDER BY FolioNum, hDate",
                placeholders(1, chunk.len())
            );
            let found = self.with_conn(|c| {
                rows(c, &sql, &params).map_err(|e| e.context("audit FolioRate: "))
            })?;
            for r in &found {
                let num: f64 = r.get(0)?;
                let rate = AuditRate {
                    date: ymd(r.get::<DateTime>(1)?),
                    amount: r.get(2)?,
                };
                for &i in idx.get(&num.to_bits()).into_iter().flatten() {
                    folios[i].rates.push(rate.clone());
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dl_time::Date;

    fn q(keys: &[&str]) -> AuditQuery {
        AuditQuery {
            from: Date::new(2026, 11, 1).expect("date").midnight(),
            max_rows: 10_000,
            log_rows: 0,
            keys: keys.iter().map(|k| (*k).to_owned()).collect(),
        }
    }

    #[test]
    fn window_query_is_read_only_and_parameterized() {
        let (sql, params) = audit_folio_query(&q(&[]));
        assert!(sql.starts_with("SELECT TOP (@p1)"), "{sql}");
        assert!(sql.contains("ArrivalDate >= @p2"));
        for bad in ["INSERT", "UPDATE", "DELETE", "EXEC"] {
            assert!(!sql.contains(bad), "{sql}");
        }
        assert_eq!(params.len(), 5);
    }

    #[test]
    fn targeted_query_repeats_the_key_placeholders() {
        let (sql, params) = audit_folio_query(&q(&["K1", "K2"]));
        assert!(
            sql.contains("ExternalConfirmNumber IN (@p2,@p3) OR Booker IN (@p2,@p3)"),
            "{sql}"
        );
        assert_eq!(params.len(), 3);
    }

    #[test]
    fn keys_are_trimmed_deduplicated_and_capped() {
        let many: Vec<String> = (0..30).map(|i| format!("k{i}")).collect();
        assert_eq!(normalize_audit_keys(&many).len(), AUDIT_MAX_KEYS);
        let k = normalize_audit_keys(&[" a ".into(), "a".into(), "".into(), "b".into()]);
        assert_eq!(k, vec!["a".to_owned(), "b".to_owned()]);
    }

    #[test]
    fn cancel_origin_follows_the_last_change_clerk() {
        assert_eq!(cancelled_by(0.0, "HLC"), "");
        assert_eq!(cancelled_by(5.0, "HLC"), "agent");
        assert_eq!(cancelled_by(5.0, "HLAGENT-CANCEL"), "agent");
        assert_eq!(cancelled_by(5.0, "HLA"), "unknown");
        assert_eq!(cancelled_by(5.0, "FD1"), "staff");
        assert!(is_hl_origin("rt:abc", ""));
        assert!(is_hl_origin("#3", ""));
        assert!(is_hl_origin("", "HLA"));
        assert!(!is_hl_origin("", "FD1"));
    }
}
