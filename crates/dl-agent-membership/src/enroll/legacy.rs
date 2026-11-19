//! Every other place the .NET `MM.Membership.Agent` can keep its identity, besides
//! `config.dpapi` ([`crate::config::ConfigStore`]).
//!
//! - **Seed configuration** (`AgentSecureConfig.Load` → `AgentConfigReader.ReadFromConfiguration`):
//!   while `config.dpapi` does not exist the .NET agent reads the `"Agent"` section of its
//!   `IConfiguration` (`appsettings.json` < `appsettings.{Environment}.json` < environment <
//!   `appsettings.hotel.json` < environment, all beside the exe, `Program.cs`) and seals it.
//!   `SmileConnectionString` falls back to `ConnectionStrings:SmileFo` only when the key is
//!   absent (`??`, so the packaged empty string does not fall back).
//! - **Portable store** (`PortableAgentConfigStore`, Linux/macOS only, `Agent:SecretStore=File`):
//!   a plain JSON file `{"Agent":{...}}` at `Agent:SecretStorePath`, default
//!   `/var/lib/mm-membership-agent/config.json`. `Agent:SecretStore=Env` is the seed
//!   configuration above without any file.
//!
//! There is no older format: the squashed MM-MEMBERSHIP history only ever knew
//! `config.dpapi` (the `secrets.dpapi` named in a comment of `install.ps1 -Purge` was never
//! written by any code). Configuration keys match case-insensitively, as
//! `Microsoft.Extensions.Configuration` does, and the JSON files may carry comments and
//! trailing commas (`JsonConfigurationProvider` allows both).

use std::path::{Path, PathBuf};

use dl_json::Value;
use dl_uuid::Uuid;

use crate::config::AgentConfig;
use crate::wire::parse;

/// The .NET agent's install folder name under `%ProgramFiles%`.
pub const INSTALL_DIR_NAME: &str = "MM.Membership.Agent";
/// `AgentSecretStoreSelector.DefaultPortablePath`.
pub const DEFAULT_PORTABLE_PATH: &str = "/var/lib/mm-membership-agent/config.json";
/// `AgentConfigReader`: the origin code when none is configured.
pub const DEFAULT_ORIGIN: &str = "Membership";

/// The `"Agent"` section (plus `ConnectionStrings:SmileFo`) after layering. `Some("")` is
/// a key present with an empty value, which hides earlier layers like in .NET.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Section {
    hub: Option<String>,
    token: Option<String>,
    hotel: Option<String>,
    smile: Option<String>,
    smile_fo: Option<String>,
    origin: Option<String>,
    store_path: Option<String>,
}

impl std::fmt::Debug for Section {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let has = |o: &Option<String>| o.as_deref().is_some_and(|s| !s.trim().is_empty());
        f.debug_struct("Section")
            .field("hub", &self.hub)
            .field("token", &has(&self.token))
            .field("hotel", &self.hotel)
            .field("smile", &(has(&self.smile) || has(&self.smile_fo)))
            .field("origin", &self.origin)
            .finish()
    }
}

impl Section {
    /// Later layers win key by key.
    fn overlay(&mut self, top: Section) {
        let pick = |a: &mut Option<String>, b: Option<String>| {
            if b.is_some() {
                *a = b;
            }
        };
        pick(&mut self.hub, top.hub);
        pick(&mut self.token, top.token);
        pick(&mut self.hotel, top.hotel);
        pick(&mut self.smile, top.smile);
        pick(&mut self.smile_fo, top.smile_fo);
        pick(&mut self.origin, top.origin);
        pick(&mut self.store_path, top.store_path);
    }

    /// The section of one parsed JSON file.
    fn from_json(v: &Value) -> Section {
        let text = |o: Option<&Value>, k: &str| {
            o.and_then(|o| parse::prop(o, k))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        let agent = parse::prop(v, "Agent");
        let conns = parse::prop(v, "ConnectionStrings");
        Section {
            hub: text(agent, "HubBaseUrl"),
            token: text(agent, "AgentToken"),
            hotel: text(agent, "HotelId"),
            smile: text(agent, "SmileConnectionString"),
            smile_fo: text(conns, "SmileFo"),
            origin: text(agent, "OriginCode"),
            store_path: text(agent, "SecretStorePath"),
        }
    }

    /// The `Agent__*` / `ConnectionStrings__SmileFo` environment variables.
    fn from_env(env: &dyn Fn(&str) -> Option<String>) -> Section {
        Section {
            hub: env("Agent__HubBaseUrl"),
            token: env("Agent__AgentToken"),
            hotel: env("Agent__HotelId"),
            smile: env("Agent__SmileConnectionString"),
            smile_fo: env("ConnectionStrings__SmileFo"),
            origin: env("Agent__OriginCode"),
            store_path: env("Agent__SecretStorePath"),
        }
    }

    /// `SmileConnectionString ?? ConnectionStrings:SmileFo`, when not blank.
    pub fn smile(&self) -> Option<String> {
        self.smile
            .clone()
            .or_else(|| self.smile_fo.clone())
            .filter(|s| !s.trim().is_empty())
    }

    /// `AgentConfigReader.ReadFromConfiguration`: `None` while a required key is blank; an
    /// error (without the value) for a `HotelId` that is not a GUID.
    pub fn config(&self) -> Result<Option<AgentConfig>, String> {
        let filled = |o: &Option<String>| o.clone().filter(|s| !s.trim().is_empty());
        let (Some(hub), Some(token), Some(hotel), Some(smile)) = (
            filled(&self.hub),
            filled(&self.token),
            filled(&self.hotel),
            self.smile(),
        ) else {
            return Ok(None);
        };
        let hotel_id = hotel
            .trim()
            .parse::<Uuid>()
            .map_err(|_| "Agent:HotelId is not a valid GUID".to_owned())?;
        Ok(Some(AgentConfig {
            hub_base_url: hub.trim_end_matches('/').to_owned(),
            agent_token: token.trim().to_owned(),
            hotel_id,
            smile_connection_string: smile,
            origin_code: filled(&self.origin)
                .map_or_else(|| DEFAULT_ORIGIN.to_owned(), |o| o.trim().to_owned()),
        }))
    }
}

/// Where the .NET agent may keep a seed configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Places {
    /// Install folders, the one the service runs from first.
    pub install_dirs: Vec<PathBuf>,
    /// `DOTNET_ENVIRONMENT` (default `Production`) for `appsettings.{Environment}.json`.
    pub environment: String,
    /// Whether the portable store exists on this platform (not on Windows).
    pub portable: bool,
}

impl Places {
    /// The folders of this machine: the folder of the service executable (when known), then
    /// `%ProgramFiles%\MM.Membership.Agent`.
    pub fn detect(service_dir: Option<PathBuf>, env: &dyn Fn(&str) -> Option<String>) -> Places {
        let mut install_dirs: Vec<PathBuf> = service_dir.into_iter().collect();
        if cfg!(windows) {
            let pf = env("ProgramFiles").unwrap_or_else(|| r"C:\Program Files".to_owned());
            let d = PathBuf::from(pf).join(INSTALL_DIR_NAME);
            if !install_dirs.contains(&d) {
                install_dirs.push(d);
            }
        }
        Places {
            install_dirs,
            environment: env("DOTNET_ENVIRONMENT")
                .filter(|e| !e.trim().is_empty())
                .unwrap_or_else(|| "Production".to_owned()),
            portable: !cfg!(windows),
        }
    }
}

/// A seed configuration found outside `config.dpapi`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seed {
    /// The files it was read from (the environment is not listed).
    pub files: Vec<PathBuf>,
    /// Whether it is the portable store (`Agent:SecretStore=File`).
    pub portable: bool,
    /// The layered section.
    pub section: Section,
}

/// Reads the seed configuration. `Ok(None)` when no file and no `Agent__*` variable has any
/// key; an error names the file that does not parse (never its content).
pub fn read_seed(
    places: &Places,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<Seed>, String> {
    let env_layer = Section::from_env(env);
    let mut section = Section::default();
    let mut files = Vec::new();
    if let Some(dir) = places.install_dirs.iter().find(|d| d.is_dir()) {
        let names = [
            "appsettings.json".to_owned(),
            format!("appsettings.{}.json", places.environment),
        ];
        for name in &names {
            read_layer(&dir.join(name), &mut section, &mut files)?;
        }
        // The environment is layered twice in `Program.cs` (before and after the hotel
        // file); only the last layer matters.
        read_layer(
            &dir.join("appsettings.hotel.json"),
            &mut section,
            &mut files,
        )?;
    }
    section.overlay(env_layer);
    if places.portable {
        let path = section
            .store_path
            .clone()
            .filter(|p| !p.trim().is_empty())
            .map_or_else(|| PathBuf::from(DEFAULT_PORTABLE_PATH), PathBuf::from);
        let mut file_section = Section::default();
        let mut portable_files = Vec::new();
        read_layer(&path, &mut file_section, &mut portable_files)?;
        if !portable_files.is_empty() {
            return Ok(Some(Seed {
                files: portable_files,
                portable: true,
                section: file_section,
            }));
        }
    }
    if section == Section::default() {
        return Ok(None);
    }
    Ok(Some(Seed {
        files,
        portable: false,
        section,
    }))
}

/// Overlays one JSON file when it exists.
fn read_layer(path: &Path, section: &mut Section, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let text = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {}", path.display(), e.kind())),
    };
    let clean = strip_jsonc(&String::from_utf8_lossy(&text));
    let v = dl_json::parse(&clean).map_err(|_| format!("{}: not valid JSON", path.display()))?;
    section.overlay(Section::from_json(&v));
    files.push(path.to_path_buf());
    Ok(())
}

/// Drops `//` and `/* */` comments outside strings and commas right before `}` or `]`,
/// which `JsonConfigurationProvider` accepts and RFC 8259 does not.
pub fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_str = false;
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            match c {
                '\\' => {
                    if let Some(n) = chars.next() {
                        out.push(n);
                    }
                }
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_str = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut prev = '\0';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
                out.push(' ');
            }
            (']' | '}', _) => {
                let keep = out.trim_end().len();
                if out[..keep].ends_with(',') {
                    out.truncate(keep - 1);
                }
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// The folder of a service command line such as
/// `"C:\Program Files\MM.Membership.Agent\MM.Membership.Agent.exe" --flag`.
pub fn exe_dir(command_line: &str) -> Option<PathBuf> {
    let line = command_line.trim();
    let exe = match line.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => {
            let lower = line.to_ascii_lowercase();
            let end = lower.find(".exe").map_or(line.len(), |i| i + 4);
            &line[..end]
        }
    };
    let sep = exe.rfind(['\\', '/'])?;
    Some(PathBuf::from(&exe[..sep]))
}

#[cfg(test)]
#[path = "legacy_tests.rs"]
pub(super) mod tests;
