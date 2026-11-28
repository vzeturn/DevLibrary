//! `pos.revenue_summary` wire contract (ADR 0149, RT-0843; ported by RT-1427): the
//! delivery, the snake_case payload and result, the `Problem` messages and the strict
//! `yyyy-MM-dd` day reader.

use crate::contracts::json::WireJson;
use crate::contracts::wire::*;
use crate::util::stj;

#[test]
fn pos_revenue_summary_is_a_durable_command_with_the_snake_case_payload_and_result() {
    assert_eq!(
        site_command_contract::find(Some(site_command_types::POS_REVENUE_SUMMARY))
            .map(|s| (s.delivery, s.max_attempts)),
        Some((SiteCommandDelivery::Durable, 3))
    );
    assert_eq!(
        site_command_types::POS_REVENUE_SUMMARY,
        "pos.revenue_summary"
    );
    let raw =
        r#"{"rvc_no":3,"date_from":"2026-11-01","date_to":"2026-11-30","reason":"acceptance"}"#;
    let p = PosRevenueSummaryPayload::from_element(&stj::parse(raw).unwrap()).unwrap();
    assert_eq!((p.rvc_no, p.reason.as_deref()), (3, Some("acceptance")));
    assert_eq!(p.problem(), None);
    assert_eq!(p.to_json(), raw);
    let no_reason = PosRevenueSummaryPayload {
        reason: None,
        ..p.clone()
    };
    assert!(!no_reason.to_json().contains("reason"), "nulls are omitted");
    assert_eq!(PosRevenueSummaryPayload::aggregate_key(3), "pos-revenue:3");

    let with = |f: &dyn Fn(&mut PosRevenueSummaryPayload)| {
        let mut x = p.clone();
        f(&mut x);
        x.problem()
    };
    assert_eq!(with(&|x| x.rvc_no = 0), Some("rvc_no must be positive"));
    assert_eq!(
        with(&|x| x.date_from = "2026-2-01".into()),
        Some("date_from must be yyyy-MM-dd")
    );
    assert_eq!(
        with(&|x| x.date_from = "2026-02-30".into()),
        Some("date_from must be yyyy-MM-dd")
    );
    assert_eq!(
        with(&|x| x.date_to = "2026-11-30 ".into()),
        Some("date_to must be yyyy-MM-dd")
    );
    assert_eq!(
        with(&|x| {
            x.date_from = "2026-11-30".into();
            x.date_to = "2026-11-29".into();
        }),
        Some("date_from must not be after date_to")
    );
    assert_eq!(
        with(&|x| x.date_to = "2026-12-02".into()),
        Some("the range must not exceed 31 days")
    );
    assert_eq!(with(&|x| x.date_to = "2026-12-01".into()), None, "31 days");
    // Leap day: 2028 has one, 2026 and 2100 do not.
    assert!(PosRevenueSummaryPayload::try_day("2028-02-29").is_some());
    assert!(PosRevenueSummaryPayload::try_day("2026-02-29").is_none());
    assert!(PosRevenueSummaryPayload::try_day("2100-02-29").is_none());
    assert_eq!(
        PosRevenueSummaryPayload::try_day("2026-03-01").unwrap()
            - PosRevenueSummaryPayload::try_day("2026-02-28").unwrap(),
        1
    );

    let res = PosRevenueSummaryResult {
        rvc_no: 3,
        date_from: "2026-11-01".into(),
        date_to: "2026-11-02".into(),
        days: vec![PosRevenueDay {
            date: "2026-11-01".into(),
            bills: 2,
            gross: "300000.00".parse().unwrap(),
            net: "270000.00".parse().unwrap(),
            paid: "270000".parse().unwrap(),
        }],
    };
    assert_eq!(
        res.to_json(),
        r#"{"rvc_no":3,"date_from":"2026-11-01","date_to":"2026-11-02","days":[{"date":"2026-11-01","bills":2,"gross":300000.00,"net":270000.00,"paid":270000}]}"#
    );
    assert_eq!(
        PosRevenueSummaryResult::from_json(&res.to_json()).ok(),
        Some(res)
    );
}
