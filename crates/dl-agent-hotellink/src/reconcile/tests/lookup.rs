//! Folio lookup and travel-agent cases.

use super::*;

fn one_night_folio(id: i32, room_code: &str, ext: &str, last_name: &str) -> Folio {
    Folio {
        id,
        folio_num: 100000.0,
        confirm_num: 499000.0,
        book_status: 1,
        folio_status: 1,
        user_info1: "rt:DLX|rp:RP#0".into(),
        room_type_code: "DLX".into(),
        room_code: room_code.into(),
        external_confirm_number: Some(ext.into()),
        arrival_date: day(2026, 6, 15),
        departure_date: day(2026, 6, 16),
        rate_amount: 100.0,
        last_name: last_name.into(),
        ..Folio::default()
    }
}

fn one_night_job(booking_id: &str, booker: &str, guest: &str) -> Job {
    Job {
        booking_id: booking_id.into(),
        booker: booker.into(),
        action: ACTION_SAVE.into(),
        check_in: "2026-06-15".into(),
        check_out: "2026-06-16".into(),
        guest_name: guest.into(),
        rooms: Some(vec![Room {
            room_type_code: "DLX".into(),
            rate_plan_id: "RP".into(),
            quantity: 1,
            rates: Some(vec![rn("2026-06-15", 100.0)]),
            ..Room::default()
        }]),
        ..Job::default()
    }
}

fn repo_by_ext(key: &str, f: Folio) -> Arc<FakeRepo> {
    let repo = FakeRepo::new();
    {
        let mut s = repo.st();
        let ci = day(2026, 6, 15);
        s.folio_rates.insert(
            100000,
            vec![FolioRate {
                folio_num: 100000.0,
                h_date: ci,
                to_date: ci,
                rate_amount: 100.0,
                rate_override: 1,
                ..FolioRate::default()
            }],
        );
        s.folios_by_ext = Some(HashMap::from([(key.to_owned(), vec![f])]));
    }
    repo
}

#[test]
fn dual_lookup_finds_old_booking_id_stamped_folio() {
    let repo = repo_by_ext("HL-999", one_night_folio(7, "201", "HL-999", "OLD GUEST"));
    new_reconciler(&repo)
        .apply(&one_night_job("HL-999", "OTA-123", "OLD GUEST"))
        .expect("apply");
    let s = repo.st();
    assert!(
        s.inserted.is_empty(),
        "matched the old BookingID stamp → modify"
    );
    assert!(s.cancelled.is_empty(), "room type unchanged → no cancel");
}

#[test]
fn lookup_by_booker_when_no_fallback_needed() {
    let repo = repo_by_ext("WALK-1", one_night_folio(8, "202", "WALK-1", "G"));
    new_reconciler(&repo)
        .apply(&one_night_job("WALK-1", "WALK-1", "G"))
        .expect("apply");
    let s = repo.st();
    assert!(
        s.inserted.is_empty() && s.cancelled.is_empty(),
        "no-op modify"
    );
}

#[test]
fn stamps_external_confirm_from_booker() {
    let repo = FakeRepo::new();
    Reconciler::new(repo.clone())
        .apply(&one_night_job("HL-999", "OTA-123", ""))
        .expect("apply");
    let s = repo.st();
    assert_eq!(s.inserted.len(), 1);
    assert_eq!(
        s.inserted[0].f.external_confirm_number.as_deref(),
        Some("OTA-123")
    );
}

#[test]
fn travel_agent_via_contact_fallback() {
    let repo = FakeRepo::new();
    {
        let mut s = repo.st();
        s.contacts.insert(
            "Agoda".into(),
            ClientContact {
                client_folio_num: 88.0,
                contact_name: "Agoda".into(),
            },
        );
        s.clients.push(SmileClient {
            client_folio_num: 88.0,
            client_name: "Agoda Co".into(),
        });
        s.client_sts.insert(
            88,
            ClientSts {
                client_code: 88.0,
                origin: "AGD".into(),
                ..ClientSts::default()
            },
        );
    }
    let mut job = save_job();
    job.source_name = "Agoda".into();
    new_reconciler(&repo).apply(&job).expect("apply");
    let s = repo.st();
    let f = &s.inserted[0].f;
    assert_eq!(
        (f.travel_agent1_code, f.origin_code.as_str()),
        (88.0, "AGD")
    );
}

#[test]
fn no_travel_agent_defaults() {
    let repo = FakeRepo::new();
    new_reconciler(&repo).apply(&save_job()).expect("apply");
    let s = repo.st();
    let f = &s.inserted[0].f;
    assert_eq!(f.travel_agent1_code, 0.0);
    assert!(f.origin_code.is_empty() && f.market_segment_code.is_empty());
}

#[test]
fn payment_code_override() {
    let repo = FakeRepo::new();
    let mut job = save_job();
    job.payment_code = "9".into();
    new_reconciler(&repo).apply(&job).expect("apply");
    assert_eq!(repo.st().inserted[0].f.payment_code, "9");
}

#[test]
fn no_breakfast_zeroes_guest_bf() {
    let repo = FakeRepo::new();
    let mut job = save_job();
    room0(&mut job).include_bf = false;
    room0(&mut job).breakfast_code.clear();
    new_reconciler(&repo).apply(&job).expect("apply");
    let s = repo.st();
    let f = &s.inserted[0].f;
    assert_eq!((f.num_guest_bf, f.num_child_bf), (0, 0));
    assert!(!f.include_bf && f.breakfast_code.is_empty());
}
