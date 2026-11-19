//! `PdaSyncAgent status [--wait <seconds>]` (port of `Hosting/StatusCommand.cs`).
//!
//! Without `--wait`: what is enrolled and the last state, exit 0. With it (the install
//! script, after starting the service): reads `status.json` every 5 s until the service
//! itself wrote `healthy` after the wait began (exit 0), or the time runs out (exit 1,
//! with the last state, code and hint). A `healthy` from before the wait does not count.

use super::cli::{EXIT_FAILED, EXIT_OK, EXIT_USAGE, STATUS_USAGE, unhandled};
use crate::config::health::{HEALTHY, REENROLL_REQUIRED};
use crate::config::time::{Clock, SystemClock, Timestamp};
use crate::config::{AgentIdentity, AgentPaths, AgentStatus, ReadError, codes};
use crate::diagnostics::SiteEventQueueStats;
use crate::util::console::LineWriter;
use crate::util::ctx::Ctx;
use std::time::Duration;

use super::cli::Options;

/// How often `--wait` reads `status.json`.
pub const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Waits for `delay` unless `ctx` ends first (`false` then); tests advance a clock.
pub type Sleep<'a> = &'a mut dyn FnMut(Duration, &Ctx) -> bool;

/// The verb on the real console and clock.
pub fn run_console(
    paths: &AgentPaths,
    opts: &Options,
    stdout: &dyn LineWriter,
    stderr: &dyn LineWriter,
) -> i32 {
    run(
        paths,
        opts,
        stdout,
        stderr,
        &SystemClock,
        &mut |d, ctx| ctx.sleep(d),
        &Ctx::background(),
    )
}

/// `NumberStyles.None` int parse: ASCII digits only, within `i32`.
fn parse_whole(raw: &str) -> Option<i32> {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    raw.parse().ok()
}

/// `StatusCommand.RunAsync`.
pub fn run(
    paths: &AgentPaths,
    opts: &Options,
    stdout: &dyn LineWriter,
    stderr: &dyn LineWriter,
    clock: &dyn Clock,
    sleep: Sleep<'_>,
    ctx: &Ctx,
) -> i32 {
    let mut wait_seconds = None;
    if let Some(raw) = opts.get("wait") {
        match parse_whole(raw) {
            Some(s) if s > 0 => wait_seconds = Some(s),
            _ => {
                stderr.write_line(&format!(
                    "[{}] --wait needs a whole number of seconds greater than 0. Usage: {STATUS_USAGE}",
                    codes::BAD_ARGUMENTS
                ));
                return EXIT_USAGE;
            }
        }
    }

    let start = clock.now();
    let id = match AgentIdentity::try_load(paths) {
        Ok(id) => id,
        Err(e) => return unhandled(stderr, &e),
    };
    stdout.write_line(&match &id {
        None => "Not enrolled.".to_owned(),
        Some(id) => format!(
            "Agent {} tenant {} -> {}",
            id.agent_id_text(),
            id.tenant_id,
            id.cloud_url
        ),
    });
    let Some(wait) = wait_seconds else {
        if let Some(st) = read(paths, stderr) {
            stdout.write_line(&state(&st));
        }
        stdout.write_line(&format!(
            "Site events: {}",
            SiteEventQueueStats::read(&paths.events_db()).describe(clock.now())
        ));
        return EXIT_OK;
    };
    if id.is_none() {
        stderr.write_line(&format!(
            "[{}] The service cannot become healthy before this machine is enrolled.",
            codes::NOT_ENROLLED
        ));
        return EXIT_FAILED;
    }
    wait_healthy(paths, stdout, stderr, clock, sleep, ctx, start, wait)
}

#[allow(clippy::too_many_arguments)]
fn wait_healthy(
    paths: &AgentPaths,
    stdout: &dyn LineWriter,
    stderr: &dyn LineWriter,
    clock: &dyn Clock,
    sleep: Sleep<'_>,
    ctx: &Ctx,
    start: Timestamp,
    wait: i32,
) -> i32 {
    let wait_d = Duration::from_secs(u64::try_from(wait).unwrap_or(0));
    let deadline = start.add(wait_d);
    stdout.write_line(&format!(
        "Waiting up to {wait} s for the service to report healthy..."
    ));
    let last = loop {
        let last = read(paths, stderr);
        if let Some(st) = &last {
            if st.state == HEALTHY && st.at_utc.unix_nanos() > start.unix_nanos() {
                stdout.write_line(&describe(st));
                return EXIT_OK;
            }
            if st.state == REENROLL_REQUIRED {
                break last; // stays until a new enroll: waiting longer changes nothing
            }
        }
        let now = clock.now();
        if now.unix_nanos() >= deadline.unix_nanos() {
            break last;
        }
        let left_nanos = u64::try_from(deadline.unix_nanos() - now.unix_nanos()).unwrap_or(0);
        let left = Duration::from_nanos(left_nanos);
        if !sleep(left.min(POLL_INTERVAL), ctx) {
            return unhandled(
                stderr,
                &"System.OperationCanceledException: The operation was canceled.",
            );
        }
    };

    let stale = match &last {
        Some(st) if st.at_utc.unix_nanos() <= start.unix_nanos() => {
            " (written before the wait began; the service has not reported since)"
        }
        _ => "",
    };
    stdout.write_line(&match &last {
        None => "State: none (the service has not written its status yet)".to_owned(),
        Some(st) => describe(st) + stale,
    });
    stderr.write_line(&match &last {
        Some(st) if st.state == REENROLL_REQUIRED => format!(
            "[{}] The service needs a new enrollment: {}",
            codes::REENROLL_REQUIRED,
            st.hint.as_deref().unwrap_or("")
        ),
        _ => format!("The service did not report healthy within {wait} s."),
    });
    EXIT_FAILED
}

/// `State: <state> <code> <hint>`, trailing spaces trimmed.
fn state(st: &AgentStatus) -> String {
    format!(
        "State: {} {} {}",
        st.state,
        st.error_code.as_deref().unwrap_or(""),
        st.hint.as_deref().unwrap_or("")
    )
    .trim_end()
    .to_owned()
}

/// [`state`] and ` (at yyyy-MM-dd HH:mm:ssZ)` (`"u"`: the instant at UTC).
fn describe(st: &AgentStatus) -> String {
    let (y, mo, d, h, mi, s, _) = st.at_utc.to_utc().parts();
    format!(
        "{} (at {y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}Z)",
        state(st)
    )
}

/// The status file, or `None` when it is missing or being replaced (read again on the
/// next poll).
fn read(paths: &AgentPaths, stderr: &dyn LineWriter) -> Option<AgentStatus> {
    match AgentStatus::try_read(paths) {
        Ok(st) => st,
        Err(e) => {
            let kind = match &e {
                ReadError::Io(io) if io.kind() == std::io::ErrorKind::PermissionDenied => {
                    "UnauthorizedAccessException"
                }
                ReadError::Io(_) => "IOException",
                ReadError::Json(_) => "JsonException",
            };
            stderr.write_line(&format!(
                "warning: {} could not be read ({kind}: {e})",
                paths.status_file().display()
            ));
            None
        }
    }
}
