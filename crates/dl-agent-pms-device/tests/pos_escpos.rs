//! Port of Pos/EscPosTests.cs.

#[path = "pos_fakes/mod.rs"]
mod fakes;

use dl_agent_pms_device::adapters::pos::{EscPos, PosPrinterStatus};
use dl_agent_pms_device::contracts::codes;
use fakes::lines;

#[test]
fn hw01_kick_is_esc_p_with_the_pin_and_2ms_pulse_units() {
    assert_eq!(
        EscPos::kick(2, 50, 500).unwrap(),
        [0x1B, 0x70, 0x00, 25, 250]
    );
    assert_eq!(EscPos::kick(5, 2, 510).unwrap(), [0x1B, 0x70, 0x01, 1, 255]);
}

#[test]
fn hw01_kick_out_of_range_is_a_bad_payload() {
    for (pin, on, off) in [(3, 50, 500), (2, 0, 500), (2, 50, 600)] {
        let e = EscPos::kick(pin, on, off).unwrap_err();
        assert_eq!(e.code, codes::BAD_PAYLOAD, "{pin} {on} {off}");
    }
    assert_eq!(
        EscPos::kick(3, 50, 500).unwrap_err().message,
        "drawer pin must be 2 or 5, got 3"
    );
    assert_eq!(
        EscPos::kick(2, 50, 600).unwrap_err().message,
        "offMs must be 2-510 ms, got 600"
    );
}

#[test]
fn hw02_display_clears_then_places_each_line_folded_to_ascii_and_fitted() {
    let l = lines(&["Cà phê sữa đá", "Tổng cộng: 1.250.000 VND"]);
    let bytes = EscPos::display(Some(&l), 20).unwrap();
    let mut expected = vec![0x1B, 0x40, 0x0C, 0x1F, 0x24, 1, 1];
    expected.extend_from_slice(b"Ca phe sua da       ");
    expected.extend_from_slice(&[0x1F, 0x24, 1, 2]);
    expected.extend_from_slice(b"Tong cong: 1.250.000");
    assert_eq!(bytes, expected);
}

#[test]
fn hw02_ascii_folding_maps_d_stroke_and_replaces_other_symbols() {
    assert_eq!(EscPos::to_ascii("Đồng ĐÁ €"), "Dong DA ?");
}

#[test]
fn hw02_display_line_count_and_width_are_bounded() {
    for (count, columns) in [(0, 20), (5, 20), (2, 41)] {
        let l = vec![Some("x".to_owned()); count];
        let e = EscPos::display(Some(&l), columns).unwrap_err();
        assert_eq!(e.code, codes::BAD_PAYLOAD, "{count} {columns}");
    }
}

#[test]
fn prt03_status_bytes_decode_online_drawer_and_paper() {
    let s = |online, drawer_pin3_high, paper_out, paper_near_end| PosPrinterStatus {
        online,
        drawer_pin3_high,
        paper_out,
        paper_near_end,
    };
    assert_eq!(
        EscPos::parse_status(0x12, 0x12).unwrap(),
        s(true, false, false, false)
    );
    assert_eq!(
        EscPos::parse_status(0x1E, 0x7E).unwrap(),
        s(false, true, true, true)
    );
}

#[test]
fn prt03_a_reply_that_is_not_a_status_byte_is_a_device_error() {
    let e = EscPos::parse_status(0xFF, 0x12).unwrap_err();
    assert_eq!(e.code, codes::ERROR);
    assert_eq!(
        e.message,
        "printer answered DLE EOT with 0xFF 0x12, not a status byte"
    );
}
