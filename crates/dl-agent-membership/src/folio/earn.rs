//! The earn pairs (`EarnPair`, `EarnPairReader.Validate`, `EarnPairSource`,
//! `EarnPairFilter`): which `FolioTransaction (code, sub)` lines count as qualifying
//! revenue, derived from SMILE's own configuration unless the operator overrides it.
//!
//! The 24 h drift check (`ReadMostPostedRoomPairAsync`) is not ported: it only logs a
//! warning and never changes the set.

use std::fmt;

use dl_tds::{Param, ParamValue};

use crate::smile::{CodeRow, EarnCatalog};

/// `PMS_EARN_ROOM_CODE_AMBIGUOUS`.
pub const ROOM_CODE_AMBIGUOUS: &str = "PMS_EARN_ROOM_CODE_AMBIGUOUS";

/// The normalised sub code, from the shared SA_ layer (RT-1432): blank/00/padded/int/text all
/// compare equal. The only definition of that normalisation lives in `dbo.SA_TrnKey`.
pub(crate) const SUB_EXPR: &str = "(SELECT K FROM dbo.SA_TrnKey(t.TransactionSubCode))";

/// One `(TransactionCode, TransactionSubCode)` that earns; `sub = None` means any sub-code.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EarnPair {
    /// The main code, trimmed.
    pub code: String,
    /// The normalized sub-code, or `None` for any.
    pub sub: Option<String>,
}

impl EarnPair {
    /// `EarnPair.Of`: a code with its normalized sub-code.
    pub fn of(code: &str, sub: Option<&str>) -> EarnPair {
        EarnPair {
            code: code.trim().to_owned(),
            sub: Some(normalize_sub(sub)),
        }
    }

    /// `EarnPair.AnySub`.
    pub fn any_sub(code: &str) -> EarnPair {
        EarnPair {
            code: code.trim().to_owned(),
            sub: None,
        }
    }
}

impl fmt::Display for EarnPair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.sub {
            Some(sub) => write!(f, "{}/{sub}", self.code),
            None => write!(f, "{}/*", self.code),
        }
    }
}

/// `EarnPair.NormalizeSub`: blank is `"0"`, all-digit codes lose their leading zeros.
pub fn normalize_sub(sub: Option<&str>) -> String {
    let s = sub.map(str::trim).unwrap_or("");
    if s.is_empty() {
        return "0".into();
    }
    if !s.bytes().all(|b| b.is_ascii_digit()) {
        return s.to_owned();
    }
    let stripped = s.trim_start_matches('0');
    if stripped.is_empty() {
        "0".into()
    } else {
        stripped.to_owned()
    }
}

/// `EarnPairSet`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EarnPairSet {
    /// The pairs, in candidate order.
    pub pairs: Vec<EarnPair>,
    /// `TransactionCode` rows with `AutoPost = 1`.
    pub auto_post_count: usize,
    /// Built from `earn_charge_codes` rather than SMILE.
    pub from_override: bool,
    /// `source:code/sub` of candidates missing from `TransactionCode`.
    pub dropped: Vec<String>,
    /// The room-charge main codes.
    pub room_codes: Vec<String>,
}

impl EarnPairSet {
    /// `IsAmbiguous`: SMILE does not say which code is the room charge.
    pub fn is_ambiguous(&self) -> bool {
        !self.from_override && self.auto_post_count != 1
    }

    /// The pairs as `code/sub,…` (log-safe: configuration codes only).
    pub fn describe(&self) -> String {
        join(&self.pairs)
    }
}

fn join(pairs: &[EarnPair]) -> String {
    pairs
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

const SOURCE_AUTO_POST: &str = "AutoPost";
const SOURCE_ROOM_TYPE: &str = "RoomType";
const SOURCE_RM_CHG_TRN: &str = "Folio.RmChgTrn";
const SOURCE_PACKAGE: &str = "PackageCode";
const SOURCE_GLOBAL: &str = "GlobalParameter.EBedSurChg";
const SOURCE_SPECIAL: &str = "SpecialService";

/// The override set (`earn_charge_codes`), when configured.
pub fn from_override(codes: &[String]) -> Option<EarnPairSet> {
    (!codes.is_empty()).then(|| EarnPairSet {
        pairs: codes.iter().map(|c| EarnPair::any_sub(c)).collect(),
        from_override: true,
        ..EarnPairSet::default()
    })
}

/// `EarnPairReader.ReadAsync` after the catalogue read: candidates in source order,
/// validated against `TransactionCode`.
pub fn resolve(catalog: &EarnCatalog, rm_chg_trn: &[i64]) -> EarnPairSet {
    let mut candidates: Vec<(&'static str, String, Option<String>)> = Vec::new();
    let row = |source, r: &CodeRow| (source, r.code.clone(), r.sub.clone());
    candidates.extend(
        catalog
            .transaction_codes
            .iter()
            .filter(|(_, auto)| *auto == 1)
            .map(|(r, _)| row(SOURCE_AUTO_POST, r)),
    );
    candidates.extend(catalog.room_types.iter().map(|r| row(SOURCE_ROOM_TYPE, r)));
    let mut seen = Vec::new();
    for v in rm_chg_trn.iter().copied().filter(|v| *v > 0) {
        if !seen.contains(&v) {
            seen.push(v);
            candidates.push((
                SOURCE_RM_CHG_TRN,
                (v / 100).to_string(),
                Some((v % 100).to_string()),
            ));
        }
    }
    candidates.extend(catalog.packages.iter().map(|r| row(SOURCE_PACKAGE, r)));
    if let Some((code, sub)) = global_extra_bed_pair(&catalog.globals) {
        candidates.push((SOURCE_GLOBAL, code, sub));
    }
    candidates.extend(catalog.specials.iter().map(|r| row(SOURCE_SPECIAL, r)));
    validate(catalog, &candidates)
}

fn key(code: &str, sub: Option<&str>) -> (String, String) {
    (
        normalize_sub(Some(code)).to_uppercase(),
        normalize_sub(sub).to_uppercase(),
    )
}

fn validate(
    catalog: &EarnCatalog,
    candidates: &[(&'static str, String, Option<String>)],
) -> EarnPairSet {
    let mut known: Vec<((String, String), EarnPair)> = Vec::new();
    for (r, _) in &catalog.transaction_codes {
        let k = key(&r.code, r.sub.as_deref());
        if !known.iter().any(|(existing, _)| *existing == k) {
            known.push((k, EarnPair::of(&r.code, r.sub.as_deref())));
        }
    }
    let mut set = EarnPairSet {
        auto_post_count: catalog
            .transaction_codes
            .iter()
            .filter(|(_, auto)| *auto == 1)
            .count(),
        ..EarnPairSet::default()
    };
    for (source, code, sub) in candidates {
        if code.trim().is_empty() || normalize_sub(Some(code)) == "0" {
            continue;
        }
        let k = key(code, sub.as_deref());
        let Some((_, pair)) = known.iter().find(|(existing, _)| *existing == k) else {
            set.dropped.push(format!(
                "{source}:{}/{}",
                code.trim(),
                normalize_sub(sub.as_deref())
            ));
            continue;
        };
        if !set.pairs.contains(pair) {
            set.pairs.push(pair.clone());
        }
        let room = matches!(
            *source,
            SOURCE_AUTO_POST | SOURCE_ROOM_TYPE | SOURCE_RM_CHG_TRN
        );
        if room
            && !set
                .room_codes
                .iter()
                .any(|c| c.eq_ignore_ascii_case(&pair.code))
        {
            set.room_codes.push(pair.code.clone());
        }
    }
    set
}

fn global_extra_bed_pair(globals: &[CodeRow]) -> Option<(String, Option<String>)> {
    let find = |name: &str| {
        globals
            .iter()
            .find(|g| g.code.eq_ignore_ascii_case(name))
            .and_then(|g| g.sub.clone())
    };
    let code = find("EBedSurChgTrnCode")?;
    if code.trim().is_empty() || normalize_sub(Some(&code)) == "0" {
        return None;
    }
    Some((code.trim().to_owned(), find("EBedSurChgTrnSub")))
}

/// `EarnPairSource.WithMemberDiscount`: the voucher discount line rides along as a main code.
pub fn with_member_discount(mut pairs: Vec<EarnPair>, member_discount_code: &str) -> Vec<EarnPair> {
    let code = member_discount_code.trim();
    if code.is_empty()
        || pairs
            .iter()
            .any(|p| p.sub.is_none() && p.code.eq_ignore_ascii_case(code))
    {
        return pairs;
    }
    pairs.push(EarnPair::any_sub(code));
    pairs
}

/// `EarnPairFilter.Build`: the parenthesised predicate on `FolioTransaction t` and its
/// parameters. `pairs` must not be empty.
pub fn filter(pairs: &[EarnPair]) -> Result<(String, Vec<Param>), String> {
    if pairs.is_empty() {
        return Err("earn pair filter needs at least one pair".into());
    }
    let mut parts = Vec::new();
    let mut params = Vec::new();
    let mut any: Vec<&str> = Vec::new();
    for p in pairs.iter().filter(|p| p.sub.is_none()) {
        if !any.iter().any(|c| c.eq_ignore_ascii_case(&p.code)) {
            any.push(&p.code);
        }
    }
    if !any.is_empty() {
        let names: Vec<String> = (0..any.len()).map(|i| format!("@EarnAny{i}")).collect();
        for (name, code) in names.iter().zip(&any) {
            params.push(Param::new(name.as_str(), *code));
        }
        parts.push(format!("t.TransactionCode IN ({})", names.join(",")));
    }
    let mut specific: Vec<&EarnPair> = Vec::new();
    for p in pairs.iter().filter(|p| p.sub.is_some()) {
        if !specific.contains(&p) {
            specific.push(p);
        }
    }
    for (i, p) in specific.iter().enumerate() {
        params.push(Param::new(
            format!("EarnC{i}").as_str(),
            ParamValue::VarChar(Some(p.code.clone())),
        ));
        params.push(Param::new(
            format!("EarnS{i}").as_str(),
            ParamValue::VarChar(p.sub.clone()),
        ));
        parts.push(format!(
            "(t.TransactionCode = @EarnC{i} AND {SUB_EXPR} = @EarnS{i})"
        ));
    }
    Ok((format!("({})", parts.join(" OR ")), params))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(c: &str, s: Option<&str>) -> CodeRow {
        CodeRow {
            code: c.into(),
            sub: s.map(Into::into),
        }
    }

    #[test]
    fn subs_normalize_like_dotnet() {
        assert_eq!(normalize_sub(None), "0");
        assert_eq!(normalize_sub(Some(" 007 ")), "7");
        assert_eq!(normalize_sub(Some("000")), "0");
        assert_eq!(normalize_sub(Some("A1")), "A1");
        assert_eq!(EarnPair::of(" RM ", Some("01")).to_string(), "RM/1");
        assert_eq!(EarnPair::any_sub("MD").to_string(), "MD/*");
    }

    #[test]
    fn candidates_are_validated_against_the_catalogue() {
        let catalog = EarnCatalog {
            transaction_codes: vec![
                (code("110", Some("1")), 1),
                (code("110", Some("2")), 0),
                (code("EB", Some("")), 0),
            ],
            room_types: vec![code("110", Some("2")), code("0", None)],
            packages: vec![code("PK", Some("1"))],
            globals: vec![
                code("EBedSurChgTrnCode", Some("EB")),
                code("EBedSurChgTrnSub", None),
            ],
            specials: vec![],
        };
        let set = resolve(&catalog, &[11001, 11001, 0]);
        assert_eq!(set.describe(), "110/1,110/2,EB/0");
        assert_eq!(set.dropped, vec!["PackageCode:PK/1".to_owned()]);
        assert_eq!(set.room_codes, vec!["110".to_owned()]);
        assert!(!set.is_ambiguous());
        let over = from_override(&["RM".into()]).unwrap();
        assert!(over.from_override && !over.is_ambiguous());
    }

    #[test]
    fn filter_parameterizes_every_code() {
        let pairs = with_member_discount(
            vec![EarnPair::of("110", Some("1")), EarnPair::any_sub("RM")],
            "MD",
        );
        let (sql, params) = filter(&pairs).unwrap();
        assert_eq!(
            sql,
            "(t.TransactionCode IN (@EarnAny0,@EarnAny1) OR (t.TransactionCode = @EarnC0 AND \
             (SELECT K FROM dbo.SA_TrnKey(t.TransactionSubCode)) = @EarnS0))"
        );
        assert_eq!(params.len(), 4);
        assert!(filter(&[]).is_err());
        assert_eq!(with_member_discount(pairs.clone(), "md").len(), pairs.len());
    }
}
