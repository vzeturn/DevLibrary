//! Port of RoundTripTests.cs.

mod support;

use std::sync::Arc;

use dl_agent_pms_device::adapters::SimulatedLockAdapter;
use dl_agent_pms_device::agent::log::NullLogger;
use dl_agent_pms_device::agent::{CommandProcessor, StopToken, SystemClock};
use dl_agent_pms_device::config::AgentOptions;
use dl_agent_pms_device::contracts::kinds;
use support::{FakeQueue, TempOutbox};

#[test]
fn i0_1_enqueued_lock_issue_is_claimed_run_by_the_simulated_adapter_and_its_result_read_back() {
    let queue = FakeQueue::new(Arc::new(SystemClock));
    let id = queue.enqueue(
        kinds::LOCK_ISSUE,
        r#"{"room":"101","from":"2026-10-14T14:00:00","to":"2026-10-16T12:00:00","cards":2}"#,
    );
    let outbox = TempOutbox::new();
    let reporter = outbox.reporter(queue.clone(), Arc::new(SystemClock), Arc::new(NullLogger));
    let processor = CommandProcessor::new(
        queue.clone(),
        Arc::new(SimulatedLockAdapter::new()),
        reporter,
        &AgentOptions::default(),
        Arc::new(NullLogger),
        None,
    );
    let stop = StopToken::new();

    assert!(processor.run_once(&stop).unwrap()); // claim -> run -> result
    assert!(!processor.run_once(&stop).unwrap()); // queue drained
    assert_eq!(outbox.count(), 0); // nothing left owed to the API

    let row = queue.read(id);
    assert_eq!(row.status, "done");
    let result = row.result.unwrap();
    let cards = result["cards"].as_array().unwrap();
    assert_eq!(cards.len(), 2);
    assert_eq!(cards[0]["room"], "101");
    assert!(
        cards[0]["cardNumber"]
            .as_str()
            .unwrap()
            .starts_with("SIM-101-")
    );
}
