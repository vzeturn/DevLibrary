//! The `dbo.Para` names the cloud never writes (ADR 0152): a verbatim copy of
//! `src/MM.PdaOrder.Sync.Contracts/Wire/OutletParams.cs` (sequence counters and the POS bump
//! `Last*`, the business date `Cur*`, bill numbering, the interface/member/AR database wiring,
//! training mode and the switches the order batches of ADR 0100 read). A test pins this copy
//! against the .NET file; change both together.

/// Name prefixes that are never written (`OutletParams.Prefixes`), compared ignoring ASCII case.
pub const PREFIXES: &[&str] = &["Last", "Cur", "BillNo"];

/// Names that are never written (`OutletParams.Names`), compared ignoring ASCII case.
pub const NAMES: &[&str] = &[
    "InterfaceDB",
    "MBRDatabase",
    "ARDatabase",
    "CityLedgerFolio",
    "PostToSmileEnabled",
    "ARNotPostToDB",
    "TrainingMode",
    "gTrainingMode",
    "MemberControl",
    "OrderPrintByMenu",
    "GroupChkTrn",
    "OrderByCourse",
    "qrCodeNeedConfirmOrder",
];

/// `OutletParams.IsProtected`: the name is listed or starts with a listed prefix.
pub fn is_protected(para_name: &str) -> bool {
    NAMES.iter().any(|n| n.eq_ignore_ascii_case(para_name))
        || PREFIXES.iter().any(|p| {
            para_name
                .get(..p.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(p))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOTNET: &str = include_str!("../../../testdata/cs/Wire/OutletParams.cs");

    /// The quoted strings between `open` and the next `;` of the .NET source.
    fn quoted_after(open: &str) -> Vec<String> {
        let start = DOTNET.find(open).expect(open) + open.len();
        let body = &DOTNET[start..];
        let body = &body[..body.find(';').unwrap()];
        body.split('"')
            .skip(1)
            .step_by(2)
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn the_list_is_the_dotnet_list() {
        assert_eq!(quoted_after("Prefixes ="), PREFIXES);
        assert_eq!(quoted_after("Names = new"), NAMES);
    }

    #[test]
    fn names_and_prefixes_match_ignoring_case() {
        for name in [
            "LastCheck",
            "lasttrn",
            "CurDate",
            "BillNoLength",
            "InterfaceDB",
            "interfacedb",
            "TrainingMode",
        ] {
            assert!(is_protected(name), "{name}");
        }
        for name in ["TipPercent", "Las", "Interface", "ạLast"] {
            assert!(!is_protected(name), "{name}");
        }
    }
}
