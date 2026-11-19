//! Byte-for-byte checks against JSON and fingerprints the .NET agent produced for the same
//! values (`tests/fixtures/golden`, exported from `MM.Membership.Agent` with
//! `System.Text.Json` and the agent's own policies).

use std::str::FromStr;

use dl_agent_membership::config::AgentConfig;
use dl_agent_membership::folio::read::fingerprint as revenue_fingerprint;
use dl_agent_membership::identity;
use dl_agent_membership::rooms::derive_housekeeping;
use dl_agent_membership::smile::RevenueRow;
use dl_agent_membership::stj::J;
use dl_agent_membership::wire::{
    AvailabilityRequest, DbIdentity, ExtraBedUsageLine, FolioObservation, Heartbeat, OccupancyLine,
    RoomInventoryRequest, RoomLine, RoomTypeCatalogRequest, RoomTypeLine, StayClassification,
    observation_batch,
};
use dl_decimal::Decimal;
use dl_hash::{Digest, Sha256};
use dl_time::{Date, DateTime, Offset, OffsetDateTime, Time};
use dl_uuid::Uuid;

fn golden(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/golden/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn date(y: i32, m: u8, d: u8) -> Date {
    Date::new(y, m, d).unwrap()
}

fn dt(y: i32, mo: u8, d: u8, h: u8, mi: u8, s: u8, nanos: u32) -> DateTime {
    DateTime::new(date(y, mo, d), Time::new(h, mi, s, nanos).unwrap())
}

fn utc(local: DateTime) -> OffsetDateTime {
    OffsetDateTime::new(local, Offset::UTC)
}

fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn uuid(s: &str) -> Uuid {
    Uuid::from_str(s).unwrap()
}

fn identity() -> DbIdentity {
    DbIdentity {
        server: "SMILESRV\\SQL2008".into(),
        database: "SMILE_FO".into(),
        create_date: dt(2019, 5, 6, 7, 8, 9, 100_456_700),
        database_guid: Some(uuid("1B4E28BA-2FA1-11D2-883F-0016D3CCA427")),
        family_guid: Some(uuid("6f9619ff-8b86-d011-b42d-00c04fc964ff")),
        service_broker_guid: None,
    }
}

#[test]
fn heartbeats_match_dotnet() {
    let full = Heartbeat {
        agent_version: "1.2.3.0".into(),
        pending_jobs: 0,
        smile_ok: true,
        walk_in_earn_ok: Some(true),
        folio_scan_watermark: Some(dt(2026, 11, 19, 10, 20, 30, 123_450_000)),
        folio_scan_lag_seconds: Some(42),
        folio_scan_at: Some(utc(dt(2026, 11, 19, 3, 20, 31, 5_000_000))),
        db_identity: Some(identity()),
        foreign_activity_age_minutes: Some(17),
        pms_now: Some(OffsetDateTime::new(
            dt(2026, 11, 19, 10, 20, 31, 250_000_000),
            Offset::VIETNAM,
        )),
        capabilities: Some(vec!["STRUCTURED_ERRORS".into(), "DB_IDENTITY".into()]),
    };
    assert_eq!(full.to_j().to_json(), golden("heartbeat_full.json"));
    let min = Heartbeat {
        agent_version: "1.0.0".into(),
        ..Heartbeat::default()
    };
    assert_eq!(min.to_j().to_json(), golden("heartbeat_min.json"));
}

#[test]
fn availability_requests_match_dotnet() {
    let full = AvailabilityRequest {
        lines: vec![
            OccupancyLine {
                room_type_code: "DLX".into(),
                date: date(2026, 11, 19),
                rooms_sold: 3,
                rooms_total: 10,
            },
            OccupancyLine {
                room_type_code: "Phòng <&>'\"+`".into(),
                date: date(2026, 11, 20),
                rooms_sold: 0,
                rooms_total: 0,
            },
        ],
        scan_elapsed_ms: Some(1234),
        scan_complete: true,
        extra_bed_usage: Some(vec![ExtraBedUsageLine {
            date: date(2026, 11, 19),
            smile_beds: 2,
        }]),
    };
    assert_eq!(full.to_j().to_json(), golden("availability.json"));
    let min = AvailabilityRequest::default();
    assert_eq!(min.to_j().to_json(), golden("availability_min.json"));
}

#[test]
fn room_inventory_and_types_match_dotnet() {
    let rooms = RoomInventoryRequest {
        rooms: vec![
            RoomLine {
                pms_room_code: "101 ".into(),
                pms_room_name: Some("Phòng 101".into()),
                room_type_code: "DLX".into(),
                floor: Some(1),
                housekeeping_status: derive_housekeeping(None, Some(true), Some(true), None).into(),
                out_of_service: true,
                out_of_service_reason: Some("Sửa ống nước".into()),
                is_occupied: true,
                arrival_due_today: false,
                departure_due_today: true,
                out_of_order: true,
                out_of_order_reason: Some("OOO lý do".into()),
                out_of_order_until: Some(date(2026, 12, 1)),
            },
            RoomLine {
                pms_room_code: "102".into(),
                pms_room_name: None,
                room_type_code: "SUP".into(),
                floor: None,
                housekeeping_status: derive_housekeeping(Some(3), None, None, None).into(),
                out_of_service: false,
                out_of_service_reason: None,
                is_occupied: false,
                arrival_due_today: false,
                departure_due_today: false,
                out_of_order: false,
                out_of_order_reason: None,
                out_of_order_until: None,
            },
        ],
        is_full_push: true,
        as_of: utc(dt(2026, 11, 19, 3, 4, 5, 123_456_700)),
    };
    assert_eq!(rooms.to_j().to_json(), golden("room_inventory.json"));
    let types = RoomTypeCatalogRequest {
        room_types: vec![
            RoomTypeLine {
                pms_room_type_code: "DLX".into(),
                pms_name: Some("Deluxe Đôi".into()),
                num_room: Some(12),
                display_order: Some(1),
            },
            RoomTypeLine {
                pms_room_type_code: "SUP".into(),
                pms_name: None,
                num_room: None,
                display_order: None,
            },
        ],
        as_of: utc(dt(2026, 11, 19, 3, 4, 5, 600_000_000)),
    };
    assert_eq!(types.to_j().to_json(), golden("room_types.json"));
}

fn revenue(last: &str, count: i64, total: &str) -> RevenueRow {
    RevenueRow {
        folio_num: String::new(),
        last_txn_id: dec(last),
        txn_count: count,
        total_amount: dec(total),
    }
}

fn observations() -> (FolioObservation, FolioObservation) {
    let full = FolioObservation {
        folio_num: dec("123456"),
        folio_sub_num: dec("1"),
        pms_folio_id: Some(987_654_321),
        folio_status: 3,
        checkout_epoch: 1_763_520_000,
        checked_out_at: Some(utc(dt(2026, 11, 19, 4, 0, 0, 0))),
        checked_in_at: Some(utc(dt(2026, 11, 17, 7, 30, 15, 120_000_000))),
        observed_at: utc(dt(2026, 11, 19, 4, 0, 1, 997_000_000)),
        arrival_date: date(2026, 11, 17),
        departure_date: date(2026, 11, 19),
        confirm_num: Some(dec("5550001")),
        external_confirm_number: Some("BK-1\"x'".into()),
        is_cancelled: false,
        cancelled_at: None,
        is_no_show: false,
        room_code: Some("101".into()),
        room_item_id_hint: Some(uuid("0f8fad5b-d9cb-469f-a165-70867728950e")),
        guest_phone: Some("0901234567".into()),
        guest_name: Some("Nguyễn Văn A".into()),
        qualifying_revenue: dec("1500000.50"),
        revenue_fingerprint: revenue_fingerprint(Some(&revenue("12345", 3, "1500000.5"))),
        booked_room_type_code: Some("DLX".into()),
        actual_room_type_code: Some("SUP".into()),
        num_adult: Some(2),
        num_child: Some(0),
        classification: StayClassification {
            origin_codes: vec!["OTA".into()],
            segment_codes: vec![],
            rate_codes: vec!["BAR".into()],
        },
        num_enf: Some(1),
        extra_bed: Some(0),
        extra_bed_special_qty: None,
    };
    let bare = FolioObservation {
        folio_num: dec("1"),
        folio_sub_num: dec("0"),
        pms_folio_id: None,
        folio_status: 1,
        checkout_epoch: 0,
        checked_out_at: None,
        checked_in_at: None,
        observed_at: utc(dt(2026, 1, 2, 3, 4, 5, 0)),
        arrival_date: date(2026, 1, 2),
        departure_date: date(2026, 1, 2),
        confirm_num: None,
        external_confirm_number: None,
        is_cancelled: true,
        cancelled_at: Some(utc(dt(2026, 1, 2, 3, 4, 6, 0))),
        is_no_show: true,
        room_code: None,
        room_item_id_hint: None,
        guest_phone: None,
        guest_name: None,
        qualifying_revenue: dec("0"),
        revenue_fingerprint: revenue_fingerprint(None),
        booked_room_type_code: None,
        actual_room_type_code: None,
        num_adult: None,
        num_child: None,
        classification: StayClassification::default(),
        num_enf: None,
        extra_bed: None,
        extra_bed_special_qty: Some(0),
    };
    (full, bare)
}

#[test]
fn folio_observations_and_their_hashes_match_dotnet() {
    let (full, bare) = observations();
    let batch = observation_batch(
        &[full.clone(), bare.clone()],
        "1.0.0",
        Some("srv|db|2019-05-06T07:08:09.100"),
    );
    assert_eq!(batch.to_json(), golden("folio_observations.json"));
    let hash = |o: &FolioObservation| {
        dl_encoding::hex::encode_upper(Sha256::digest(o.to_j().to_json().as_bytes()))
    };
    assert_eq!(
        format!("{}\n{}\n", hash(&full), hash(&bare)),
        golden("folio_observation_hashes.txt")
    );
}

#[test]
fn revenue_fingerprints_match_dotnet() {
    for line in golden("revenue_fingerprints.txt").lines() {
        let f: Vec<&str> = line.split('|').collect();
        let row = revenue(f[0], f[1].parse().unwrap(), f[2]);
        assert_eq!(revenue_fingerprint(Some(&row)), f[3], "{line}");
    }
}

#[test]
fn identity_fingerprints_match_dotnet() {
    let no_guid = DbIdentity {
        database_guid: None,
        ..identity()
    };
    let padded = DbIdentity {
        server: " Srv ".into(),
        database: " Db ".into(),
        create_date: dt(2020, 1, 1, 0, 0, 0, 999_999_900),
        database_guid: None,
        family_guid: None,
        service_broker_guid: None,
    };
    let got = [identity(), no_guid, padded]
        .iter()
        .map(identity::fingerprint)
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(format!("{got}\n"), golden("identity_fingerprints.txt"));
}

#[test]
fn housekeeping_derivation_matches_dotnet() {
    let int = |v: Option<i64>| v.map_or("-".to_owned(), |v| v.to_string());
    let flag = |v: Option<bool>| match v {
        None => "-",
        Some(true) => "True",
        Some(false) => "False",
    };
    let mut lines = Vec::new();
    for s in [
        None,
        Some(0),
        Some(1),
        Some(2),
        Some(3),
        Some(4),
        Some(5),
        Some(-1),
    ] {
        for c in [None, Some(true), Some(false)] {
            for i in [None, Some(true), Some(false)] {
                for t in [None, Some(0), Some(1)] {
                    lines.push(format!(
                        "{},{},{},{}={}",
                        int(s),
                        flag(c),
                        flag(i),
                        int(t),
                        derive_housekeeping(s, c, i, t)
                    ));
                }
            }
        }
    }
    assert_eq!(
        format!("{}\n", lines.join("\n")),
        golden("housekeeping.txt")
    );
}

#[test]
fn strings_decimals_and_datetimes_match_dotnet() {
    let ascii: String = (0u8..128).map(char::from).collect();
    let strings = J::Arr(vec![
        J::str(&ascii),
        J::str("Đà Nẵng 😀 \u{2028}\u{2029}\u{a0}\u{ff}\u{fffd}"),
    ]);
    assert_eq!(strings.to_json(), golden("strings.json"));
    let decimals = J::Arr(
        [
            "0",
            "0.00",
            "1.50",
            "-2.5",
            "123456789012345678",
            "0.0000001",
            "79228162514264337593543950335",
            "-0.010",
        ]
        .into_iter()
        .map(|s| J::Dec(dec(s)))
        .collect(),
    );
    assert_eq!(decimals.to_json(), golden("decimals.json"));
    let datetimes = J::Arr(
        [
            dt(2026, 1, 2, 3, 4, 5, 0),
            dt(2026, 1, 2, 3, 4, 5, 100_000_000),
            dt(2026, 1, 2, 0, 0, 0, 100),
            dt(2026, 1, 2, 0, 0, 0, 123_000_000),
            dt(1, 1, 1, 0, 0, 0, 0),
        ]
        .into_iter()
        .map(J::datetime)
        .collect(),
    );
    assert_eq!(datetimes.to_json(), golden("datetimes.json"));
}

#[test]
fn agent_config_plaintext_matches_dotnet_and_reads_back() {
    let cfg = AgentConfig {
        hub_base_url: "https://hub.example.test/".into(),
        agent_token: "tok-ABC+/=\"x".into(),
        hotel_id: uuid("0F8FAD5B-D9CB-469F-A165-70867728950E"),
        smile_connection_string:
            "Server=SMILE\\SQL2008;Database=SMILE_FO;User Id=sa;Password=p@ss;word<>".into(),
        origin_code: "MM".into(),
    };
    let golden = golden("agent_config.json");
    assert_eq!(cfg.to_json(), golden);
    let back = AgentConfig::from_json(golden.as_bytes()).unwrap();
    assert_eq!(back.to_json(), golden);
}
