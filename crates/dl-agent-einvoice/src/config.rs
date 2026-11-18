//! Agent configuration. `Rules` is the part of `AgentConfig` the collection pipeline
//! reads; the full sealed config lives in `agent_config`.

/// The tenant rules that change how money is collected.
#[derive(Debug, Clone, PartialEq)]
pub struct Rules {
    /// Recompute VAT to the legal rate and move the delta out of service charge.
    pub recalc_tax_into_sc: bool,
    /// FO TransactionCodes whose charges are không chịu thuế (KCT).
    pub fo_exempt_codes: Vec<String>,
    /// The property's nominal service-charge rate (0.05 = 5%); 0 disables the FO
    /// net/gross check.
    pub standard_service_charge_rate: f64,
}

impl Default for Rules {
    fn default() -> Rules {
        Rules {
            recalc_tax_into_sc: true,
            fo_exempt_codes: Vec::new(),
            standard_service_charge_rate: 0.05,
        }
    }
}

impl Rules {
    /// `IsExemptCode`.
    pub fn is_exempt_code(&self, code: &str) -> bool {
        self.fo_exempt_codes.iter().any(|c| c == code)
    }
}
