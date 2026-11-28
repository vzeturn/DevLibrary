//! The menu the cloud writes back (`MasterEntities.Menu.cs` of the .NET agent, RT-0831,
//! ADR 0156): items (services), item groups, menu categories, the items of a category,
//! discount codes and meal periods (sale hours). Hiding is a field
//! (`IActive`, `MenuActive`, `ItemShow`, `IsVoided`); a create (ADR 0157) copies the vendor
//! columns of a template row (`master_create_no_template` without one). The statements are the .NET ones, byte for byte.
//! Entities with several writable columns keep a field the editor left out
//! (`keeps_missing`): `dbo.Unicode2TCVN(NULL)` is NULL, so `COALESCE(dbo.Unicode2TCVN(@Col),
//! Col)` keeps a TCVN3 name too. `PrintName`, stock, cost, recipe and every other `ItemDef`
//! column stay the POS's.

use super::MasterEntity;
use super::column::MasterColumn;
use super::key::MasterKeyColumn;

/// `item` -> `dbo.ItemDef`: name (TCVN3 `char(70)`), price (whole VND), group, class,
/// on-sale, open-price and combo (`isPkg`, ADR 0160) flags. A new combo still copies a plain
/// item as its template.
pub const ITEM: MasterEntity = MasterEntity {
    wire: "item",
    table: "ItemDef",
    key: &[MasterKeyColumn::str("ICode", 12)],
    writable: &[
        MasterColumn::tcvn3("IName", 70),
        MasterColumn::numeric("Price", 0, 999_999_999_999),
        MasterColumn::text("IGroup", 20),
        MasterColumn::text("IClass", 20),
        MasterColumn::boolean("IActive"),
        MasterColumn::boolean("OpenPrice"),
        MasterColumn::boolean("isPkg"),
    ],
    update_sql: concat!(
        "UPDATE dbo.ItemDef SET IName = COALESCE(dbo.Unicode2TCVN(@IName), IName), Price = COALESCE(@Price, Price), ",
        "IGroup = COALESCE(@IGroup, IGroup), IClass = COALESCE(@IClass, IClass), IActive = COALESCE(@IActive, IActive), ",
        "OpenPrice = COALESCE(@OpenPrice, OpenPrice), isPkg = COALESCE(@isPkg, isPkg), LastChangeBy = 'CLOUD', LastChangeTime = GETDATE() WHERE ICode = @Key;"
    ),
    guard_column: None,
    insert_sql: Some(concat!(
        "INSERT INTO dbo.ItemDef (ICode, IName, PrintName, IGroup, IClass, IActive, Price, OpenPrice, ICurrency, ItemType, IUnit, ",
        "Status, AllLoc, RVC, InTaxCode, isPOSItem, isBOItem, OpenItem, UseRecipe, StockInterface, isPkg, LastChangeBy, LastChangeTime) ",
        "SELECT TOP (1) @Key, dbo.Unicode2TCVN(@IName), dbo.Unicode2TCVN(@IName), @IGroup, @IClass, @IActive, @Price, @OpenPrice, t.ICurrency, ",
        "t.ItemType, t.IUnit, t.Status, t.AllLoc, t.RVC, t.InTaxCode, t.isPOSItem, t.isBOItem, 0, 0, 0, COALESCE(@isPkg, 0), 'CLOUD', GETDATE() FROM dbo.ItemDef t ",
        "WHERE t.IClass = @IClass AND ISNULL(t.isPkg, 0) = 0 AND ISNULL(t.isVoucher, 0) = 0 AND ISNULL(t.OpenItem, 0) = 0 ",
        "ORDER BY CASE WHEN t.IGroup = @IGroup THEN 0 ELSE 1 END, t.ICode;"
    )),
    is_protected: None,
    keeps_missing: true,
};

/// `item_group` -> `dbo.IGroupDef`: group name (TCVN3 `char(30)`).
pub const ITEM_GROUP: MasterEntity = MasterEntity {
    wire: "item_group",
    table: "IGroupDef",
    key: &[MasterKeyColumn::int("GCode")],
    writable: &[MasterColumn::tcvn3("GName", 30)],
    update_sql: "UPDATE dbo.IGroupDef SET GName = dbo.Unicode2TCVN(@GName) WHERE GCode = @Key;",
    keeps_missing: false,
    insert_sql: Some(concat!(
        "INSERT INTO dbo.IGroupDef (GCode, GName, RptGroup, GLevel, GParent, RVC, Status) SELECT TOP (1) @Key, dbo.Unicode2TCVN(@GName), ",
        "t.RptGroup, t.GLevel, t.GParent, t.RVC, t.Status FROM dbo.IGroupDef t ORDER BY t.GLevel, t.GCode;"
    )),
    ..ITEM
};

/// `menu_category` -> `dbo.ItemMenu`: name (TCVN3 `varchar(100)`), active flag and sort
/// order of one outlet's menu. Key text `rvc:MenuNo`.
pub const MENU_CATEGORY: MasterEntity = MasterEntity {
    wire: "menu_category",
    table: "ItemMenu",
    key: &[
        MasterKeyColumn::outlet("RVC"),
        MasterKeyColumn::str("MenuNo", 30),
    ],
    writable: &[
        MasterColumn::tcvn3("MenuName", 100),
        MasterColumn::boolean("MenuActive"),
        MasterColumn::int("MenuOrder", 0, 9999),
    ],
    update_sql: concat!(
        "UPDATE dbo.ItemMenu SET MenuName = COALESCE(dbo.Unicode2TCVN(@MenuName), MenuName), MenuActive = COALESCE(@MenuActive, MenuActive), ",
        "MenuOrder = COALESCE(@MenuOrder, MenuOrder) WHERE RVC = @Rvc AND MenuNo = @Key;"
    ),
    insert_sql: Some(concat!(
        "INSERT INTO dbo.ItemMenu (MenuNo, RVC, MenuName, MenuActive, MenuOrder, MainMenuNo, Status, TvM, WebActive, HasSubMenu, PNo, ",
        "ShowWPrice) SELECT TOP (1) @Key, @Rvc, dbo.Unicode2TCVN(@MenuName), @MenuActive, @MenuOrder, t.MainMenuNo, t.Status, t.TvM, t.WebActive, 0, ",
        "t.PNo, t.ShowWPrice FROM dbo.ItemMenu t ORDER BY CASE WHEN t.RVC = @Rvc THEN 0 ELSE 1 END, ",
        "CASE WHEN t.ParentMenu IS NULL OR t.ParentMenu = '' THEN 0 ELSE 1 END, t.RVC, t.MenuNo;"
    )),
    ..ITEM
};

/// `menu_item` -> `dbo.RVCItemMenu`: whether an item shows in a category of an outlet.
/// Key text `rvc:menu:item`.
pub const MENU_ITEM: MasterEntity = MasterEntity {
    wire: "menu_item",
    table: "RVCItemMenu",
    key: &[
        MasterKeyColumn::outlet("RVC"),
        MasterKeyColumn::str("ItemMenu", 30).named("Menu"),
        MasterKeyColumn::str("ItemCode", 30),
    ],
    writable: &[MasterColumn::boolean("ItemShow")],
    update_sql: "UPDATE dbo.RVCItemMenu SET ItemShow = @ItemShow WHERE RVC = @Rvc AND ItemMenu = @Menu AND ItemCode = @Key;",
    keeps_missing: false,
    insert_sql: Some(concat!(
        "INSERT INTO dbo.RVCItemMenu (RVC, ItemCode, ItemMenu, ItemShow, Status) SELECT @Rvc, @Key, @Menu, @ItemShow, ",
        "COALESCE((SELECT TOP (1) s.Status FROM dbo.RVCItemMenu s WHERE s.RVC = @Rvc ORDER BY s.ItemMenu, s.ItemCode), 0) ",
        "WHERE EXISTS (SELECT 1 FROM dbo.ItemMenu m WHERE m.RVC = @Rvc AND m.MenuNo = @Menu) AND EXISTS (SELECT 1 FROM dbo.ItemDef i WHERE i.ICode = @Key);"
    )),
    ..ITEM
};

/// `discount_code` -> `dbo.DiscountCode`: name (ASCII, capture reads it without the TCVN3
/// decode), by-amount flag, amount (`numeric(18,2)`), open flag and void. Voiding stamps
/// `VoidedBy`/`VoidedAt` unless already stamped; restoring clears them. `RVC` may be NULL
/// (= 0).
pub const DISCOUNT_CODE: MasterEntity = MasterEntity {
    wire: "discount_code",
    table: "DiscountCode",
    key: &[
        MasterKeyColumn::outlet_null_as_zero("RVC"),
        MasterKeyColumn::str("DscCode", 20),
    ],
    writable: &[
        MasterColumn::text("DscName", 100),
        MasterColumn::boolean("DscByAmount"),
        MasterColumn::numeric("DscAmount", 2, 999_999_999_999),
        MasterColumn::boolean("isOpen"),
        MasterColumn::boolean("IsVoided"),
    ],
    update_sql: concat!(
        "UPDATE dbo.DiscountCode SET DscName = COALESCE(@DscName, DscName), DscByAmount = COALESCE(@DscByAmount, DscByAmount), ",
        "DscAmount = COALESCE(@DscAmount, DscAmount), isOpen = COALESCE(@isOpen, isOpen), ",
        "VoidedBy = CASE @IsVoided WHEN 1 THEN COALESCE(VoidedBy, N'CLOUD') WHEN 0 THEN NULL ELSE VoidedBy END, ",
        "VoidedAt = CASE @IsVoided WHEN 1 THEN COALESCE(VoidedAt, SYSDATETIME()) WHEN 0 THEN NULL ELSE VoidedAt END, ",
        "IsVoided = COALESCE(@IsVoided, IsVoided) WHERE ISNULL(RVC, 0) = @Rvc AND DscCode = @Key;"
    ),
    insert_sql: Some(concat!(
        "INSERT INTO dbo.DiscountCode (RVC, DscCode, DscName, DscByAmount, DscAmount, isOpen, IsVoided, VoidedBy, VoidedAt, MbrDefault, isAuto, ",
        "isVoucher, DscOnTotal, DscPriority, RequireStaff) SELECT TOP (1) NULLIF(@Rvc, 0), @Key, @DscName, @DscByAmount, @DscAmount, @isOpen, @IsVoided, ",
        "CASE @IsVoided WHEN 1 THEN N'CLOUD' END, CASE @IsVoided WHEN 1 THEN SYSDATETIME() END, 0, 0, 0, t.DscOnTotal, t.DscPriority, t.RequireStaff ",
        "FROM dbo.DiscountCode t ORDER BY CASE WHEN ISNULL(t.RVC, 0) = @Rvc THEN 0 ELSE 1 END, ISNULL(t.isAuto, 0), t.DscCode;"
    )),
    ..ITEM
};

/// `meal_period` -> `dbo.MealPeriod`: sale hours, name (ASCII `varchar(16)`), from/to as
/// the vendor's integer `HHMM`, and status.
pub const MEAL_PERIOD: MasterEntity = MasterEntity {
    wire: "meal_period",
    table: "MealPeriod",
    key: &[
        MasterKeyColumn::outlet("RVCNo"),
        MasterKeyColumn::int("MealNo"),
    ],
    writable: &[
        MasterColumn::text("MealName", 16),
        MasterColumn::int("FromTime", 0, 2359),
        MasterColumn::int("ToTime", 0, 2359),
        MasterColumn::int("Status", 0, 9),
    ],
    update_sql: concat!(
        "UPDATE dbo.MealPeriod SET MealName = COALESCE(@MealName, MealName), FromTime = COALESCE(@FromTime, FromTime), ",
        "ToTime = COALESCE(@ToTime, ToTime), Status = COALESCE(@Status, Status) WHERE RVCNo = @Rvc AND MealNo = @Key;"
    ),
    insert_sql: Some(concat!(
        "INSERT INTO dbo.MealPeriod (RVCNo, MealNo, MealName, FromTime, ToTime, Status, AutoChange) VALUES (@Rvc, @Key, @MealName, @FromTime, ",
        "@ToTime, @Status, COALESCE((SELECT TOP (1) t.AutoChange FROM dbo.MealPeriod t WHERE t.RVCNo = @Rvc ORDER BY t.MealNo), 0));"
    )),
    ..ITEM
};
