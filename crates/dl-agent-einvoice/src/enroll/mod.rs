//! Enrollment: a port of `Enroll` (`enroll.go`) for `dl-agent einvoice-enroll`. It
//! exchanges the one-time token for the tenant's API key, tests the SMILE FO and POS
//! databases and seals `config.enc`, which the `einvoice` module then runs from.
//! The Windows service is dl-agent's own, installed by its `enroll`/`install` steps, so
//! there is no service step here.

macro_rules! say {
    ($out:expr, $($arg:tt)*) => {{
        let _ = write!($out, $($arg)*);
        let _ = $out.flush();
    }};
}

mod exchange;
mod ping;
mod prompt;
mod tool;

#[cfg(test)]
mod tests;

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use dl_time::OffsetDateTime;

use crate::agent_config::AgentConfig;
use crate::dsn::Dsn;
use crate::legacy::go_quote;
use crate::platform::Platform;

pub use exchange::exchange_token;
pub use ping::{Ping, PingError, ping_dsn, ping_negotiated};
pub use prompt::HideInput;
pub use tool::einvoice_enroll_tool;

use prompt::{Prompter, prompt_sql, split_server_address};

const DEFAULT_SQL_HOST: &str = ".";
/// Where `einvoice-enroll` reads the one-time token when `--token` is not given, so it
/// stays off the command line (the bench script and other installers set it).
pub const TOKEN_ENV: &str = "DL_AGENT_EINVOICE_TOKEN";
const DEFAULT_SQL_USER: &str = "smile";
const FO_DATABASE: &str = "SMILE_FO";
const POS_DATABASE: &str = "SMILE_POS";
/// Offered when the operator gives neither a port nor a named instance.
const DEFAULT_SQL_PORT: &str = "1433";

/// Midnight in Vietnam (UTC+7) of `date` (`YYYY-MM-DD`), or of today.
pub fn vn_midnight(date: Option<&str>) -> Result<OffsetDateTime, String> {
    let ict = dl_time::Offset::from_hms(7, 0, 0).map_err(|e| e.to_string())?;
    let d = match date {
        Some(s) => s
            .trim()
            .parse::<dl_time::Date>()
            .map_err(|e| format!("agent: collect-from {s:?}: expected YYYY-MM-DD: {e}"))?,
        None => OffsetDateTime::now_utc().to_offset(ict).date(),
    };
    Ok(OffsetDateTime::new(d.midnight(), ict))
}

/// `YYYY-MM-DD` of `t` in Vietnam time.
fn vn_date(t: OffsetDateTime) -> String {
    let ict = dl_time::Offset::from_hms(7, 0, 0).unwrap_or(dl_time::Offset::UTC);
    let s = t.to_offset(ict).to_rfc3339();
    s.get(..10).unwrap_or(&s).to_owned()
}

/// The SQL password the zero-prompt enroll (`/agent/bootstrap/<token>`) falls back to: the
/// SMILE service account default the Go agent compiles in (`defaultSQLPassword`, an
/// accepted risk in the 2026-08-02 audit). Kept out of the source: the release build sets
/// `MMEI_BUILD_DEFAULT_SQL_PASSWORD` from a build secret. Empty in other builds, which then
/// need `--sql-password`, `MMEI_SQL_PASSWORD` or `--sql-prompt`.
pub fn default_sql_password() -> &'static str {
    option_env!("MMEI_BUILD_DEFAULT_SQL_PASSWORD").unwrap_or("")
}

/// Everything [`enroll`] needs from its caller.
pub struct EnrollOptions<'a> {
    /// Where `config.enc` is sealed.
    pub config_dir: PathBuf,
    /// Platform base URL.
    pub platform_url: String,
    /// One-time enrollment token.
    pub enroll_token: String,
    /// This machine's name, sent with the exchange.
    pub agent_host: String,
    /// `--sql-host` (empty = `MMEI_SQL_HOST`, then the local instance).
    pub sql_host: String,
    /// `--sql-user` (empty = `MMEI_SQL_USER`, then `smile`).
    pub sql_user: String,
    /// `--sql-password` (empty = `MMEI_SQL_PASSWORD`, then [`default_sql_password`]).
    pub sql_password: String,
    /// `--sql-auth`: `sql` (a SQL login) or `windows` (integrated authentication as the
    /// service account, no user or password); empty = `MMEI_SQL_AUTH`, then `sql`.
    pub sql_auth: String,
    /// `--collect-from` (None = the tenant's Hub value, then today).
    pub collect_from: Option<OffsetDateTime>,
    /// `--sql-prompt`: ask for the connection and test it before spending the token.
    pub prompt_sql: bool,
    /// Where the answers come from (standard input); required with `prompt_sql`.
    pub input: Option<&'a mut dyn BufRead>,
    /// Turns console echo off while the password is typed.
    pub hide_input: Option<HideInput<'a>>,
    /// Progress output (standard output).
    pub out: &'a mut dyn Write,
    /// Version string reported with the exchange (`dl-agent-<version>`).
    pub agent_version: String,
    /// Connection test; None = [`ping_dsn`].
    pub ping: Option<Ping<'a>>,
    /// Environment lookup (`MMEI_SQL_*`, `MMEI_AGENT_MOCK`).
    pub env: &'a dyn Fn(&str) -> Option<String>,
}

/// `Enroll`. With `prompt_sql` the operator first enters and verifies the SQL Server
/// connection, before the token is spent, so an abandoned attempt neither burns it nor
/// revokes the tenant's working agent key. Then: exchange the token, test FO and POS
/// (non-fatal, with the transport fallback), install the collection procedures, read the
/// tenant config, seal `config.enc`, install and start the service. Nothing is persisted
/// on an error before the config is sealed.
pub fn enroll(opts: EnrollOptions<'_>) -> Result<(), String> {
    let EnrollOptions {
        config_dir,
        platform_url,
        enroll_token,
        agent_host,
        sql_host,
        sql_user,
        sql_password,
        sql_auth,
        collect_from: collect_from_flag,
        prompt_sql: prompted,
        input,
        hide_input,
        out,
        agent_version,
        ping,
        env,
    } = opts;
    if platform_url.is_empty() || enroll_token.is_empty() {
        return Err(format!(
            "enroll requires --platform-url and --token (or {TOKEN_ENV})"
        ));
    }
    let mock = env("MMEI_AGENT_MOCK").as_deref() == Some("1");
    let default_ping = |d: &Dsn| ping_dsn(d, mock);
    let ping: Ping<'_> = ping.unwrap_or(&default_ping);
    let pick = |arg: &str, var: &str, def: &str| -> String {
        if !arg.is_empty() {
            return arg.to_owned();
        }
        env(var)
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| def.to_owned())
    };
    let windows_auth = match pick(&sql_auth, "MMEI_SQL_AUTH", "sql")
        .to_ascii_lowercase()
        .as_str()
    {
        "sql" => false,
        "windows" => true,
        other => {
            return Err(format!(
                "enroll: --sql-auth {other:?}: expected sql or windows"
            ));
        }
    };
    // SSMS forms (`host\instance`, `host,port`) work here as at the prompt.
    let (host, port_or_inst) =
        split_server_address(&pick(&sql_host, "MMEI_SQL_HOST", DEFAULT_SQL_HOST));
    // Windows authentication: a DSN without a user name logs in through SSPI.
    let (user, password) = if windows_auth {
        (String::new(), String::new())
    } else {
        (
            pick(&sql_user, "MMEI_SQL_USER", DEFAULT_SQL_USER),
            pick(&sql_password, "MMEI_SQL_PASSWORD", default_sql_password()),
        )
    };
    let base = |database: &str| {
        let mut d = Dsn {
            host: host.clone(),
            database: database.to_owned(),
            user: user.clone(),
            password: password.clone(),
            ..Dsn::default()
        };
        if port_or_inst.chars().all(|c| c.is_ascii_digit()) {
            d.port = port_or_inst.clone();
        } else {
            d.instance = port_or_inst.clone();
        }
        d
    };
    let (mut fo, mut pos) = (base(FO_DATABASE), base(POS_DATABASE));
    if prompted {
        let Some(input) = input else {
            return Err("agent: prompted enroll needs an interactive console".into());
        };
        let mut p = Prompter {
            input,
            out: &mut *out,
            hide: hide_input,
        };
        (fo, pos) = prompt_sql(&mut p, fo, pos, ping)?;
    } else if password.is_empty() && !windows_auth {
        return Err(
            "enroll needs the SQL password: --sql-password, MMEI_SQL_PASSWORD or --sql-prompt (this build has no default password)"
                .into(),
        );
    }

    // One device id per install, minted before the exchange so the key is bound to it.
    let device_id = dl_uuid::Uuid::new_v4();
    say!(out, "Exchanging enrollment token ...\n");
    let (api_key, tenant) = exchange_token(
        &platform_url,
        &enroll_token,
        &agent_host,
        device_id,
        &agent_version,
    )?;
    say!(out, "Enrolled tenant {tenant}.\n\n");

    let login = if fo.user.is_empty() {
        "Windows authentication".to_owned()
    } else {
        format!("user {}", go_quote(&fo.user))
    };
    say!(
        out,
        "Using SQL Server {} ({login}), databases {} + {}\n",
        go_quote(&fo.host),
        fo.database,
        pos.database
    );
    for (label, dsn) in [("FO", &mut fo), ("POS", &mut pos)] {
        say!(out, "  testing {label} ({}) ... ", dsn.database);
        // An old SQL Server (2008, or 2012 without the TLS 1.2 update) cannot complete the
        // default TLS 1.2 handshake; the transport that worked is what gets sealed.
        let (negotiated, r) = ping_negotiated(dsn, ping);
        *dsn = negotiated;
        if let Err(e) = r {
            say!(
                out,
                "FAILED ({e}) — sealing anyway; the service will retry\n"
            );
            continue;
        }
        let note = dsn.transport().describe();
        if note.is_empty() {
            say!(out, "OK\n");
        } else {
            say!(out, "OK ({note})\n");
        }
        if mock {
            continue;
        }
        say!(
            out,
            "  installing collection procedures into {} ... ",
            dsn.database
        );
        match crate::install::install(&dsn.to_string()) {
            Ok(()) => say!(out, "OK\n"),
            Err(e) => say!(
                out,
                "FAILED ({e}) — collection will not run until the procedures are installed\n"
            ),
        }
    }

    // Precedence: -collect-from > the tenant's Hub value > today.
    let mut collect_from = vn_midnight(None)?;
    let mut collect_sources = String::new();
    let mut recalc = true; // default on; the Hub config overrides it when reachable
    match collect_from_flag {
        Some(t) => collect_from = t,
        None => {
            let cli = Platform::new(&platform_url, &api_key, device_id);
            match cli.config(Instant::now() + Duration::from_secs(10)) {
                Ok(remote) => {
                    if let Some(t) = remote.collect_from {
                        collect_from = t;
                    }
                    collect_sources = remote.collect_sources;
                    recalc = remote.recalc_tax_into_sc;
                }
                Err(e) => say!(
                    out,
                    "Could not read tenant config from Hub ({e}); using defaults — the service will sync on its first cycle\n"
                ),
            }
        }
    }
    say!(
        out,
        "Collecting invoices dated {} onward (Vietnam time)\n",
        vn_date(collect_from)
    );

    let mut cfg = AgentConfig {
        config_dir: config_dir.clone(),
        ..Default::default()
    };
    cfg.platform_base_url = platform_url;
    cfg.api_key = api_key;
    cfg.device_id = device_id;
    cfg.sql_dsn = fo.to_string();
    cfg.pos_sql_dsn = pos.to_string();
    cfg.collect_from = Some(collect_from);
    cfg.collect_sources = collect_sources;
    cfg.recalc_tax_into_sc = recalc;
    cfg.apply_defaults_for_zero();
    cfg.save().map_err(|e| format!("agent: seal config: {e}"))?;
    say!(out, "\nSealed config to {}\n", config_dir.display());
    say!(
        out,
        "Done. The einvoice module reads it on its next check.\n"
    );
    Ok(())
}
