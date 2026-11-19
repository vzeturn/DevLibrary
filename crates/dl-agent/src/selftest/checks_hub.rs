//! Core checks against the hub: the signed heartbeat, the clock, the binary and its channel,
//! the update manifest (fetched and verified, never installed) and the report spool.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dl_json::json;

use super::{CORE, Check, Outcome, PROBE_TIMEOUT, Trigger, bounded, checks_core};
use crate::cli::App;
use crate::command::CommandResult;
use crate::hub::{HttpTransport, HubClient};
use crate::identity::Identity;
use crate::manifest::Manifest;
use crate::watchdog::{self, Service};
use crate::{current_target, now};

/// A service heartbeat younger than this proves the signed link works.
pub const BEAT_FRESH_SECONDS: u64 = 600;
/// How long the command-line self-test waits for a just-started service's first heartbeat.
pub const BEAT_WAIT: Duration = Duration::from_secs(45);
/// What a command taken by the command-line heartbeat is answered with.
pub const TAKEN_BY_SELFTEST: &str =
    "sys: taken by the command-line self-test while the service was not running; send it again";

/// What the hub check learnt, for the checks after it.
pub(crate) struct Link {
    /// Signed client, when enrolled.
    pub client: Option<Arc<HubClient>>,
    /// The channel the hub assigned (empty: unknown).
    pub channel: String,
    /// Hub clock minus local clock, when known.
    pub skew: Option<i64>,
}

fn link_skip(dir: &Path) -> Link {
    Link {
        client: None,
        channel: watchdog::read_beat(dir)
            .map(|b| b.channel)
            .unwrap_or_default(),
        skew: None,
    }
}

/// Runs a hub call on its own thread with the probe deadline.
fn call<T: Send + 'static>(
    client: &Arc<HubClient>,
    name: &str,
    f: impl FnOnce(&HubClient) -> Result<T, crate::Error> + Send + 'static,
) -> Result<T, String> {
    let c = Arc::clone(client);
    match bounded(name, PROBE_TIMEOUT * 3, move || f(&c)) {
        Ok(r) => r.map_err(|e| e.to_string()),
        Err(e) => Err(format!("the hub call {e}")),
    }
}

/// The service's last heartbeat, waiting up to `wait` for a fresh one.
fn fresh_beat(dir: &Path, wait: Duration) -> (Option<watchdog::Beat>, bool) {
    let until = Instant::now() + wait;
    loop {
        let beat = watchdog::read_beat(dir);
        let fresh = beat
            .as_ref()
            .is_some_and(|b| b.hub_at > 0 && now().saturating_sub(b.hub_at) <= BEAT_FRESH_SECONDS);
        if fresh || Instant::now() >= until {
            return (beat, fresh);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// A minimal heartbeat from the command line, when the service is not running.
fn direct(app: &App, client: &Arc<HubClient>, dir: &Path) -> (Outcome, Link) {
    let id = client.identity();
    let beat = watchdog::read_beat(dir);
    let config_version = crate::config::ConfigStore::load(dir).map_or(0, |c| c.current.version);
    let body = json!({
        "agent_id": id.agent_id, "product": app.product, "version": app.version.to_string(),
        "channel": beat.map(|b| b.channel).unwrap_or_default(), "target": current_target(),
        "hostname": crate::hub::hostname(), "config_version": config_version,
    });
    let started = Instant::now();
    match call(client, "heartbeat", move |c| c.heartbeat(&body)) {
        Ok(answer) => {
            for cmd in &answer.commands {
                let r = CommandResult::new(&cmd.id, false, TAKEN_BY_SELFTEST, now());
                if let Err(e) = client.command_result(&r) {
                    dl_log::warn!("selftest: cannot hand back a command"; command = %cmd.id, error = %e);
                }
            }
            let mut detail = format!(
                "signed heartbeat accepted in {} ms (sent by the self-test: the service is not running)",
                started.elapsed().as_millis()
            );
            if !answer.commands.is_empty() {
                detail.push_str(&format!(
                    "; {} pending command(s) answered as not run",
                    answer.commands.len()
                ));
            }
            let link = Link {
                client: Some(Arc::clone(client)),
                channel: answer.channel,
                skew: Some(client.skew()),
            };
            (Outcome::pass(detail), link)
        }
        Err(e) => (
            Outcome::fail(format!("hub did not accept the signed heartbeat: {e}")).remedy(
                "check the network or proxy to the hub; a revoked agent must be enrolled again",
            ),
            Link {
                client: Some(Arc::clone(client)),
                channel: String::new(),
                skew: None,
            },
        ),
    }
}

/// Hub reachable and the signed heartbeat accepted. With the service running (or inside it)
/// the service's own last heartbeat is the proof, so no command is taken from it; otherwise
/// the self-test sends one heartbeat itself.
pub(crate) fn hub(
    app: &App,
    dir: &Path,
    id: Option<Identity>,
    service: Option<Service>,
    trigger: Trigger,
) -> (Check, Link) {
    let title = "Hub reachable, signed heartbeat accepted";
    let Some(id) = id else {
        return (
            Check::skip(CORE, "hub", title, "not enrolled"),
            link_skip(dir),
        );
    };
    let started = Instant::now();
    let client = Arc::new(HubClient::new(
        Box::new(HttpTransport::with_timeout(PROBE_TIMEOUT)),
        id,
    ));
    let via_service = trigger == Trigger::Command || service == Some(Service::Running);
    let (outcome, link) = if via_service {
        let wait = if trigger == Trigger::Command {
            Duration::ZERO
        } else {
            BEAT_WAIT
        };
        let (beat, fresh) = fresh_beat(dir, wait);
        let channel = beat.as_ref().map(|b| b.channel.clone()).unwrap_or_default();
        let skew = beat.as_ref().filter(|_| fresh).map(|b| b.skew);
        let outcome = match beat {
            Some(b) if fresh => Outcome::pass(format!(
                "the service's signed heartbeat was accepted {} s ago",
                now().saturating_sub(b.hub_at)
            )),
            Some(b) if b.hub_at > 0 => Outcome::fail(format!(
                "the service last reached the hub {} min ago",
                now().saturating_sub(b.hub_at) / 60
            )),
            _ => Outcome::fail("the service has not reached the hub yet"),
        };
        let outcome = if outcome.status == super::Status::Fail {
            outcome.remedy("check the network or proxy to the hub and logs/agent.log")
        } else {
            outcome
        };
        let link = Link {
            client: Some(client),
            channel,
            skew,
        };
        (outcome, link)
    } else {
        direct(app, &client, dir)
    };
    (
        Check::new(CORE, "hub", title, outcome, started.elapsed()),
        link,
    )
}

/// Hub clock against the local clock; the hub refuses requests more than 300 s off.
pub(crate) fn clock(skew: Option<i64>) -> Check {
    let title = "Clock in step with the hub";
    let Some(s) = skew else {
        return Check::skip(CORE, "clock", title, "hub not reached");
    };
    let outcome = match s.unsigned_abs() {
        0..=60 => Outcome::pass(format!("{s:+} s")),
        61..=300 => Outcome::warn(format!("{s:+} s; the hub refuses more than 300 s"))
            .remedy("turn on time sync (w32tm /resync or timedatectl set-ntp true)"),
        _ => Outcome::fail(format!("{s:+} s; the hub refuses more than 300 s")).remedy(
            "set the clock and turn on time sync (w32tm /resync or timedatectl set-ntp true)",
        ),
    };
    Check::new(CORE, "clock", title, outcome, Duration::ZERO)
}

/// The binary's product, version, target and channel.
pub(crate) fn version(app: &App, channel: &str) -> Check {
    let what = format!("{} {} ({})", app.product, app.version, current_target());
    let outcome = if channel.is_empty() {
        Outcome::warn(format!("{what}; channel unknown"))
            .remedy("the hub assigns the channel on the first accepted heartbeat")
    } else {
        Outcome::pass(format!("{what}; channel {channel}"))
    };
    Check::new(
        CORE,
        "version",
        "Binary version and channel",
        outcome,
        Duration::ZERO,
    )
}

fn verify_manifest(app: &App, channel: &str, text: &str) -> Outcome {
    let keys = crate::agent::release_keys();
    if keys.is_empty() {
        return Outcome::fail("this build has no release keys: every update is refused")
            .remedy("install a build from the release pipeline");
    }
    let m = match Manifest::parse(text).and_then(|m| m.verify(&keys).map(|()| m)) {
        Ok(m) => m,
        Err(e) => {
            return Outcome::fail(format!("manifest refused: {e}"))
                .remedy("tell support: the published release does not verify");
        }
    };
    if m.product != app.product || m.channel.as_str() != channel {
        return Outcome::fail(format!(
            "manifest is for {}/{}, not {}/{channel}",
            m.product, m.channel, app.product
        ));
    }
    let relation = match m.version.cmp(&app.version) {
        std::cmp::Ordering::Greater => "newer than this binary (the service installs it)",
        std::cmp::Ordering::Equal => "this binary",
        std::cmp::Ordering::Less => "older than this binary",
    };
    let detail = format!(
        "{} {channel} {} signed by {}: {relation}",
        m.product, m.version, m.key_id
    );
    if m.artifact_for(&current_target()).is_none() {
        return Outcome::warn(format!("{detail}; no build for {}", current_target()));
    }
    Outcome::pass(detail)
}

/// The update manifest for the current channel: fetched and verified, never installed.
pub(crate) fn update(app: &App, link: &Link) -> Check {
    let title = "Update manifest signed";
    let Some(client) = &link.client else {
        return Check::skip(CORE, "update", title, "not enrolled");
    };
    if link.channel.is_empty() {
        return Check::skip(CORE, "update", title, "channel unknown (hub not reached)");
    }
    let started = Instant::now();
    let (product, channel) = (app.product.clone(), link.channel.clone());
    let outcome = match call(client, "manifest", move |c| c.manifest(&product, &channel)) {
        Ok(None) => Outcome::warn(format!(
            "no release published for {}/{}",
            app.product, link.channel
        )),
        Ok(Some(text)) => verify_manifest(app, &link.channel, &text),
        Err(e) => Outcome::fail(format!("cannot fetch the manifest: {e}")),
    };
    Check::new(CORE, "update", title, outcome, started.elapsed())
}

/// The data directory takes writes, the spool on disk parses and the hub accepts reports.
pub(crate) fn spool(dir: &Path, link: &Link) -> Check {
    let title = "Report spool writable and flushable";
    let started = Instant::now();
    let disk = checks_core::spool_disk(dir);
    let outcome = match (&link.client, disk.status) {
        (_, super::Status::Fail) => disk,
        (None, _) => Outcome {
            detail: format!("{}; not enrolled: flush not tried", disk.detail),
            ..disk
        },
        (Some(client), _) => {
            let host = crate::hub::hostname();
            match call(client, "reports", move |c| c.reports(&[], &host)) {
                Ok(_) => Outcome {
                    detail: format!("{}; the hub accepts reports", disk.detail),
                    ..disk
                },
                Err(e) => Outcome::fail(format!("{}; the hub refused reports: {e}", disk.detail)),
            }
        }
    };
    Check::new(CORE, "spool", title, outcome, started.elapsed())
}
