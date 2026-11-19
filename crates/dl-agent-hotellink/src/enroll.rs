//! The `hl-enroll` sub-command: writes the HL `config.json` the way the hub's install script
//! does (local `SMILE_FO`, secrets DPAPI-encrypted). The hotel token comes from
//! `DL_AGENT_HL_TOKEN` or the first line of standard input, the SQL password (with
//! `--sql-user`) from `DL_AGENT_HL_SQL_PASSWORD` or the next line: never from the command
//! line, where other users of the machine could read them.

use std::io::{BufRead, Write};
use std::path::Path;

use dl_agent::Error;
use dl_agent::tool::{Tool, ToolContext};
use dl_cli::{Arg, Command, Matches};

use crate::config::{self, Enrollment};

/// The registered sub-command name.
pub const NAME: &str = "hl-enroll";
/// Where the hotel token can come from instead of standard input.
pub const TOKEN_ENV: &str = "DL_AGENT_HL_TOKEN";
/// Where the SQL password can come from instead of standard input.
pub const PASSWORD_ENV: &str = "DL_AGENT_HL_SQL_PASSWORD";

/// The tool to register with `App::tool`.
pub fn hl_enroll_tool() -> Tool {
    Tool {
        name: NAME,
        grammar,
        run,
    }
}

fn grammar(cmd: Command) -> Command {
    cmd.about("Writes the SMILE Hotel Link config.json (token from DL_AGENT_HL_TOKEN or stdin)")
        .arg(Arg::option("hub-url").required().help("HL hub base URL"))
        .arg(
            Arg::option("hotel-id")
                .required()
                .help("Hotel id on the hub"),
        )
        .arg(
            Arg::option("instance-id")
                .required()
                .help("This install's instance id (X-Instance-Id)"),
        )
        .arg(Arg::option("sql-user").help("SQL login (default: Windows authentication)"))
        .arg(Arg::flag("force").help("Write again although a config with a token exists"))
}

/// What the command line asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Args {
    pub hub_url: String,
    pub hotel_id: String,
    pub instance_id: String,
    pub sql_user: String,
    pub force: bool,
}

fn run(_ctx: &ToolContext<'_>, m: &Matches) -> Result<(), Error> {
    let text = |k: &str| m.value(k).unwrap_or_default().trim().to_owned();
    let args = Args {
        hub_url: text("hub-url"),
        hotel_id: text("hotel-id"),
        instance_id: text("instance-id"),
        sql_user: text("sql-user"),
        force: m.flag("force"),
    };
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = std::io::stdout();
    // Echo off while a secret may be typed; nothing to hide when stdin is not a console.
    let echo = dl_winos::console::hide_input().ok();
    let r = run_args(
        &args,
        &config::default_config_path(),
        &mut input,
        &mut out,
        &|k| std::env::var(k).ok(),
    );
    drop(echo);
    r.map_err(Error::Config)
}

/// A secret from `env`, else one line of `input`.
fn secret(
    what: &str,
    var: &str,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<String, String> {
    if let Some(v) = env(var)
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
    {
        return Ok(v);
    }
    write!(out, "{what} (or set {var}): ")
        .and_then(|()| out.flush())
        .map_err(|e| format!("stdout: {e}"))?;
    let mut line = String::new();
    input
        .read_line(&mut line)
        .map_err(|e| format!("read {what}: {e}"))?;
    let v = line.trim().to_owned();
    if v.is_empty() {
        return Err(format!("no {what}: set {var} or pass it on standard input"));
    }
    Ok(v)
}

/// The tool body with its environment injected.
pub(crate) fn run_args(
    args: &Args,
    path: &Path,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<(), String> {
    for (k, v) in [
        ("--hub-url", &args.hub_url),
        ("--hotel-id", &args.hotel_id),
        ("--instance-id", &args.instance_id),
    ] {
        if v.is_empty() {
            return Err(format!("{k} is required"));
        }
    }
    if !args.force && has_token(path)? {
        writeln!(
            out,
            "config {} already has a token, kept (pass --force to replace it)",
            path.display()
        )
        .map_err(|e| format!("stdout: {e}"))?;
        return Ok(());
    }
    let token = secret("hotel token", TOKEN_ENV, input, out, env)?;
    let sql_password = if args.sql_user.is_empty() {
        String::new()
    } else {
        secret("SQL password", PASSWORD_ENV, input, out, env)?
    };
    config::write_enrollment(
        path,
        &Enrollment {
            hub_url: args.hub_url.clone(),
            hotel_id: args.hotel_id.clone(),
            token,
            instance_id: args.instance_id.clone(),
            sql_user: args.sql_user.clone(),
            sql_password,
        },
    )?;
    writeln!(out, "wrote {}", path.display()).map_err(|e| format!("stdout: {e}"))?;
    Ok(())
}

/// `Ok(true)` when the config exists and has a token; an unreadable one is an error.
fn has_token(path: &Path) -> Result<bool, String> {
    match config::load(path) {
        Ok(Some(c)) => Ok(!c.token.is_empty()),
        Ok(None) => Ok(false),
        Err(e) => Err(format!("{e} (fix or remove it, or pass --force)")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobqueue::tests::temp_dir;

    fn args() -> Args {
        Args {
            hub_url: "https://hl.example.vn/".into(),
            hotel_id: "h1".into(),
            instance_id: "i-1".into(),
            ..Args::default()
        }
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn token_from_stdin_writes_windows_auth_config() {
        let path = temp_dir("enroll-stdin").join("config.json");
        let mut input: &[u8] = b"tok-1\n";
        let mut out = Vec::new();
        run_args(&args(), &path, &mut input, &mut out, &no_env).expect("enroll");
        let raw = std::fs::read_to_string(&path).expect("read");
        assert!(
            !raw.contains("tok-1") && raw.contains("enc:"),
            "token stored as an enc: secret"
        );
        let c = config::load(&path).expect("load").expect("cfg");
        assert_eq!(c.token, "tok-1");
        assert_eq!(c.hub_url, "https://hl.example.vn");
        assert_eq!(
            (c.sql.server.as_str(), c.sql.database.as_str()),
            (".", "SMILE_FO")
        );
        assert!(c.sql.trusted_connection);
        assert!(
            !String::from_utf8_lossy(&out).contains("tok-1"),
            "never echoed"
        );
    }

    #[test]
    fn token_and_password_from_env() {
        let path = temp_dir("enroll-env").join("config.json");
        let mut input: &[u8] = b"";
        let env = |k: &str| match k {
            TOKEN_ENV => Some("tok-env".to_owned()),
            PASSWORD_ENV => Some("pw".to_owned()),
            _ => None,
        };
        let a = Args {
            sql_user: "smile".into(),
            ..args()
        };
        run_args(&a, &path, &mut input, &mut Vec::new(), &env).expect("enroll");
        let c = config::load(&path).expect("load").expect("cfg");
        assert_eq!(
            (
                c.token.as_str(),
                c.sql.user.as_str(),
                c.sql.password.as_str()
            ),
            ("tok-env", "smile", "pw")
        );
        assert!(!c.sql.trusted_connection);
    }

    #[test]
    fn keeps_a_config_with_a_token_unless_forced() {
        let path = temp_dir("enroll-keep").join("config.json");
        run_args(
            &args(),
            &path,
            &mut &b"first\n"[..],
            &mut Vec::new(),
            &no_env,
        )
        .expect("first");
        let mut out = Vec::new();
        run_args(&args(), &path, &mut &b"second\n"[..], &mut out, &no_env).expect("kept");
        assert!(String::from_utf8_lossy(&out).contains("kept"));
        assert_eq!(
            config::load(&path).expect("load").expect("cfg").token,
            "first"
        );
        let forced = Args {
            force: true,
            ..args()
        };
        run_args(
            &forced,
            &path,
            &mut &b"second\n"[..],
            &mut Vec::new(),
            &no_env,
        )
        .expect("forced");
        assert_eq!(
            config::load(&path).expect("load").expect("cfg").token,
            "second"
        );
    }

    #[test]
    fn missing_token_is_an_error() {
        let path = temp_dir("enroll-none").join("config.json");
        let e = run_args(&args(), &path, &mut &b"\n"[..], &mut Vec::new(), &no_env).unwrap_err();
        assert!(e.contains(TOKEN_ENV), "{e}");
        assert!(!path.exists());
    }

    #[test]
    fn password_required_with_sql_user() {
        let path = temp_dir("enroll-pw").join("config.json");
        let a = Args {
            sql_user: "smile".into(),
            ..args()
        };
        let e = run_args(&a, &path, &mut &b"tok\n"[..], &mut Vec::new(), &no_env).unwrap_err();
        assert!(e.contains(PASSWORD_ENV), "{e}");
    }
}
