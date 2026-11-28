//! `pos.order_residue` wire contract (RT-1365): the snake_case shape of the cloud's
//! `PosOrderResidue*` records, the `Problem` messages (same text) and the delivery rule.

use crate::contracts::json::WireJson;
use crate::contracts::wire::*;

const PAYLOAD: &str = r#"{"min_age_minutes":90,"reason":"acceptance"}"#;
const RESULT: &str = concat!(
    r#"{"min_age_minutes":90,"empty_open_checks":[{"rvc_no":4,"check_no":400000101,"opened_local":"2026-11-26T08:00:00"}],"#,
    r#""orphan_info_rows":[{"check_no":400000101,"trn_no":1,"sent":false}],"stale_open_logs":[],"#,
    r#""orphan_send_audits":[400000101],"other_rows_ignored":2,"sent_orphan_info_rows":1,"truncated":false}"#
);

fn payload(min: i32, reason: Option<&str>) -> PosOrderResiduePayload {
    PosOrderResiduePayload {
        min_age_minutes: min,
        reason: reason.map(str::to_owned),
    }
}

#[test]
fn the_command_is_durable_with_the_dotnet_names() {
    assert_eq!(site_command_types::POS_ORDER_RESIDUE, "pos.order_residue");
    assert_eq!(PosOrderResiduePayload::AGGREGATE, "pos-residue");
    let spec = site_command_contract::find(Some(site_command_types::POS_ORDER_RESIDUE)).unwrap();
    assert_eq!(spec.delivery, SiteCommandDelivery::Durable);
    assert_eq!(spec.max_attempts, 3);
}

#[test]
fn payload_and_result_round_trip_in_the_dotnet_shape() {
    let p = PosOrderResiduePayload::from_json(PAYLOAD).unwrap();
    assert_eq!(p, payload(90, Some("acceptance")));
    assert_eq!(p.to_json(), PAYLOAD);
    assert_eq!(payload(120, None).to_json(), r#"{"min_age_minutes":120}"#);

    let r = PosOrderResidueResult::from_json(RESULT).unwrap();
    assert_eq!(r.empty_open_checks[0].check_no, 400000101);
    assert_eq!(r.orphan_send_audits, vec![400000101]);
    assert_eq!(r.to_json(), RESULT);
}

#[test]
fn problems_use_the_dotnet_messages() {
    assert_eq!(payload(120, None).problem(), None);
    assert_eq!(payload(10080, Some(&"r".repeat(300))).problem(), None);
    assert_eq!(
        payload(0, None).problem(),
        Some("min_age_minutes must be 1..10080")
    );
    assert_eq!(
        payload(10081, None).problem(),
        Some("min_age_minutes must be 1..10080")
    );
    let long = "r".repeat(301);
    assert!(
        payload(120, Some(&long))
            .problem()
            .unwrap()
            .contains("reason")
    );
}
