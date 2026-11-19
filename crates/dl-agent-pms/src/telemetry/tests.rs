use std::collections::BTreeMap;

use super::*;
use crate::wire::OpError;

struct Rig {
    tel: Telemetry,
    rep: Arc<MemoryReporter>,
    now: Arc<Mutex<Instant>>,
}

fn rig() -> Rig {
    let rep = Arc::new(MemoryReporter::default());
    let now = Arc::new(Mutex::new(Instant::now()));
    let n = now.clone();
    let tel = Telemetry::with_clock(
        Some(rep.clone() as Arc<dyn Reporter>),
        Box::new(move || *lock(&n)),
    );
    Rig { tel, rep, now }
}

impl Rig {
    fn advance(&self, d: Duration) {
        *lock(&self.now) += d;
    }
}

fn op(name: &str) -> Op {
    Op {
        id: "6f1c2b7e-8d4a-4c3b-9e2f-1a2b3c4d5e6f".into(),
        op: name.into(),
        contract_ver: 1,
        target_db: "SMILE_FO".into(),
        args: Value::Object(Map::new()),
        request_key: None,
        expected_stamp: None,
        deadline_ms: 1000,
        replay: None,
    }
}

fn result(status: &str, code: &str, number: Option<i64>) -> OpResult {
    OpResult {
        id: "x".into(),
        status: status.into(),
        sets: Vec::new(),
        rowcounts: Vec::new(),
        outputs: BTreeMap::new(),
        error: (!code.is_empty()).then(|| OpError {
            code: code.into(),
            sql_number: number,
            message: "Violation of PRIMARY KEY constraint; duplicate key value is (42)".into(),
        }),
        replayed: None,
    }
}

#[test]
fn counts_ops_and_reports_without_messages() {
    let r = rig();
    r.tel.op_started();
    assert_eq!(r.tel.snapshot().inflight, 1);
    r.tel
        .op_done(&op("pms.room.get"), &result("applied", "", None));
    r.tel.op_ended();
    r.tel
        .op_done(&op("pms.x.y"), &result("rejected", "OP_REJECTED", None));
    r.tel
        .op_done(&op("pms.x.y"), &result("failed", "SQL_ERROR", Some(2627)));
    r.tel
        .op_done(&op("pms.x.y"), &result("failed", "OP_TIMEOUT", None));
    let s = r.tel.snapshot();
    assert_eq!((s.inflight, s.ok, s.rejected, s.failed), (0, 1, 1, 2));
    assert!(s.last_op_unix > 0);
    assert_eq!(r.rep.codes(), vec![CODE_OP_REJECTED, CODE_SQL_FAILED]);
    for c in r.rep.contexts() {
        assert!(!c.contains("duplicate key"), "{c}");
        assert!(!c.contains("42)"), "{c}");
    }
    assert!(r.rep.contexts()[1].contains("\"sql_number\":2627"));
}

#[test]
fn rate_limits_per_code_and_counts_what_it_held_back() {
    let r = rig();
    for _ in 0..3 {
        r.tel
            .op_done(&op("a.b.c"), &result("rejected", "OP_REJECTED", None));
    }
    assert_eq!(r.rep.codes().len(), 1);
    r.advance(REPORT_EVERY + Duration::from_secs(1));
    r.tel
        .op_done(&op("a.b.c"), &result("rejected", "OP_REJECTED", None));
    assert_eq!(r.rep.codes().len(), 2);
    assert!(r.rep.contexts()[1].contains("\"suppressed\":2"));
}

#[test]
fn health_goes_down_after_the_threshold_and_reports_once() {
    let r = rig();
    let t = Duration::from_secs(30);
    assert_eq!(r.tel.health(t), Health::Pending);
    r.tel.disconnected(DownKind::Stream, "dial stream: refused");
    assert_eq!(r.tel.health(t), Health::Pending);
    r.advance(Duration::from_secs(31));
    r.tel.disconnected(DownKind::Stream, "dial stream: timeout");
    assert_eq!(
        r.tel.health(t),
        Health::Down {
            for_secs: 31,
            reason: "dial stream: timeout".into()
        }
    );
    r.tel.health(t);
    assert_eq!(r.rep.codes(), vec![CODE_STREAM_FAILED]);
    r.tel.connected();
    assert_eq!(r.tel.health(t), Health::Up);
    r.tel.stopped();
    assert_eq!(r.tel.health(t), Health::Pending);
    r.tel.connected();
    assert_eq!(r.tel.snapshot().reconnects, 1);
    r.tel
        .disconnected(DownKind::Auth, "stream refused the token");
    r.advance(REPORT_EVERY);
    assert!(matches!(r.tel.health(t), Health::Down { .. }));
    assert_eq!(r.rep.codes(), vec![CODE_STREAM_FAILED, CODE_AUTH_FAILED]);
}
