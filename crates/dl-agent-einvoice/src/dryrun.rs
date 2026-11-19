//! `einvoice.collect_dry_run`: the bills of a date window, fetched with `sp_FetchFO` /
//! `sp_FetchPOS` and built into the exact push items a cycle would upload, counted and
//! hashed, with one scrubbed sample. It never uploads, never marks a bill collected and
//! never writes back: the only statements it runs are SELECTs and the fetch procedures.
//!
//! The window lists bills with the filter of the shipped select procedures (printed and
//! not void; POS checks not charged to a folio) but ignores their "already pushed" test, so
//! a site that is already collecting still shows its bills, each with the state the next
//! cycle would see (`new`, `retry`, `changed`, `unchanged`).

use std::time::{Duration, Instant};

use dl_json::{Map, Value};
use dl_time::Date;

use crate::config::Rules;
use crate::sql::{CollectKey, Sql, WindowBill};

/// Bills per source when the command names no limit.
pub const DEFAULT_LIMIT: i64 = 20;
/// The most bills per source one dry run builds.
pub const MAX_LIMIT: i64 = 200;
/// The longest window, in days.
pub const MAX_DAYS: i64 = 92;
/// Invoice lines kept in the sample.
const SAMPLE_LINES: usize = 5;
/// Bills listed with their hashes in the output.
const LISTED: usize = 50;
/// Payload keys whose values name or reach a person or a buyer: masked in the sample.
const PERSONAL: &[&str] = &[
    "buyer",
    "customer",
    "guest",
    "contact",
    "email",
    "tel",
    "phone",
    "passport",
    "citizen",
    "id_number",
    "addr",
    "company_name",
    "confirm_no",
];

/// What to dry-run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DryRun {
    /// First day (Vietnam time).
    pub from: Date,
    /// Last day, inclusive.
    pub to: Date,
    /// `FO` and/or `POS`.
    pub sources: Vec<&'static str>,
    /// Bills per source.
    pub limit: i64,
}

impl DryRun {
    /// Reads `{"from": "YYYY-MM-DD", "to": "YYYY-MM-DD", "sources": "FO,POS", "limit": 20}`;
    /// every key is optional (`to` = `today`, `from` = `to`, sources = the site's).
    ///
    /// # Errors
    /// A malformed date, a reversed or too long window, an unknown source.
    pub fn parse(
        args: &Value,
        site_sources: &[&'static str],
        today: Date,
    ) -> Result<DryRun, String> {
        let date = |k: &str| -> Result<Option<Date>, String> {
            match args.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(v) => {
                    let s = v
                        .as_str()
                        .ok_or_else(|| format!("{k}: expected \"YYYY-MM-DD\""))?;
                    s.trim()
                        .parse::<Date>()
                        .map(Some)
                        .map_err(|e| format!("{k} {s:?}: expected YYYY-MM-DD: {e}"))
                }
            }
        };
        let to = date("to")?.unwrap_or(today);
        let from = date("from")?.unwrap_or(to);
        if from > to {
            return Err(format!("from {from} is after to {to}"));
        }
        let days = (to.midnight() - from.midnight()).whole_days() + 1;
        if days > MAX_DAYS {
            return Err(format!(
                "window of {days} days is longer than {MAX_DAYS}; split it"
            ));
        }
        let sources = match args.get("sources").and_then(Value::as_str).map(str::trim) {
            None | Some("") => site_sources.to_vec(),
            Some("FO") => vec!["FO"],
            Some("POS") => vec!["POS"],
            Some("FO,POS" | "POS,FO") => vec!["FO", "POS"],
            Some(other) => return Err(format!("sources {other:?}: expected FO, POS or FO,POS")),
        };
        let limit = match args.get("limit") {
            None | Some(Value::Null) => DEFAULT_LIMIT,
            Some(v) => v
                .as_i64()
                .filter(|n| (1..=MAX_LIMIT).contains(n))
                .ok_or_else(|| format!("limit: expected 1..={MAX_LIMIT}"))?,
        };
        Ok(DryRun {
            from,
            to,
            sources,
            limit,
        })
    }
}

/// Runs the dry run against `sql` (its POS item-name profile already set) under `rules`.
/// SQL failures of one source are reported in that source's entry, not as an error.
pub fn run(sql: &Sql, rules: &Rules, d: &DryRun, deadline: Instant) -> Value {
    let from = d.from.midnight();
    let to = d.to.midnight() + dl_time::Duration::from_days(1);
    let mut sources = Map::new();
    for source in &d.sources {
        let timeout = left(deadline);
        let entry = match sql.window(source, from, to, d.limit, timeout) {
            Ok(bills) => build(sql, rules, source, &bills, deadline),
            Err(e) => {
                let mut m = Map::new();
                m.insert("database", sql.database(source));
                m.insert("error", e);
                m
            }
        };
        sources.insert(*source, Value::Object(entry));
    }
    let mut window = Map::new();
    window.insert("from", d.from.to_string());
    window.insert("to", d.to.to_string());
    window.insert(
        "filter",
        "shipped select procedures (printed, not void; POS not on a folio), already-pushed bills included",
    );
    let mut out = Map::new();
    out.insert("dry_run", true);
    out.insert("uploaded", false);
    out.insert("written_back", false);
    out.insert("window", Value::Object(window));
    out.insert(
        "pos_item_name",
        format!("{:?}", sql.pos_item_name()).to_lowercase(),
    );
    out.insert("recalc_tax_into_sc", rules.recalc_tax_into_sc);
    out.insert("sources", Value::Object(sources));
    Value::Object(out)
}

fn left(deadline: Instant) -> Duration {
    deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_secs(1))
}

/// Fetches and builds every listed bill of one source.
fn build(sql: &Sql, rules: &Rules, source: &str, bills: &[WindowBill], deadline: Instant) -> Map {
    const STATES: [&str; 4] = ["new", "retry", "changed", "unchanged"];
    let mut counts = [0u64; 4];
    let (mut built, mut would_push, mut invoiced, mut total_amount) = (0u64, 0u64, 0u64, 0i64);
    let (mut listed, mut errors, mut sample) = (Vec::new(), Vec::new(), None);
    let mut timed_out = false;
    for b in bills {
        let state = b.state();
        if let Some(i) = STATES.iter().position(|s| *s == state) {
            counts[i] += 1;
        }
        if b.invoice_number.is_some() {
            invoiced += 1;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            break;
        }
        let key = CollectKey {
            source: source.to_owned(),
            id: b.id.clone(),
        };
        let item = crate::fetch::collected(sql, rules, &key, left(deadline)).and_then(|inv| {
            let lines = inv.lines.len();
            crate::push_item(inv).map(|item| (item, lines))
        });
        let mut row = Map::new();
        row.insert("source_ref", key.source_ref());
        row.insert("state", state);
        row.insert("source_hash", b.source_hash.as_deref().map(hex));
        if let Some(n) = &b.invoice_number {
            row.insert("invoice_number", n.as_str());
        }
        match item {
            Ok((item, lines)) => {
                built += 1;
                if state != "unchanged" {
                    would_push += 1;
                }
                total_amount += item.total_amount;
                row.insert("payload_hash", item.payload_hash.as_str());
                row.insert("total_amount", item.total_amount);
                row.insert("lines", lines as u64);
                if sample.is_none() {
                    sample = Some(scrubbed(&item.to_json()));
                }
            }
            Err(e) => {
                row.insert("error", e.as_str());
                errors.push(Value::from(format!("{}: {e}", key.source_ref())));
            }
        }
        if listed.len() < LISTED {
            listed.push(Value::Object(row));
        }
    }
    let mut m = Map::new();
    m.insert("database", sql.database(source));
    m.insert("in_window", bills.first().map_or(0, |b| b.total));
    m.insert("listed", bills.len() as u64);
    m.insert("built", built);
    m.insert("failed", errors.len() as u64);
    m.insert("would_push", would_push);
    m.insert("invoiced", invoiced);
    m.insert("total_amount", total_amount);
    let mut states = Map::new();
    for (s, n) in STATES.iter().zip(counts) {
        states.insert(*s, n);
    }
    m.insert("states", Value::Object(states));
    if timed_out {
        m.insert("timed_out", true);
    }
    m.insert("bills", Value::Array(listed));
    m.insert("errors", Value::Array(errors));
    m.insert("sample", sample.unwrap_or(Value::Null));
    m
}

fn hex(b: &[u8]) -> String {
    dl_encoding::hex::encode(b)
}

/// The push item with every personal value masked and the lines cut to a few.
pub fn scrubbed(item_json: &str) -> Value {
    let Ok(mut v) = dl_json::parse(item_json) else {
        return Value::from("<push item did not parse>");
    };
    scrub(&mut v);
    if let Some(lines) = v
        .get_mut("payload")
        .and_then(|p| p.get_mut("lines"))
        .and_then(Value::as_array_mut)
        && lines.len() > SAMPLE_LINES
    {
        let more = lines.len() - SAMPLE_LINES;
        lines.truncate(SAMPLE_LINES);
        lines.push(Value::from(format!("... {more} more lines")));
    }
    v
}

fn scrub(v: &mut Value) {
    match v {
        Value::Object(m) => {
            for (k, val) in m.iter_mut() {
                let key = k.to_ascii_lowercase();
                if !key.starts_with("seller") && PERSONAL.iter().any(|p| key.contains(p)) {
                    mask(val);
                } else {
                    scrub(val);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(scrub),
        _ => {}
    }
}

fn mask(v: &mut Value) {
    match v {
        Value::String(s) if !s.is_empty() => {
            *v = Value::from(format!("<{} chars>", s.chars().count()));
        }
        Value::Object(_) | Value::Array(_) => scrub(v),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use dl_json::json;

    use super::*;

    fn day(s: &str) -> Date {
        s.parse().unwrap()
    }

    #[test]
    fn defaults_to_today_and_the_site_sources() {
        let d = DryRun::parse(&json!({}), &["FO", "POS"], day("2026-11-19")).unwrap();
        assert_eq!(d.from, day("2026-11-19"));
        assert_eq!(d.to, day("2026-11-19"));
        assert_eq!(d.sources, vec!["FO", "POS"]);
        assert_eq!(d.limit, DEFAULT_LIMIT);
    }

    #[test]
    fn reads_a_window_sources_and_limit() {
        let args = json!({"from": "2026-11-01", "to": "2026-11-18", "sources": "POS", "limit": 5});
        let d = DryRun::parse(&args, &["FO", "POS"], day("2026-11-19")).unwrap();
        assert_eq!((d.from, d.to), (day("2026-11-01"), day("2026-11-18")));
        assert_eq!(d.sources, vec!["POS"]);
        assert_eq!(d.limit, 5);
    }

    #[test]
    fn rejects_bad_windows_and_arguments() {
        let today = day("2026-11-19");
        let bad = [
            json!({"from": "2026-11-20", "to": "2026-11-19"}),
            json!({"from": "19/11/2026"}),
            json!({"from": "2026-01-01", "to": "2026-11-19"}),
            json!({"sources": "BO"}),
            json!({"limit": 0}),
            json!({"limit": 201}),
            json!({"to": 20261119}),
        ];
        for args in bad {
            assert!(DryRun::parse(&args, &["FO"], today).is_err(), "{args:?}");
        }
    }

    #[test]
    fn the_sample_masks_people_and_keeps_money() {
        let item = r#"{"source_ref":"FO:1","payload_hash":"ab","payload":{"header":{"seller_tax_code":"0301","customer_tax":"0123456789","customer_email":"a@b.vn","guest_name":"Nguyễn Văn A","buyer_contact_name":"","total_amount":110000},"lines":[{"name":"Phở","amount":1},{"name":"b"},{"name":"c"},{"name":"d"},{"name":"e"},{"name":"f"},{"name":"g"}]},"buyer_name":"Công ty X","buyer_email":null,"total_amount":110000}"#;
        let v = scrubbed(item);
        let h = v.pointer("/payload/header").unwrap();
        assert_eq!(h.get("seller_tax_code").unwrap(), &Value::from("0301"));
        assert_eq!(h.get("customer_tax").unwrap(), &Value::from("<10 chars>"));
        assert_eq!(h.get("customer_email").unwrap(), &Value::from("<6 chars>"));
        assert_eq!(h.get("guest_name").unwrap(), &Value::from("<12 chars>"));
        assert_eq!(h.get("buyer_contact_name").unwrap(), &Value::from(""));
        assert_eq!(h.get("total_amount").unwrap(), &Value::from(110000));
        assert_eq!(v.get("buyer_name").unwrap(), &Value::from("<9 chars>"));
        assert_eq!(v.get("buyer_email").unwrap(), &Value::Null);
        let lines = v.pointer("/payload/lines").unwrap().as_array().unwrap();
        assert_eq!(lines.len(), SAMPLE_LINES + 1);
        assert_eq!(lines[0].get("name").unwrap(), &Value::from("Phở"));
        assert_eq!(lines[SAMPLE_LINES], Value::from("... 2 more lines"));
    }
}
