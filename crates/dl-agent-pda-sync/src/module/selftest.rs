//! The module's self-test (`dl-agent selftest`, the `selftest` command). Read only and
//! bounded: `agent.json` loads, the last `status.json` state, the PDA cloud's ready route
//! answers, `pda_sync` signs in to `SMILE_POS` and reads the captured and agent tables,
//! the order grants are in place, the data folder is writable, and the print relay
//! settings. Nothing is enrolled, captured, queued or sent.

use std::sync::Arc;

use dl_agent::selftest::{Check, Outcome, PROBE_TIMEOUT, SelfTest};
use dl_agent::supervisor::ModuleContext;

use super::{NAME, NOT_ENROLLED, settings};
use crate::bootstrap::cloud_probe::{CloudProbe, HttpCloudProbe};
use crate::bootstrap::models::SqlInstance;
use crate::bootstrap::probes::{
    AgentLoginProbe, OrderGrantsProbe, SqlAgentLoginProbe, SqlOrderGrantsProbe,
};
use crate::config::health::{DEGRADED, HEALTHY, REENROLL_REQUIRED};
use crate::config::{AgentIdentity, AgentOptions, AgentPaths, AgentStatus};
use crate::printrelay::policy::PrinterTargetPolicy;
use crate::security::names;
use crate::util::ctx::Ctx;

const REENROLL: &str = "enroll again: dl-agent pda-enroll --code <code> --url <PDA cloud url>";
const GRANTS: &str = "apply the order grants: dl-agent pda-grants (as a SQL administrator)";

/// The self-test to register with `App::selftest`.
pub fn pda_sync_selftest() -> SelfTest {
    SelfTest { module: NAME, run }
}

fn run(ctx: &ModuleContext) -> Vec<Check> {
    checks(ctx)
}

/// The checks over the module's folder and settings.
pub fn checks(ctx: &ModuleContext) -> Vec<Check> {
    let host = match settings::host_context(ctx.config(), ctx.data_dir()) {
        Ok(h) => h,
        Err(e) => {
            let o = Outcome::fail(e).remedy("fix modules.pda-sync in the hub config");
            return vec![Check::timed(NAME, "settings", "Settings bind", || o)];
        }
    };
    let title = "Enrolled (agent.json)";
    let identity = match AgentIdentity::try_load(&host.paths) {
        Ok(Some(id)) => id,
        Ok(None) => return vec![Check::skip(NAME, "enrolled", title, NOT_ENROLLED)],
        Err(e) => {
            let o = Outcome::fail(format!("agent.json: {e}")).remedy(REENROLL);
            return vec![Check::timed(NAME, "enrolled", title, || o)];
        }
    };
    let detail = format!(
        "agent {} of tenant {} at {}",
        identity.agent_id_text(),
        identity.tenant_id,
        identity.cloud_url
    );
    let mut out = vec![
        Check::timed(NAME, "enrolled", title, || Outcome::pass(detail)),
        state(&host.paths),
        cloud(&identity.cloud_url),
    ];
    out.extend(sql(&host.paths, &identity));
    out.push(data_folder(&host.paths));
    out.push(print_relay(&host.options));
    out
}

/// The last state the runtime wrote.
fn state(paths: &AgentPaths) -> Check {
    let o = match AgentStatus::try_read(paths) {
        Ok(None) => Outcome::warn("no status.json yet: the module has not run a cycle"),
        Err(e) => Outcome::fail(format!("status.json: {e}")),
        Ok(Some(s)) => {
            let mut d = s.state.clone();
            if let Some(c) = &s.error_code {
                d.push_str(&format!(" [{c}]"));
            }
            if let Some(h) = &s.hint {
                d.push_str(&format!(": {h}"));
            }
            d.push_str(&format!(" at {}", s.at_utc.to_stj()));
            match s.state.as_str() {
                HEALTHY => Outcome::pass(d),
                REENROLL_REQUIRED => Outcome::fail(d).remedy(REENROLL),
                DEGRADED => Outcome::warn(d),
                _ => Outcome::warn(d),
            }
        }
    };
    Check::timed(NAME, "state", "Runtime state (status.json)", || o)
}

/// `GET /ready` on the PDA cloud, with the DNS and clock-skew findings of enroll.
fn cloud(url: &str) -> Check {
    let url = url.to_owned();
    Check::bounded(
        NAME,
        "cloud",
        "PDA cloud answers",
        PROBE_TIMEOUT * 3,
        move || {
            let r = HttpCloudProbe::create_default().probe(&url, &Ctx::background());
            let text = r
                .findings
                .iter()
                .map(|f| format!("[{}] {}", f.code, f.message))
                .collect::<Vec<_>>()
                .join("; ");
            let status = r
                .http_status
                .map_or_else(|| "no answer".to_owned(), |s| format!("HTTP {s}"));
            if !r.ok() {
                Outcome::fail(text).remedy("check the network, firewall or proxy to the PDA cloud")
            } else if !r.findings.is_empty() {
                Outcome::warn(format!("{url} ready ({status}); {text}"))
            } else {
                Outcome::pass(format!("{url} ready ({status})"))
            }
        },
    )
}

/// `pda_sync` signs in and reads, and has the order grants.
fn sql(paths: &AgentPaths, identity: &AgentIdentity) -> Vec<Check> {
    let title = "pda_sync reads SMILE_POS";
    let (Some(instance), Some(database)) =
        (identity.sql_instance.clone(), identity.sql_database.clone())
    else {
        let o = Outcome::fail("agent.json has no SQL target").remedy(REENROLL);
        return vec![Check::timed(NAME, "sql", title, || o)];
    };
    let secrets = crate::security::create(paths);
    let password = match secrets.load(names::SQL_PASSWORD) {
        Ok(Some(p)) => String::from_utf8_lossy(&p).into_owned(),
        Ok(None) => {
            let o = Outcome::fail("the pda_sync password is missing").remedy(REENROLL);
            return vec![Check::timed(NAME, "sql", title, || o)];
        }
        Err(e) => {
            let o = Outcome::fail(format!("secret store: {e}")).remedy(REENROLL);
            return vec![Check::timed(NAME, "sql", title, || o)];
        }
    };
    crate::reporting::scrubber::register(Some(&password));
    let at = format!("{instance}/{database}");
    let password = Arc::new(password);
    let (i, d, p, a) = (
        instance.clone(),
        database.clone(),
        Arc::clone(&password),
        at.clone(),
    );
    let login = Check::bounded(NAME, "sql", title, PROBE_TIMEOUT * 2, move || {
        let inst = SqlInstance::new(i, "agent.json");
        match SqlAgentLoginProbe.verify(&inst, &d, &p, &Ctx::background()) {
            Ok(()) => Outcome::pass(format!(
                "{a}: dbo.TmpInvoice and the agent tables read as pda_sync"
            )),
            Err(e) => Outcome::fail(format!("{a}: {e}")).remedy(REENROLL),
        }
    });
    let grants = Check::bounded(
        NAME,
        "grants",
        "Order grants in place",
        PROBE_TIMEOUT * 2,
        move || match SqlOrderGrantsProbe.missing(
            &instance,
            &database,
            &password,
            &Ctx::background(),
        ) {
            Ok(m) if m.is_empty() => Outcome::pass(format!("{at}: order grants in place")),
            Ok(m) => Outcome::fail(format!("{at}: missing {}", m.join(", "))).remedy(GRANTS),
            Err(e) => Outcome::fail(format!("{at}: {e}")),
        },
    );
    vec![login, grants]
}

/// The data folder takes a write (the queues and state live there).
fn data_folder(paths: &AgentPaths) -> Check {
    Check::timed(NAME, "data", "Data folder writable", || {
        let dir = paths.data_dir();
        let probe = dir.join(format!(".selftest-{}", std::process::id()));
        match std::fs::write(&probe, b"ok").and_then(|()| std::fs::remove_file(&probe)) {
            Ok(()) => Outcome::pass(format!("{} writable", dir.display())),
            Err(e) => Outcome::fail(format!("{}: {e}", dir.display()))
                .remedy("give the dl-agent service account write access to its data folder"),
        }
    })
}

/// The print relay settings (ADR 0098: on by default at sites without a speaker). With no
/// `PrinterSubnets` the relay prints to any private LAN address (RT-1309, ADR 0010).
fn print_relay(options: &AgentOptions) -> Check {
    let p = &options.print_relay;
    let o = if !p.enabled {
        Outcome::pass("off")
    } else {
        match PrinterTargetPolicy::create(p, None) {
            Ok(policy) => Outcome::pass(format!(
                "on: prints to {}, queue discovery {}",
                policy.describe(),
                if p.discover_queues { "on" } else { "off" }
            )),
            Err(e) => Outcome::fail(format!("the print relay stays off: {e}"))
                .remedy("fix modules.pda-sync.PrintRelay in the hub config"),
        }
    };
    Check::timed(NAME, "print_relay", "Print relay settings", || o)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dl_agent::selftest::Status;

    #[test]
    fn print_relay_with_no_subnet_prints_to_any_private_lan() {
        let c = print_relay(&AgentOptions::default());
        assert_eq!(c.status, Status::Pass, "{}", c.detail);
        assert!(
            c.detail
                .starts_with("on: prints to any private LAN address (10.0.0.0/8"),
            "{}",
            c.detail
        );
    }

    #[test]
    fn print_relay_with_a_bad_subnet_fails_with_the_reason() {
        let mut o = AgentOptions::default();
        o.print_relay.printer_subnets = vec![Some("8.8.8.0/24".into())];
        let c = print_relay(&o);
        assert_eq!(c.status, Status::Fail);
        assert!(c.detail.contains("8.8.8.0/24"), "{}", c.detail);
    }
}
