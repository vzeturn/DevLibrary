//! The legacy bundle contract (`Legacy/LegacyBundle.cs`, ADR 0085): classification of
//! `pda.db` tables and `manifest.json` (written with `JsonSerializerDefaults.Web`,
//! indented, enums in camelCase, nulls written).

use crate::contracts::json::string_enum;
use crate::contracts::time::DateTimeOffset;
use crate::util::stj::{self, Obj, ReadError, ReadOptions, Style, Writer};

string_enum! {
    /// What the cloud does with one table of a bundle. JSON: camelCase names.
    LegacyDisposition {
        /// `TenantCli legacy-import` writes the rows.
        Import = 0,
        /// Exported for the record only.
        Archive = 1,
        /// The cloud cannot take these rows.
        Unsupported = 2,
        /// Never exported.
        Exclude = 3,
    }
}

impl LegacyDisposition {
    /// The camelCase JSON name (`JsonStringEnumConverter(JsonNamingPolicy.CamelCase)`).
    pub fn json_name(self) -> &'static str {
        match self {
            LegacyDisposition::Import => "import",
            LegacyDisposition::Archive => "archive",
            LegacyDisposition::Unsupported => "unsupported",
            LegacyDisposition::Exclude => "exclude",
        }
    }
}

/// Bundle format name.
pub const FORMAT: &str = "pda-legacy-bundle";
/// Bundle format version.
pub const FORMAT_VERSION: i32 = 1;
/// Manifest file name.
pub const MANIFEST_FILE: &str = "manifest.json";
/// The RVC list read from SMILE_POS.
pub const RVC_LIST_TABLE: &str = "dbo.DefRVCList";
/// The plain brand values of the legacy registry key.
pub const REGISTRY_BRANDING_TABLE: &str = "registry.branding";
/// The VietQR bank account of the legacy registry (ADR 0099).
pub const REGISTRY_VIETQR_TABLE: &str = "registry.vietqr";
/// Maintenance mode of a cutover export.
pub const MAINTENANCE_MODE: &str = "maintenance";
/// Maintenance mode of an `--allow-live` export.
pub const LIVE_REHEARSAL_MODE: &str = "live_rehearsal";
/// Maintenance mode of `legacy-export --vietqr-only`.
pub const VIETQR_ONLY_MODE: &str = "vietqr_only";

/// Where the bundle came from.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LegacyBundleSource {
    /// App version.
    pub app_version: Option<String>,
    /// SQLite schema version.
    pub sqlite_schema_version: Option<i32>,
    /// SQL database.
    pub sql_database: String,
    /// Exporter.
    pub exporter: String,
    /// `None` for `pda.db`; `sqlserver:dbo` / `sqlserver:pda` (ADR 0094).
    pub store: Option<String>,
}

/// Why the exporter judged the site quiet.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LegacyBundleMaintenance {
    /// Mode.
    pub mode: String,
    /// Evidence.
    pub evidence: String,
}

/// One exported table: gzip JSON lines.
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyBundleFile {
    /// Table.
    pub table: String,
    /// Bundle-relative path.
    pub path: String,
    /// Rows.
    pub rows: i64,
    /// SHA-256 of the file.
    pub sha256: String,
    /// Disposition.
    pub disposition: LegacyDisposition,
}

/// State the exporter saw but did not export.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LegacyLeftOut {
    /// Table.
    pub table: String,
    /// Rows.
    pub rows: i64,
    /// Reason.
    pub reason: String,
}

/// `manifest.json` of a legacy bundle.
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyBundleManifest {
    /// [`FORMAT`].
    pub format: String,
    /// [`FORMAT_VERSION`].
    pub format_version: i32,
    /// Created.
    pub created_at: DateTimeOffset,
    /// Source.
    pub source: LegacyBundleSource,
    /// Maintenance.
    pub maintenance: LegacyBundleMaintenance,
    /// Files.
    pub files: Vec<LegacyBundleFile>,
    /// Left out.
    pub left_out: Vec<LegacyLeftOut>,
}

fn opt_str(w: &mut Writer, name: &str, v: &Option<String>) {
    w.key(name);
    match v {
        Some(s) => w.string(s),
        None => w.null(),
    };
}

impl LegacyBundleManifest {
    /// `JsonSerializer.Serialize(manifest, LegacyBundle.Json)`: indented with the
    /// platform newline, camelCase, nulls written.
    pub fn to_json(&self) -> String {
        let mut w = Writer::new(Style::Indented);
        w.begin_object();
        w.key("format").string(&self.format);
        w.key("formatVersion").int(self.format_version.into());
        w.key("createdAt")
            .unescaped(&self.created_at.to_json_string());
        w.key("source").begin_object();
        opt_str(&mut w, "appVersion", &self.source.app_version);
        w.key("sqliteSchemaVersion");
        match self.source.sqlite_schema_version {
            Some(n) => w.int(n.into()),
            None => w.null(),
        };
        w.key("sqlDatabase").string(&self.source.sql_database);
        w.key("exporter").string(&self.source.exporter);
        opt_str(&mut w, "store", &self.source.store);
        w.end_object();
        w.key("maintenance").begin_object();
        w.key("mode").string(&self.maintenance.mode);
        w.key("evidence").string(&self.maintenance.evidence);
        w.end_object();
        w.key("files").begin_array();
        for f in &self.files {
            w.begin_object();
            w.key("table").string(&f.table);
            w.key("path").string(&f.path);
            w.key("rows").int(f.rows);
            w.key("sha256").string(&f.sha256);
            w.key("disposition").string(f.disposition.json_name());
            w.end_object();
        }
        w.end_array();
        w.key("leftOut").begin_array();
        for l in &self.left_out {
            w.begin_object();
            w.key("table").string(&l.table);
            w.key("rows").int(l.rows);
            w.key("reason").string(&l.reason);
            w.end_object();
        }
        w.end_array();
        w.end_object();
        w.finish()
    }

    /// `JsonSerializer.Deserialize<LegacyBundleManifest>(json, LegacyBundle.Json)`:
    /// case-insensitive camelCase, numbers also from strings; `Ok(None)` for `null`.
    pub fn from_json(json: &str) -> Result<Option<LegacyBundleManifest>, ReadError> {
        let v = stj::parse(json)?;
        let Some(o) = Obj::of(&v, stj::WEB, "$")? else {
            return Ok(None);
        };
        let source = o
            .opt_obj("source", |s| {
                Ok(LegacyBundleSource {
                    app_version: s.opt_string("app_version")?,
                    sqlite_schema_version: s.opt_int("sqlite_schema_version")?,
                    sql_database: s.string("sql_database")?,
                    exporter: s.string("exporter")?,
                    store: s.opt_string("store")?,
                })
            })?
            .unwrap_or_default();
        let maintenance = o
            .opt_obj("maintenance", |m| {
                Ok(LegacyBundleMaintenance {
                    mode: m.string("mode")?,
                    evidence: m.string("evidence")?,
                })
            })?
            .unwrap_or_default();
        let created_at = match o.iso("created_at")? {
            None => DateTimeOffset::MIN,
            Some(iso) => DateTimeOffset::from_iso(iso)
                .ok_or_else(|| ReadError::new(&o.path_of("created_at"), "out of range"))?,
        };
        Ok(Some(LegacyBundleManifest {
            format: o.string("format")?,
            format_version: o.int("format_version")?,
            created_at,
            source,
            maintenance,
            files: o.list("files", read_file)?,
            left_out: o.list("left_out", read_left_out)?,
        }))
    }
}

fn read_file(
    v: &dl_json::Value,
    opts: ReadOptions,
    path: &str,
) -> Result<LegacyBundleFile, ReadError> {
    let o = Obj::require(v, opts, path)?;
    let disposition = match o.get("disposition") {
        None => LegacyDisposition::Import,
        Some(v) => stj::read_enum(v, &o.path_of("disposition"), LegacyDisposition::MEMBERS)?,
    };
    Ok(LegacyBundleFile {
        table: o.string("table")?,
        path: o.string("path")?,
        rows: o.int("rows")?,
        sha256: o.string("sha256")?,
        disposition,
    })
}

fn read_left_out(
    v: &dl_json::Value,
    opts: ReadOptions,
    path: &str,
) -> Result<LegacyLeftOut, ReadError> {
    let o = Obj::require(v, opts, path)?;
    Ok(LegacyLeftOut {
        table: o.string("table")?,
        rows: o.int("rows")?,
        reason: o.string("reason")?,
    })
}

/// A bundle that moves only the VietQR account.
pub fn is_vietqr_only(m: &LegacyBundleManifest) -> bool {
    m.maintenance.mode == VIETQR_ONLY_MODE
        && m.files.len() == 1
        && m.files[0].table == REGISTRY_VIETQR_TABLE
}

/// The bundle-relative path of a table's file; `None` for a name that is not a valid
/// bundle table name (C#: `ArgumentException`).
pub fn path_for(table: &str) -> Option<String> {
    is_valid_table_name(table).then(|| format!("tables/{table}.jsonl.gz"))
}

/// `^[A-Za-z0-9_]+(\.[A-Za-z0-9_]+)?$` (.NET `$` also matches before a final `\n`).
pub fn is_valid_table_name(table: &str) -> bool {
    let t = table.strip_suffix('\n').unwrap_or(table);
    let word = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    match t.split_once('.') {
        None => word(t),
        Some((a, b)) => word(a) && word(b),
    }
}

/// The `pda.db` tables the importer writes.
pub const IMPORT_TABLES: &[&str] = &[
    "pda_config",
    "pda_branding",
    "pda_diet_allergen_catalog",
    "pda_item_meta",
    "pda_item_recommend",
    "pda_option_group",
    "pda_option_group_item",
    "pda_option_profile",
    "pda_option_profile_group",
    "pda_item_option_assignment",
    "pda_modifier_group_meta",
    "pda_printer",
    "pda_kitchen_station",
    "pda_class_station",
    "pda_item_station",
    "pda_item_sla",
    "pda_kds_threshold",
    "pda_print_theme",
    "pda_print_theme_version",
    "pda_print_template",
    "pda_print_template_version",
    "pda_print_profile",
    "pda_print_profile_printer",
    "pda_print_rule",
    "pda_section",
    "pda_table_assignment",
    "pda_table_note",
    "pda_table_status",
    "pda_guest_hidden",
    "pda_sts_slot",
    "pda_ird_config",
    "pda_item_stock",
    "pda_inventory_movement",
    "pda_gift_card",
    "pda_mealplan_entitlement",
    "pda_cash_declaration",
    "pda_removed_payment_audit",
    "pda_reservation_audit",
    "pda_room_charge_signature",
    "pda_ai_suggestion_feedback",
];

const ARCHIVED: &[(&str, &str)] = &[
    (
        "pda_audit_log",
        "the cloud audit log is a hash chain; foreign rows would break its verification",
    ),
    (
        "pda_einvoice",
        "different e-invoice model and provider state; importing could issue an invoice twice",
    ),
    (
        "pda_gift_card_redeem_ledger",
        "different ledger semantics; balances move with pda_gift_card",
    ),
    ("pda_kds_line", "kitchen ticket history"),
    ("pda_kds_auto_done", "kitchen ticket history"),
    (
        "pda_legacy_item_image",
        "no cloud home for images of legacy-schema items",
    ),
    (
        "pda_clerk_role",
        "roles are assigned again in the cloud role matrix once users are captured",
    ),
    (
        "pda_check_assignment",
        "open-check overlay; cutover requires zero open checks",
    ),
    (
        "pda_check_course",
        "open-check overlay; cutover requires zero open checks",
    ),
    (
        "pda_check_prebill",
        "open-check overlay; cutover requires zero open checks",
    ),
    (
        "pda_check_split",
        "open-check overlay; cutover requires zero open checks",
    ),
    (
        "pda_check_version",
        "open-check overlay; cutover requires zero open checks",
    ),
    (
        "pda_mealplan_reserve",
        "open-check overlay; cutover requires zero open checks",
    ),
    (
        "pda_ird_delivery",
        "open-check overlay; cutover requires zero open checks",
    ),
    ("pda_waitlist", "same-day queue"),
    ("pda_review_meta", "guest review history"),
];

const EXCLUDED: &[(&str, &str)] = &[
    (
        "pda_refresh_token",
        "secret: login sessions; users sign in again",
    ),
    ("pda_login_attempt", "transient lockout state"),
    ("pda_check_invoice_token", "secret: guest invoice links"),
    ("pda_guest_session", "secret: guest QR sessions"),
    (
        "pda_guest_request",
        "transient guest requests tied to sessions",
    ),
    (
        "pda_speaker_device",
        "secret: device tokens; speakers are paired again",
    ),
    ("pda_op_idempotency", "transient idempotency keys"),
    ("pda_payment_idempotency", "transient idempotency keys"),
    ("pda_print_job", "transient print queue"),
    ("pda_kds_event", "transient kitchen events"),
    ("pda_kds_rollup_15m", "derived kitchen statistics"),
    ("pda_order_item_companion", "derived from the open checks"),
    ("schema_version", "recorded in the manifest"),
    ("sqlite_sequence", "SQLite bookkeeping"),
    ("_migrated_domain", "legacy migration bookkeeping"),
];

fn lookup(list: &'static [(&str, &str)], table: &str) -> Option<&'static str> {
    list.iter().find(|(t, _)| *t == table).map(|(_, r)| *r)
}

/// Classifies a `pda.db` table with the reason; an unknown table is excluded.
pub fn classify(table: &str) -> (LegacyDisposition, String) {
    if IMPORT_TABLES.contains(&table) {
        return (LegacyDisposition::Import, "imported".into());
    }
    if let Some(r) = lookup(ARCHIVED, table) {
        return (LegacyDisposition::Archive, r.into());
    }
    if table.starts_with("pda_spa_") {
        return (
            LegacyDisposition::Unsupported,
            "the cloud spa model differs; a legacy spa site needs its own migration".into(),
        );
    }
    if let Some(r) = lookup(EXCLUDED, table) {
        return (LegacyDisposition::Exclude, r.into());
    }
    if table.starts_with("pda_mfa_") {
        return (
            LegacyDisposition::Exclude,
            "secret: MFA state; users enroll again".into(),
        );
    }
    (
        LegacyDisposition::Exclude,
        format!("not on the format v{FORMAT_VERSION} allow-list"),
    )
}

/// Imported tables with legacy columns the cloud has no home for (the import maps'
/// dropped columns).
pub const PARTLY_IMPORTED: &[(&str, &[&str])] = &[
    ("pda_config", &["last_change_by"]),
    ("pda_option_profile_group", &["rvc_no"]),
    (
        "pda_modifier_group_meta",
        &["last_change_by", "last_change_time"],
    ),
    ("pda_printer", &["created_at"]),
    ("pda_print_theme", &["created_at"]),
    ("pda_print_template", &["created_at"]),
    (
        "pda_print_profile",
        &["created_at", "layout_json", "watermark_default_off"],
    ),
];

/// Whether `legacy-archive` writes this bundle file into the site's archive database
/// (ADR 0089), with the reason.
pub fn archives_on_prem(file: &LegacyBundleFile) -> (bool, String) {
    if file.table == RVC_LIST_TABLE {
        return (
            false,
            "outlet names, imported; the list itself stays in SMILE_POS".into(),
        );
    }
    if file.table == REGISTRY_VIETQR_TABLE {
        return (
            false,
            "imported; the account stays in the legacy registry, never in the archive database"
                .into(),
        );
    }
    if file.table == REGISTRY_BRANDING_TABLE {
        return (
            true,
            "imported only when pda_config has no brand row; the registry values are kept as read"
                .into(),
        );
    }
    if let Some((_, dropped)) = PARTLY_IMPORTED.iter().find(|(t, _)| *t == file.table) {
        let mut cols: Vec<&str> = dropped.to_vec();
        cols.sort_by(|a, b| crate::contracts::hashing::ordinal_cmp(a, b));
        return (
            true,
            format!("imported without {}; kept whole", cols.join(", ")),
        );
    }
    match file.disposition {
        LegacyDisposition::Archive | LegacyDisposition::Unsupported => {
            (true, classify(&file.table).1)
        }
        LegacyDisposition::Import => (false, "imported".into()),
        LegacyDisposition::Exclude => (false, "excluded from the bundle".into()),
    }
}

/// True for a `pda_config` key that holds a credential (never exported):
/// `pass(word|wd)|secret|token|api[._-]?key|private[._-]?key|credential`, ignoring case.
pub fn is_secret_config_key(key: &str) -> bool {
    // .NET's case-insensitive regex also matches the Kelvin sign for 'k'.
    let k: String = key
        .chars()
        .map(|c| {
            if c == '\u{212a}' {
                'k'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    let b = k.as_bytes();
    let at = |i: usize, s: &str| b.get(i..i + s.len()) == Some(s.as_bytes());
    let sep = |i: usize| matches!(b.get(i), Some(b'.' | b'_' | b'-'));
    (0..b.len()).any(|i| {
        at(i, "password")
            || at(i, "passwd")
            || at(i, "secret")
            || at(i, "token")
            || at(i, "credential")
            || (at(i, "api") && (at(i + 3, "key") || (sep(i + 3) && at(i + 4, "key"))))
            || (at(i, "private") && (at(i + 7, "key") || (sep(i + 7) && at(i + 8, "key"))))
    })
}
