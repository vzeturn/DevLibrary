//! `txn.counters` wire contract (ADR 0161, RT-0848): names, `Problem` messages in the .NET
//! order, the snake_case shape and `HighWater`.

use crate::contracts::json::WireJson;
use crate::contracts::wire::*;

const PAYLOAD: &str = r#"{"rvc_no":4,"counters":[{"name":"LastCheck","floor":500},{"name":"CloseNumber","floor":0}],"actor":"cloud"}"#;
const RESULT: &str = r#"{"rvc_no":4,"counters":[{"name":"LastCheck","before":120,"stored":500,"in_use":118},{"name":"CloseNumber","before":41,"stored":41}]}"#;

fn floor(name: &str, floor: i64) -> TxnCounterFloor {
    TxnCounterFloor {
        name: name.into(),
        floor,
    }
}

fn payload() -> TxnCountersPayload {
    TxnCountersPayload::from_json(PAYLOAD).unwrap()
}

#[test]
fn names_match_the_dotnet_constants() {
    assert_eq!(txn_command_types::COUNTERS, "txn.counters");
    assert!(txn_command_types::is_txn(Some(txn_command_types::COUNTERS)));
    assert!(!txn_command_types::is_covering(Some(
        txn_command_types::COUNTERS
    )));
    assert_eq!(
        site_command_contract::find(Some(txn_command_types::COUNTERS)).map(|s| s.delivery),
        Some(SiteCommandDelivery::Durable)
    );
    assert_eq!(
        txn_counter_names::ALL,
        ["LastCheck", "LastTrn", "LastPayment", "CloseNumber"]
    );
    assert_eq!(txn_counter_names::MAX_COUNTER, 99_999_999);
    assert_eq!(txn_counter_names::aggregate_key(4), "txn.counters:4");
    assert!(txn_counter_names::is_known(Some("LastPayment")));
    assert!(!txn_counter_names::is_known(Some("lastcheck")), "ordinal");
    assert!(!txn_counter_names::is_known(None));
}

#[test]
fn payload_and_result_round_trip_in_the_dotnet_shape() {
    let p = payload();
    assert_eq!(
        p.counters,
        vec![floor("LastCheck", 500), floor("CloseNumber", 0)]
    );
    assert_eq!(p.problem(), None);
    assert_eq!(p.to_json(), PAYLOAD);

    let r = TxnCountersResult::from_json(RESULT).unwrap();
    assert_eq!(r.counters[0].in_use, Some(118));
    assert_eq!(r.counters[1].in_use, None);
    assert_eq!(r.to_json(), RESULT, "a null in_use is omitted");
    assert_eq!(r.high_water("LastCheck"), Some(500));
    assert_eq!(r.high_water("CloseNumber"), Some(41));
    assert_eq!(r.high_water("LastTrn"), None);
    let in_use_above = TxnCountersResult {
        rvc_no: 4,
        counters: vec![TxnCounterState {
            name: "LastTrn".into(),
            before: 10,
            stored: 10,
            in_use: Some(12),
        }],
    };
    assert_eq!(in_use_above.high_water("LastTrn"), Some(12));
}

type Change = Box<dyn Fn(&mut TxnCountersPayload)>;

#[test]
fn problems_use_the_dotnet_messages_in_the_dotnet_order() {
    let cases: Vec<(Change, &str)> = vec![
        (Box::new(|p| p.rvc_no = 0), "RvcNo must be 1..255."),
        (Box::new(|p| p.rvc_no = 256), "RvcNo must be 1..255."),
        (Box::new(|p| p.actor = " ".into()), "Actor is required."),
        (
            Box::new(|p| p.counters.clear()),
            "Counters must list at least one counter.",
        ),
        (
            Box::new(|p| p.counters[0].name = "Nope".into()),
            "Counter is not allowed.",
        ),
        (
            Box::new(|p| p.counters[1].name = "LastCheck".into()),
            "Counter LastCheck is listed twice.",
        ),
        (
            Box::new(|p| p.counters[0].floor = -1),
            "Floor of LastCheck must be 0..99999999.",
        ),
        (
            Box::new(|p| p.counters[1].floor = 100_000_000),
            "Floor of CloseNumber must be 0..99999999.",
        ),
    ];
    for (change, message) in cases {
        let mut p = payload();
        change(&mut p);
        assert_eq!(p.problem().as_deref(), Some(message));
    }
    let null_entry = PAYLOAD.replace(r#"{"name":"LastCheck","floor":500}"#, "null");
    assert_eq!(
        TxnCountersPayload::from_json(&null_entry)
            .unwrap()
            .problem()
            .as_deref(),
        Some("Counter is not allowed.")
    );
    let mut max = payload();
    max.counters[0].floor = txn_counter_names::MAX_COUNTER;
    assert_eq!(max.problem(), None);
}
