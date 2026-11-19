//! `LegacyBuildsTests.cs`.

use crate::contracts::legacy::LegacyBuildStanding::{self, *};
use crate::contracts::legacy::builds;

#[test]
fn standing_reads_the_numeric_part_of_the_product_version() {
    let cases: [(Option<&str>, LegacyBuildStanding); 15] = [
        (Some("3.30.32"), Reference),
        (Some("3.30.32+4f1c2ab"), Reference),
        (Some("3.31.0.0"), Reference),
        (Some("3.30.31"), Older),
        (Some("3.11.26"), Older),
        (Some("3.11.26.0"), Older),
        (Some(" 3.20.1-rc1 "), Older),
        (Some("3.11.25"), Unverified),
        (Some("1.2.0"), Unverified),
        (Some("3.9"), Unverified),
        (None, Unknown),
        (Some(""), Unknown),
        (Some("dev"), Unknown),
        (Some("3"), Unknown),
        (Some("1.2.3.4.5"), Unknown),
    ];
    for (v, expected) in cases {
        assert_eq!(builds::standing(v), expected, "{v:?}");
    }
}

#[test]
fn the_reference_and_the_oldest_checked_build_are_what_the_constants_say() {
    assert_eq!(builds::standing(Some(builds::REFERENCE)), Reference);
    assert_eq!(builds::standing(Some(builds::OLDEST_VERIFIED)), Older);
}

#[test]
fn describe_tells_the_operator_what_to_expect_and_never_refuses() {
    assert!(builds::describe(Some("3.30.32")).contains("3.30.32"));
    let older = builds::describe(Some("3.14.2"));
    assert!(older.contains("supported") && older.contains("cloud defaults"));
    let unverified = builds::describe(Some("1.2.0"));
    assert!(unverified.contains("--dry-run") && unverified.contains(builds::OLDEST_VERIFIED));
    assert!(builds::describe(None).contains("not recorded"));
    assert!(
        builds::describe_store(None, Some("sqlserver:dbo"))
            .starts_with("version not recorded, a build")
    );
}
