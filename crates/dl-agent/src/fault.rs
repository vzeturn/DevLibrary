//! Builds that fail on purpose, for the rollback smoke test (PROTOCOL.md §6.6). Only
//! `scripts/build-dl-agent.sh --fault probation` sets `DL_AGENT_FAULT`; release builds have
//! no fault.

/// The fault compiled into this binary (`DL_AGENT_FAULT` at build time), if any.
pub const COMPILED: Option<&str> = option_env!("DL_AGENT_FAULT");

/// The only fault the agent acts on: the build never passes probation, so it is rolled back
/// when probation ends and the failure is reported like any bad release.
pub const PROBATION: &str = "probation";

/// Why a build with `fault` fails probation, or `None` for a normal build.
pub fn probation_failure(fault: Option<&str>) -> Option<&'static str> {
    (fault == Some(PROBATION))
        .then_some("fault build (DL_AGENT_FAULT=probation) never passes probation")
}

/// The version text `--version` prints: a fault build says so, so nobody ships it by mistake.
pub fn version_label(version: &str, fault: Option<&str>) -> String {
    match fault {
        Some(f) if !f.is_empty() => format!("{version} (fault build: {f})"),
        _ => version.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fault_build_names_its_fault_in_the_version() {
        assert_eq!(version_label("1.3.9", None), "1.3.9");
        assert_eq!(version_label("1.3.9", Some("")), "1.3.9");
        assert_eq!(
            version_label("1.3.9", Some("probation")),
            "1.3.9 (fault build: probation)"
        );
    }

    #[test]
    fn only_the_probation_fault_fails_probation() {
        assert!(probation_failure(Some("probation")).is_some());
        for other in [None, Some(""), Some("other")] {
            assert_eq!(probation_failure(other), None);
        }
    }
}
