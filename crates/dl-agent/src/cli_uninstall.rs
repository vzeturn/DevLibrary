//! `uninstall`: tells the hub, removes the watchdog, the service and (Windows) the installed
//! program files and Defender exclusion. The data directory (identity, spool, logs) stays unless
//! `--purge`, so a reinstall keeps the machine's identity; `--keep-files` keeps the program
//! files for an installer that replaces them right after. `irm <hub>/uninstall.ps1 | iex`
//! runs this.

use std::path::Path;
use std::time::Duration;

use dl_winsvc::codes;

use crate::cli::{App, EXIT_CONFIG, svc, systemctl, unit_path};
use crate::cli_install::{Failure, install_exe, running_from};
use crate::hub::{HttpTransport, HubClient};
use crate::identity::Identity;
use crate::report::{ProblemReport, Severity};
use crate::{Error, now};

/// Runs `uninstall`; returns the exit code.
pub fn main(app: &App, dir: &Path, purge: bool, files: bool) -> i32 {
    tell_hub(app, dir, purge);
    // First, so the watchdog does not start the service again while it is removed.
    crate::service_watch::unregister(app);
    let removed = if cfg!(windows) {
        windows(app, dir, files)
    } else if cfg!(target_os = "linux") {
        linux(app)
    } else {
        Err(Error::Config(
            "service uninstall is supported on Windows and Linux (systemd)".into(),
        ))
    };
    if let Err(e) = removed {
        return Failure::new(
            e.to_string(),
            "run it as Administrator (root on Linux)",
            EXIT_CONFIG,
        )
        .print();
    }
    if purge && dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(dir) {
            return Failure::new(
                format!("cannot delete {}: {e}", dir.display()),
                "delete it by hand",
                EXIT_CONFIG,
            )
            .print();
        }
        println!("Deleted {}", dir.display());
    } else if dir.exists() {
        println!(
            "Kept {} (identity, spool, logs); --purge deletes it",
            dir.display()
        );
    }
    println!("{} removed", app.display);
    0
}

/// An `agent_uninstalled` report, so the hub shows why the agent went quiet. Not fatal.
fn tell_hub(app: &App, dir: &Path, purge: bool) {
    let Ok(id) = Identity::load(dir) else {
        return;
    };
    let msg = format!(
        "{} {} uninstalled on this machine{}",
        app.product,
        app.version,
        if purge {
            "; its identity was deleted"
        } else {
            ""
        }
    );
    let r = ProblemReport::new(Severity::Info, "agent", "agent_uninstalled", &msg, now());
    let client = HubClient::new(Box::new(HttpTransport::new()), id);
    if let Err(e) = client.reports(&[r], &crate::hub::hostname()) {
        eprintln!("Warning: the hub was not told ({e}); it will show the agent as silent");
    }
}

fn linux(app: &App) -> Result<(), Error> {
    systemctl(&["disable", "--now", &app.service])?;
    crate::error::io("remove unit", std::fs::remove_file(unit_path(app)))?;
    systemctl(&["daemon-reload"])
}

fn windows(app: &App, dir: &Path, files: bool) -> Result<(), Error> {
    let manager = dl_winsvc::scm::Manager::connect().map_err(svc)?;
    match manager.open(&app.service) {
        Ok(service) => {
            if let Err(e) = service.stop_and_wait(Duration::from_secs(30)) {
                eprintln!("Warning: stop: {e}");
            }
            match service.delete() {
                Ok(()) => println!("Service {} removed", app.service),
                Err(e) if e.code() == Some(codes::SERVICE_MARKED_FOR_DELETE) => {}
                Err(e) => return Err(svc(e)),
            }
        }
        Err(e) if e.code() == Some(codes::SERVICE_DOES_NOT_EXIST) => {
            println!("Service {} was not installed", app.service);
        }
        Err(e) => return Err(svc(e)),
    }
    if !files {
        return Ok(());
    }
    let exe = install_exe(&app.service, crate::cli_install::env_var);
    let Some(install_dir) = exe.parent() else {
        return Ok(());
    };
    remove_defender_exclusion(install_dir, dir);
    remove_program(&exe, running_from(&exe))
}

fn remove_defender_exclusion(install_dir: &Path, dir: &Path) {
    let q = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "''"));
    let script = format!(
        "if (Get-Command Remove-MpPreference -ErrorAction SilentlyContinue) {{ Remove-MpPreference -ExclusionPath @({}, {}) }}",
        q(install_dir),
        q(dir)
    );
    if let Err(e) = crate::service_watch::run_tool(
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command", &script],
    ) {
        eprintln!("Warning: Windows Defender exclusion not removed: {e}");
    }
}

/// The program and the copies an update leaves next to it (`.new`, `.old`): only these, so
/// files someone else put in the folder are never deleted.
pub fn program_files(exe: &Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![exe.to_path_buf()];
    for ext in ["exe.new", "exe.old"] {
        out.push(exe.with_extension(ext));
    }
    out
}

/// Deletes the program files, then the folder when it is empty; when this binary runs from
/// it, a detached `cmd` does that a few seconds after this process exits.
fn remove_program(exe: &Path, from_it: bool) -> Result<(), Error> {
    let Some(install_dir) = exe.parent() else {
        return Ok(());
    };
    if !install_dir.exists() {
        return Ok(());
    }
    let files = program_files(exe);
    if !from_it {
        for f in files.iter().filter(|f| f.exists()) {
            crate::error::io("remove a program file", std::fs::remove_file(f))?;
        }
        if let Err(e) = std::fs::remove_dir(install_dir) {
            eprintln!("Warning: kept {} ({e})", install_dir.display());
        } else {
            println!("Deleted {}", install_dir.display());
        }
        return Ok(());
    }
    let quoted: Vec<String> = files
        .iter()
        .map(|f| format!("\"{}\"", f.display()))
        .collect();
    let script = format!(
        "ping 127.0.0.1 -n 4 > nul & del /f /q {} 2> nul & rmdir \"{}\" 2> nul",
        quoted.join(" "),
        install_dir.display()
    );
    crate::error::io("schedule the program removal", spawn_cmd(&script))?;
    println!("{} is deleted in a few seconds", install_dir.display());
    Ok(())
}

/// `cmd /d /c <script>` with no window, the script passed as written (cmd does not take the
/// `\"` quoting Rust would apply).
#[cfg(windows)]
fn spawn_cmd(script: &str) -> std::io::Result<std::process::Child> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("cmd.exe")
        .raw_arg(format!("/d /c {script}"))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
}

#[cfg(not(windows))]
fn spawn_cmd(_script: &str) -> std::io::Result<std::process::Child> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "cmd.exe is Windows only",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_program_and_its_update_copies_are_deleted() {
        let exe = Path::new("C:/Program Files/DevLibrary/dl-agent/dl-agent.exe");
        let names: Vec<String> = program_files(exe)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            ["dl-agent.exe", "dl-agent.exe.new", "dl-agent.exe.old"]
        );
    }
}
