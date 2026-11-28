//! The master entities the agent writes (`MasterEntities` and `MasterEntities.Outlet.cs` of
//! the .NET agent, ADR 0147, 0151, 0152): the same statements, byte for byte. The groups of
//! 0151 are update-only; the outlet entities of 0152 also create.

use super::column::MasterColumn;
use super::combo::COMBO_LINE;
use super::key::{MasterKey, MasterKeyColumn};
use super::menu::{DISCOUNT_CODE, ITEM, ITEM_GROUP, MEAL_PERIOD, MENU_CATEGORY, MENU_ITEM};
use super::{MasterEntity, outlet_params};

/// `item_class` -> `dbo.IClassDef` (key `ClsCode`, writable `ClsName`).
pub const ITEM_CLASS: MasterEntity = MasterEntity {
    wire: "item_class",
    table: "IClassDef",
    key: &[MasterKeyColumn::int("ClsCode")],
    writable: &[MasterColumn::text("ClsName", 50)],
    update_sql: "UPDATE dbo.IClassDef SET ClsName = @ClsName, LastChangeTime = GETDATE() WHERE ClsCode = @Key;",
    guard_column: None,
    insert_sql: None,
    is_protected: None,
    keeps_missing: false,
};

/// `payment_mode` -> `dbo.PaymentMode`: only the display name `PDesc` (the code `PMode` is
/// what posted lines reference; `TrnCode`/`PType` steer posting).
pub const PAYMENT_MODE: MasterEntity = MasterEntity {
    wire: "payment_mode",
    table: "PaymentMode",
    key: &[MasterKeyColumn::int("PNo")],
    writable: &[MasterColumn::text("PDesc", 60)],
    update_sql: "UPDATE dbo.PaymentMode SET PDesc = @PDesc WHERE PNo = @Key;",
    ..ITEM_CLASS
};

/// `tax_class` -> `dbo.TaxClassDef`: the rate in percent.
pub const TAX_CLASS: MasterEntity = MasterEntity {
    wire: "tax_class",
    table: "TaxClassDef",
    key: &[MasterKeyColumn::int("TaxCode")],
    writable: &[MasterColumn::numeric("TaxRate", 4, 100)],
    update_sql: "UPDATE dbo.TaxClassDef SET TaxRate = @TaxRate WHERE TaxCode = @Key;",
    ..ITEM_CLASS
};

/// `special_tax` -> `dbo.STaxDef`: the rate in percent.
pub const SPECIAL_TAX: MasterEntity = MasterEntity {
    wire: "special_tax",
    table: "STaxDef",
    key: &[MasterKeyColumn::int("STaxCode")],
    writable: &[MasterColumn::numeric("STaxRate", 4, 100)],
    update_sql: "UPDATE dbo.STaxDef SET STaxRate = @STaxRate WHERE STaxCode = @Key;",
    ..ITEM_CLASS
};

/// `currency` -> `dbo.CurrencyDef`: the exchange rate only, never the base currency. Key
/// text `rvc:CrCode`.
pub const CURRENCY: MasterEntity = MasterEntity {
    wire: "currency",
    table: "CurrencyDef",
    key: &[
        MasterKeyColumn::outlet_null_as_zero("RVC"),
        MasterKeyColumn::int("CrCode"),
    ],
    writable: &[MasterColumn::numeric("ExRate", 6, 1_000_000_000)],
    update_sql: "UPDATE dbo.CurrencyDef SET ExRate = @ExRate WHERE ISNULL(RVC, 0) = @Rvc AND CrCode = @Key;",
    guard_column: Some("IsBase"),
    ..ITEM_CLASS
};

/// `table_area` -> `dbo.TableArea`: the area name (nvarchar, Unicode). A new area has no
/// picture and sorts with the defaults of the POS (0).
pub const TABLE_AREA: MasterEntity = MasterEntity {
    wire: "table_area",
    table: "TableArea",
    key: &[
        MasterKeyColumn::outlet("RVC"),
        MasterKeyColumn::int("TableArea"),
    ],
    writable: &[MasterColumn::unicode("TableAreaName", 50)],
    update_sql: "UPDATE dbo.TableArea SET TableAreaName = @TableAreaName WHERE RVC = @Rvc AND TableArea = @Key;",
    insert_sql: Some(
        "INSERT INTO dbo.TableArea (TableArea, TableAreaName, RVC, OrderInRed, SectionID, HHExclude) VALUES (@Key, @TableAreaName, @Rvc, 0, 0, 0);",
    ),
    ..ITEM_CLASS
};

/// `table` -> `dbo.TableList`: table or bed, description, area and active flag, edited one
/// field at a time (`keeps_missing`). A new table is free (`TableStatus = 0`), in no merge
/// group, and takes `TableType`/`ShowBookBeforeMin` from a table of the store (same outlet
/// first, then same area); a store with no table at all refuses the create.
pub const TABLE: MasterEntity = MasterEntity {
    wire: "table",
    table: "TableList",
    key: &[
        MasterKeyColumn::outlet("RVC"),
        MasterKeyColumn::str("TableNum", 20),
    ],
    writable: &[
        MasterColumn::text("Description", 60),
        MasterColumn::int("TableArea", 0, i32::MAX),
        MasterColumn::boolean("IsActive"),
    ],
    update_sql: concat!(
        "UPDATE dbo.TableList SET Description = COALESCE(@Description, Description), TableArea = COALESCE(@TableArea, TableArea), ",
        "IsActive = COALESCE(@IsActive, IsActive), LastChangedBy = 'CLOUD', LastChangedTime = GETDATE() WHERE RVC = @Rvc AND TableNum = @Key;"
    ),
    insert_sql: Some(concat!(
        "INSERT INTO dbo.TableList (TableNum, TableType, TableArea, Description, RVC, TableStatus, LastChangedBy, LastChangedTime, GrpOrder, ",
        "ShowBookBeforeMin, IsActive) SELECT TOP (1) @Key, t.TableType, @TableArea, @Description, @Rvc, 0, 'CLOUD', GETDATE(), 0, t.ShowBookBeforeMin, ",
        "@IsActive FROM dbo.TableList t ORDER BY CASE WHEN t.RVC = @Rvc THEN 0 ELSE 1 END, CASE WHEN t.TableArea = @TableArea THEN 0 ELSE 1 END, ",
        "t.RVC, t.TableNum;"
    )),
    keeps_missing: true,
    ..ITEM_CLASS
};

/// `outlet_param` -> `dbo.Para`: the parameter value only. A new parameter copies type,
/// description and status from the same name at another outlet; a name no outlet has is
/// refused (the POS would not read it). Protected names ([`outlet_params`]) are never
/// written.
pub const OUTLET_PARAM: MasterEntity = MasterEntity {
    wire: "outlet_param",
    table: "Para",
    key: &[
        MasterKeyColumn::outlet("RVC"),
        MasterKeyColumn::str("ParaName", 50),
    ],
    writable: &[MasterColumn::text("ParaStr", 500)],
    update_sql: "UPDATE dbo.Para SET ParaStr = @ParaStr WHERE RVC = @Rvc AND ParaName = @Key;",
    insert_sql: Some(concat!(
        "INSERT INTO dbo.Para (ParaName, ParaStr, ParaType, ParaDesc, RVC, Status) SELECT TOP (1) @Key, @ParaStr, t.ParaType, t.ParaDesc, @Rvc, ",
        "t.Status FROM dbo.Para t WHERE t.ParaName = @Key ORDER BY t.RVC;"
    )),
    is_protected: Some(protected_para),
    ..ITEM_CLASS
};

/// `OutletParams.IsProtected` on the `ParaName` part.
fn protected_para(key: &MasterKey) -> bool {
    key.str(1).is_some_and(outlet_params::is_protected)
}

/// Every allow-listed entity.
pub const ENTITIES: &[&MasterEntity] = &[
    &ITEM_CLASS,
    &PAYMENT_MODE,
    &TAX_CLASS,
    &SPECIAL_TAX,
    &CURRENCY,
    &TABLE_AREA,
    &TABLE,
    &OUTLET_PARAM,
    &ITEM,
    &ITEM_GROUP,
    &MENU_CATEGORY,
    &MENU_ITEM,
    &DISCOUNT_CODE,
    &MEAL_PERIOD,
    &COMBO_LINE,
];
