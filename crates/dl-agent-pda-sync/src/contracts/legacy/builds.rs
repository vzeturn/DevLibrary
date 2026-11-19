//! Supported on-prem builds (`Legacy/LegacyBuilds.cs`, ADR 0093).

/// How a bundle's source build relates to the builds the import maps were checked
/// against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LegacyBuildStanding {
    /// The bundle does not say which build wrote it.
    Unknown,
    /// The reference build or later.
    Reference,
    /// Between the oldest checked build and the reference.
    Older,
    /// Older than the oldest checked build.
    Unverified,
}

/// The last on-prem build; the import maps list its columns.
pub const REFERENCE: &str = "3.30.32";
/// The first tagged build that keeps its data in `pda.db`.
pub const OLDEST_VERIFIED: &str = "3.11.26";
/// How events name the store of a bundle whose rows came from `pda.db`.
pub const PDA_DB_STORE: &str = "pda.db";

const REFERENCE_VERSION: (i32, i32, i32) = (3, 30, 32);
const OLDEST_VERSION: (i32, i32, i32) = (3, 11, 26);

/// The numeric part of a product version (`3.30.32+abc123`, `3.11.26.0`) as
/// (major, minor, build); `None` when there is none.
pub fn parse(app_version: Option<&str>) -> Option<(i32, i32, i32)> {
    let text = app_version?.trim();
    if text.is_empty() {
        return None;
    }
    let end = text
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || *c == '.'))
        .map_or(text.len(), |(i, _)| i);
    let parts: Vec<&str> = text[..end].split('.').filter(|p| !p.is_empty()).collect();
    if !(2..=4).contains(&parts.len()) {
        return None;
    }
    let mut n = [0i32; 3];
    for (i, p) in parts.iter().take(3).enumerate() {
        n[i] = p.parse().ok()?;
    }
    Some((n[0], n[1], n[2]))
}

/// The standing of a build.
pub fn standing(app_version: Option<&str>) -> LegacyBuildStanding {
    match parse(app_version) {
        None => LegacyBuildStanding::Unknown,
        Some(v) if v >= REFERENCE_VERSION => LegacyBuildStanding::Reference,
        Some(v) if v >= OLDEST_VERSION => LegacyBuildStanding::Older,
        Some(_) => LegacyBuildStanding::Unverified,
    }
}

/// One line for the operator about a bundle and the store it was read from.
pub fn describe_store(app_version: Option<&str>, store: Option<&str>) -> String {
    let Some(store) = store else {
        return describe(app_version);
    };
    let version = match app_version {
        Some(v) if !v.trim().is_empty() => v,
        _ => "version not recorded",
    };
    format!(
        "{version}, a build from before the pda.db store, read straight from SQL Server ({store}). \
         The bundle holds what the first pda.db build would have moved across; the import checks every \
         table and column: run it with --dry-run first."
    )
}

/// One line for the operator, printed after `build` by the export and by the import.
pub fn describe(app_version: Option<&str>) -> String {
    let v = app_version.unwrap_or("");
    match standing(app_version) {
        LegacyBuildStanding::Reference => format!("{v}, the build the import maps were written for."),
        LegacyBuildStanding::Older => format!(
            "{v}, older than {REFERENCE}: supported. Tables it does not have are skipped; columns it does not have take the cloud defaults."
        ),
        LegacyBuildStanding::Unverified => format!(
            "{v}, older than {OLDEST_VERIFIED}, the oldest build the import maps were checked against. \
             The import still checks every table and column and refuses what it has no home for: run it with --dry-run first."
        ),
        LegacyBuildStanding::Unknown => {
            "not recorded (the export had no --site-root). The import checks every table and column, whatever the build."
                .to_owned()
        }
    }
}
