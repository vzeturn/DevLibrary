//! The six menu entities of `master.upsert` (RT-0831, ADR 0156): the allow-list, the key
//! shapes and the update statements, which must be the .NET agent's byte for byte (the
//! statement is read from `MasterEntities.Menu.cs`). The real SQL is in
//! `commands_master_menu_sql.rs`.

use dl_agent_pda_sync::commands::master::{ENTITIES, find_entity};

const MENU: [(&str, &str); 6] = [
    ("item", "ItemDef"),
    ("item_group", "IGroupDef"),
    ("menu_category", "ItemMenu"),
    ("menu_item", "RVCItemMenu"),
    ("discount_code", "DiscountCode"),
    ("meal_period", "MealPeriod"),
];

#[test]
fn the_six_menu_entities_are_allow_listed() {
    for (wire, table) in MENU {
        let entity = find_entity(wire).unwrap_or_else(|| panic!("{wire} not allow-listed"));
        assert_eq!(entity.table, table);
        assert!(entity.insert_sql.is_some(), "{wire} can be created");
        assert!(ENTITIES.iter().any(|e| e.wire == wire));
    }
    assert_eq!(ENTITIES.len(), 15);
}

#[test]
fn keeps_missing_matches_the_dotnet_entities() {
    for (wire, keeps) in [
        ("item", true),
        ("item_group", false),
        ("menu_category", true),
        ("menu_item", false),
        ("discount_code", true),
        ("meal_period", true),
    ] {
        assert_eq!(find_entity(wire).unwrap().keeps_missing, keeps, "{wire}");
    }
}

#[test]
fn keys_parse_in_the_documented_text_shapes() {
    let ok = [
        ("item", "AB-1"),
        ("item_group", "12"),
        ("menu_category", "1:MENU01"),
        ("menu_item", "1:MENU01:AB-1"),
        ("discount_code", "0:DSC10"),
        ("meal_period", "2:3"),
    ];
    for (wire, key) in ok {
        let entity = find_entity(wire).unwrap();
        assert!(entity.parse_key(key).is_some(), "{wire} {key}");
    }
    let bad = [
        ("item", "AB:1"),
        ("item", "ABCDEFGHIJKLM"),
        ("item_group", "x"),
        ("menu_category", "MENU01"),
        ("menu_item", "1:MENU01"),
        ("discount_code", "DSC10"),
        ("meal_period", "2:x"),
    ];
    for (wire, key) in bad {
        let entity = find_entity(wire).unwrap();
        assert!(entity.parse_key(key).is_none(), "{wire} {key}");
    }
}

#[test]
fn the_menu_item_key_binds_menu_and_item_as_separate_parameters() {
    let entity = find_entity("menu_item").unwrap();
    let params: Vec<&str> = entity.key.iter().map(|k| k.param).collect();
    assert_eq!(params, ["Rvc", "Menu", "Key"]);
    assert!(
        entity
            .key_predicate()
            .contains("ItemMenu = @Menu AND ItemCode = @Key")
    );
}

#[test]
fn the_discount_code_outlet_reads_null_as_zero() {
    let entity = find_entity("discount_code").unwrap();
    assert_eq!(
        entity.key_predicate(),
        "ISNULL(RVC, 0) = @Rvc AND DscCode = @Key"
    );
}
