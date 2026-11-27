//! RT-1318: capture batches carry `t_detected` (the start of the capture pass) and
//! `t_read` (the rows reached the batch writer); `t_sent` is added by the uploader.

use super::test_support::*;
use crate::contracts::mapping::RawValue;
use crate::util::hop_clock::{self, PassStamp};

fn site() -> Harness {
    let h = Harness::new();
    h.source.table("Invoice").table("ReOpenLog");
    h.source.add(
        "Invoice",
        vec![row(&[
            ("CheckNo", RawValue::I32(1)),
            ("CloseTime", local(0, 10)),
            ("SubTotal", dec("100")),
        ])],
    );
    h
}

#[test]
fn a_batch_written_in_a_pass_carries_the_pass_start_and_the_read_time() {
    let h = site();
    let pass = PassStamp::begin();
    let started = hop_clock::pass_started().expect("in a pass");
    h.engine().run_initial_snapshot(&bg()).unwrap();
    drop(pass);
    let batches = h.sink.batches();
    assert!(!batches.is_empty());
    for b in &batches {
        assert_eq!(b.hops.t_detected, Some(started));
        assert!(
            b.hops.t_read.is_some_and(|read| read >= started),
            "{:?}",
            b.hops
        );
        assert_eq!(b.hops.t_sent, None, "added at upload time");
    }
}

#[test]
fn a_batch_written_outside_a_pass_has_no_detection_stamp() {
    let h = site();
    h.engine().run_initial_snapshot(&bg()).unwrap();
    let batches = h.sink.batches();
    assert!(!batches.is_empty());
    assert!(
        batches
            .iter()
            .all(|b| b.hops.t_detected.is_none() && b.hops.t_read.is_some())
    );
}
