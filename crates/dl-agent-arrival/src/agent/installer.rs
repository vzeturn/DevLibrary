//! `PmsProcedureInstaller`: installs the versioned PMS procedure bundle (the same
//! `pms-procedures.sql` the .NET agent embeds) on the hotel's database. The installed version
//! is a database extended property, so an up-to-date PMS costs one tiny query at startup.
//!
//! The `picture` phase (SMILE_PICTURE objects, run from `master`) carries its own marker and
//! only runs when its own content changed; a changed bundle is marked `pending:` before the
//! first batch, so a half-installed bundle is always reinstalled. Errors carry counts, SQL
//! error numbers and batch positions only: never SQL text.

use std::sync::OnceLock;
use std::time::Duration;

use dl_tds::{Client, Param};

use crate::agent::tds;
use crate::contracts::vault_ops::sha256_hex;

/// Extended property holding the installed bundle hash.
pub const PROPERTY_NAME: &str = "MMArrival.PmsBundle";
/// Extended property holding the installed picture-phase hash.
pub const PICTURE_PROPERTY_NAME: &str = "MMArrival.PmsBundle.Picture";
/// Prefix of a bundle whose install started but did not finish.
pub const PENDING_PREFIX: &str = "pending:";
/// Picture-phase hash of the only bundle shipped before the picture marker existed.
pub const LEGACY_PICTURE_HASH: &str = "26c979cd08417911";
/// Phase run from `master`.
pub const PICTURE_PHASE: &str = "picture";

const MARKER: &str = "-- @@mmarrival:batch ";
const BATCH_TIMEOUT: Duration = Duration::from_secs(120);
const SOURCE: &str = include_str!("../../assets/pms-procedures.sql");

/// One batch of the bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    /// `picture` (run on master) or `pms`.
    pub phase: String,
    /// The batch text (trimmed).
    pub sql: String,
}

/// The bundle this agent carries.
#[derive(Debug)]
pub struct Bundle {
    /// The text with CRLF turned into LF (what the hash covers).
    pub text: String,
    /// The batches in order.
    pub batches: Vec<Batch>,
    /// `BundleHash`.
    pub hash: String,
    /// `PictureHash`.
    pub picture_hash: String,
}

/// The embedded bundle.
pub fn bundle() -> &'static Bundle {
    static BUNDLE: OnceLock<Bundle> = OnceLock::new();
    BUNDLE.get_or_init(|| {
        let text = SOURCE.replace("\r\n", "\n");
        let batches = parse(&text);
        let picture: Vec<&str> = batches
            .iter()
            .filter(|b| b.phase == PICTURE_PHASE)
            .map(|b| b.sql.as_str())
            .collect();
        Bundle {
            hash: short_hash(&text),
            picture_hash: short_hash(&picture.join("\n")),
            text,
            batches,
        }
    })
}

/// `ShortHash`: the first 16 hex digits of the SHA-256 of the UTF-8 text.
pub fn short_hash(text: &str) -> String {
    sha256_hex(text.as_bytes())[..16].to_string()
}

/// `Parse`: `(phase, sql)` batches in order.
pub fn parse(text: &str) -> Vec<Batch> {
    let mut out = Vec::new();
    let mut phase: Option<String> = None;
    let mut sb = String::new();
    for line in text.split('\n') {
        if let Some(rest) = line.strip_prefix(MARKER) {
            if let Some(p) = phase.take() {
                out.push(Batch {
                    phase: p,
                    sql: sb.trim().to_string(),
                });
            }
            phase = Some(rest.trim().to_string());
            sb.clear();
        } else if phase.is_some() {
            sb.push_str(line);
            sb.push('\n');
        }
    }
    if let Some(p) = phase {
        out.push(Batch {
            phase: p,
            sql: sb.trim().to_string(),
        });
    }
    out
}

/// `Plan`: whether to run the bundle and whether to include the picture phase.
pub fn plan(
    force: bool,
    installed: Option<&str>,
    installed_picture: Option<&str>,
    bundle_hash: &str,
    picture_hash: &str,
) -> (bool, bool) {
    if force {
        return (true, true);
    }
    if installed == Some(bundle_hash) {
        return (false, false);
    }
    let legacy_complete = installed.is_some_and(|i| !i.starts_with(PENDING_PREFIX));
    let picture = installed_picture.or(if legacy_complete {
        Some(LEGACY_PICTURE_HASH)
    } else {
        None
    });
    (true, picture != Some(picture_hash))
}

/// Why an install did not complete (no SQL text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallError(pub String);

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn sql_failure(what: &str, e: &dl_tds::Error) -> InstallError {
    dl_log::warn!("PMS procedure install step failed"; step = what, error = %e);
    match e.number() {
        Some(n) => InstallError(format!("{what} failed (SQL error {n})")),
        None => InstallError(format!("{what} failed ({})", tds::exec_error(e))),
    }
}

/// `EnsureInstalledAsync`: installs the bundle when the PMS has a different (or no) version;
/// `Ok(true)` when it ran.
///
/// # Errors
/// [`InstallError`] with counts and SQL error numbers.
pub fn ensure_installed(connection: &str, force: bool) -> Result<bool, InstallError> {
    let b = bundle();
    let mut conn = tds::connect(connection, None).map_err(|e| sql_failure("connect", &e))?;
    let installed = read_marker(&mut conn, PROPERTY_NAME)?;
    let installed_picture = read_marker(&mut conn, PICTURE_PROPERTY_NAME)?;
    let (run, run_picture) = plan(
        force,
        installed.as_deref(),
        installed_picture.as_deref(),
        &b.hash,
        &b.picture_hash,
    );
    if !run {
        dl_log::info!("PMS procedure bundle already installed."; hash = %b.hash);
        return Ok(false);
    }
    dl_log::info!(
        "Installing PMS procedure bundle.";
        hash = %b.hash,
        installed = %installed.as_deref().unwrap_or("none"),
        picture = if run_picture { "included" } else { "unchanged" }
    );
    // Marked first: a stop half-way leaves a bundle the next start reinstalls.
    write_marker(
        &mut conn,
        PROPERTY_NAME,
        &format!("{PENDING_PREFIX}{}", b.hash),
    )?;
    if run_picture {
        // SMILE_PICTURE objects first: the signature procedures compile against them.
        let mut master = tds::connect(connection, Some("master"))
            .map_err(|e| sql_failure("connect to master", &e))?;
        let failed = run_batches(&mut master, b, true);
        if !failed.is_empty() {
            return Err(InstallError(format!(
                "{} SMILE_PICTURE batch(es) failed ({}); the bundle was not installed. {}",
                failed.len(),
                picture_hint(&failed),
                failed.join(" | ")
            )));
        }
        write_marker(&mut conn, PICTURE_PROPERTY_NAME, &b.picture_hash)?;
    } else if installed_picture.is_none() {
        write_marker(&mut conn, PICTURE_PROPERTY_NAME, &b.picture_hash)?;
    }
    let failed = run_batches(&mut conn, b, false);
    if !failed.is_empty() {
        return Err(InstallError(format!(
            "{} PMS bundle batch(es) failed; the bundle was not marked installed. {}",
            failed.len(),
            failed.join(" | ")
        )));
    }
    write_marker(&mut conn, PROPERTY_NAME, &b.hash)?;
    dl_log::info!("PMS procedure bundle installed."; hash = %b.hash);
    Ok(true)
}

/// What the service login lacks, from the SQL error numbers of the failed picture batches
/// (`arrival.picture_probe` answers the exact statements for this server).
pub fn picture_hint(failed: &[String]) -> &'static str {
    let has = |n: &str| failed.iter().any(|f| f.contains(&format!("[{n}]")));
    if has("262") || has("1802") {
        "the service login may not create SMILE_PICTURE: as an administrator CREATE DATABASE [SMILE_PICTURE], give the login a user there with db_owner, then send arrival.install_procedures"
    } else if has("916") || has("4060") {
        "the service login has no user in SMILE_PICTURE: as an administrator USE [SMILE_PICTURE]; CREATE USER [<login>] FOR LOGIN [<login>]; EXEC sp_addrolemember N'db_owner', N'<login>'; then send arrival.install_procedures"
    } else if has("229") || has("1088") || has("2760") || has("15247") {
        "the service login may not create or alter tables in SMILE_PICTURE: as an administrator USE [SMILE_PICTURE]; EXEC sp_addrolemember N'db_ddladmin', N'<login>'; plus db_datareader and db_datawriter (or db_owner), then send arrival.install_procedures"
    } else {
        "send arrival.picture_probe for what the service login lacks, or run the install as an administrator"
    }
}

/// Runs the batches of one side; returns `"#<position> [<number>]"` per failure.
fn run_batches(conn: &mut Client, b: &Bundle, picture: bool) -> Vec<String> {
    let mut failed = Vec::new();
    if let Err(e) = conn.set_timeout(Some(BATCH_TIMEOUT)) {
        failed.push(format!("timeout not set ({})", tds::exec_error(&e)));
        return failed;
    }
    for (i, batch) in b.batches.iter().enumerate() {
        if (batch.phase == PICTURE_PHASE) != picture {
            continue;
        }
        if let Err(e) = conn.batch(&batch.sql) {
            let number = e
                .number()
                .map_or_else(|| tds::exec_error(&e).to_string(), |n| n.to_string());
            dl_log::error!("PMS bundle batch failed"; batch = i, number = %number);
            failed.push(format!("#{i} [{number}]"));
            if conn.is_broken() {
                failed.push("connection lost".into());
                break;
            }
        }
    }
    failed
}

pub(crate) fn read_marker(conn: &mut Client, name: &str) -> Result<Option<String>, InstallError> {
    let results = conn
        .query(
            "SELECT CAST(value AS NVARCHAR(64)) FROM sys.extended_properties WHERE class = 0 AND name = @name",
            &[Param::new("@name", name)],
        )
        .map_err(|e| sql_failure("reading the bundle marker", &e))?;
    Ok(results
        .rows()
        .first()
        .and_then(|r| match r.values().first() {
            Some(dl_tds::Value::String(s)) => Some(s.clone()),
            _ => None,
        }))
}

fn write_marker(conn: &mut Client, name: &str, value: &str) -> Result<(), InstallError> {
    conn.execute(
        "IF EXISTS (SELECT 1 FROM sys.extended_properties WHERE class = 0 AND name = @name)
    EXEC sys.sp_updateextendedproperty @name = @name, @value = @value;
ELSE
    EXEC sys.sp_addextendedproperty @name = @name, @value = @value;",
        &[Param::new("@name", name), Param::new("@value", value)],
    )
    .map(|_| ())
    .map_err(|e| sql_failure("writing the bundle marker", &e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_matches_the_dotnet_rules() {
        assert_eq!(plan(true, Some("h"), Some("p"), "h", "p"), (true, true));
        assert_eq!(plan(false, Some("h"), None, "h", "p"), (false, false));
        assert_eq!(plan(false, None, None, "h", "p"), (true, true));
        assert_eq!(plan(false, Some("old"), Some("p"), "h", "p"), (true, false));
        assert_eq!(
            plan(false, Some("old"), None, "h", LEGACY_PICTURE_HASH),
            (true, false)
        );
        assert_eq!(
            plan(false, Some("pending:h"), None, "h", LEGACY_PICTURE_HASH),
            (true, true)
        );
    }

    #[test]
    fn picture_failures_name_the_missing_grant() {
        let f = |s: &[&str]| s.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
        assert!(picture_hint(&f(&["#0 [262]"])).contains("CREATE DATABASE"));
        assert!(picture_hint(&f(&["#1 [916]", "#2 [916]"])).contains("CREATE USER"));
        assert!(picture_hint(&f(&["#3 [1088]"])).contains("db_ddladmin"));
        assert!(picture_hint(&f(&["#4 [Timeout]"])).contains("picture_probe"));
    }

    #[test]
    fn parse_splits_on_markers_and_trims() {
        let text = "header\n-- @@mmarrival:batch picture\n\nA\n-- @@mmarrival:batch pms \nB\nC\n";
        let batches = parse(text);
        assert_eq!(batches.len(), 2);
        assert_eq!(
            batches[0],
            Batch {
                phase: "picture".into(),
                sql: "A".into()
            }
        );
        assert_eq!(batches[1].phase, "pms");
        assert_eq!(batches[1].sql, "B\nC");
    }
}
