use super::*;
use crate::jobs::payload::parse_payload;

#[test]
fn full_names_split_at_the_last_space() {
    assert_eq!(
        split_full_name(Some(" Nguyen Van  An ")),
        (Some("Nguyen Van".into()), Some("An".into()))
    );
    assert_eq!(
        split_full_name(Some("Madonna")),
        (Some("Madonna".into()), None)
    );
    assert_eq!(split_full_name(Some("  ")), (None, None));
}

#[test]
fn gender_maps_to_smile_bits() {
    assert_eq!(smile_gender(Some(" Male ")), Some(1));
    assert_eq!(smile_gender(Some("f")), Some(0));
    assert_eq!(smile_gender(Some("other")), None);
}

#[test]
fn blank_guest_fields_come_from_the_member() {
    let p = parse_payload(
        r#"{"member":{"FullName":"Le Thi Hoa","Phone":"0911","Country":"VN","Gender":"Female"},
            "rooms":[{"GuestLastName":" ","guest":{"Email":"g@x.vn"}},
                     {"GuestLastName":"Pham","GuestFirstName":"Minh"}]}"#,
    )
    .unwrap();
    let p = apply(p);
    let a = &p.rooms[0];
    assert_eq!(
        (a.guest_last_name.as_str(), a.guest_first_name.as_str()),
        ("Le Thi", "Hoa")
    );
    let g = a.guest.as_ref().unwrap();
    assert_eq!(g.email.as_deref(), Some("g@x.vn"));
    assert_eq!(g.phone.as_deref(), Some("0911"));
    assert_eq!(g.nationality.as_deref(), Some("VN"));
    assert_eq!(g.full_name.as_deref(), Some("Le Thi Hoa"));
    let b = &p.rooms[1];
    assert_eq!(b.guest_last_name, "Pham");
    assert_eq!(b.guest.as_ref().unwrap().gender.as_deref(), Some("Female"));
}

#[test]
fn without_member_or_guest_the_payload_is_untouched() {
    let p = parse_payload(r#"{"rooms":[{"GuestLastName":" X "}]}"#).unwrap();
    assert_eq!(apply(p.clone()), p);
}
