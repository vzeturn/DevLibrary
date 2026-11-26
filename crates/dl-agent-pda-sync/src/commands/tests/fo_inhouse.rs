//! `fo.inhouse_search` SQL (`FoInHouseSearchCommandTests`, `FoSchemaContractTests` SQL cases):
//! every constant is the C# text byte for byte, read from the C# sources.

use dl_tds::ParamValue;

use crate::commands::fo_inhouse as f;

const CS_SOURCE: &str =
    include_str!("../../../testdata/cs/Commands/FoInHouseSearchCommandHandler.cs");
const CS_ROOM_CHARGE: &str = include_str!("../../../testdata/cs/Commands/SqlTxnRoomChargeUnit.cs");

/// The text of C# raw string constant `name` (`const string Name = """ ... """;` or the
/// interpolated `$$"""` form), with the closing quotes' indentation removed from every line
/// as the compiler does, and each `{{Other}}` replaced by constant `Other` of the same file.
fn cs_raw(file: &str, name: &str) -> String {
    let src = file.replace("\r\n", "\n");
    let head = format!("const string {name} = ");
    let mut start = src.find(&head).map(|i| i + head.len()).expect(name);
    let interpolated = src[start..].starts_with("$$");
    if interpolated {
        start += 2;
    }
    assert!(
        src[start..].starts_with("\"\"\"\n"),
        "{name} is not a raw string"
    );
    let body = &src[start + 4..];
    let close = body.find("\"\"\";").expect("closing quotes");
    let last_nl = body[..close].rfind('\n').expect("closing line");
    let indent = close - last_nl - 1;
    let mut text = body[..last_nl]
        .split('\n')
        .map(|l| {
            if l.len() >= indent {
                &l[indent..]
            } else {
                l.trim_start()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    while interpolated && let Some(at) = text.find("{{") {
        let end = text[at..].find("}}").expect("}}") + at;
        let inner = cs_raw(file, &text[at + 2..end]);
        text.replace_range(at..end + 2, &inner);
    }
    text
}

/// The text of a one-line C# constant (`const string Name = "...";`, possibly wrapped).
fn cs_line(file: &str, name: &str) -> String {
    let src = file.replace("\r\n", "\n");
    let head = format!("const string {name} =");
    let start = src.find(&head).map(|i| i + head.len()).expect(name);
    let open = src[start..].find('"').expect("literal") + start + 1;
    let end = src[open..].find('"').expect("end") + open;
    src[open..end].to_owned()
}

#[test]
fn the_search_sql_is_the_csharp_text_byte_for_byte() {
    assert_eq!(f::COLUMNS, cs_raw(CS_SOURCE, "Columns"));
    assert_eq!(f::SEARCH_SQL, cs_raw(CS_SOURCE, "Sql"));
    assert_eq!(f::IN_HOUSE_SQL, cs_raw(CS_SOURCE, "SqlInHouse"));
    assert!(CS_SOURCE.contains(&format!(
        "internal const int InHouseCap = {};",
        f::IN_HOUSE_CAP
    )));
}

#[test]
fn the_database_resolution_is_the_room_charge_text_byte_for_byte() {
    assert_eq!(
        f::INTERFACE_DB_SQL,
        cs_line(CS_ROOM_CHARGE, "InterfaceDbSql")
    );
    assert_eq!(
        f::DATABASE_ACCESS_SQL,
        cs_line(CS_ROOM_CHARGE, "DatabaseAccessSql")
    );
    assert_eq!(
        f::DEFAULT_DATABASE,
        cs_line(CS_ROOM_CHARGE, "DefaultDatabase")
    );
    assert!(CS_ROOM_CHARGE.contains(r#"[GeneratedRegex("^[A-Za-z0-9_]+$")]"#));
}

#[test]
fn the_timeouts_are_the_csharp_ones() {
    assert!(CS_SOURCE.contains("private const int TimeoutSeconds = 5;"));
    assert_eq!(f::SEARCH_TIMEOUT.as_secs(), 5);
    assert!(CS_ROOM_CHARGE.contains("private const int TimeoutSeconds = 15;"));
    assert_eq!(crate::commands::sql::COMMAND_TIMEOUT.as_secs(), 15);
}

#[test]
fn the_sql_reads_in_house_folios_only_with_every_value_a_parameter() {
    let sql = f::SEARCH_SQL;
    for part in [
        "FolioStatus = 2",
        "TOP (@Limit)",
        "{0}.dbo.Folio",
        "RTRIM(RoomCode) = @Q",
        "CAST(FolioNum AS varchar(20)) = @Q",
    ] {
        assert!(sql.contains(part), "{part}");
    }
    for word in ["INSERT", "UPDATE", "DELETE", "{1}", "LIKE"] {
        assert!(!sql.contains(word), "{word}");
    }
    let all = f::IN_HOUSE_SQL;
    for part in ["FolioStatus = 2", "TOP (@Limit)", "{0}.dbo.Folio"] {
        assert!(all.contains(part), "{part}");
    }
    for word in ["INSERT", "UPDATE", "DELETE", "@Q", "LIKE"] {
        assert!(!all.contains(word), "{word}");
    }
    assert_eq!(f::IN_HOUSE_CAP, 500);
}

#[test]
fn the_contract_folio_columns_are_read_by_the_search() {
    for column in [
        "FolioNum",
        "RoomCode",
        "FirstName",
        "LastName",
        "ArrivalDate",
        "DepartureDate",
        "FolioStatus",
    ] {
        assert!(f::SEARCH_SQL.contains(column), "{column}");
        assert!(f::IN_HOUSE_SQL.contains(column), "{column}");
    }
}

#[test]
fn a_number_binds_limit_int_and_varchar_query() {
    let (sql, params) = f::search_statement("305", 7);
    assert_eq!(sql, f::SEARCH_SQL);
    let names: Vec<&str> = params.iter().map(|p| p.name()).collect();
    assert_eq!(names, ["Limit", "Q"]);
    assert_eq!(params[0].value(), &ParamValue::I32(Some(7)));
    assert_eq!(
        params[1].value(),
        &ParamValue::VarChar(Some("305".to_owned()))
    );
}

#[test]
fn a_name_reads_every_in_house_folio_up_to_the_cap() {
    let (sql, params) = f::search_statement("A12", 7);
    assert_eq!(sql, f::IN_HOUSE_SQL);
    let names: Vec<&str> = params.iter().map(|p| p.name()).collect();
    assert_eq!(names, ["Limit"]);
    assert_eq!(params[0].value(), &ParamValue::I32(Some(500)));
}
