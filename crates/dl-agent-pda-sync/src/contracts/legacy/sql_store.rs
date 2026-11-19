//! The SQL Server tables of the builds before `pda.db` and the `pda.db` tables they
//! become (`Legacy/LegacySqlStore.cs`, ADR 0094). Pure data.

use std::sync::OnceLock;

use crate::contracts::map::OrderedMap;
use crate::util::stj::eq_ignore_case;

/// How one SQL Server value of an old build is written into a bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LegacySqlKind {
    /// As it is.
    Raw,
    /// Text without trailing spaces.
    Trim,
    /// A bit as 0 / 1.
    Flag,
    /// A UTC date and time: `yyyy-MM-ddTHH:mm:ssZ`.
    Stamp,
    /// A server-local date and time converted to UTC, then as [`LegacySqlKind::Stamp`].
    LocalStamp,
    /// A calendar date: `yyyy-MM-dd`.
    Date,
    /// Money without decimals, as text.
    Fixed0,
    /// Money with two decimals, as text.
    Fixed2,
    /// A decimal as invariant text, every stored digit kept.
    Number,
    /// A whole number stored as decimal, the fraction dropped.
    Truncated,
    /// A number kept as text.
    Text,
    /// Bytes.
    Blob,
}

impl LegacySqlKind {
    /// `Enum.Parse<LegacySqlKind>(name, ignoreCase: true)`.
    pub fn parse(name: &str) -> Option<LegacySqlKind> {
        use LegacySqlKind::*;
        [
            ("Raw", Raw),
            ("Trim", Trim),
            ("Flag", Flag),
            ("Stamp", Stamp),
            ("LocalStamp", LocalStamp),
            ("Date", Date),
            ("Fixed0", Fixed0),
            ("Fixed2", Fixed2),
            ("Number", Number),
            ("Truncated", Truncated),
            ("Text", Text),
            ("Blob", Blob),
        ]
        .into_iter()
        .find(|(n, _)| eq_ignore_case(n, name.trim()))
        .map(|(_, k)| k)
    }
}

/// A fixed value written for a SQL NULL, an absent column or a constant (C# `object`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyValue {
    /// A string.
    Str(&'static str),
    /// An integer.
    Int(i64),
}

/// One SQL Server column and the `pda.db` column it becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacySqlColumn {
    /// SQL Server column.
    pub source: String,
    /// `pda.db` column.
    pub target: String,
    /// Conversion.
    pub kind: LegacySqlKind,
    /// Written instead of a SQL NULL.
    pub if_null: Option<LegacyValue>,
    /// Written when the build lacks the column (`None`: the row does not carry it).
    pub if_absent: Option<LegacyValue>,
}

/// One SQL Server table of an old build and the `pda.db` table it is exported as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacySqlTable {
    /// SQL Server table (unqualified).
    pub source: String,
    /// `pda.db` table.
    pub target: String,
    /// Columns.
    pub columns: Vec<LegacySqlColumn>,
    /// Columns the on-prem move added with a fixed value.
    pub constants: OrderedMap<LegacyValue>,
}

/// The `pda.db` table an old `Pda_PrintProfile` without `TemplateId` is split into.
pub const PRINT_TEMPLATE_TARGET: &str = "pda_print_template";
/// The SQL Server table whose oldest shape carries its own header, footer and layout.
pub const PRINT_PROFILE_SOURCE: &str = "Pda_PrintProfile";

/// `RVCNo` -> `rvc_no`, `CGGroupCode` -> `cg_group_code`: the `pda.db` naming.
pub fn snake(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            continue;
        }
        let starts_word = i > 0
            && c.is_uppercase()
            && (!chars[i - 1].is_uppercase() || chars.get(i + 1).is_some_and(|n| n.is_lowercase()));
        if starts_word && !out.is_empty() {
            out.push('_');
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// A column spec: `Source[>target][:kind]`, optionally with a null/absent default.
enum Spec {
    S(&'static str),
    IfNull(&'static str, LegacyValue),
    IfAbsent(&'static str, LegacyValue),
}

use LegacyValue::{Int, Str};
use Spec::{IfAbsent, IfNull, S};

fn column(spec: &str) -> LegacySqlColumn {
    let (name, kind) = match spec.split_once(':') {
        Some((n, k)) => (n, LegacySqlKind::parse(k).unwrap_or(LegacySqlKind::Raw)),
        None => (spec, LegacySqlKind::Raw),
    };
    let (source, target) = match name.split_once('>') {
        Some((s, t)) => (s.to_owned(), t.to_owned()),
        None => (name.to_owned(), snake(name)),
    };
    LegacySqlColumn {
        source,
        target,
        kind,
        if_null: None,
        if_absent: None,
    }
}

fn named(
    source: &str,
    target: &str,
    constants: &[(&str, LegacyValue)],
    cols: Vec<Spec>,
) -> LegacySqlTable {
    let columns = cols
        .into_iter()
        .map(|s| match s {
            S(x) => column(x),
            IfNull(x, v) => LegacySqlColumn {
                if_null: Some(v),
                ..column(x)
            },
            IfAbsent(x, v) => LegacySqlColumn {
                if_absent: Some(v),
                ..column(x)
            },
        })
        .collect();
    LegacySqlTable {
        source: source.to_owned(),
        target: target.to_owned(),
        columns,
        constants: constants.iter().map(|(k, v)| (*k, v.clone())).collect(),
    }
}

fn t(source: &str, cols: Vec<Spec>) -> LegacySqlTable {
    named(source, &snake(source), &[], cols)
}

macro_rules! cols {
    ($($e:expr),* $(,)?) => { vec![$(Spec::from($e)),*] };
}

impl From<&'static str> for Spec {
    fn from(s: &'static str) -> Spec {
        S(s)
    }
}

fn build_tables() -> Vec<LegacySqlTable> {
    vec![
        // Imported by the cloud.
        t(
            "Pda_Config",
            cols![
                "RVCNo",
                "Key:trim",
                "Value",
                "LastChangeBy",
                "LastChangeTime:stamp"
            ],
        ),
        t(
            "Pda_Branding",
            cols![
                "RVCNo",
                "Name",
                "SubName",
                "Tagline",
                "PromoFooter",
                "AccentHex",
                "LogoSvg",
                "UpdatedAt:stamp"
            ],
        ),
        t(
            "Pda_DietAllergenCatalog",
            cols![
                "RVCNo",
                "Kind:trim",
                "Code:trim",
                "LabelVi",
                "LabelEn",
                "Icon:trim",
                "IsActive:flag",
                "SortOrder",
                "LastChangeBy:trim",
                "LastChangeTime:localstamp"
            ],
        ),
        t(
            "Pda_ItemMeta",
            cols![
                "RvcNo",
                "ItemCode:trim",
                IfNull("Allergens", Str("")),
                IfNull("DietaryTags", Str("")),
                IfNull("SpiceLevel", Int(0)),
                IfNull("ShowOptions:flag", Int(0)),
                "DescEn",
                "DescKo",
                "DescRu",
                "LastChangeBy",
                "LastChangeTime:stamp"
            ],
        ),
        t(
            "Pda_ItemRecommend",
            cols![
                "RvcNo",
                "MenuNo",
                "ItemCode:trim",
                "TagType:trim",
                "SortOrder",
                "IsActive:flag",
                "LastChangeBy:trim",
                "LastChangeTime:localstamp"
            ],
        ),
        t(
            "Pda_OptionGroup",
            cols![
                "RVCNo",
                "GroupCode",
                "GroupName",
                "IsActive:flag",
                "SortOrder",
                "LastChangeBy",
                "LastChangeTime:stamp"
            ],
        ),
        t(
            "Pda_OptionGroupItem",
            cols![
                "RVCNo",
                "GroupCode",
                "OptionId",
                "OptionName",
                "PriceDelta:fixed2",
                "IsActive:flag",
                "SortOrder"
            ],
        ),
        t(
            "Pda_OptionProfile",
            cols![
                "RVCNo",
                "ProfileId",
                "ProfileName",
                "IsActive:flag",
                "LastChangeBy",
                "LastChangeTime:stamp"
            ],
        ),
        t(
            "Pda_OptionProfileGroup",
            cols![
                "RVCNo",
                "ProfileId",
                "CGGroupCode",
                "IsRequired:flag",
                "MinSelect",
                "MaxSelect",
                "IsSingleChoice:flag",
                "PresetOptionIds",
                "SortOrder",
                "GroupCode"
            ],
        ),
        t(
            "Pda_ItemOptionAssignment",
            cols![
                "AssignmentId",
                "RVCNo",
                "ItemCode",
                "ProfileId",
                "CGGroupCode",
                "Mode",
                "IsRequired:flag",
                "MinSelect",
                "MaxSelect",
                "IsSingleChoice:flag",
                "PresetOptionIds",
                "SortOrder",
                "GroupCode",
                "LastChangeBy",
                "LastChangeTime:stamp"
            ],
        ),
        t(
            "Pda_ModifierGroupMeta",
            cols![
                "RVCNo",
                "CGGroupCode",
                "IsRequired:flag",
                "MinSelect",
                "MaxSelect",
                "IsSingleChoice:flag",
                "PresetOptionIds",
                "LastChangeBy",
                "LastChangeTime:stamp"
            ],
        ),
        t(
            "Pda_Printer",
            cols![
                "PrinterId",
                "RVC",
                "Name:trim",
                "Transport:trim",
                "Address:trim",
                "PaperWidth",
                "Encoding:trim",
                "Copies",
                "CutPaper:flag",
                "KickDrawer:flag",
                "AgentKey:trim",
                "IsActive:flag",
                "CreatedAt:stamp"
            ],
        ),
        t(
            "Pda_KitchenStation",
            cols![
                "StationId",
                "RVC",
                "Name:trim",
                "PrinterId",
                "SortOrder",
                "IsActive:flag",
                "UpdatedAt:stamp"
            ],
        ),
        t(
            "Pda_ClassStation",
            cols!["RVC", "ClassCode:trim", "StationId", "UpdatedAt:stamp"],
        ),
        t(
            "Pda_ItemStation",
            cols!["RVC", "ItemCode:trim", "StationId", "UpdatedAt:stamp"],
        ),
        t(
            "Pda_ItemSla",
            cols!["RVC", "ItemCode:trim", "TargetMinutes", "UpdatedAt:stamp"],
        ),
        t(
            "Pda_PrintTheme",
            cols![
                "ThemeId",
                "RVC",
                "Name:trim",
                "Mode:trim",
                "PaletteJson",
                "BudgetDisplay",
                "BudgetBadge",
                "Status:trim",
                "PublishedVersion",
                "DraftJson",
                "IsActive:flag",
                "CreatedAt:stamp",
                "UpdatedAt:stamp",
                "FontFamily:trim",
                "FontSource:trim"
            ],
        ),
        t(
            "Pda_PrintThemeVersion",
            cols![
                "ThemeId",
                "VersionNo",
                "Mode:trim",
                "PaletteJson",
                "BudgetDisplay",
                "BudgetBadge",
                "PublishedAt:stamp",
                "FontFamily:trim",
                "FontSource:trim"
            ],
        ),
        t(
            "Pda_PrintTemplate",
            cols![
                "TemplateId",
                "RVC",
                "Name:trim",
                "DocType:trim",
                "HeaderTpl",
                "FooterTpl",
                "LayoutJson",
                "IsActive:flag",
                "CreatedAt:stamp",
                "LogoRaster:blob",
                "LogoWidth",
                "LogoHeight",
                "Status:trim",
                "PublishedVersion",
                "ThemeId",
                "ThemeVersion",
                "LogoSource:blob",
                "LogoMime:trim",
                "LogoDither:trim",
                "QrSize"
            ],
        ),
        t(
            "Pda_PrintTemplateVersion",
            cols![
                "TemplateId",
                "VersionNo",
                "DocType:trim",
                "HeaderTpl",
                "FooterTpl",
                "LayoutJson",
                "ThemeId",
                "ThemeVersion",
                "PublishedAt:stamp"
            ],
        ),
        t(
            PRINT_PROFILE_SOURCE,
            cols![
                "ProfileId",
                "RVC",
                "Name:trim",
                "DocType:trim",
                "PrinterId",
                "Priority",
                "IsActive:flag",
                "CreatedAt:stamp",
                "TemplateId",
                "LayoutJson",
                "RenderMode:trim"
            ],
        ),
        t("Pda_PrintProfilePrinter", cols!["ProfileId", "PrinterId"]),
        t(
            "Pda_PrintRule",
            cols![
                "RuleId",
                "ProfileId",
                "CritType:trim",
                "Op:trim",
                "Value:trim",
                "GroupNo"
            ],
        ),
        t(
            "Pda_Section",
            cols!["RVC", "SectionId", "Name:trim", "Color"],
        ),
        // Scope arrived late; the build that added it stamped every assignment as lasting ('table').
        t(
            "Pda_TableAssignment",
            cols![
                "RVC",
                "TableNo:trim",
                "ServerInitial:trim",
                "SectionId",
                IfAbsent("Scope", Str("table")),
                "AssignedAt:stamp"
            ],
        ),
        t(
            "Pda_TableStatus",
            cols![
                "RVC",
                "TableNo:trim",
                "Status:trim",
                "SetBy:trim",
                "SetAt:stamp"
            ],
        ),
        t(
            "Pda_StsSlot",
            cols![
                "RVCNo",
                "SlotNo",
                "SlotLabel",
                "IsEnabled:flag",
                "SortOrder",
                "LastChangeBy:trim",
                "LastChangeTime:stamp"
            ],
        ),
        t(
            "Pda_IrdConfig",
            cols!["RVC", "Enabled:flag", "UpdatedAt:stamp"],
        ),
        t(
            "Pda_ItemStock",
            cols![
                "RVC",
                "ItemCode:trim",
                "QtyOnHand:number",
                "Par:number",
                "Track:flag",
                "UpdatedBy:trim",
                "UpdatedAt:stamp"
            ],
        ),
        t(
            "Pda_GiftCard",
            cols![
                "CardNo:trim",
                "RVC",
                "InitialValue:fixed0",
                "Balance:fixed0",
                "Status:trim",
                "IssuedAt:stamp",
                "ExpiresAt:date",
                IfNull("IssuedBy:trim", Str("")),
                "VoidedBy:trim",
                "VoidedAt:stamp",
                "UpdatedAt:stamp"
            ],
        ),
        named(
            "Pda_MealPlanEntitlement",
            "pda_mealplan_entitlement",
            &[("reserved_covers", Int(0))],
            cols![
                "RVC",
                "RoomNo:trim",
                "Plan:trim",
                "GuestId:text",
                "RemainingCovers",
                "ValidFrom:date",
                "ValidTo:date",
                "UpdatedAt:localstamp"
            ],
        ),
        t(
            "Pda_CashDeclaration",
            cols![
                "RVC",
                "ShiftDate:date",
                "Cashier:trim",
                "DeclaredCash:fixed2",
                "SystemCash:fixed2",
                "Variance:fixed2",
                IfNull("Status:trim", Str("PENDING")),
                "ApprovedBy:trim",
                "ApprovedAt:stamp",
                "CreatedAt:stamp"
            ],
        ),
        t(
            "Pda_RemovedPaymentAudit",
            cols![
                "AuditId",
                "RemovedAt:stamp",
                "RemovedBy",
                "RVC",
                "CheckNo",
                "TrnSeq",
                "TrnCode",
                "TenderMode:trim",
                "Amount:number",
                "RowCashier:trim",
                "Reason"
            ],
        ),
        t(
            "Pda_ReservationAudit",
            cols![
                "AuditId",
                "CancelledAt:stamp",
                "CancelledBy:trim",
                "ReservationId",
                "RVC:trim",
                "Booker:trim",
                "GuestInfo:trim",
                "TableNum:trim",
                "FromDateTime>from_datetime:stamp",
                "ToDateTime>to_datetime:stamp",
                "NumAdult",
                "NumChild"
            ],
        ),
        // Kept for the record (legacy-archive); the cloud model differs.
        t(
            "Pda_AuditLog",
            cols![
                "AuditId",
                "OccurredAt:stamp",
                "Initial:trim",
                "RVC",
                "Action:trim",
                "CheckNo:text",
                "TrnSeq",
                "Detail",
                "CorrelationId",
                "Result",
                "Category",
                "Severity",
                "Outcome",
                "ActorId",
                "Device",
                "IpAddress",
                "UserAgent",
                "SessionId",
                "EntityType",
                "EntityKey",
                "BeforeJson",
                "AfterJson",
                "ContainsPii:flag",
                "TraceParent"
            ],
        ),
        named(
            "Pda_EInvoice",
            "pda_einvoice",
            &[],
            cols![
                "CheckNo",
                "RvcNo",
                "InvoiceNo:trim",
                "Fkey",
                "IssuedAt:stamp",
                "LookupUrl",
                "Status:trim",
                "CompanyName",
                "TaxCode"
            ],
        ),
        t(
            "Pda_LegacyItemImage",
            cols![
                "ImageId",
                "ICode>icode:trim",
                "RVC",
                "ImageData:blob",
                IfNull("MimeType:trim", Str("image/jpeg")),
                "UploadedAt:stamp",
                "UploadedBy:trim"
            ],
        ),
        t(
            "PDA_ClerkRole",
            cols!["Initial:trim", "ClerkRole", "UpdatedAt:stamp"],
        ),
        t(
            "Pda_CheckAssignment",
            cols![
                "RVC",
                "CheckNo:fixed0",
                "ServerInitial:trim",
                "SectionId",
                "UpdatedBy:trim",
                "UpdatedAt:stamp"
            ],
        ),
        t(
            "Pda_CheckCourse",
            cols![
                "RvcNo",
                "CheckNo",
                "TrnSeq",
                "CourseNo",
                "FireState:trim",
                "HeldBy:trim",
                "HeldTime:stamp",
                "FiredBy:trim",
                "FiredTime:stamp"
            ],
        ),
        t(
            "Pda_CheckSplit",
            cols![
                "RvcNo",
                "CheckNo",
                "ParentCheckNo",
                "Ordinal",
                "SetSize",
                "CreatedAt:stamp"
            ],
        ),
        t(
            "Pda_CheckVersion",
            cols!["RVC", "CheckNo:truncated", "Version", "UpdatedAt:stamp"],
        ),
        t(
            "Pda_IrdDelivery",
            cols![
                "Id",
                "RVC",
                "CheckNo:text",
                "RoomNo",
                "Stage",
                "Runner",
                "CreatedAt:stamp",
                "UpdatedAt:stamp"
            ],
        ),
        t(
            "Pda_Waitlist",
            cols![
                "WaitlistId",
                "RVC",
                "PartyName",
                "PartySize",
                "Phone",
                "QuotedWaitMin",
                "Status",
                "Notes",
                "SeatedCheckNo:text",
                "SeatedTableNo",
                "CreatedBy",
                "CreatedAt:stamp",
                "NotifiedAt:stamp",
                "SeatedAt:stamp"
            ],
        ),
        t("Pda_ReviewMeta", cols!["IDReview", "ReviewTime:stamp"]),
    ]
}

static TABLES: OnceLock<Vec<LegacySqlTable>> = OnceLock::new();

/// Every mapped table, in declaration order.
pub fn tables() -> &'static [LegacySqlTable] {
    TABLES.get_or_init(build_tables)
}

/// SQL Server tables of the old builds that never leave the site, with the reason
/// (looked up case-insensitively).
pub const NEVER_EXPORTED: &[(&str, &str)] = &[
    ("Pda_RefreshToken", "sign-in sessions: never exported"),
    ("Pda_LoginAttempt", "sign-in attempts: never exported"),
    ("Pda_GuestSession", "guest sessions: never exported"),
    (
        "Pda_CheckInvoiceToken",
        "one-time invoice links: never exported",
    ),
    (
        "Pda_GuestRequest",
        "guest requests still open at the export: transient",
    ),
    (
        "Pda_GuestRequestModifier",
        "guest requests still open at the export: transient",
    ),
    (
        "Pda_OpIdempotency",
        "replay protection of the on-prem app: transient",
    ),
    (
        "Pda_PaymentIdempotency",
        "replay protection of the on-prem app: transient",
    ),
    ("Pda_PrintJob", "print queue of the on-prem app: transient"),
    (
        "Pda_ReceiptDelivery",
        "receipt e-mail queue of the on-prem app: transient",
    ),
    (
        "Pda_PmsPostQueue",
        "room-charge posting queue of the on-prem app: transient",
    ),
    (
        "Pda_PmsTxnCodeMap",
        "room-charge posting codes: later builds have no table for them; keep the database backup",
    ),
    ("Pda_ErrorLog", "diagnostic log of the on-prem app"),
];

/// The reason a SQL Server table is never exported, if it is one of [`NEVER_EXPORTED`].
pub fn never_exported_reason(table: &str) -> Option<&'static str> {
    NEVER_EXPORTED
        .iter()
        .find(|(t, _)| eq_ignore_case(t, table))
        .map(|(_, r)| *r)
}

/// The map of a SQL Server table by its unqualified name (case-insensitive).
pub fn find(source_table: &str) -> Option<&'static LegacySqlTable> {
    tables()
        .iter()
        .find(|t| eq_ignore_case(&t.source, source_table))
}
