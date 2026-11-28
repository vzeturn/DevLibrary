//! Golden vectors of `CashierMaster.cs` (ADR 0150), computed by the .NET contract with the
//! PIN key `0..=31`: the Rust agent must hash and open byte for byte the same.

use crate::contracts::StrMap;
use crate::contracts::cashier::{
    CashierWrite, canonical, is_writable_key, open_pin, row_hash, seal_pin,
};
use crate::contracts::staff;
use crate::contracts::wire::StaffClerk;

fn key() -> Vec<u8> {
    (0u8..32).collect()
}

const LOOKUP_1234: &str = "daKwZh9ZPnya83K6q9F1HEqmErii/WBaH8SWORUZPdE=";

#[allow(clippy::too_many_arguments)] // mirrors the StaffClerk fields of the golden vectors
fn clerk(
    key: &str,
    name: &str,
    active: bool,
    lookup: Option<&str>,
    status: &str,
    security: Option<i32>,
    rvcs: &[i32],
    functions: &[&str],
) -> StaffClerk {
    StaffClerk {
        key: key.to_owned(),
        name: name.to_owned(),
        active,
        pin_lookup: lookup.map(str::to_owned),
        pin_status: status.to_owned(),
        security,
        rvcs: rvcs.to_vec(),
        functions: functions.iter().map(|f| (*f).to_owned()).collect(),
    }
}

#[test]
fn the_pin_key_vectors_match_dotnet() {
    let k = key();
    assert_eq!(
        dl_encoding::base64::STANDARD.encode(staff::lookup(&k, "1234").unwrap()),
        LOOKUP_1234
    );
    assert_eq!(staff::key_id(&k), "630dcd2966c43366");
}

#[test]
fn the_row_hash_matches_dotnet() {
    let c01 = clerk(
        "C01",
        "Lan Phạm",
        true,
        Some(LOOKUP_1234),
        "set",
        Some(1),
        &[2, 1],
        &["PT_Pickup", "PT_NewCheck"],
    );
    assert_eq!(
        row_hash(&c01),
        "2917d0dc87088b97f12193b3317e3f87ddd2c477ea6c22749b94956c2ec118f7"
    );
    let mgr = clerk("MGR", "Quản lý", false, None, "none", None, &[], &[]);
    assert_eq!(
        row_hash(&mgr),
        "83a22f0e2f5c057ca58f4dad55e66be1c8974c1fa288dcc5b6db0507d9e8a935"
    );
}

#[test]
fn the_seal_opens_only_for_its_clerk() {
    let sealed = "LPE7wZ0e38C2UmXjhINlZ1i8H0O/ytMwbyfOfyKiNUM=";
    let pin = open_pin(&key(), "C01", Some(sealed)).expect("opens");
    assert_eq!(pin.as_str(), "5678");
    assert_eq!(format!("{pin:?}"), "ClearPin(***)");
    assert!(open_pin(&key(), "C02", Some(sealed)).is_none());
    assert!(open_pin(&[9; 32], "C01", Some(sealed)).is_none());
    assert!(open_pin(&key(), "C01", None).is_none());
    assert!(open_pin(&key(), "C01", Some("")).is_none());
    assert!(open_pin(&key(), "C01", Some("not base64!")).is_none());
    assert!(open_pin(&key()[..16], "C01", Some(sealed)).is_none());
    let mine = seal_pin(&key(), "N1", "2468", [3; 12]).unwrap();
    assert_eq!(
        open_pin(&key(), "N1", Some(&mine)).unwrap().as_str(),
        "2468"
    );
}

fn fields(pairs: &[(&str, Option<&str>)]) -> StrMap {
    pairs
        .iter()
        .map(|(k, v)| (*k, v.map(str::to_owned)))
        .collect()
}

fn listing(m: &StrMap) -> String {
    let mut parts: Vec<String> = m
        .iter()
        .map(|(k, v)| format!("{k}={}", v.as_deref().unwrap_or("")))
        .collect();
    parts.sort();
    parts.join("|")
}

#[test]
fn the_write_fields_match_dotnet() {
    let w = CashierWrite {
        name: "Lan Phạm".to_owned(),
        active: true,
        security: 1,
        rvcs: vec![2, 1, 2],
        functions: vec!["PT_Payment".to_owned(), "cashier".to_owned()],
        pin_sealed: None,
        pin_key_id: None,
    };
    let f = w.to_fields();
    assert_eq!(
        listing(&f),
        "active=1|functions=Cashier,PT_Payment|name=Lan Phạm|rvcs=1,2|security=1"
    );
    let back = CashierWrite::parse(&f).unwrap();
    assert_eq!(back.rvcs, vec![1, 2]);
    assert_eq!(back.functions, vec!["Cashier", "PT_Payment"]);
}

#[test]
fn parse_reports_problems_without_values() {
    let ok = [
        ("name", Some(" Lan ")),
        ("active", Some("1")),
        ("security", Some("5")),
        ("rvcs", Some("3,1,,3")),
        ("functions", Some("pt_payment,Cashier")),
    ];
    let w = CashierWrite::parse(&fields(&ok)).unwrap();
    assert_eq!(w.name, "Lan");
    assert_eq!(w.rvcs, vec![1, 3]);
    assert_eq!(w.functions, vec!["Cashier", "PT_Payment"]);
    assert_eq!(format!("{w:?}"), "CashierWrite { pin: false, .. }");

    let problem = |change: (&str, Option<&str>)| {
        let mut f = fields(&ok);
        f.insert(change.0, change.1.map(str::to_owned));
        CashierWrite::parse(&f).unwrap_err()
    };
    assert_eq!(problem(("secret", Some("x"))), "field not allowed: secret");
    assert_eq!(
        problem(("name", Some("  "))),
        "name must be 1 to 50 characters"
    );
    assert_eq!(
        problem(("name", Some("a\tb"))),
        "name must be 1 to 50 characters"
    );
    assert_eq!(
        problem(("name", Some(&"x".repeat(51)))),
        "name must be 1 to 50 characters"
    );
    assert_eq!(problem(("active", Some("yes"))), "active must be 1 or 0");
    assert_eq!(
        problem(("security", Some("100"))),
        "security must be an integer 0 to 99"
    );
    assert_eq!(
        problem(("security", Some("-1"))),
        "security must be an integer 0 to 99"
    );
    assert_eq!(
        problem(("security", None)),
        "security must be an integer 0 to 99"
    );
    assert_eq!(
        problem(("rvcs", Some("1,0"))),
        "rvcs must be positive integers"
    );
    assert_eq!(
        problem(("rvcs", Some("1, 2"))),
        "rvcs must be positive integers"
    );
    let many: Vec<String> = (1..=201).map(|i| i.to_string()).collect();
    assert_eq!(problem(("rvcs", Some(&many.join(",")))), "too many rvcs");
    assert_eq!(
        problem(("functions", Some("PT_Payment,Other"))),
        "functions must be mapped function ids"
    );
    assert_eq!(
        problem(("pin_sealed", Some("abc"))),
        "pin_sealed and pin_key_id go together"
    );
    let mut empty = fields(&ok);
    empty.insert("rvcs", None);
    empty.insert("functions", Some(String::new()));
    let w = CashierWrite::parse(&empty).unwrap();
    assert!(w.rvcs.is_empty() && w.functions.is_empty());
}

#[test]
fn keys_and_functions_follow_dotnet() {
    assert!(is_writable_key("C01"));
    assert!(is_writable_key("A B"));
    assert!(!is_writable_key(""));
    assert!(!is_writable_key(" C01"));
    assert!(!is_writable_key("ABCDEFGHIJK"));
    assert!(!is_writable_key("sml"));
    assert!(!is_writable_key("Lâm"));
    assert_eq!(
        canonical([" pt_pickup", "Other", "PT_PICKUP", "cashier"]),
        vec!["Cashier", "PT_Pickup"]
    );
}
