use super::*;

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

const CREATE: &str = r#"{
  "action": "Create",
  "bookingId": "6f1c2d3e-0000-4000-8000-000000000001",
  "confirmationCode": "AZ-123",
  "memberId": "6f1c2d3e-0000-4000-8000-0000000000aa",
  "travelAgentCode": 12,
  "memberCode": "  M001 ",
  "smileConfirmNum": 0,
  "forceCreate": "true",
  "specialRequests": "   ",
  "member": {"FullName": "Nguyen Van A", "Country": "VN", "DateOfBirth": "0001-01-01"},
  "voucher": {"VoucherId": "6f1c2d3e-0000-4000-8000-0000000000bb", "Code": "V1", "Type": "PercentOff", "Value": 0.1, "MaxDiscountAmount": 50000},
  "rooms": [{
    "Id": "6f1c2d3e-0000-4000-8000-000000000101",
    "RoomTypeCode": "DLX",
    "Arrival": "2026-12-01",
    "Departure": "2026-12-03T00:00:00",
    "NumAdult": 2, "NumChild": 1.5,
    "GuestLastName": "Tran", "GuestFirstName": "Binh",
    "nights": [{"Date": "2026-12-01", "Net": 1000000.50, "NetExclBed": 900000}],
    "PricingModel": "OCCUPANCY_V1", "ExtraBeds": 1, "ExtraBedNightly": 100000,
    "guest": {"Phone": "0900", "Nationality": "VN"}
  }],
  "expectedFolios": [{"FolioNum": 0}, {"FolioNum": 77, "RoomItemId": "nope"}]
}"#;

#[test]
fn create_payload_reads_every_field_the_writers_use() {
    let p = parse_payload(CREATE).unwrap();
    assert_eq!(p.confirmation_code, "AZ-123");
    assert_eq!(p.travel_agent_code, Some(12));
    assert_eq!(p.member_code.as_deref(), Some("M001"));
    assert_eq!(p.smile_confirm_num, None);
    assert!(p.force_create);
    assert_eq!(p.special_requests, None);
    let m = p.member.as_ref().unwrap();
    assert_eq!(m.nationality.as_deref(), Some("VN"));
    assert_eq!(m.date_of_birth, None);
    let v = p.voucher.as_ref().unwrap();
    assert_eq!((v.kind.as_str(), v.value), ("PercentOff", d("0.1")));
    assert_eq!(v.max_discount_amount, Some(d("50000")));
    let r = &p.rooms[0];
    assert_eq!(r.departure, Date::new(2026, 12, 3).unwrap());
    assert_eq!((r.num_adult, r.num_child), (2, 0));
    assert_eq!(r.nights[0].net, d("1000000.50"));
    assert_eq!(r.nights[0].net_excl_bed, Some(d("900000")));
    let o = r.occupancy.as_ref().unwrap();
    assert_eq!(o.extra_beds, Some(1));
    assert_eq!(r.guest.as_ref().unwrap().phone.as_deref(), Some("0900"));
    let e = p.expected_folios.as_ref().unwrap();
    assert_eq!(e.len(), 1);
    assert_eq!((e[0].folio_num, e[0].room_item_id), (d("77"), Uuid::NIL));
    assert!(p.is_known_in_smile());
}

#[test]
fn bad_json_is_an_error_and_a_fractional_agent_code_is_ignored() {
    assert!(parse_payload("{").is_err());
    let p = parse_payload(r#"{"travelAgentCode": 12.5, "rooms": "x"}"#).unwrap();
    assert_eq!(p.travel_agent_code, None);
    assert!(p.rooms.is_empty());
    assert!(!p.is_known_in_smile());
}

#[test]
fn link_folio_needs_every_key_and_one_folio() {
    let ok = r#"{"bookingId":"6f1c2d3e-0000-4000-8000-000000000001",
        "memberId":"6f1c2d3e-0000-4000-8000-0000000000aa",
        "confirmationCode":" AZ-1 ","confirmNum":55,
        "folios":[{"FolioNum":101,"BookingRoomId":"6f1c2d3e-0000-4000-8000-000000000101","RoomLabel":" A "}]}"#;
    let l = parse_link_folio(ok).unwrap();
    assert_eq!(l.external_confirm_number, "AZ-1");
    assert_eq!(l.confirm_num, d("55"));
    assert_eq!(l.folios[0].room_label.as_deref(), Some("A"));
    assert_eq!(
        parse_link_folio(&ok.replace("\"confirmNum\":55", "\"confirmNum\":0")).unwrap_err(),
        "LINK_FOLIO payload has no positive ConfirmNum."
    );
    assert_eq!(
        parse_link_folio(&ok.replace("\"FolioNum\":101,", "")).unwrap_err(),
        "LINK_FOLIO payload has a folio without FolioNum or BookingRoomId."
    );
    let none = ok.replace(
        r#"[{"FolioNum":101,"BookingRoomId":"6f1c2d3e-0000-4000-8000-000000000101","RoomLabel":" A "}]"#,
        "[]",
    );
    assert_eq!(
        parse_link_folio(&none).unwrap_err(),
        "LINK_FOLIO payload lists no folio."
    );
}
