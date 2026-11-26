//! `fo.inhouse_search` name matching on decoded TCVN3 names (port of the C#
//! `FoInHouseMatchTests`, RT-1296): staff type Unicode with or without accents.

use dl_agent_pda_sync::commands::fo_inhouse_match::{filter, fold, is_number};
use dl_agent_pda_sync::contracts::wire::FoInHouseGuest;
use dl_vn::tcvn3;

fn encode(text: &str) -> Vec<u8> {
    tcvn3::encode(text).unwrap()
}

fn read(room: &str, folio: i64, stored_name: &[u8]) -> FoInHouseGuest {
    FoInHouseGuest {
        room_no: room.to_owned(),
        folio_num: folio,
        guest_name: tcvn3::decode_bytes(stored_name),
        arrival_date: Some("2026-11-24".to_owned()),
        departure_date: Some("2026-11-28".to_owned()),
    }
}

fn in_house() -> Vec<FoInHouseGuest> {
    vec![
        read("101", 9001, &encode("Trần Thị Bình")),
        read("204", 9002, &encode("Nguyễn Văn An")),
        read("305", 9003, &encode("John Nguyen")),
    ]
}

fn folios(guests: &[FoInHouseGuest]) -> Vec<i64> {
    guests.iter().map(|g| g.folio_num).collect()
}

#[test]
fn the_fixture_name_is_stored_as_tcvn3_not_unicode() {
    let an = encode("Nguyễn Văn An");
    assert_eq!(an.len(), "Nguyễn Văn An".chars().count());
    assert!(an.iter().any(|&b| b > 0x7F));
    assert_ne!(String::from_utf8_lossy(&an), "Nguyễn Văn An");
    assert_eq!(tcvn3::decode_bytes(&an), "Nguyễn Văn An");
}

#[test]
fn an_accented_or_plain_query_finds_the_tcvn3_stored_name() {
    for query in ["Nguyễn Văn An", "nguyễn văn", "NGUYEN VAN AN", "văn  an"] {
        let hits = filter(in_house(), query, 20);
        let an: Vec<_> = hits.iter().filter(|g| g.folio_num == 9002).collect();
        assert_eq!(an.len(), 1, "{query}");
        assert_eq!(an[0].guest_name, "Nguyễn Văn An");
    }
}

#[test]
fn a_word_start_ranks_before_a_name_that_only_contains_the_text() {
    let mut guests = in_house();
    guests.push(read("100", 9000, &encode("Hoàng Anh")));

    assert_eq!(folios(&filter(guests, "an", 20)), [9002, 9000, 9001]);
    assert_eq!(folios(&filter(in_house(), "Nguyễn", 20)), [9002, 9003]);
}

#[test]
fn an_exact_room_comes_first_and_the_limit_holds() {
    let mut rooms = in_house();
    rooms.push(read("A12", 9010, &encode("Lê A12 Bình")));

    assert_eq!(folios(&filter(rooms.clone(), "a12", 20)), [9010]);
    assert_eq!(folios(&filter(rooms.clone(), "bình", 1)), [9001]);
    assert!(filter(rooms, "xyz", 20).is_empty());
}

#[test]
fn fold_drops_accents_and_dd_and_collapses_spaces() {
    assert_eq!(fold("Đặng  Thị Ánh"), "dang thi anh");
    assert_eq!(fold("  Lê "), "le");
}

#[test]
fn only_digits_go_to_the_exact_sql() {
    assert!(is_number("305"));
    assert!(!is_number("A12"));
    assert!(!is_number("an"));
}
