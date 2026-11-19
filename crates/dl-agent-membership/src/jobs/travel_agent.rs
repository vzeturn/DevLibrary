//! `TravelAgentProfile`: the travel agent's `ClientSts` classification copied onto a new
//! folio, with the width checks of `Sanitize`.

/// `UserDefineCount`.
pub const USER_DEFINE_COUNT: usize = 8;
/// `ContactNameMaxLength`.
pub const CONTACT_NAME_MAX: usize = 30;

/// `TravelAgentProfile`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TravelAgent {
    /// `Code` (`ClientSts.ClientCode`).
    pub code: i64,
    /// `ClientName` (trimmed at the end).
    pub client_name: Option<String>,
    /// `IsDeleted`.
    pub is_deleted: bool,
    /// `Origin`.
    pub origin: Option<String>,
    /// `MarketSegment`.
    pub market_segment: Option<String>,
    /// `MarketBusiness`.
    pub market_business: Option<String>,
    /// `MarketChannel`.
    pub market_channel: Option<String>,
    /// `Geographic1`.
    pub geographic1: Option<String>,
    /// `Geographic2`.
    pub geographic2: Option<String>,
    /// `UserDefines` (8).
    pub user_defines: Vec<Option<String>>,
}

impl TravelAgent {
    /// `Unresolved`: the configured code with every classification empty.
    pub fn unresolved(code: i64) -> TravelAgent {
        TravelAgent {
            code,
            client_name: None,
            is_deleted: false,
            origin: None,
            market_segment: None,
            market_business: None,
            market_channel: None,
            geographic1: None,
            geographic2: None,
            user_defines: vec![None; USER_DEFINE_COUNT],
        }
    }

    /// `ContactName`: the client name cut to 30 characters.
    pub fn contact_name(&self) -> Option<String> {
        self.client_name
            .as_ref()
            .map(|n| n.chars().take(CONTACT_NAME_MAX).collect())
    }

    /// `EffectiveOriginCode`.
    pub fn effective_origin<'a>(&'a self, fallback: &'a str) -> &'a str {
        match self.origin.as_deref() {
            Some(o) if !o.is_empty() => o,
            _ => fallback,
        }
    }

    /// `Sanitize`: every value wider than its folio column is emptied, with a warning.
    pub fn sanitize(&self) -> (TravelAgent, Vec<String>) {
        let mut out = self.clone();
        let mut warnings = Vec::new();
        let code = self.code;
        let mut check = |source: &str, target: &str, value: &mut Option<String>, width: usize| {
            if let Some(v) = value.as_deref() {
                let len = v.chars().count();
                if len > width {
                    warnings.push(format!(
                        "ClientSts.{source} của mã TA {code} là '{v}' ({len} ký tự) nhưng Folio.{target} chỉ chứa {width} ký tự — để trống cột này, sửa mã phân loại của TA này trong SMILE."
                    ));
                    *value = None;
                }
            }
        };
        check("Origin", "OriginCode", &mut out.origin, 10);
        check("MSegment", "MarketSegmentCode", &mut out.market_segment, 20);
        check(
            "MBusiness",
            "MarketBusinessCode",
            &mut out.market_business,
            3,
        );
        check("MChanel", "MarketChannelCode", &mut out.market_channel, 3);
        check("GEO1", "Geographic1Code", &mut out.geographic1, 3);
        check("GEO2", "Geographic2Code", &mut out.geographic2, 3);
        for (i, u) in out.user_defines.iter_mut().enumerate() {
            check(
                &format!("UDef{}", i + 1),
                &format!("UserDefineCode{}", i + 1),
                u,
                3,
            );
        }
        if warnings.is_empty() {
            (self.clone(), warnings)
        } else {
            (out, warnings)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_empties_only_the_too_wide_columns() {
        let mut ta = TravelAgent::unresolved(7);
        ta.origin = Some("WAY-TOO-LONG-ORIGIN".into());
        ta.market_business = Some("ABC".into());
        ta.user_defines[2] = Some("ABCD".into());
        let (clean, warnings) = ta.sanitize();
        assert_eq!(warnings.len(), 2);
        assert!(warnings[1].contains("UDef3") && warnings[1].contains("UserDefineCode3"));
        assert_eq!(clean.origin, None);
        assert_eq!(clean.market_business.as_deref(), Some("ABC"));
        assert_eq!(clean.user_defines[2], None);
        assert_eq!(clean.effective_origin("WEB"), "WEB");
    }

    #[test]
    fn contact_name_is_cut_to_thirty_characters() {
        let mut ta = TravelAgent::unresolved(1);
        ta.client_name = Some("x".repeat(40));
        assert_eq!(ta.contact_name().map(|n| n.len()), Some(30));
    }
}
