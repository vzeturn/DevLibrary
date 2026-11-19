//! Tests for `einvoice-enroll`: skip when already enrolled, corrupt config, `--force`,
//! setup-arg words reaching the tool, the token from the environment, Windows
//! authentication, and no secret in any output.

use std::ffi::OsString;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use dl_agent::tool::with_setup_args;

use super::*;
use crate::enroll::tests::fake_platform;

const TOKEN: &str = "TOKEN-xyz-123";
const PASSWORD: &str = "PW-xyz-456";
const API_KEY: &str = "dl_key_SECRET";

fn temp_dir() -> PathBuf {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!(
        "dl-enroll-tool-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn mock_env(k: &str) -> Option<String> {
    (k == "MMEI_AGENT_MOCK").then(|| "1".to_owned())
}

struct Outcome {
    result: Result<(), String>,
    out: String,
}

fn run_tool(args: &Args) -> Outcome {
    run_tool_env(args, &mock_env)
}

fn run_tool_env(args: &Args, env: &dyn Fn(&str) -> Option<String>) -> Outcome {
    let mut input = Cursor::new(Vec::new());
    let mut out = Vec::new();
    let result = run_args(args, Some(&mut input), &mut out, None, None, env);
    Outcome {
        result,
        out: String::from_utf8_lossy(&out).into_owned(),
    }
}

fn args(dir: &std::path::Path, url: &str) -> Args {
    Args {
        token: TOKEN.into(),
        platform_url: url.into(),
        sql_password: PASSWORD.into(),
        config_dir: Some(dir.to_owned()),
        ..Args::default()
    }
}

fn load(dir: &std::path::Path) -> AgentConfig {
    AgentConfig::load(dir).unwrap().expect("sealed config")
}

#[test]
fn enrolls_a_fresh_site_without_leaking_secrets() {
    let (url, exchanges) = fake_platform();
    let dir = temp_dir();
    let r = run_tool(&args(&dir, &url));
    assert!(r.result.is_ok(), "{:?}\n{}", r.result, r.out);
    assert_eq!(exchanges.load(Ordering::SeqCst), 1);
    assert_eq!(load(&dir).api_key, API_KEY);
    for secret in [TOKEN, PASSWORD, API_KEY] {
        assert!(!r.out.contains(secret), "{secret} printed:\n{}", r.out);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn skips_without_spending_the_token_when_already_enrolled() {
    let (url, exchanges) = fake_platform();
    let dir = temp_dir();
    assert!(run_tool(&args(&dir, &url)).result.is_ok());
    let before = std::fs::read(dir.join("config.enc")).unwrap();
    let r = run_tool(&args(&dir, &url));
    assert!(r.result.is_ok(), "{:?}", r.result);
    assert_eq!(r.out, "da co cau hinh E-Invoice, giu nguyen\n");
    assert_eq!(exchanges.load(Ordering::SeqCst), 1, "token spent again");
    assert_eq!(std::fs::read(dir.join("config.enc")).unwrap(), before);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_corrupt_config_is_an_error_and_is_left_alone() {
    let (url, exchanges) = fake_platform();
    let dir = temp_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.enc"), b"not a sealed config").unwrap();
    let r = run_tool(&args(&dir, &url));
    let err = r.result.unwrap_err();
    assert!(
        err.contains("config.enc") && err.contains("--force"),
        "{err}"
    );
    assert_eq!(exchanges.load(Ordering::SeqCst), 0);
    assert_eq!(
        std::fs::read(dir.join("config.enc")).unwrap(),
        b"not a sealed config"
    );
    for secret in [TOKEN, PASSWORD] {
        assert!(!err.contains(secret) && !r.out.contains(secret));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn force_enrolls_again_over_a_valid_or_a_corrupt_config() {
    let (url, exchanges) = fake_platform();
    let dir = temp_dir();
    assert!(run_tool(&args(&dir, &url)).result.is_ok());
    let force = Args {
        force: true,
        ..args(&dir, &url)
    };
    assert!(run_tool(&force).result.is_ok());
    assert_eq!(exchanges.load(Ordering::SeqCst), 2);
    std::fs::write(dir.join("config.enc"), b"garbage").unwrap();
    assert!(run_tool(&force).result.is_ok());
    assert_eq!(exchanges.load(Ordering::SeqCst), 3);
    assert_eq!(load(&dir).api_key, API_KEY);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_bad_collect_from_date_fails_before_the_exchange() {
    let (url, exchanges) = fake_platform();
    let dir = temp_dir();
    let a = Args {
        collect_from: Some("1/7".into()),
        ..args(&dir, &url)
    };
    assert!(run_tool(&a).result.unwrap_err().contains("collect-from"));
    assert_eq!(exchanges.load(Ordering::SeqCst), 0);
    assert!(!dir.join("config.enc").exists());
}

#[test]
fn setup_args_words_reach_the_tool() {
    let tools = [einvoice_enroll_tool()];
    let os = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
    let argv = with_setup_args(
        os(&["dl-agent", NAME, "--data", "/d"]),
        &tools,
        Some("--token T1 --platform-url https://p.example --sql-prompt --force"),
    );
    let cmd = Command::new("dl-agent")
        .subcommand((tools[0].grammar)(Command::new(NAME)).arg(Arg::option("data")));
    let m = cmd.parse_from(argv).unwrap();
    let (name, sub) = m.subcommand().unwrap();
    assert_eq!(name, NAME);
    let a = Args::from_matches(sub);
    assert_eq!(a.token, "T1");
    assert_eq!(a.platform_url, "https://p.example");
    assert!(a.sql_prompt && a.force);
}

#[test]
fn platform_url_is_required_and_the_token_may_come_from_env() {
    let tools = [einvoice_enroll_tool()];
    let cmd = Command::new("dl-agent").subcommand((tools[0].grammar)(Command::new(NAME)));
    assert!(cmd.parse_from(["dl-agent", NAME]).is_err());
    assert!(
        cmd.parse_from(["dl-agent", NAME, "--token", "t"]).is_err(),
        "platform-url missing"
    );
    assert!(
        cmd.parse_from(["dl-agent", NAME, "--platform-url", "https://p"])
            .is_ok(),
        "--token is optional: DL_AGENT_EINVOICE_TOKEN may carry it"
    );
}

fn env_with<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |k: &str| {
        mock_env(k).or_else(|| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_owned())
        })
    }
}

#[test]
fn the_token_comes_from_env_when_the_argument_is_absent() {
    let (url, exchanges) = fake_platform();
    let dir = temp_dir();
    let a = Args {
        token: String::new(),
        ..args(&dir, &url)
    };
    let r = run_tool_env(&a, &env_with(&[(TOKEN_ENV, TOKEN)]));
    assert!(r.result.is_ok(), "{:?}\n{}", r.result, r.out);
    assert_eq!(exchanges.load(Ordering::SeqCst), 1);
    assert_eq!(load(&dir).api_key, API_KEY);
    assert!(!r.out.contains(TOKEN), "token printed:\n{}", r.out);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_token_argument_wins_over_env() {
    let env = env_with(&[(TOKEN_ENV, "FROM-ENV")]);
    let from_arg = Args {
        token: "FROM-ARG".into(),
        ..Args::default()
    };
    assert_eq!(resolve_token(&from_arg, &env).unwrap(), "FROM-ARG");
    assert_eq!(resolve_token(&Args::default(), &env).unwrap(), "FROM-ENV");
}

#[test]
fn without_a_token_anywhere_nothing_is_exchanged() {
    let (url, exchanges) = fake_platform();
    let dir = temp_dir();
    let a = Args {
        token: String::new(),
        ..args(&dir, &url)
    };
    for env in [env_with(&[]), env_with(&[(TOKEN_ENV, "  ")])] {
        let err = run_tool_env(&a, &env).result.unwrap_err();
        assert!(err.contains("--token") && err.contains(TOKEN_ENV), "{err}");
    }
    assert_eq!(exchanges.load(Ordering::SeqCst), 0);
    assert!(!dir.join("config.enc").exists());
}

#[test]
fn windows_authentication_seals_a_dsn_without_a_login() {
    let (url, _) = fake_platform();
    let dir = temp_dir();
    let a = Args {
        sql_password: String::new(),
        sql_host: "SRV\\SMILE".into(),
        ..args(&dir, &url)
    };
    let r = run_tool_env(
        &a,
        &env_with(&[("MMEI_SQL_AUTH", "Windows"), ("MMEI_SQL_USER", "sa")]),
    );
    assert!(r.result.is_ok(), "{:?}\n{}", r.result, r.out);
    assert!(r.out.contains("Windows authentication"), "{}", r.out);
    let cfg = load(&dir);
    for dsn in [&cfg.sql_dsn, &cfg.pos_sql_dsn] {
        let parsed: dl_tds::Config = dsn.parse().expect("sealed DSN parses");
        assert!(parsed.integrated_auth(), "{dsn}");
        assert!(
            dsn.contains("SRV") && dsn.contains("SMILE") && !dsn.contains("sa:"),
            "{dsn}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_sql_login_keeps_its_user_and_an_unknown_auth_is_refused() {
    let (url, exchanges) = fake_platform();
    let dir = temp_dir();
    let a = Args {
        sql_user: "smile".into(),
        sql_host: "SRV,1444".into(),
        ..args(&dir, &url)
    };
    let r = run_tool_env(&a, &env_with(&[("MMEI_SQL_AUTH", "sql")]));
    assert!(r.result.is_ok(), "{:?}", r.result);
    let cfg = load(&dir);
    let parsed: dl_tds::Config = cfg.sql_dsn.parse().unwrap();
    assert!(!parsed.integrated_auth());
    assert!(cfg.sql_dsn.contains("SRV:1444"), "{}", cfg.sql_dsn);
    let _ = std::fs::remove_dir_all(&dir);

    let bad = Args {
        sql_auth: "kerberos".into(),
        force: true,
        ..args(&dir, &url)
    };
    let err = run_tool(&bad).result.unwrap_err();
    assert!(err.contains("sql or windows"), "{err}");
    assert_eq!(exchanges.load(Ordering::SeqCst), 1);
    let _ = std::fs::remove_dir_all(&dir);
}
