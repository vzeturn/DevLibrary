//! The `Bool`, `Int`, `Tcvn3Text` and `Unicode` column kinds of `master.upsert` (RT-0831):
//! the value rules and problem texts (the .NET agent's `MasterColumnKindsTests` holds the same
//! table) and the update-SQL guard. The real SQL is in `commands_master_kinds_sql.rs`.

use dl_agent_pda_sync::commands::master::{ENTITIES, MasterColumn, encode_tcvn3};
use dl_agent_pda_sync::contracts::mapping::tcvn3;

/// 17 characters, all stored unchanged by dbo.Unicode2TCVN.
const SPA: &str = "Gội đầu thảo dược";
const FLAG: MasterColumn = MasterColumn::boolean("Flag");
const QTY: MasterColumn = MasterColumn::int("Qty", -5, 100);
const NAME: MasterColumn = MasterColumn::tcvn3("Name", 17);
const UNAME: MasterColumn = MasterColumn::unicode("UName", 5);
const TCVN3_REFUSED: &str = "Name must be ASCII or Vietnamese text that TCVN3 stores unchanged (no capital letter with a tone mark)";
const CONTROL: &str = "must be valid Unicode text without control characters";

#[test]
fn bool_accepts_exactly_0_1_true_and_false() {
    for (value, expected) in [("0", false), ("1", true), ("true", true), ("false", false)] {
        assert_eq!(FLAG.problem(value), None, "{value}");
        assert_eq!(FLAG.bool_value(value), Some(expected), "{value}");
    }
}

#[test]
fn bool_refuses_anything_else() {
    for value in [
        "", "01", "-0", "+1", " 1", "1 ", "1.0", "2", "TRUE", "True", "yes", "1\n",
    ] {
        assert_eq!(
            FLAG.problem(value).as_deref(),
            Some("Flag must be 0, 1, true or false"),
            "{value:?}"
        );
    }
}

#[test]
fn int_accepts_a_signed_integer_within_its_range() {
    for (value, expected) in [
        ("-5", -5),
        ("100", 100),
        ("0", 0),
        ("01", 1),
        ("-0", 0),
        ("-05", -5),
        ("0000000000000100", 100),
    ] {
        assert_eq!(QTY.problem(value), None, "{value}");
        assert_eq!(QTY.int_value(value), Some(expected), "{value}");
    }
}

#[test]
fn int_refuses_anything_else() {
    for value in [
        "",
        "-",
        "+1",
        " 1",
        "1 ",
        "1.0",
        "1e2",
        "--1",
        "1,0",
        "101",
        "-6",
        "TRUE",
        "\u{0661}",
        "\u{FF11}",
        "99999999999999999999",
    ] {
        assert_eq!(
            QTY.problem(value).as_deref(),
            Some("Qty must be an integer from -5 to 100"),
            "{value:?}"
        );
    }
}

#[test]
fn int_reaches_the_int32_bounds_and_no_further() {
    let wide = MasterColumn::int("Qty", i32::MIN, i32::MAX);
    assert_eq!(wide.problem("-2147483648"), None);
    assert_eq!(wide.problem("2147483647"), None);
    for value in ["-2147483649", "2147483648"] {
        assert_eq!(
            wide.problem(value).as_deref(),
            Some("Qty must be an integer from -2147483648 to 2147483647")
        );
    }
}

#[test]
fn tcvn3_accepts_text_the_vendor_function_stores_unchanged() {
    for value in [
        SPA,
        "Ăn uống",
        "Đồ uống",
        "ĂÂÊÔƠƯĐ",
        "Uống trà",
        "Gội Đầu Thảo Dược",
        " Spa 1",
        "a",
    ] {
        assert_eq!(NAME.problem(value), None, "{value}");
        assert_eq!(tcvn3::decode(&encode_tcvn3(value).unwrap()), value);
    }
}

#[test]
fn tcvn3_refuses_what_would_not_read_back() {
    let spa_long = format!("{SPA}!");
    let control = format!("Name {CONTROL}");
    let cases: [(&str, &str); 16] = [
        (&spa_long, "Name must be at most 17 characters"),
        ("", "Name must not be empty or end with a blank"),
        ("Spa ", "Name must not be empty or end with a blank"),
        ("   ", "Name must not be empty or end with a blank"),
        ("Gội\tđầu", &control),
        ("Gội\nđầu", &control),
        ("a\u{0085}", &control),
        ("a\u{007F}", &control),
        ("Á", TCVN3_REFUSED),
        ("Ấn Độ", TCVN3_REFUSED),
        ("GỘI ĐẦU", TCVN3_REFUSED),
        ("Aà", TCVN3_REFUSED),
        ("ü", TCVN3_REFUSED),
        ("€", TCVN3_REFUSED),
        ("a\u{0301}", TCVN3_REFUSED),
        ("🌿", TCVN3_REFUSED),
    ];
    for (value, problem) in cases {
        assert_eq!(NAME.problem(value).as_deref(), Some(problem), "{value:?}");
    }
}

#[test]
fn unicode_accepts_text_without_control_characters() {
    for value in ["", "Gội đ", "Ấn Độ", "ab  ", "🌿🌿", "€ü"] {
        assert_eq!(UNAME.problem(value), None, "{value:?}");
    }
}

#[test]
fn unicode_refuses_control_characters_and_overlong_text() {
    let control = format!("UName {CONTROL}");
    let cases: [(&str, &str); 5] = [
        ("a\nb", &control),
        ("\u{007F}", &control),
        ("\u{009F}", &control),
        ("abcdef", "UName must be at most 5 characters"),
        // Six UTF-16 units, as nvarchar counts them.
        ("🌿🌿🌿", "UName must be at most 5 characters"),
    ];
    for (value, problem) in cases {
        assert_eq!(UNAME.problem(value).as_deref(), Some(problem), "{value:?}");
    }
}

#[test]
fn tcvn3_encodes_like_the_vendor_function() {
    // dbo.Unicode2TCVN: ộ -> 0xE9, đ -> 0xAE, Ư -> 0xA6; ASCII stays; Á -> 0xB8 (the
    // lower-case byte, so it is refused).
    assert_eq!(
        encode_tcvn3("Gộ đƯ"),
        Some(vec![0x47, 0xE9, 0x20, 0xAE, 0xA6])
    );
    assert_eq!(encode_tcvn3("Á"), Some(vec![0xB8]));
    assert_eq!(encode_tcvn3("ü"), None);
}

#[test]
fn the_accepted_alphabet_is_printable_ascii_and_74_vietnamese_letters() {
    let probe = MasterColumn::tcvn3("C", 2);
    let accepted: Vec<char> = (0u32..0x1F00)
        .filter_map(char::from_u32)
        .filter(|ch| probe.problem(&format!("{ch}x")).is_none())
        .collect();
    assert_eq!(accepted.len(), 95 + 74);
    let capitals = (0u32..0x1F00)
        .filter_map(char::from_u32)
        .filter(|ch| *ch > '~' && encode_tcvn3(&ch.to_string()).is_some())
        .filter(|ch| {
            MasterColumn::tcvn3("C", 1)
                .problem(&ch.to_string())
                .is_some()
        })
        .count();
    assert_eq!(capitals, 60);
}

#[test]
#[should_panic(expected = "max must not be below min")]
fn int_bounds_are_guarded() {
    let _ = MasterColumn::int("Qty", 2, 1);
}

#[test]
#[should_panic(expected = "a text column holds 1 to 4000 characters")]
fn tcvn3_length_is_guarded() {
    let _ = MasterColumn::tcvn3("Name", 4001);
}

#[test]
#[should_panic(expected = "a text column holds 1 to 4000 characters")]
fn unicode_length_is_guarded() {
    let _ = MasterColumn::unicode("UName", 0);
}

#[test]
fn every_listed_entity_writes_its_columns_the_way_their_kind_needs() {
    for entity in ENTITIES {
        for column in entity.writable {
            assert_eq!(
                column.sql_problem(entity.update_sql),
                None,
                "{}.{}",
                entity.wire,
                column.name
            );
        }
    }
}

const GOOD_SQL: &str = concat!(
    "UPDATE dbo.T SET ",
    dl_agent_pda_sync::master_tcvn3_set!("Name"),
    ", NameX = @NameX, UName = @UName WHERE Id = @Key;"
);

#[test]
fn a_tcvn3_column_is_written_only_through_the_vendor_function() {
    assert_eq!(
        GOOD_SQL,
        "UPDATE dbo.T SET Name = dbo.Unicode2TCVN(@Name), NameX = @NameX, UName = @UName WHERE Id = @Key;"
    );
    assert_eq!(NAME.sql_problem(GOOD_SQL), None);
    assert_eq!(UNAME.sql_problem(GOOD_SQL), None);
    let only = Some(
        "Name is a TCVN3 column and must be written only as dbo.Unicode2TCVN(@Name)".to_owned(),
    );
    assert_eq!(
        NAME.sql_problem("UPDATE dbo.T SET Name = @Name WHERE Id = @Key;"),
        only
    );
    assert_eq!(
        NAME.sql_problem(
            "UPDATE dbo.T SET Name = dbo.Unicode2TCVN(@Name), Alt = @Name WHERE Id = @Key;"
        ),
        only
    );
    assert_eq!(
        NAME.sql_problem("UPDATE dbo.T SET NameX = dbo.Unicode2TCVN(@NameX) WHERE Id = @Key;"),
        only
    );
    assert_eq!(
        UNAME
            .sql_problem("UPDATE dbo.T SET UName = dbo.Unicode2TCVN(@UName) WHERE Id = @Key;")
            .as_deref(),
        Some("UName is not a TCVN3 column and must not be written through dbo.Unicode2TCVN")
    );
}
