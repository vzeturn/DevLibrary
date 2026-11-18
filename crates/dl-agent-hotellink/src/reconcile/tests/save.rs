//! New-booking cases.

use super::*;

#[test]
fn save_new_booking_builds_folio_exactly() {
    let repo = FakeRepo::new();
    {
        let mut s = repo.st();
        s.clients.push(SmileClient {
            client_folio_num: 77.0,
            client_name: "Booking.com".into(),
        });
        s.client_sts.insert(
            77,
            ClientSts {
                client_code: 77.0,
                origin: "OTA".into(),
                m_segment: "ONLINE".into(),
                u_def: ["U1", "U2", "U3", "U4", "U5", "U6", "U7", "U8"].map(String::from),
            },
        );
    }
    let r = new_reconciler(&repo);
    r.apply(&save_job()).expect("apply");
    let s = repo.st();
    assert_eq!(s.inserted.len(), 1, "inserted");
    let got = &s.inserted[0];
    let f = &got.f;
    // BuildFolioAsync (BookingProcessor.cs:137-231)
    assert_eq!(f.folio_num, 100001.0);
    assert_eq!(f.confirm_num, 500001.0);
    assert_eq!(f.arrival_date, day(2026, 6, 12));
    assert_eq!(f.departure_date, day(2026, 6, 14));
    assert_eq!(f.room_type_code, "DLX01");
    assert_eq!(f.room_type_booked, "DLX01");
    assert_eq!((f.num_adult, f.num_child), (3, 1));
    assert_eq!((f.num_guest_bf, f.num_child_bf), (3, 1));
    assert_eq!(f.travel_agent1_code, 77.0);
    assert_eq!(f.origin_code, "OTA");
    assert_eq!(f.market_segment_code, "ONLINE");
    assert_eq!(f.user_define_code1, "U1");
    assert_eq!(f.user_define_code8, "U8");
    assert_eq!(f.rate_amount, 1500000.0);
    assert_eq!(f.booker, "EXT-1");
    assert_eq!(f.last_name, "NGUYEN VAN DUC");
    assert_eq!(f.tel_num_m, "0901");
    assert_eq!(f.mail_m, "a@b.c");
    assert_eq!(f.book_status, 1);
    assert_eq!(f.breakfast_code, "ABU");
    assert!(f.include_bf);
    assert_eq!(f.payment_code, "7", "PaymentCode (default)");
    assert_eq!(
        f.external_confirm_number.as_deref(),
        Some("EXT-1"),
        "ExternalConfirmNumber must be the OTA code (Booker), not BookingID"
    );
    let rates = &got.rates;
    assert_eq!(rates.len(), 2);
    assert_eq!(rates[0].rate_amount, 1500000.0);
    assert_eq!(rates[1].rate_amount, 1600000.0);
    assert_eq!(rates[0].rate_override, 1);
    assert_eq!(rates[1].h_date, day(2026, 6, 13));
    assert_eq!(rates[1].to_date, rates[1].h_date);
    let an = &got.an;
    assert_eq!(an.folio_num, f.folio_num);
    assert_eq!(an.last_name, f.last_name);
    assert_eq!(an.confirm_num, f.confirm_num);
    assert_eq!(an.a_origin_code, "OTA");
    assert_eq!(an.email, "a@b.c");
    assert_eq!(an.adt_status, 1);
    assert_eq!(an.arrival_time, f.arrival_date);
    assert_eq!(an.departure_time, f.departure_date);
}

#[test]
fn lock_by_room_does_not_block_new_booking() {
    let repo = FakeRepo::new();
    repo.st().global_params.insert("CancelByRoom".into(), 1);
    new_reconciler(&repo).apply(&save_job()).expect("apply");
    assert_eq!(repo.st().inserted.len(), 1);
}

#[test]
fn quantity_two_creates_two_folios() {
    let repo = FakeRepo::new();
    let mut job = save_job();
    room0(&mut job).quantity = 2;
    new_reconciler(&repo).apply(&job).expect("apply");
    let s = repo.st();
    assert_eq!(s.inserted.len(), 2);
    assert_eq!(s.confirm_num_calls, 1, "one ConfirmNum for the whole booking");
    assert_eq!(s.inserted[0].f.confirm_num, s.inserted[1].f.confirm_num);
    assert_ne!(s.inserted[0].f.folio_num, s.inserted[1].f.folio_num);
}
