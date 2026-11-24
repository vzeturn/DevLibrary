//! Self-test after an install or an upgrade (RT-1054): the first start of a new version runs
//! the self-test once (`Trigger::Install`), keeps it and sends it to the hub. The marker file
//! [`MARKER_FILE`] holds the version that already ran it. Everything here is best-effort: a
//! failure is logged and never changes the agent's exit code.

use std::path::{Path, PathBuf};

use super::execute::{execute, save_last, upload_note};
use super::{Run, Trigger};
use crate::cli::App;

/// The version whose post-install self-test has run, kept in the data directory.
pub const MARKER_FILE: &str = "selftest.installed";

fn marker(dir: &Path) -> PathBuf {
    dir.join(MARKER_FILE)
}

/// Whether the running `version` has not had its post-install self-test yet (no marker, an
/// unreadable marker, or a marker of another version).
pub fn needed(dir: &Path, version: &str) -> bool {
    match std::fs::read_to_string(marker(dir)) {
        Ok(text) => text.trim() != version,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => {
            dl_log::warn!("selftest: cannot read the post-install marker"; error = %e);
            true
        }
    }
}

/// Runs the self-test with [`Trigger::Install`], keeps it, uploads it and writes the marker.
/// The marker is written even when the upload fails: the run is kept locally and a retry on
/// every start would only repeat it.
pub fn run(app: &App, dir: &Path) -> Run {
    let run = execute(app, dir, Trigger::Install, None, &dl_json::Value::Null);
    save_last(&run, dir);
    let note = upload_note(&run, dir);
    dl_log::info!("post-install selftest finished"; run_id = %run.run_id, summary = %dl_json::to_string(&run.summary()), upload = %note);
    if let Err(e) = std::fs::write(marker(dir), app.version.to_string()) {
        dl_log::warn!("selftest: cannot write the post-install marker"; error = %e);
    }
    run
}

/// Starts [`run`] on its own thread when [`needed`]. Never blocks and never fails the caller.
pub fn spawn_if_needed(app: &App, dir: &Path) {
    let version = app.version.to_string();
    if !needed(dir, &version) {
        return;
    }
    let (app, dir) = (app.clone(), dir.to_path_buf());
    let spawned = std::thread::Builder::new()
        .name("selftest-post-install".into())
        .spawn(move || {
            run(&app, &dir);
        });
    if let Err(e) = spawned {
        dl_log::warn!("selftest: cannot start the post-install run"; error = %e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testdir::TestDir;

    fn app() -> App {
        App::new("dl-agent-test", "Test agent")
    }

    #[test]
    fn needed_compares_the_marker_with_the_running_version() {
        let d = TestDir::new("st-post-needed");
        assert!(needed(&d.0, "1.0.0"), "no marker yet");
        std::fs::write(d.0.join(MARKER_FILE), "1.0.0\n").unwrap();
        assert!(!needed(&d.0, "1.0.0"));
        assert!(needed(&d.0, "1.0.1"), "an upgrade needs a new run");
    }

    #[test]
    fn run_keeps_the_run_and_writes_the_marker() {
        let d = TestDir::new("st-post-run");
        let a = app();
        let r = run(&a, &d.0);
        assert_eq!(r.trigger, Trigger::Install);
        assert!(d.0.join(super::super::LAST_RUN_FILE).is_file());
        let text = std::fs::read_to_string(d.0.join(MARKER_FILE)).unwrap();
        assert_eq!(text, a.version.to_string());
        assert!(!needed(&d.0, &a.version.to_string()));
    }
}
