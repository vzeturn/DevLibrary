//! The proxy the agent reaches the hub through, read once per process. First match wins:
//!
//! 1. the environment: `HTTPS_PROXY` / `HTTP_PROXY` (either case) with `NO_PROXY`;
//! 2. the machine's WinHTTP setting (`netsh winhttp set proxy`), Windows only;
//! 3. the Internet Options of the service account (machine-wide when the
//!    `ProxySettingsPerUser = 0` policy is set), Windows only, when `ProxyEnable = 1`.
//!
//! A proxy whose URL carries no credentials gets `DL_AGENT_PROXY_USER` /
//! `DL_AGENT_PROXY_PASSWORD` as Basic credentials when they are set. Nothing here shows the
//! password: [`Choice::describe`] names the source, the proxy and "credentials yes/no".

use std::sync::OnceLock;

use dl_http::{Proxy, ProxyRules};

/// Environment variables with the proxy's Basic credentials.
pub const USER_ENV: &str = "DL_AGENT_PROXY_USER";
/// See [`USER_ENV`].
pub const PASSWORD_ENV: &str = "DL_AGENT_PROXY_PASSWORD";

/// Where the proxy setting came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// No proxy anywhere: direct connections.
    Direct,
    /// `HTTPS_PROXY` / `HTTP_PROXY`.
    Env,
    /// `netsh winhttp set proxy`.
    WinHttp,
    /// Internet Options (`ProxyServer` / `ProxyOverride`).
    InternetOptions,
}

impl Source {
    /// The name shown in the self-test.
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Direct => "direct",
            Source::Env => "environment",
            Source::WinHttp => "WinHTTP (netsh winhttp)",
            Source::InternetOptions => "Internet Options",
        }
    }
}

/// A Windows proxy setting as stored: `ProxyServer` and `ProxyOverride`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowsSetting {
    /// `host:port` or `http=…;https=…`.
    pub server: String,
    /// `ProxyOverride` / the WinHTTP bypass list.
    pub bypass: String,
}

/// The proxy in use and where it came from.
#[derive(Debug, Clone)]
pub struct Choice {
    /// What the HTTP client uses.
    pub proxy: Proxy,
    /// Where it came from.
    pub source: Source,
    /// Settings that were found but could not be used (bad address, unreadable registry).
    pub problems: Vec<String>,
}

impl Choice {
    /// `WinHTTP (netsh winhttp): p:8080; 1 bypass entry + <local>; credentials no`.
    pub fn describe(&self) -> String {
        match &self.proxy {
            Proxy::Rules(r) => format!("{}: {}", self.source.as_str(), r.describe()),
            _ => "direct (no proxy in the environment, WinHTTP or Internet Options)".into(),
        }
    }
}

/// Decodes the binary `WinHttpSettings` / `DefaultConnectionSettings` value: flags at
/// byte 8 (`2` = proxy set), then the length-prefixed proxy server and bypass list.
/// `None` when no proxy is set or the data is cut short.
pub fn parse_connection_blob(b: &[u8]) -> Option<WindowsSetting> {
    let dword = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
    };
    if dword(8)? & 2 == 0 {
        return None;
    }
    let text = |at: usize| -> Option<(String, usize)> {
        let n = usize::try_from(dword(at)?).ok()?;
        let s = b.get(at + 4..at + 4 + n)?;
        Some((
            String::from_utf8_lossy(s)
                .trim_end_matches('\0')
                .to_string(),
            at + 4 + n,
        ))
    };
    let (server, next) = text(12)?;
    let bypass = text(next).map(|(s, _)| s).unwrap_or_default();
    (!server.trim().is_empty()).then_some(WindowsSetting { server, bypass })
}

/// Picks the proxy from the three sources (see the module docs); `env` reads a variable.
pub fn choose(
    env: impl Fn(&str) -> Option<String>,
    winhttp: Option<WindowsSetting>,
    options: Option<WindowsSetting>,
) -> Choice {
    let mut problems = Vec::new();
    let creds = |r: ProxyRules, problems: &mut Vec<String>| {
        let user = env(USER_ENV).unwrap_or_default();
        let pass = env(PASSWORD_ENV).unwrap_or_default();
        match r.clone().with_credentials(&user, &pass) {
            Ok(r) => r,
            Err(e) => {
                problems.push(format!("{USER_ENV}: {e}"));
                r
            }
        }
    };
    match ProxyRules::from_env(&env) {
        Ok(Some(r)) => {
            let proxy = Proxy::Rules(creds(r, &mut problems));
            return Choice {
                proxy,
                source: Source::Env,
                problems,
            };
        }
        Ok(None) => {}
        // An explicit but broken setting must not turn into a silent direct connection:
        // keep reading the environment per request so every call reports the error.
        Err(e) => {
            return Choice {
                proxy: Proxy::FromEnv,
                source: Source::Env,
                problems: vec![format!("HTTPS_PROXY/HTTP_PROXY: {e}")],
            };
        }
    }
    for (source, setting) in [
        (Source::WinHttp, winhttp),
        (Source::InternetOptions, options),
    ] {
        let Some(s) = setting else { continue };
        match ProxyRules::from_windows(&s.server, &s.bypass) {
            Ok(r) => {
                let proxy = Proxy::Rules(creds(r, &mut problems));
                return Choice {
                    proxy,
                    source,
                    problems,
                };
            }
            Err(e) => problems.push(format!("{}: {e}", source.as_str())),
        }
    }
    Choice {
        proxy: Proxy::None,
        source: Source::Direct,
        problems,
    }
}

/// The choice for this process (environment and registry read on first use).
pub fn current() -> &'static Choice {
    static CHOICE: OnceLock<Choice> = OnceLock::new();
    CHOICE.get_or_init(|| {
        let mut problems = Vec::new();
        let (winhttp, options) = windows::read(&mut problems);
        let mut c = choose(|n| std::env::var(n).ok(), winhttp, options);
        c.problems.extend(problems);
        for p in &c.problems {
            dl_log::warn!("proxy setting ignored"; problem = %p);
        }
        c
    })
}

#[cfg(windows)]
mod windows {
    use super::{WindowsSetting, parse_connection_blob};
    use dl_winos::registry::{Hive, Key, Value, View};

    const SETTINGS: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Internet Settings";
    const POLICY: &str = r"SOFTWARE\Policies\Microsoft\Windows\CurrentVersion\Internet Settings";

    fn open(hive: Hive, path: &str, problems: &mut Vec<String>) -> Option<Key> {
        match Key::open(hive, path, View::Wow64_64) {
            Ok(k) => Some(k),
            Err(e) if e.code() == Some(2) => None,
            Err(e) => {
                problems.push(format!("registry {path}: {e}"));
                None
            }
        }
    }

    fn value(key: &Key, name: &str, problems: &mut Vec<String>) -> Option<Value> {
        key.value(name).unwrap_or_else(|e| {
            problems.push(format!("registry value {name}: {e}"));
            None
        })
    }

    /// `(WinHTTP, Internet Options)` settings that name a proxy.
    pub(super) fn read(
        problems: &mut Vec<String>,
    ) -> (Option<WindowsSetting>, Option<WindowsSetting>) {
        let winhttp = open(
            Hive::LocalMachine,
            &format!(r"{SETTINGS}\Connections"),
            problems,
        )
        .and_then(|k| match value(&k, "WinHttpSettings", problems)? {
            Value::Binary(b) => parse_connection_blob(&b),
            _ => None,
        });
        let per_machine = open(Hive::LocalMachine, POLICY, problems)
            .and_then(|k| value(&k, "ProxySettingsPerUser", problems))
            == Some(Value::Dword(0));
        let (hive, path) = if per_machine {
            (Hive::LocalMachine, SETTINGS)
        } else {
            (
                Hive::CurrentUser,
                r"Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            )
        };
        let options = open(hive, path, problems).and_then(|k| {
            if value(&k, "ProxyEnable", problems) != Some(Value::Dword(1)) {
                return None;
            }
            let text = |name: &str, problems: &mut Vec<String>| match value(&k, name, problems) {
                Some(Value::String(s) | Value::ExpandString(s)) => s,
                _ => String::new(),
            };
            let server = text("ProxyServer", problems);
            let bypass = text("ProxyOverride", problems);
            (!server.trim().is_empty()).then_some(WindowsSetting { server, bypass })
        });
        (winhttp, options)
    }
}

#[cfg(not(windows))]
mod windows {
    use super::WindowsSetting;

    pub(super) fn read(_: &mut Vec<String>) -> (Option<WindowsSetting>, Option<WindowsSetting>) {
        (None, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(vars: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |n| {
            vars.iter()
                .find(|(k, _)| *k == n)
                .map(|(_, v)| (*v).to_string())
        }
    }

    fn setting(server: &str, bypass: &str) -> Option<WindowsSetting> {
        Some(WindowsSetting {
            server: server.into(),
            bypass: bypass.into(),
        })
    }

    fn blob(flags: u32, server: &str, bypass: &str) -> Vec<u8> {
        let mut b = vec![0x28, 0, 0, 0, 0, 0, 0, 0];
        b.extend_from_slice(&flags.to_le_bytes());
        for s in [server, bypass] {
            b.extend_from_slice(&(s.len() as u32).to_le_bytes());
            b.extend_from_slice(s.as_bytes());
        }
        b
    }

    #[test]
    fn winhttp_blob_decodes_and_direct_is_none() {
        assert_eq!(
            parse_connection_blob(&blob(3, "proxy.corp:8080", "<local>;*.corp")),
            setting("proxy.corp:8080", "<local>;*.corp")
        );
        // `netsh winhttp reset proxy` leaves flags = 1 (direct).
        let direct = [
            0x18, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(parse_connection_blob(&direct), None);
        let mut cut = blob(3, "proxy.corp:8080", "");
        cut.truncate(20);
        assert_eq!(parse_connection_blob(&cut), None);
        assert_eq!(parse_connection_blob(&[]), None);
    }

    #[test]
    fn environment_wins_then_winhttp_then_internet_options() {
        let c = choose(
            env(&[("HTTPS_PROXY", "http://envp:3128")]),
            setting("winp:8080", ""),
            setting("iep:8080", ""),
        );
        assert_eq!(c.source, Source::Env);
        assert!(c.describe().contains("envp:3128"), "{}", c.describe());
        let c = choose(
            env(&[]),
            setting("winp:8080", "<local>"),
            setting("iep:8080", ""),
        );
        assert_eq!(c.source, Source::WinHttp);
        assert_eq!(
            c.describe(),
            "WinHTTP (netsh winhttp): winp:8080; 0 bypass entries + <local>; credentials no"
        );
        let c = choose(env(&[]), None, setting("http=iep:80;https=iep:443", ""));
        assert_eq!(c.source, Source::InternetOptions);
        let c = choose(env(&[]), None, None);
        assert_eq!(c.source, Source::Direct);
        assert!(matches!(c.proxy, Proxy::None));
        assert!(c.describe().starts_with("direct"));
    }

    #[test]
    fn credentials_come_from_the_environment_and_are_never_shown() {
        let c = choose(
            env(&[(USER_ENV, "svc"), (PASSWORD_ENV, "s3cret!")]),
            setting("winp:8080", ""),
            None,
        );
        assert!(
            c.describe().ends_with("credentials yes"),
            "{}",
            c.describe()
        );
        assert!(!c.describe().contains("s3cret") && !format!("{c:?}").contains("s3cret"));
        let Proxy::Rules(r) = &c.proxy else {
            panic!("{c:?}")
        };
        let p = r
            .proxy_for(&dl_http::Url::parse("https://agent.example/").unwrap())
            .unwrap();
        assert_eq!((p.username(), p.password()), ("svc", Some("s3cret%21")));
    }

    #[test]
    fn a_bad_setting_is_reported_not_silently_direct() {
        let c = choose(env(&[("HTTPS_PROXY", "https://p:1")]), None, None);
        assert_eq!(c.source, Source::Env);
        assert!(matches!(c.proxy, Proxy::FromEnv));
        assert_eq!(c.problems.len(), 1);
        // A broken Windows setting falls through to the next source, with the problem kept.
        let c = choose(
            env(&[]),
            setting("socks=s:1080", ""),
            setting("iep:8080", ""),
        );
        assert_eq!(c.source, Source::InternetOptions);
        assert!(c.problems[0].starts_with("WinHTTP"), "{:?}", c.problems);
    }
}
