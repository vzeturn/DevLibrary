//! The module's self-test (`dl-agent selftest`, the `selftest` command): the identity and
//! where it came from, the tenant key (fingerprint) and every sealed legal vault file under
//! the module's `install_dir`, the cloud answers over HTTP/1.1, the link's last state, the
//! last recorded problem, where the PMS connection comes from (the core's `sql.json` SMILE_FO
//! entry or `PmsConnectionString`), the PMS connection runs the agent's read probe, the procedure
//! bundle installed is this build's, and the SMILE_PICTURE phase. Read only: no poll is sent
//! (a poll would take the cloud's requests), no key, vault or PMS object is created.

use std::path::Path;
use std::time::Duration;

use dl_agent::selftest::{Check, Outcome, PROBE_TIMEOUT, SelfTest};
use dl_agent::supervisor::ModuleContext;
use dl_http::{Client, ClientOptions, Version};

use super::{ModuleConfig, NAME, NOT_ENROLLED, commands, picture};
use crate::agent::cloud::{self, POLL_PATH};
use crate::agent::core_sql;
use crate::agent::installer::{self, PROPERTY_NAME};
use crate::agent::layout::Layout;
use crate::agent::link_status;
use crate::agent::problems::ProblemJournal;
use crate::agent::secrets::{Identity, TenantKeyStore};
use crate::agent::settings::Settings;
use crate::agent::tds;
use crate::contracts::key_vault::TenantKeyVault;
use crate::legacy::tenant_key;
use crate::stj;
use crate::tools::files;

/// The self-test to register with `App::selftest`.
pub fn arrival_selftest() -> SelfTest {
    SelfTest { module: NAME, run }
}

/// The checks (also the `selftest` command's answer).
pub fn run(ctx: &ModuleContext) -> Vec<Check> {
    let title = "Identity reads";
    let config = match ModuleConfig::from_value(ctx.config()) {
        Ok(c) => c,
        Err(e) => {
            let o = Outcome::fail(format!("modules.{NAME}: {e}"))
                .remedy("fix the module's section in agent.json");
            return vec![Check::timed(NAME, "identity", title, || o)];
        }
    };
    let layout = Layout::detect(config.install_dir.as_deref(), ctx.data_dir());
    let mut settings = match Settings::load(layout.install_dir()) {
        Ok(s) => s,
        Err(e) => {
            let o = Outcome::fail(format!("{}: {e}", layout.install_dir().display()))
                .remedy("fix appsettings.json in the install folder");
            return vec![Check::timed(NAME, "identity", title, || o)];
        }
    };
    let path = layout.resolve(&settings.identity_file);
    let identity = match Identity::load(&path) {
        Ok(Some(i)) => i,
        Ok(None) => {
            let reason = format!("not enrolled: no identity at {}", path.display());
            return vec![Check::skip(NAME, "identity", title, &reason)];
        }
        Err(e) => {
            let o = Outcome::fail(format!("{}: {e}", path.display())).remedy(NOT_ENROLLED);
            return vec![Check::timed(NAME, "identity", title, || o)];
        }
    };
    let detail = format!(
        "{}: tenant {}, agent {}, key id {}",
        path.display(),
        identity.tenant_id,
        identity.agent_id,
        identity.kid
    );
    let mut checks = vec![Check::timed(NAME, "identity", title, || {
        Outcome::pass(detail)
    })];
    let record = files::read_record(layout.install_dir());
    let source = commands::identity_source(record.as_ref());
    checks.push(Check::timed(
        NAME,
        "identity_source",
        "Identity source",
        || Outcome::pass(source),
    ));
    let key_source = record
        .as_ref()
        .and_then(|r| r.get("tenant_key"))
        .and_then(|k| k.get("source"))
        .and_then(dl_json::Value::as_str)
        .map(str::to_owned);
    let mut fingerprint = None;
    checks.push(Check::timed(NAME, "tenant_key", "Tenant key reads", || {
        let (o, fp) = tenant_key(&settings, &layout, key_source.as_deref());
        fingerprint = fp;
        o
    }));
    let vault = layout.resolve(&settings.vault_directory);
    checks.push(Check::timed(
        NAME,
        "vault",
        "Legal vault opens with this key",
        || vault_outcome(&vault, fingerprint.as_deref()),
    ));
    checks.push(link_check(&path));
    checks.push(last_problem(&path));
    let base = settings.cloud_base_url.clone();
    checks.push(Check::bounded(
        NAME,
        "cloud",
        "Cloud reachable over HTTP/1.1",
        PROBE_TIMEOUT * 2,
        move || reachable(&base),
    ));
    let core_sql = core_sql::apply(&mut settings, ctx.data_dir());
    let source = settings.pms_source;
    checks.push(Check::timed(
        NAME,
        "pms_source",
        "PMS connection source",
        || match core_sql {
            Ok(s) => Outcome::pass(match s.pin() {
                Some(rung) => format!("{} SMILE_FO, transport {}", s.name(), rung.name()),
                None => s.name().to_string(),
            }),
            Err(e) => Outcome::warn(format!("{e}; PmsConnectionString is used"))
                .remedy("run dl-agent sql-setup again for SMILE_FO"),
        },
    ));
    let connection = settings.pms_connection_string.clone();
    checks.push(Check::bounded(
        NAME,
        "pms",
        "PMS read query runs",
        PROBE_TIMEOUT * 2,
        move || match tds::probe(tds::Target::new(&connection, source)) {
            Ok(p) => Outcome::pass(p.line()),
            Err(e) if e.is_tls() => Outcome::fail(format!("TLS handshake failed: {e}"))
                .remedy("set Encrypt=False (old SQL Server) or install a TLS 1.2 certificate"),
            Err(e) => {
                Outcome::fail(e.to_string()).remedy("check SQL Server and PmsConnectionString")
            }
        },
    ));
    let connection = settings.pms_connection_string.clone();
    checks.push(Check::bounded(
        NAME,
        "bundle",
        "PMS procedure bundle is this build's",
        PROBE_TIMEOUT * 2,
        move || bundle_outcome(tds::Target::new(&connection, source)),
    ));
    let connection = settings.pms_connection_string;
    checks.push(Check::bounded(
        NAME,
        "picture",
        "SMILE_PICTURE phase",
        PROBE_TIMEOUT * 3,
        move || picture_outcome(tds::Target::new(&connection, source)),
    ));
    checks
}

fn tenant_key(
    settings: &Settings,
    layout: &Layout,
    enrolled_from: Option<&str>,
) -> (Outcome, Option<String>) {
    let file = layout.resolve(&settings.tenant_key_file);
    let mut source = if settings.tenant_key.trim().is_empty() {
        file.display().to_string()
    } else {
        "Agent:TenantKey (settings)".to_owned()
    };
    if let Some(from) = enrolled_from {
        source.push_str(&format!(" [enrol: {from}]"));
    }
    match TenantKeyStore::new(&settings.tenant_key, file).load_existing() {
        Ok(Some(k)) => match TenantKeyVault::new(&k) {
            Ok(v) => {
                let fp = v.fingerprint();
                let o = Outcome::pass(format!("{source}: key {fp} opens"));
                (o, Some(fp))
            }
            Err(e) => (
                Outcome::fail(format!("{source}: {e}"))
                    .remedy("restore the site's agent-tenant-key.bin (see data/dl-enroll.json)"),
                None,
            ),
        },
        Ok(None) => (
            Outcome::warn(format!(
                "{source}: no key yet; the service creates it on the first sealed field"
            ))
            .remedy("start the service once; a site that had a key must restore that file"),
            None,
        ),
        Err(e) => (
            Outcome::fail(format!("{source}: {e}")).remedy(
                "the key was sealed on another machine or account; restore it from the old PC",
            ),
            None,
        ),
    }
}

/// Every sealed file must carry the key's fingerprint: a file sealed by another key is a
/// legal document this install can no longer open.
fn vault_outcome(dir: &Path, key: Option<&str>) -> Outcome {
    if !dir.exists() {
        return Outcome::pass(format!(
            "{}: not created yet (no document stored)",
            dir.display()
        ));
    }
    let scan = match tenant_key::scan_vault(dir) {
        Ok(s) => s,
        Err(e) => {
            return Outcome::fail(e)
                .remedy("give the service account read access to the vault folder");
        }
    };
    let foreign: Vec<String> = scan
        .by_fingerprint
        .iter()
        .filter(|(fp, _)| Some(fp.as_str()) != key)
        .map(|(fp, n)| format!("{n} by key {fp}"))
        .collect();
    let name = key.unwrap_or("none");
    if !foreign.is_empty() {
        return Outcome::fail(format!(
            "{}: {} sealed file(s), this key {name}; not openable: {}",
            dir.display(),
            scan.files,
            foreign.join(", ")
        ))
        .remedy(
            "never delete these files: restore the key that sealed them (the old agent-tenant-key.bin, \
             or the platform's escrowed key) and enrol again",
        );
    }
    if scan.malformed > 0 {
        return Outcome::warn(format!(
            "{}: {} file(s) too short to be sealed",
            dir.display(),
            scan.malformed
        ))
        .remedy("look at those files; a sealed file starts with the key fingerprint");
    }
    Outcome::pass(format!(
        "{}: {} sealed file(s), all key {name}",
        dir.display(),
        scan.files
    ))
}

/// `link-status.json` next to the identity: the link's last change of state.
fn link_check(identity: &Path) -> Check {
    let file = link_status::LinkStatusFile::next_to(identity);
    let path = file.path().to_path_buf();
    let title = "Cloud link state";
    let text = match files::read_optional(&path) {
        Ok(Some(t)) => t,
        Ok(None) => {
            return Check::skip(
                NAME,
                "cloud_link",
                title,
                "the link has not run on this install",
            );
        }
        Err(e) => return Check::timed(NAME, "cloud_link", title, || Outcome::fail(e)),
    };
    let v = stj::parse(text.trim_start_matches('\u{feff}'));
    Check::timed(NAME, "cloud_link", title, || match v {
        Ok(v) => {
            let field = |k: &str| v.get(k).and_then(dl_json::Value::as_str).unwrap_or("?");
            let state = field("state");
            let at = field("atUtc");
            if state == link_status::CONNECTED {
                Outcome::pass(format!("connected since {at} (agent {})", field("agentId")))
            } else {
                Outcome::warn(format!("{state} since {at}: {}", field("problem")))
                    .remedy("see the problem; the link retries by itself")
            }
        }
        Err(_) => Outcome::warn(format!("{} does not parse", path.display())),
    })
}

/// How long a recorded problem keeps the check at `warn`; older ones are shown as history.
const PROBLEM_WARN_HOURS: i64 = 24;

/// The newest warning or error of the problem journal: `warn` while it is recent, `pass`
/// (still naming it) once it is older than [`PROBLEM_WARN_HOURS`], so a problem that is over
/// does not keep the self-test yellow for good.
fn last_problem(identity: &Path) -> Check {
    let title = "Last recorded problem";
    let journal = ProblemJournal::next_to(identity);
    match journal.latest_problem() {
        Ok(None) => Check::timed(NAME, "last_problem", title, || {
            Outcome::pass("none recorded")
        }),
        Ok(Some(p)) => {
            let at = p.last_at.as_deref().unwrap_or(&p.at);
            let detail = format!(
                "{} {} [{}] {}{}",
                at,
                p.level,
                p.source,
                p.message,
                if p.count > 1 {
                    format!(" (x{})", p.count)
                } else {
                    String::new()
                }
            );
            let recent = is_recent(at, &problem_cutoff());
            Check::timed(NAME, "last_problem", title, || {
                if recent {
                    Outcome::warn(detail).remedy("the hub's diagnostics carry the full journal")
                } else {
                    Outcome::pass(format!(
                        "none in the last {PROBLEM_WARN_HOURS} h; last: {detail}"
                    ))
                }
            })
        }
        Err(e) => Check::timed(NAME, "last_problem", title, || Outcome::warn(e)),
    }
}

/// Round-trip UTC text of the moment [`PROBLEM_WARN_HOURS`] ago.
fn problem_cutoff() -> String {
    stj::num::format_utc_o(
        &(dl_time::OffsetDateTime::now_utc() - dl_time::Duration::from_hours(PROBLEM_WARN_HOURS)),
    )
}

/// Whether a journal time (round-trip UTC text, which sorts as time) is at or after `cutoff`.
/// A time that is not in that form counts as recent: better yellow than a hidden problem.
fn is_recent(at: &str, cutoff: &str) -> bool {
    !(at.len() >= 20 && at.ends_with('Z') && at.as_bytes()[10] == b'T') || at >= cutoff
}

/// The bundle marker in the PMS against this build's.
fn bundle_outcome(target: tds::Target<'_>) -> Outcome {
    let want = &installer::bundle().hash;
    let mut conn = match tds::connect(target, None) {
        Ok(c) => c,
        Err(e) => return Outcome::fail(format!("connect: {}", tds::exec_error(&e))),
    };
    match installer::read_marker(&mut conn, PROPERTY_NAME) {
        Ok(Some(h)) if h == *want => Outcome::pass(format!("installed {h}")),
        Ok(Some(h)) => Outcome::warn(format!("installed {h}, this build ships {want}"))
            .remedy("the module installs it on start; or send the command install_procedures"),
        Ok(None) => Outcome::warn(format!("not installed (this build ships {want})"))
            .remedy("the module installs it on start; or send the command install_procedures"),
        Err(e) => Outcome::fail(e.0),
    }
}

fn picture_outcome(target: tds::Target<'_>) -> Outcome {
    match picture::probe(target) {
        Ok((f, a)) => {
            let detail = format!(
                "{}: SMILE_PICTURE {} as {}",
                a.state,
                if f.picture_exists {
                    "exists"
                } else {
                    "missing"
                },
                f.login
            );
            match a.state {
                "installed" => Outcome::pass(detail),
                "pending" => Outcome::warn(detail).remedy(a.needs.join(" | ")),
                _ => Outcome::fail(detail).remedy(a.needs.join(" | ")),
            }
        }
        Err(e) => Outcome::fail(e),
    }
}

/// An unauthenticated GET of the poll path: any answer proves the route and the protocol;
/// no envelope is sent, so nothing is taken.
fn reachable(base: &str) -> Outcome {
    let url = match cloud::check_base_url(base) {
        Ok(u) => u,
        Err(e) => {
            return Outcome::fail(e.to_string()).remedy("set CloudBaseUrl to the https:// address");
        }
    };
    let target = format!("{url}{POLL_PATH}");
    let client = Client::with_options(ClientOptions {
        timeout: Some(PROBE_TIMEOUT),
        connect_timeout: Some(PROBE_TIMEOUT.min(Duration::from_secs(10))),
        proxy: crate::agent::cloud::proxy(),
        max_redirects: 0,
        ..ClientOptions::default()
    });
    match client.get(&target) {
        Ok(r) => {
            let protocol = match r.version {
                Version::Http11 => "HTTP/1.1",
                Version::Http10 => "HTTP/1.0",
            };
            let detail = format!("{url} answered {protocol} {}", r.status.as_u16());
            if r.status.as_u16() >= 500 || r.version != Version::Http11 {
                Outcome::warn(detail).remedy("the cloud or a proxy in between is not healthy")
            } else {
                Outcome::pass(detail)
            }
        }
        Err(e) => {
            Outcome::fail(format!("{url}: {e}")).remedy("check the network or proxy to the cloud")
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_problem_older_than_the_window_is_history_not_a_warning() {
        let cutoff = "2026-11-19T16:00:00.0000000Z";
        assert!(!super::is_recent("2026-11-19T14:38:27.8649581Z", cutoff));
        assert!(super::is_recent("2026-11-19T16:00:00.0000000Z", cutoff));
        assert!(super::is_recent("2026-11-20T01:02:03.0000000Z", cutoff));
        assert!(super::is_recent("not a time", cutoff));
        assert!(super::problem_cutoff().ends_with('Z'));
    }

    use dl_agent::selftest::Status;
    use dl_json::json;

    use super::*;

    fn temp(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        std::env::temp_dir().join(format!("arr-st-{tag}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn an_install_without_identity_is_not_enrolled() {
        let dir = temp("none").join("bench").join("arrival");
        let cfg = json!({"install_dir": dir.display().to_string()});
        let checks = run(&ModuleContext::standalone(NAME, cfg, dir.clone()));
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, Status::Skip);
        assert!(checks[0].detail.contains("bench"), "{}", checks[0].detail);
        assert!(!dir.exists(), "the self-test created nothing");
    }

    #[test]
    fn a_missing_tenant_key_is_not_created() {
        let dir = temp("key");
        let layout = Layout::detect(Some(&dir), &dir);
        let settings = Settings::load(&dir).unwrap();
        let (o, fp) = tenant_key(&settings, &layout, None);
        assert_eq!(o.status, Status::Warn, "{}", o.detail);
        assert!(fp.is_none());
        assert!(!dir.exists(), "no key file was written");
    }

    #[test]
    fn a_vault_file_of_another_key_fails() {
        let dir = temp("foreign");
        let guests = dir.join("guests");
        std::fs::create_dir_all(&guests).unwrap();
        let mine = TenantKeyVault::new(&[1u8; 32]).unwrap();
        let other = TenantKeyVault::new(&[2u8; 32]).unwrap();
        std::fs::write(guests.join("a"), mine.wrap(b"a", "guests/a").unwrap()).unwrap();
        let fp = mine.fingerprint();
        assert_eq!(vault_outcome(&dir, Some(&fp)).status, Status::Pass);
        std::fs::write(guests.join("b"), other.wrap(b"b", "guests/b").unwrap()).unwrap();
        let o = vault_outcome(&dir, Some(&fp));
        assert_eq!(o.status, Status::Fail, "{}", o.detail);
        assert!(o.detail.contains(&other.fingerprint()), "{}", o.detail);
        assert!(o.remedy.unwrap().contains("never delete"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn link_state_and_last_problem_read_the_files_next_to_the_identity() {
        let dir = temp("link");
        std::fs::create_dir_all(&dir).unwrap();
        let identity = dir.join("agent-identity.bin");
        assert_eq!(link_check(&identity).status, Status::Skip);
        assert_eq!(last_problem(&identity).status, Status::Pass);
        std::fs::write(
            dir.join(link_status::FILE_NAME),
            r#"{"state":"failed","agentId":"a","kid":"k","atUtc":"2026-11-19T01:00:00+00:00","problem":"Cannot reach the cloud"}"#,
        )
        .unwrap();
        let c = link_check(&identity);
        assert_eq!(c.status, Status::Warn);
        assert!(c.detail.contains("Cannot reach the cloud"), "{}", c.detail);
        let j = ProblemJournal::next_to(&identity);
        j.record("Error", "arrival", "PMS login failed", None);
        let c = last_problem(&identity);
        assert_eq!(c.status, Status::Warn);
        assert!(c.detail.contains("PMS login failed"), "{}", c.detail);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_vault_is_fine_and_a_plain_cloud_url_fails() {
        assert_eq!(vault_outcome(&temp("vault"), None).status, Status::Pass);
        let o = reachable("http://arrival.example");
        assert_eq!(o.status, Status::Fail);
        assert!(o.remedy.unwrap().contains("CloudBaseUrl"));
    }
}
