//! Combo components the cloud writes back (`MasterEntities.Combo.cs` of the .NET agent,
//! RT-0979, ADR 0160). A combo is an `ItemDef` row with `isPkg = 1` (the item entity writes
//! that flag); its components are `dbo.PkgDef` rows. The POS's `PostItem` posts one line per
//! component at `CASE WHEN UseItemPrice = 1 THEN ItemDef.Price ELSE IPrice END` x (`IQty`,
//! 0 read as 1) in `DisplayOrder`, so the cloud writes exactly those four columns.
//! `PkgDef` is a heap with every column nullable; the logical key is `PkgCode:ItemCode`. A
//! create inserts only when the combo code is an item marked `isPkg = 1` and the component is
//! another existing item (`master_create_no_template` otherwise). Removing a component is out
//! of scope (no DELETE grant). The statements are the .NET ones, byte for byte.

use super::MasterEntity;
use super::column::MasterColumn;
use super::key::MasterKeyColumn;

/// `combo_line` -> `dbo.PkgDef`: quantity, own price (whole VND), use-item-price flag and
/// display order of one component. Key text `combo:item`.
pub const COMBO_LINE: MasterEntity = MasterEntity {
    wire: "combo_line",
    table: "PkgDef",
    key: &[
        MasterKeyColumn::str("PkgCode", 20).named("Pkg"),
        MasterKeyColumn::str("ItemCode", 20),
    ],
    writable: &[
        MasterColumn::numeric("IQty", 2, 9_999),
        MasterColumn::numeric("IPrice", 0, 999_999_999_999),
        MasterColumn::boolean("UseItemPrice"),
        MasterColumn::int("DisplayOrder", 0, 9999),
    ],
    update_sql: concat!(
        "UPDATE dbo.PkgDef SET IQty = COALESCE(@IQty, IQty), IPrice = COALESCE(@IPrice, IPrice), UseItemPrice = COALESCE(@UseItemPrice, UseItemPrice), ",
        "DisplayOrder = COALESCE(@DisplayOrder, DisplayOrder) WHERE PkgCode = @Pkg AND ItemCode = @Key;"
    ),
    guard_column: None,
    insert_sql: Some(concat!(
        "INSERT INTO dbo.PkgDef (PkgCode, ItemCode, IQty, IPrice, UseItemPrice, DisplayOrder) SELECT @Pkg, @Key, @IQty, @IPrice, @UseItemPrice, ",
        "@DisplayOrder WHERE @Pkg <> @Key AND EXISTS (SELECT 1 FROM dbo.ItemDef p WHERE p.ICode = @Pkg AND ISNULL(p.isPkg, 0) = 1) ",
        "AND EXISTS (SELECT 1 FROM dbo.ItemDef i WHERE i.ICode = @Key);"
    )),
    is_protected: None,
    keeps_missing: true,
};
