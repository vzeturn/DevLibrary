//! The combo entity of `master.upsert` (RT-0979, ADR 0160): allow-listed on `dbo.PkgDef`,
//! keyed `combo:item`, the update and insert statements the .NET agent's byte for byte (read
//! from `MasterEntities.Combo.cs`), and the item entity writing `isPkg`. The real SQL is in
//! `commands_master_combo_sql.rs`.

use dl_agent_pda_sync::commands::master::{ColumnKind, ENTITIES, find_entity};

#[test]
fn combo_line_is_allow_listed_on_pkgdef_and_can_be_created() {
    let entity = find_entity("combo_line").expect("combo_line allow-listed");
    assert_eq!(entity.table, "PkgDef");
    assert!(entity.keeps_missing);
    assert!(entity.insert_sql.is_some());
    assert_eq!(ENTITIES.last().map(|e| e.wire), Some("combo_line"));
    let names: Vec<&str> = entity.writable.iter().map(|c| c.name).collect();
    assert_eq!(names, ["IQty", "IPrice", "UseItemPrice", "DisplayOrder"]);
}

#[test]
fn the_key_binds_combo_and_item_as_separate_parameters() {
    let entity = find_entity("combo_line").unwrap();
    let params: Vec<&str> = entity.key.iter().map(|k| k.param).collect();
    assert_eq!(params, ["Pkg", "Key"]);
    assert_eq!(entity.key_predicate(), "PkgCode = @Pkg AND ItemCode = @Key");
    assert!(entity.parse_key("CB01:SV-1").is_some());
    for bad in ["CB01", "CB01:SV-1:X", "ABCDEFGHIJKLMNOPQRSTU:SV-1"] {
        assert!(entity.parse_key(bad).is_none(), "{bad}");
    }
}

#[test]
fn quantity_price_and_order_are_bounded_like_the_dotnet_columns() {
    let entity = find_entity("combo_line").unwrap();
    let col = |n: &str| entity.writable.iter().find(|c| c.name == n).unwrap();
    assert_eq!(
        col("IQty").kind,
        ColumnKind::Numeric {
            scale: 2,
            max: 9_999
        }
    );
    assert!(col("IQty").problem("1.50").is_none());
    assert!(col("IQty").problem("10000").is_some());
    assert!(col("IQty").problem("1.505").is_some());
    assert!(col("IPrice").problem("180000").is_none());
    assert!(col("IPrice").problem("1.5").is_some());
    assert!(col("DisplayOrder").problem("10000").is_some());
    assert!(col("UseItemPrice").problem("2").is_some());
}
