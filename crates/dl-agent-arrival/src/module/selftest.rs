//! The module's self-test (`dl-agent selftest`, the `selftest` command): the identity, the
//! tenant key and the legal vault under the module's `install_dir` read, the cloud answers
//! over HTTP/1.1, and the PMS connection runs the agent's read probe. Read only: no poll is
//! sent (a poll would take the cloud's requests), no key or vault file is created.

use std::path::Path;
use std::time::Duration;

use dl_agent::selftest::{Check, Outcome, PROBE_TIMEOUT, SelfTest};
use dl_agent::supervisor::ModuleContext;
use dl_http::{Client, ClientOptions, Proxy, Version};

use super::{ModuleConfig, NAME, NOT_ENROLLED};
use crate::agent::cloud::{self, POLL_PATH};
use crate::agent::layout::Layout;
use crate::agent::secrets::{Identity, TenantKeyStore};
use crate::agent::settings::Settings;
use crate::agent::tds;

/// The self-test to register with `App::selftest`.
pub fn arrival_selftest() -> SelfTest {
    SelfTest { module: NAME, run }
}

fn run(ctx: &ModuleContext) -> Vec<Check> {
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
    let settings = match Settings::load(layout.install_dir()) {
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
    checks.push(Check::timed(NAME, "tenant_key", "Tenant key reads", || {
        tenant_key(&settings, &layout)
    }));
    let vault = layout.resolve(&settings.vault_directory);
    checks.push(Check::timed(NAME, "vault", "Legal vault reads", || {
        vault_outcome(&vault)
    }));
    let base = settings.cloud_base_url.clone();
    checks.push(Check::bounded(
        NAME,
        "cloud",
        "Cloud reachable over HTTP/1.1",
        PROBE_TIMEOUT * 2,
        move || reachable(&base),
    ));
    let connection = settings.pms_connection_string;
    checks.push(Check::bounded(
        NAME,
        "pms",
        "PMS read query runs",
        PROBE_TIMEOUT * 2,
        move || match tds::probe(&connection) {
            Ok(p) => Outcome::pass(p.line()),
            Err(e) if e.is_tls() => Outcome::fail(format!("TLS handshake failed: {e}"))
                .remedy("set Encrypt=False (old SQL Server) or install a TLS 1.2 certificate"),
            Err(e) => {
                Outcome::fail(e.to_string()).remedy("check SQL Server and PmsConnectionString")
            }
        },
    ));
    checks
}

fn tenant_key(settings: &Settings, layout: &Layout) -> Outcome {
    let file = layout.resolve(&settings.tenant_key_file);
    let source = if settings.tenant_key.trim().is_empty() {
        file.display().to_string()
    } else {
        "Agent:TenantKey (settings)".to_owned()
    };
    match TenantKeyStore::new(&settings.tenant_key, file).load_existing() {
        Ok(Some(k)) => Outcome::pass(format!("{source}: a {}-byte key opens", k.len())),
        Ok(None) => Outcome::warn(format!(
            "{source}: no key yet; the service creates it on the first sealed field"
        ))
        .remedy("start the service once; a site that had a key must restore that file"),
        Err(e) => Outcome::fail(format!("{source}: {e}"))
            .remedy("the key was sealed on another machine or account; restore it from the old PC"),
    }
}

fn vault_outcome(dir: &Path) -> Outcome {
    match std::fs::read_dir(dir) {
        Ok(entries) => Outcome::pass(format!("{}: {} entr(ies)", dir.display(), entries.count())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Outcome::pass(format!(
            "{}: not created yet (no document stored)",
            dir.display()
        )),
        Err(e) => Outcome::fail(format!("{}: {e}", dir.display()))
            .remedy("give the service account read access to the vault folder"),
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
        proxy: Proxy::FromEnv,
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
        let o = tenant_key(&settings, &layout);
        assert_eq!(o.status, Status::Warn, "{}", o.detail);
        assert!(!dir.exists(), "no key file was written");
    }

    #[test]
    fn a_missing_vault_is_fine_and_a_plain_cloud_url_fails() {
        assert_eq!(vault_outcome(&temp("vault")).status, Status::Pass);
        let o = reachable("http://arrival.example");
        assert_eq!(o.status, Status::Fail);
        assert!(o.remedy.unwrap().contains("CloudBaseUrl"));
    }
}
