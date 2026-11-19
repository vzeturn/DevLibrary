//! The walk-in backfill: plan validation (the .NET messages), the state file, and a run
//! over two pages against the recording session.

use std::sync::Mutex;

use dl_decimal::Decimal;
use dl_tds::Value;
use dl_time::{Date, DateTime, OffsetDateTime, Time};

use super::*;
use crate::checkout::tests::{Db, Env, Hub};
use crate::earn::pairs::EarnSettings;
use crate::earn::tests::smile;
use crate::smile::SmileError;
use crate::writer::session::{Called, Rec};

fn d(y: i32, m: u8, day: u8) -> Date {
    Date::new(y, m, day).unwrap()
}

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dl-mb-backfill-{tag}-{}-{}",
        std::process::id(),
        OffsetDateTime::now_utc().unix_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(STATE_FILE)
}

#[test]
fn plans_are_validated_like_dotnet() {
    let p = Path::new("/x/state.json");
    let today = d(2026, 11, 19);
    let plan = Plan::try_create(Some(" 2026-01-01 "), None, Some(9000), p, today).unwrap();
    assert_eq!(
        (plan.from, plan.to, plan.page_size),
        (d(2026, 1, 1), today, 500)
    );
    assert_eq!(
        Plan::try_create(Some("2026-01-01"), Some("2026-01-31"), Some(0), p, today)
            .unwrap()
            .page_size,
        1
    );
    assert!(
        Plan::try_create(None, None, None, p, today)
            .unwrap_err()
            .starts_with("Thiếu --backfill-from")
    );
    assert!(
        Plan::try_create(Some("01/03/2026"), None, None, p, today)
            .unwrap_err()
            .contains("không đọc được")
    );
    assert_eq!(
        Plan::try_create(Some("2026-02-01"), Some("2026-01-01"), None, p, today).unwrap_err(),
        "Khoảng backfill đảo ngược: --backfill-from 2026-02-01 sau --backfill-to 2026-01-01."
    );
    assert!(
        Plan::try_create(Some("2025-01-01"), Some("2026-02-05"), None, p, today)
            .unwrap_err()
            .contains("vượt trần 400 ngày")
    );
}

fn raw(h: u8) -> DateTime {
    DateTime::new(d(2026, 3, 1), Time::new(h, 0, 0, 0).unwrap())
}

fn group(conf: i64, h: u8) -> Rec {
    Rec::new(vec![
        ("ConfirmNum", Value::Decimal(Decimal::from(conf))),
        ("FirstFolioNum", Value::Decimal(Decimal::from(conf + 1000))),
        ("CheckedOutAt", Value::DateTime(raw(h))),
        ("GuestPhone", Value::String("0901".into())),
        ("GuestName", Value::Null),
    ])
}

fn paged_smile() -> crate::writer::recorder::Recorder {
    let mut r = smile(1);
    let mut page = 0;
    r.on("JOIN n ON n.FolioNum", move |_| {
        page += 1;
        Ok(Called {
            rows: if page == 1 {
                vec![group(9001, 10)]
            } else {
                Vec::new()
            },
            status: Some(0),
        })
    });
    r
}

#[test]
fn a_run_pages_by_cursor_and_saves_it() {
    let state = tmp("run");
    let plan = Plan::try_create(
        Some("2026-03-01"),
        Some("2026-03-01"),
        Some(1),
        &state,
        d(2026, 11, 19),
    )
    .unwrap();
    let db = Db(Mutex::new(paged_smile()));
    let hub = Hub::default();
    let pairs = PairSource::new(EarnSettings::default());
    let (s, result) = run(&plan, &db, &hub, &Env, &pairs, "MobileNum");
    result.unwrap();
    assert_eq!((s.pages, s.processed, s.failed), (1, 1, 0));
    assert!(s.is_clean());
    assert_eq!(hub.walk_ins.lock().unwrap()[0].folio_ref, "CONF:9001");
    let texts = db.texts();
    let pages: Vec<_> = texts.iter().filter(|t| t.contains("JOIN n ON")).collect();
    assert_eq!(pages.len(), 2);
    assert!(!pages[0].contains("@CursorAt"));
    assert!(pages[1].contains("g.CheckedOutAt = @CursorAt AND g.ConfirmNum > @CursorConf"));
    let saved = load_state(&plan).expect("state saved after the page");
    assert_eq!(
        saved.cursor,
        Some(WalkInCursor {
            checked_out_at: raw(10),
            confirm_num: Decimal::from(9001i64)
        })
    );
    assert_eq!(saved.processed, 1);
    // Another range ignores the file.
    let other = Plan {
        to: d(2026, 3, 2),
        ..plan.clone()
    };
    assert!(load_state(&other).is_none());
    let _ = std::fs::remove_dir_all(state.parent().unwrap());
}

#[test]
fn a_failed_group_stops_saving_the_cursor() {
    let state = tmp("fail");
    let plan =
        Plan::try_create(Some("2026-03-01"), None, Some(1), &state, d(2026, 11, 19)).unwrap();
    let mut r = paged_smile();
    r.on("AS K,", |_| Err(SmileError::Server(1205)));
    let db = Db(Mutex::new(r));
    let (s, result) = run(
        &plan,
        &db,
        &Hub::default(),
        &Env,
        &PairSource::default(),
        "MobileNum",
    );
    result.unwrap();
    assert_eq!((s.processed, s.failed), (0, 1));
    assert!(!s.is_clean());
    assert!(load_state(&plan).is_none(), "no cursor past a failed group");
    let _ = std::fs::remove_dir_all(state.parent().unwrap());
}

#[test]
fn the_state_reads_what_dotnet_writes() {
    let json = br#"{
  "from": "2026-01-01",
  "to": "2026-02-01",
  "cursorCheckedOutAt": "2026-01-15T10:20:30",
  "cursorConfirmNum": 9001,
  "processed": 42,
  "updatedAt": "2026-02-01T03:04:05.1234567+00:00"
}"#;
    let s = State::from_json(json).unwrap();
    assert_eq!(s.processed, 42);
    assert_eq!(s.cursor.unwrap().confirm_num, Decimal::from(9001i64));
    assert_eq!(
        s.to_json(),
        "{\"from\":\"2026-01-01\",\"to\":\"2026-02-01\",\"cursorCheckedOutAt\":\"2026-01-15T10:20:30\",\
         \"cursorConfirmNum\":9001,\"processed\":42,\"updatedAt\":\"2026-02-01T03:04:05.1234567+00:00\"}"
    );
}
