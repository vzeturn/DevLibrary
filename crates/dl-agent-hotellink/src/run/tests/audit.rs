//! Port of Go `audit_test.go` (RT-0702): the agent answers hub audit requests.

use super::*;
use crate::jobmodel::{AuditFolio, AuditRate, AuditRequest};
use crate::pms::AuditResult;
use crate::reconcile::tests::day;

fn folio(num: f64, arrival: &str) -> AuditFolio {
    AuditFolio {
        folio_num: num,
        arrival_date: arrival.into(),
        rates: vec![AuditRate {
            date: arrival.into(),
            amount: 913557.0,
        }],
        ..AuditFolio::default()
    }
}

#[test]
fn window_and_defaults_are_clamped() {
    let repo = FakeRepo::new();
    {
        let mut st = repo.st();
        st.hotel_date = Some(day(2026, 11, 28));
        st.audit_result = Some(AuditResult::default());
    }
    let r = runner(&crate::testhub::dead_url(), &repo);
    let req = AuditRequest {
        id: "a1".into(),
        window_days: 500,
        ..AuditRequest::default()
    };
    let snap = r.build_pms_audit(&req);
    assert_eq!(snap.error, "");
    assert_eq!(snap.window_days, 60);
    assert_eq!(snap.hotel_date, "2026-11-28");
    assert_eq!(snap.from, "2026-09-29");
    let q = repo.st().audit_queries[0].clone();
    assert_eq!(q.max_rows, 2000);
    assert!(q.keys.is_empty());
    let snap = r.build_pms_audit(&AuditRequest::default());
    assert_eq!(snap.window_days, 7);
}

#[test]
fn targeted_audit_returns_rates_and_normalises_keys() {
    let repo = FakeRepo::new();
    {
        let mut st = repo.st();
        st.hotel_date = Some(day(2026, 11, 28));
        st.audit_result = Some(AuditResult {
            folios: vec![folio(502523.0, "2026-11-27"), folio(502524.0, "2026-11-27")],
            truncated: true,
            log_err: Some("audit FolioLog: boom".into()),
        });
    }
    let r = runner(&crate::testhub::dead_url(), &repo);
    let req = AuditRequest {
        id: "a2".into(),
        keys: vec![" K1 ".into(), "K1".into(), "".into(), "K2".into()],
        log_rows: 20,
        ..AuditRequest::default()
    };
    let snap = r.build_pms_audit(&req);
    assert_eq!(snap.keys, vec!["K1".to_owned(), "K2".to_owned()]);
    assert_eq!(snap.folios.len(), 2);
    assert!(snap.truncated);
    assert_eq!(snap.last_arrival, "2026-11-27");
    assert_eq!(snap.log_error, "audit FolioLog: boom");
    assert_eq!(repo.st().audit_queries[0].keys, snap.keys);
    let json = snap.to_json();
    assert!(
        json.contains(r#""rates":[{"date":"2026-11-27","amount":913557}]"#),
        "{json}"
    );
    assert!(json.contains(r#""keys":["K1","K2"]"#), "{json}");
}

#[test]
fn failures_end_up_in_the_snapshot() {
    let repo = FakeRepo::new();
    repo.st().hotel_date = Some(day(2026, 11, 28));
    let r = runner(&crate::testhub::dead_url(), &repo);
    let snap = r.build_pms_audit(&AuditRequest::default());
    assert!(
        snap.error.contains("cannot read the PMS audit"),
        "{}",
        snap.error
    );
    assert!(snap.folios.is_empty());

    repo.st().hotel_date = None;
    let snap = r.build_pms_audit(&AuditRequest::default());
    assert!(
        snap.warning.contains("hotel date unavailable"),
        "{}",
        snap.warning
    );
    for _ in 0..5 {
        r.breaker.on_failure();
    }
    let snap = r.build_pms_audit(&AuditRequest::default());
    assert!(snap.error.contains("circuit breaker"), "{}", snap.error);
}

#[test]
fn poll_with_audits_posts_the_snapshot() {
    let hub = Hub::new();
    hub.st().audits = vec![r#"{"id":"a9","windowDays":3,"keys":["BK1"]}"#.into()];
    let repo = FakeRepo::new();
    {
        let mut st = repo.st();
        st.hotel_date = Some(day(2026, 11, 28));
        st.audit_result = Some(AuditResult {
            folios: vec![folio(1.0, "2026-11-27")],
            ..AuditResult::default()
        });
    }
    let run = start(runner(&hub.fake.url, &repo));
    wait_until(
        || !hub.fake.hits("/api/agent/pms-audit").is_empty(),
        "snapshot posted",
    );
    let body = hub.fake.hits("/api/agent/pms-audit")[0].body.clone();
    assert!(body.contains(r#""requestId":"a9""#), "{body}");
    assert!(body.contains(r#""folioNum":1"#), "{body}");
    drop(run);
}
