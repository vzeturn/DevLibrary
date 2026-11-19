//! The module's self-test (`dl-agent selftest`, the `selftest` command): the appsettings in
//! the module's configured folder bind and validate, the token file reads, and the PMS
//! device-agent API answers. Read only: no command is claimed and no device is touched
//! (a claim would take a key-card or print job from the queue).

use std::time::Duration;

use dl_agent::selftest::{Check, Outcome, PROBE_TIMEOUT, SelfTest};
use dl_agent::supervisor::ModuleContext;

use super::NAME;
use super::prepare::{Prepared, prepare, process_env};
use super::settings::Settings;

/// The self-test to register with `App::selftest`.
pub fn pms_device_selftest() -> SelfTest {
    SelfTest { module: NAME, run }
}

fn run(ctx: &ModuleContext) -> Vec<Check> {
    let settings = Settings::from_config(ctx.config());
    let dir = settings.appsettings_dir(ctx.data_dir());
    let title = "Device agent settings bind";
    if !dir.join("appsettings.json").exists() {
        let reason = format!("not enrolled: no appsettings.json in {}", dir.display());
        return vec![Check::skip(NAME, "config", title, &reason)];
    }
    let ready = match prepare(&settings, ctx.data_dir(), &process_env()) {
        Prepared::Waiting(detail) => {
            let o = Outcome::fail(format!("{}: {detail}", dir.display()))
                .remedy("fix appsettings.json or the token file the detail names");
            return vec![Check::timed(NAME, "config", title, || o)];
        }
        Prepared::Ready(r) => r,
    };
    let o = &ready.options;
    let config = Outcome::pass(format!(
        "{} ({}): adapter {}, agent {}; token file readable",
        ready.content_root.display(),
        ready.environment,
        if o.adapter.is_empty() {
            "default"
        } else {
            &o.adapter
        },
        o.agent_id
    ));
    let base = o.api_base_url.clone();
    vec![
        Check::timed(NAME, "config", title, || config),
        Check::bounded(
            NAME,
            "api",
            "PMS device API reachable",
            PROBE_TIMEOUT * 2,
            move || reachable(&base),
        ),
    ]
}

/// An unauthenticated GET of the API base: any HTTP answer proves the route; nothing is
/// claimed.
fn reachable(base: &str) -> Outcome {
    let client = dl_http::Client::with_options(dl_http::ClientOptions {
        timeout: Some(PROBE_TIMEOUT),
        connect_timeout: Some(PROBE_TIMEOUT.min(Duration::from_secs(10))),
        max_redirects: 0,
        ..dl_http::ClientOptions::default()
    });
    match client.get(base) {
        Ok(r) => Outcome::pass(format!("{base} answered HTTP {}", r.status.as_u16())),
        Err(e) => Outcome::fail(format!("{base}: {e}"))
            .remedy("check the network, proxy or ApiBaseUrl in appsettings.json"),
    }
}

#[cfg(test)]
mod tests {
    use dl_agent::selftest::Status;
    use dl_json::json;

    use super::*;

    #[test]
    fn a_workstation_without_appsettings_is_not_enrolled() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("pmsdev-st-{}-{nanos}", std::process::id()));
        let cfg = json!({"appsettings_dir": dir.join("bench").display().to_string()});
        let checks = run(&ModuleContext::standalone(NAME, cfg, dir));
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, Status::Skip);
        assert!(checks[0].detail.contains("bench"), "{}", checks[0].detail);
    }

    #[test]
    fn an_unreachable_api_fails() {
        let o = reachable("http://127.0.0.1:9/");
        assert_eq!(o.status, Status::Fail);
        assert!(o.remedy.is_some());
    }
}
