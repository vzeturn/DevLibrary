//! The existence query of the bootstrap steps (`SqlServerAdmin.ExistsAsync`): one
//! `CASE @kind` over `sys.*` per [`StepKind`](super::models::StepKind), kept beside
//! [`admin`](super::admin) so that file stays within its size budget.

/// `SqlServerAdmin.ExistsAsync`'s query (CRLF line ends, as the C# raw literal built on
/// Windows).
pub fn exists_query() -> String {
    EXISTS_SQL.replace('\n', "\r\n")
}

const EXISTS_SQL: &str = "SELECT CASE @kind
  WHEN 'Login' THEN (SELECT COUNT(*) FROM sys.server_principals WHERE name = @name)
  WHEN 'User'  THEN (SELECT COUNT(*) FROM sys.database_principals WHERE name = @name)
  WHEN 'Table' THEN (SELECT COUNT(*) FROM sys.tables WHERE name = @name AND schema_id = SCHEMA_ID('dbo'))
  WHEN 'Grants' THEN (SELECT CASE WHEN
        EXISTS (SELECT 1 FROM sys.database_role_members m
                JOIN sys.database_principals r ON r.principal_id = m.role_principal_id AND r.name = 'db_datareader'
                JOIN sys.database_principals u ON u.principal_id = m.member_principal_id AND u.name = @name)
        AND (SELECT COUNT(*) FROM sys.database_permissions p
             JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
             JOIN sys.objects o ON o.object_id = p.major_id AND o.name LIKE N'Pda[_]Sync%'
             WHERE p.class = 1 AND p.state = 'G' AND p.permission_name IN ('INSERT','UPDATE')) >= 3
        THEN 1 ELSE 0 END)
  WHEN 'CommandGrants' THEN (SELECT CASE WHEN
        (SELECT COUNT(DISTINCT p.permission_name) FROM sys.database_permissions p
           JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
          WHERE p.class = 1 AND p.state = 'G' AND p.minor_id = 0 AND p.major_id = OBJECT_ID(N'dbo.TableReservation')
            AND p.permission_name IN ('INSERT','DELETE')) = 2
        AND (SELECT COUNT(*) FROM sys.database_permissions p
           JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
           JOIN sys.columns c ON c.object_id = p.major_id AND c.column_id = p.minor_id
          WHERE p.class = 1 AND p.state = 'G' AND p.permission_name = 'UPDATE' AND p.major_id = OBJECT_ID(N'dbo.tmpTrnInfo')
            AND c.name IN (N'IsDone', N'DoneTime', N'KdsAcceptedTime', N'KdsAcceptedBy'))
          = (SELECT COUNT(*) FROM sys.columns WHERE object_id = OBJECT_ID(N'dbo.tmpTrnInfo')
              AND name IN (N'IsDone', N'DoneTime', N'KdsAcceptedTime', N'KdsAcceptedBy'))
        THEN 1 ELSE 0 END)
  WHEN 'OrderGrants' THEN (SELECT CASE WHEN
        (SELECT COUNT(*) FROM sys.database_permissions p
           JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
          WHERE p.class = 1 AND p.state = 'G' AND p.minor_id = 0 AND p.permission_name = 'EXECUTE'
            AND p.major_id IN (OBJECT_ID(N'dbo.GetRVCSeqNumber'), OBJECT_ID(N'dbo.PostITrn'),
                               OBJECT_ID(N'dbo.UpdateTmpInvoice'), OBJECT_ID(N'dbo.Unicode2TCVN'))) = 4
        AND EXISTS (SELECT 1 FROM sys.database_permissions p
           JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
          WHERE p.class = 1 AND p.state = 'G' AND p.minor_id = 0 AND p.permission_name = 'INSERT'
            AND p.major_id = OBJECT_ID(N'dbo.VoidTrn'))
        THEN 1 ELSE 0 END)
  WHEN 'OrderGrantsOw1c' THEN (SELECT CASE WHEN
        (SELECT COUNT(*) FROM sys.database_permissions p
           JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
           JOIN sys.columns c ON c.object_id = p.major_id AND c.column_id = p.minor_id
          WHERE p.class = 1 AND p.state = 'G' AND p.permission_name = 'UPDATE' AND p.major_id = OBJECT_ID(N'dbo.tmpInvoice')
            AND c.name IN (N'NGuest', N'NChild', N'LastChgCashier', N'LastChgTime')) = 4
        THEN 1 ELSE 0 END)
  WHEN 'OrderGrantsOw1e' THEN (SELECT CASE WHEN
        EXISTS (SELECT 1 FROM sys.database_permissions p
           JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
           JOIN sys.columns c ON c.object_id = p.major_id AND c.column_id = p.minor_id
          WHERE p.class = 1 AND p.state = 'G' AND p.permission_name = 'UPDATE' AND p.major_id = OBJECT_ID(N'dbo.tmpTrn')
            AND c.name = N'CheckNo')
        AND EXISTS (SELECT 1 FROM sys.database_permissions p
           JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
          WHERE p.class = 1 AND p.state = 'G' AND p.minor_id = 0 AND p.permission_name = 'DELETE'
            AND p.major_id = OBJECT_ID(N'dbo.tmpInvoice'))
        THEN 1 ELSE 0 END)
  WHEN 'SequenceGrants' THEN (SELECT CASE WHEN
        NOT EXISTS (SELECT 1 FROM sys.objects o
          WHERE o.schema_id = SCHEMA_ID(N'dbo') AND o.type IN ('SO', 'U')
            AND ((o.name LIKE N'LastCheck[0-9]%' AND SUBSTRING(o.name, 10, 128) NOT LIKE N'%[^0-9]%')
              OR (o.name LIKE N'LastTrn[0-9]%' AND SUBSTRING(o.name, 8, 128) NOT LIKE N'%[^0-9]%'))
            AND NOT EXISTS (SELECT 1 FROM sys.database_permissions p
               JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
              WHERE p.class = 1 AND p.state = 'G' AND p.minor_id = 0 AND p.permission_name = CASE o.type WHEN 'U' THEN 'INSERT' ELSE 'UPDATE' END
                AND p.major_id = o.object_id))
        THEN 1 ELSE 0 END)
  WHEN 'MasterGrants' THEN (SELECT CASE WHEN
  NOT EXISTS (SELECT 1 FROM (VALUES (N'IClassDef', N'ClsName', N'ClsName'), (N'IClassDef', N'LastChangeTime', N'LastChangeTime'),
                                    (N'PaymentMode', N'PDesc', N'PDesc'), (N'TaxClassDef', N'TaxRate', N'TaxRate'),
                                    (N'STaxDef', N'STaxRate', N'STaxRate'), (N'CurrencyDef', N'ExRate', N'ExRate'),
                                    (N'TableArea', N'TableAreaName', N'TableAreaName'),
                                    (N'TableList', N'Description', N'IsActive'), (N'TableList', N'TableArea', N'IsActive'),
                                    (N'TableList', N'IsActive', N'IsActive'), (N'TableList', N'LastChangedBy', N'IsActive'),
                                    (N'TableList', N'LastChangedTime', N'IsActive'), (N'Para', N'ParaStr', N'ParaStr'),
                                    (N'ItemDef', N'IName', N'LastChangeBy'), (N'ItemDef', N'Price', N'LastChangeBy'),
                                    (N'ItemDef', N'IGroup', N'LastChangeBy'), (N'ItemDef', N'IClass', N'LastChangeBy'),
                                    (N'ItemDef', N'IActive', N'LastChangeBy'), (N'ItemDef', N'OpenPrice', N'LastChangeBy'),
                                    (N'ItemDef', N'isPkg', N'LastChangeBy'),
                                    (N'ItemDef', N'LastChangeBy', N'LastChangeBy'), (N'ItemDef', N'LastChangeTime', N'LastChangeBy'),
                                    (N'IGroupDef', N'GName', N'GName'), (N'ItemMenu', N'MenuName', N'MenuOrder'),
                                    (N'ItemMenu', N'MenuActive', N'MenuOrder'), (N'ItemMenu', N'MenuOrder', N'MenuOrder'),
                                    (N'RVCItemMenu', N'ItemShow', N'ItemShow'), (N'DiscountCode', N'DscName', N'VoidedAt'),
                                    (N'DiscountCode', N'DscByAmount', N'VoidedAt'), (N'DiscountCode', N'DscAmount', N'VoidedAt'),
                                    (N'DiscountCode', N'isOpen', N'VoidedAt'), (N'DiscountCode', N'IsVoided', N'VoidedAt'),
                                    (N'DiscountCode', N'VoidedBy', N'VoidedAt'), (N'DiscountCode', N'VoidedAt', N'VoidedAt'),
                                    (N'MealPeriod', N'MealName', N'Status'), (N'MealPeriod', N'FromTime', N'Status'),
                                    (N'MealPeriod', N'ToTime', N'Status'), (N'MealPeriod', N'Status', N'Status'),
                                    (N'PkgDef', N'IQty', N'DisplayOrder'), (N'PkgDef', N'IPrice', N'DisplayOrder'),
                                    (N'PkgDef', N'UseItemPrice', N'DisplayOrder'), (N'PkgDef', N'DisplayOrder', N'DisplayOrder')) w(t, col, guard)
    WHERE COL_LENGTH(N'dbo.' + w.t, w.guard) IS NOT NULL
      AND NOT EXISTS (SELECT 1 FROM sys.database_permissions p
         JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
         JOIN sys.columns c ON c.object_id = p.major_id AND c.column_id = p.minor_id
        WHERE p.class = 1 AND p.state = 'G' AND p.permission_name = 'UPDATE' AND p.major_id = OBJECT_ID(N'dbo.' + w.t)
          AND c.name = w.col))
  AND NOT EXISTS (SELECT 1 FROM (VALUES (N'TableArea', N'TableAreaName'), (N'TableList', N'IsActive'), (N'Para', N'ParaStr'),
                                    (N'ItemDef', N'LastChangeBy'), (N'IGroupDef', N'GName'), (N'ItemMenu', N'MenuOrder'),
                                    (N'RVCItemMenu', N'ItemShow'), (N'DiscountCode', N'VoidedAt'), (N'MealPeriod', N'Status'),
                                    (N'PkgDef', N'DisplayOrder')) i(t, guard)
    WHERE COL_LENGTH(N'dbo.' + i.t, i.guard) IS NOT NULL
      AND NOT EXISTS (SELECT 1 FROM sys.database_permissions p
         JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
        WHERE p.class = 1 AND p.state = 'G' AND p.minor_id = 0 AND p.permission_name = 'INSERT'
          AND p.major_id = OBJECT_ID(N'dbo.' + i.t)))
  THEN 1 ELSE 0 END)
  WHEN 'CashierMasterGrants' THEN (SELECT CASE WHEN
        (OBJECT_ID(N'dbo.CashierDef') IS NULL OR (EXISTS (SELECT 1 FROM sys.database_permissions p
           JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
          WHERE p.class = 1 AND p.state = 'G' AND p.minor_id = 0 AND p.permission_name = 'INSERT'
            AND p.major_id = OBJECT_ID(N'dbo.CashierDef'))
          AND EXISTS (SELECT 1 FROM sys.database_permissions p
           JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
           JOIN sys.columns c ON c.object_id = p.major_id AND c.column_id = p.minor_id
          WHERE p.class = 1 AND p.state = 'G' AND p.permission_name = 'UPDATE'
            AND p.major_id = OBJECT_ID(N'dbo.CashierDef') AND c.name = N'TouchID')))
        AND (OBJECT_ID(N'dbo.CashierRVC') IS NULL OR EXISTS (SELECT 1 FROM sys.database_permissions p
           JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
          WHERE p.class = 1 AND p.state = 'G' AND p.minor_id = 0 AND p.permission_name = 'DELETE'
            AND p.major_id = OBJECT_ID(N'dbo.CashierRVC')))
        AND (OBJECT_ID(N'dbo.ClerkPermission') IS NULL OR EXISTS (SELECT 1 FROM sys.database_permissions p
           JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
           JOIN sys.columns c ON c.object_id = p.major_id AND c.column_id = p.minor_id
          WHERE p.class = 1 AND p.state = 'G' AND p.permission_name = 'UPDATE'
            AND p.major_id = OBJECT_ID(N'dbo.ClerkPermission') AND c.name = N'Allowed'))
        THEN 1 ELSE 0 END)
  WHEN 'TxnApplied' THEN (SELECT CASE WHEN
        OBJECT_ID(N'dbo.Pda_TxnApplied', N'U') IS NOT NULL
        AND (SELECT COUNT(DISTINCT p.permission_name) FROM sys.database_permissions p
           JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = N'pda_sync'
          WHERE p.class = 1 AND p.state = 'G' AND p.minor_id = 0 AND p.major_id = OBJECT_ID(N'dbo.Pda_TxnApplied')
            AND p.permission_name IN ('SELECT','INSERT','UPDATE')) = 3
        THEN 1 ELSE 0 END)
  WHEN 'TxnGrants' THEN (SELECT CASE WHEN
        NOT EXISTS (SELECT 1 FROM (VALUES (N'dbo.TmpInvoice'), (N'dbo.TmpTrn'), (N'dbo.TmpPayment'), (N'dbo.Invoice'),
                                          (N'dbo.Trn'), (N'dbo.Payment')) t(name)
          CROSS JOIN (VALUES ('INSERT'), ('DELETE')) k(perm)
          WHERE OBJECT_ID(t.name, N'U') IS NOT NULL
            AND NOT EXISTS (SELECT 1 FROM sys.database_permissions p
               JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
              WHERE p.class = 1 AND p.state = 'G' AND p.minor_id = 0 AND p.permission_name = k.perm
                AND p.major_id = OBJECT_ID(t.name, N'U')))
        THEN 1 ELSE 0 END)
  WHEN 'TxnStoreKey' THEN (SELECT CASE WHEN
        OBJECT_ID(N'dbo.Pda_TxnApplied', N'U') IS NULL
        OR COL_LENGTH(N'dbo.Pda_TxnApplied', N'StoreKey') IS NOT NULL
        THEN 1 ELSE 0 END)
  WHEN 'TxnShiftGrants' THEN (SELECT CASE WHEN
        (OBJECT_ID(N'dbo.CashierShift', N'U') IS NULL OR (
          EXISTS (SELECT 1 FROM sys.database_permissions p
             JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
            WHERE p.class = 1 AND p.state = 'G' AND p.minor_id = 0 AND p.permission_name = 'INSERT'
              AND p.major_id = OBJECT_ID(N'dbo.CashierShift', N'U'))
          AND EXISTS (SELECT 1 FROM sys.database_permissions p
             JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
             JOIN sys.columns c ON c.object_id = p.major_id AND c.column_id = p.minor_id
            WHERE p.class = 1 AND p.state = 'G' AND p.permission_name = 'UPDATE'
              AND p.major_id = OBJECT_ID(N'dbo.CashierShift', N'U') AND c.name = N'CashEnd')))
        AND (OBJECT_ID(N'dbo.ShiftMoney', N'U') IS NULL OR (SELECT COUNT(DISTINCT p.permission_name) FROM sys.database_permissions p
             JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
            WHERE p.class = 1 AND p.state = 'G' AND p.minor_id = 0 AND p.permission_name IN ('INSERT','DELETE')
              AND p.major_id = OBJECT_ID(N'dbo.ShiftMoney', N'U')) = 2)
        AND (OBJECT_ID(N'dbo.DailyCashLog', N'U') IS NULL OR EXISTS (SELECT 1 FROM sys.database_permissions p
             JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
            WHERE p.class = 1 AND p.state = 'G' AND p.minor_id = 0 AND p.permission_name = 'INSERT'
              AND p.major_id = OBJECT_ID(N'dbo.DailyCashLog', N'U')))
        THEN 1 ELSE 0 END)
  WHEN 'PoNumberGrants' THEN (SELECT CASE WHEN
        COL_LENGTH(N'dbo.tmpInvoice', N'PONumber') IS NULL OR EXISTS (SELECT 1 FROM sys.database_permissions p
           JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
           JOIN sys.columns c ON c.object_id = p.major_id AND c.column_id = p.minor_id
          WHERE p.class = 1 AND p.state = 'G' AND p.permission_name = 'UPDATE' AND p.major_id = OBJECT_ID(N'dbo.tmpInvoice')
            AND c.name = N'PONumber')
        THEN 1 ELSE 0 END)
  WHEN 'FullRwGrants' THEN (SELECT CASE WHEN
        IS_ROLEMEMBER(N'db_datareader', @name) = 1 AND IS_ROLEMEMBER(N'db_datawriter', @name) = 1
        AND EXISTS (SELECT 1 FROM sys.database_permissions p
           JOIN sys.database_principals u ON u.principal_id = p.grantee_principal_id AND u.name = @name
          WHERE p.class = 0 AND p.state = 'G' AND p.permission_name = 'EXECUTE')
        THEN 1 ELSE 0 END)
  ELSE 0 END";
