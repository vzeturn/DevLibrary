//! The configuration sources of `Host.CreateApplicationBuilder(args)`, in .NET precedence
//! (later wins): `DOTNET_*` environment variables and the command line (host settings),
//! `appsettings.json`, `appsettings.{Environment}.json`, all environment variables, the
//! command line. Keys are case-insensitive and `__` in environment variable names is `:`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::dotnet::{DotnetError, fold_key};

/// A flat, case-insensitive key/value view (`IConfiguration`). Keys use `:` between
/// sections; a `None` value is a JSON `null` or an empty JSON object or array.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Configuration {
    data: BTreeMap<String, (String, Option<String>)>,
}

impl Configuration {
    /// An empty configuration.
    pub fn new() -> Configuration {
        Configuration::default()
    }

    /// Sets `key`, replacing a value under any casing of the same key (the first casing
    /// is kept, like a .NET `OrdinalIgnoreCase` dictionary).
    pub fn set(&mut self, key: &str, value: Option<String>) {
        match self.data.entry(fold_key(key)) {
            std::collections::btree_map::Entry::Occupied(mut e) => e.get_mut().1 = value,
            std::collections::btree_map::Entry::Vacant(e) => {
                e.insert((key.to_owned(), value));
            }
        }
    }

    /// The value of `key`, compared case-insensitively.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.data
            .get(&fold_key(key))
            .and_then(|(_, v)| v.as_deref())
    }

    /// Copies every entry of `other` over this configuration.
    pub fn merge(&mut self, other: &Configuration) {
        for (key, value) in other.data.values() {
            self.set(key, value.clone());
        }
    }

    /// The entries as `(key, value)`, sorted by folded key.
    pub fn iter(&self) -> impl Iterator<Item = (&str, Option<&str>)> {
        self.data.values().map(|(k, v)| (k.as_str(), v.as_deref()))
    }
}

/// What the .NET host reads from its process: arguments, environment, directories.
#[derive(Debug, Clone, Default)]
pub struct HostInputs {
    /// Command-line arguments without the program name.
    pub args: Vec<String>,
    /// Environment variables, in enumeration order.
    pub env: Vec<(String, String)>,
    /// The current directory (the content root of a console run).
    pub current_dir: PathBuf,
    /// `AppContext.BaseDirectory`: the directory of the executable.
    pub base_dir: PathBuf,
    /// `Environment.SystemDirectory` (`C:\Windows\system32`; empty off Windows). A service
    /// starts there, and then the content root is `base_dir`.
    pub system_dir: PathBuf,
}

impl HostInputs {
    /// The inputs of the running process. Non-Unicode arguments and variables are converted
    /// lossily instead of panicking.
    pub fn from_process() -> HostInputs {
        let base_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_default();
        HostInputs {
            args: std::env::args_os()
                .skip(1)
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
            env: std::env::vars_os()
                .map(|(k, v)| {
                    (
                        k.to_string_lossy().into_owned(),
                        v.to_string_lossy().into_owned(),
                    )
                })
                .collect(),
            current_dir: std::env::current_dir().unwrap_or_else(|_| base_dir.clone()),
            base_dir,
            system_dir: system_directory(),
        }
    }
}

#[cfg(windows)]
fn system_directory() -> PathBuf {
    std::env::var_os("SystemRoot")
        .or_else(|| std::env::var_os("windir"))
        .map(|root| PathBuf::from(root).join("system32"))
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows\system32"))
}

#[cfg(not(windows))]
fn system_directory() -> PathBuf {
    PathBuf::new()
}

/// The loaded configuration with the host settings derived from it.
#[derive(Debug, Clone)]
pub struct LoadedConfiguration {
    /// All sources merged (`builder.Configuration`).
    pub configuration: Configuration,
    /// `IHostEnvironment.EnvironmentName`: host key `environment`, default `Production`.
    pub environment: String,
    /// `IHostEnvironment.ContentRootPath`: where the appsettings files are read.
    pub content_root: PathBuf,
}

/// Loads every source like `Host.CreateApplicationBuilder(args)`. Errors are the
/// exceptions the .NET builder throws before the host exists (command line, JSON files).
pub fn load(inputs: &HostInputs) -> Result<LoadedConfiguration, DotnetError> {
    let command_line = parse_command_line(&inputs.args)?;
    let mut host = environment_variables(&inputs.env, Some("DOTNET_"));
    host.merge(&command_line);

    let content_root = match host.get("contentRoot") {
        Some(root) if Path::new(root).is_absolute() => PathBuf::from(root),
        Some(root) => inputs.base_dir.join(root),
        // `AppContext.BaseDirectory` ends with a separator, and so does the content root
        // path the host logs (`Path` equality ignores it).
        None if same_dir(&inputs.current_dir, &inputs.system_dir) => {
            let mut dir = inputs.base_dir.clone().into_os_string();
            if !dir.to_string_lossy().ends_with(std::path::is_separator) {
                dir.push(std::path::MAIN_SEPARATOR_STR);
            }
            PathBuf::from(dir)
        }
        None => inputs.current_dir.clone(),
    };
    let environment = host.get("environment").unwrap_or("Production").to_owned();

    let mut configuration = host;
    for name in [
        "appsettings.json".to_owned(),
        format!("appsettings.{environment}.json"),
    ] {
        if let Some(file) = json_file(&content_root.join(name))? {
            configuration.merge(&file);
        }
    }
    configuration.merge(&environment_variables(&inputs.env, None));
    configuration.merge(&command_line);
    Ok(LoadedConfiguration {
        configuration,
        environment,
        content_root,
    })
}

/// `string.Equals(cwd, Environment.SystemDirectory, OrdinalIgnoreCase)`.
fn same_dir(a: &Path, b: &Path) -> bool {
    !b.as_os_str().is_empty()
        && super::dotnet::eq_ignore_case(&a.to_string_lossy(), &b.to_string_lossy())
}

/// `EnvironmentVariablesConfigurationProvider`: `__` becomes `:`; with a prefix, only the
/// variables starting with it (case-insensitively), prefix removed.
pub fn environment_variables(env: &[(String, String)], prefix: Option<&str>) -> Configuration {
    let mut c = Configuration::new();
    for (name, value) in env {
        let key = name.replace("__", ":");
        let key = match prefix {
            None => key,
            Some(p) => {
                let p = p.replace("__", ":");
                match key.get(..p.len()) {
                    Some(head) if super::dotnet::eq_ignore_case(head, &p) => {
                        key[p.len()..].to_owned()
                    }
                    _ => continue,
                }
            }
        };
        c.set(&key, Some(value.clone()));
    }
    c
}

/// `CommandLineConfigurationProvider` without switch mappings: `--key=value`, `/key=value`,
/// `key=value`, `--key value` and `/key value`. `-key value` and a bare `key` are ignored;
/// `-key=value` is an error.
pub fn parse_command_line(args: &[String]) -> Result<Configuration, DotnetError> {
    let mut c = Configuration::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (current, key_start) = if arg.starts_with("--") {
            (arg.clone(), 2)
        } else if arg.starts_with('-') {
            (arg.clone(), 1)
        } else if let Some(rest) = arg.strip_prefix('/') {
            (format!("--{rest}"), 2)
        } else {
            (arg.clone(), 0)
        };
        match current.find('=') {
            None => {
                if key_start != 2 {
                    continue;
                }
                let Some(value) = it.next() else { break };
                c.set(&current[key_start..], Some(value.clone()));
            }
            Some(eq) => {
                if key_start == 1 {
                    return Err(DotnetError::new(
                        "System.FormatException",
                        format!("The short switch '{arg}' is not defined in the switch mappings."),
                    ));
                }
                c.set(&current[key_start..eq], Some(current[eq + 1..].to_owned()));
            }
        }
    }
    Ok(c)
}

/// An optional JSON settings file (`AddJsonFile(path, optional: true)`); `None` when it
/// does not exist.
fn json_file(path: &Path) -> Result<Option<Configuration>, DotnetError> {
    let failed = || {
        DotnetError::new(
            "System.IO.InvalidDataException",
            format!(
                "Failed to load configuration from file '{}'.",
                path.display()
            ),
        )
    };
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(
                failed().with_inner(DotnetError::new("System.IO.IOException", e.to_string()))
            );
        }
    };
    parse_json_settings(&bytes)
        .map(Some)
        .map_err(|e| failed().with_inner(e))
}

/// `JsonConfigurationFileParser`: flattens a JSON object into `a:b:0` keys. Comments and
/// trailing commas are allowed; numbers keep their source text; `true`/`false` become
/// `True`/`False`; a key repeated under another casing is an error.
pub fn parse_json_settings(bytes: &[u8]) -> Result<Configuration, DotnetError> {
    let text = std::str::from_utf8(bytes).map_err(|e| parse_error(&e.to_string()))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let cleaned = strip_comments_and_trailing_commas(text)
        .ok_or_else(|| parse_error("unterminated comment"))?;
    let opts = dl_json::ParseOptions {
        keep_raw_numbers: true,
        ..dl_json::ParseOptions::default()
    };
    let root = dl_json::parse_with(&cleaned, opts).map_err(|e| parse_error(&e.to_string()))?;
    if !root.is_object() {
        return Err(DotnetError::new(
            "System.FormatException",
            format!(
                "Top-level JSON element must be an object. Instead, '{}' was found.",
                value_kind(&root)
            ),
        ));
    }
    let mut out = Configuration::new();
    flatten(&root, "", &mut out)?;
    Ok(out)
}

fn parse_error(detail: &str) -> DotnetError {
    DotnetError::new("System.FormatException", "Could not parse the JSON file.").with_inner(
        DotnetError::new("System.Text.Json.JsonReaderException", detail),
    )
}

/// `JsonValueKind` names.
fn value_kind(v: &dl_json::Value) -> &'static str {
    match v {
        dl_json::Value::Null => "Null",
        dl_json::Value::Bool(true) => "True",
        dl_json::Value::Bool(false) => "False",
        dl_json::Value::Number(_) => "Number",
        dl_json::Value::String(_) => "String",
        dl_json::Value::Array(_) => "Array",
        dl_json::Value::Object(_) => "Object",
    }
}

fn flatten(v: &dl_json::Value, path: &str, out: &mut Configuration) -> Result<(), DotnetError> {
    let child = |k: &str| {
        if path.is_empty() {
            k.to_owned()
        } else {
            format!("{path}:{k}")
        }
    };
    match v {
        dl_json::Value::Object(map) => {
            if map.is_empty() && !path.is_empty() {
                set_unique(out, path, None)?;
            }
            for (k, item) in map {
                flatten(item, &child(k), out)?;
            }
        }
        dl_json::Value::Array(items) => {
            if items.is_empty() {
                set_unique(out, path, None)?;
            }
            for (i, item) in items.iter().enumerate() {
                flatten(item, &child(&i.to_string()), out)?;
            }
        }
        dl_json::Value::Null => set_unique(out, path, None)?,
        dl_json::Value::Bool(b) => set_unique(
            out,
            path,
            Some(if *b { "True" } else { "False" }.to_owned()),
        )?,
        dl_json::Value::Number(n) => set_unique(
            out,
            path,
            Some(n.as_raw_str().map_or_else(|| n.to_string(), str::to_owned)),
        )?,
        dl_json::Value::String(s) => set_unique(out, path, Some(s.clone()))?,
    }
    Ok(())
}

fn set_unique(
    out: &mut Configuration,
    key: &str,
    value: Option<String>,
) -> Result<(), DotnetError> {
    if out.data.contains_key(&fold_key(key)) {
        return Err(DotnetError::new(
            "System.FormatException",
            format!("A duplicate key '{key}' was found."),
        ));
    }
    out.set(key, value);
    Ok(())
}

/// Replaces `//` and `/* */` comments outside strings, and commas directly before `}` or
/// `]`, with spaces (so positions in error messages stay right). `None` for an unterminated
/// block comment.
fn strip_comments_and_trailing_commas(text: &str) -> Option<String> {
    let b = text.as_bytes();
    let mut out: Vec<u8> = b.to_vec();
    let mut i = 0;
    let mut in_string = false;
    while i < b.len() {
        let c = b[i];
        if in_string {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match (c, b.get(i + 1)) {
            (b'"', _) => in_string = true,
            (b'/', Some(b'/')) => {
                while i < b.len() && b[i] != b'\n' {
                    out[i] = b' ';
                    i += 1;
                }
                continue;
            }
            (b'/', Some(b'*')) => {
                let end = text[i + 2..].find("*/")? + i + 2;
                for o in &mut out[i..end + 2] {
                    if *o != b'\n' {
                        *o = b' ';
                    }
                }
                i = end + 2;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    // Second pass on the comment-free text: drop a comma whose next significant byte closes
    // the container and whose previous significant byte is a value.
    let mut in_string = false;
    let mut last_significant: Option<u8> = None;
    let mut i = 0;
    while i < out.len() {
        let c = out[i];
        if in_string {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_string = false;
                last_significant = Some(b'"');
            }
            i += 1;
            continue;
        }
        if c == b'"' {
            in_string = true;
        } else if c == b',' {
            let next = out[i + 1..]
                .iter()
                .copied()
                .find(|x| !x.is_ascii_whitespace());
            let closes = matches!(next, Some(b'}' | b']'));
            let after_value = !matches!(last_significant, None | Some(b'{' | b'[' | b','));
            if closes && after_value {
                out[i] = b' ';
                i += 1;
                continue;
            }
        }
        if !c.is_ascii_whitespace() {
            last_significant = Some(c);
        }
        i += 1;
    }
    // Only ASCII bytes were replaced by ASCII spaces, so the text is still UTF-8.
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    fn pairs(c: &Configuration) -> Vec<(String, Option<String>)> {
        c.iter()
            .map(|(k, v)| (k.to_owned(), v.map(str::to_owned)))
            .collect()
    }

    fn one(k: &str, v: &str) -> Vec<(String, Option<String>)> {
        vec![(k.to_owned(), Some(v.to_owned()))]
    }

    /// The cases measured with `CommandLineConfigurationProvider` on .NET 10.
    #[test]
    fn command_line_matches_dotnet() {
        type Entries = Vec<(String, Option<String>)>;
        let cases: [(&[&str], Entries); 15] = [
            (&["--Agent:AgentId=FD01"], one("Agent:AgentId", "FD01")),
            (&["--Agent:AgentId", "FD01"], one("Agent:AgentId", "FD01")),
            (&["/Agent:AgentId=FD01"], one("Agent:AgentId", "FD01")),
            (&["/Agent:AgentId", "FD01"], one("Agent:AgentId", "FD01")),
            (&["Agent:AgentId=FD01"], one("Agent:AgentId", "FD01")),
            (&["Agent:AgentId", "FD01"], vec![]),
            (&["-Agent:AgentId", "FD01"], vec![]),
            (&["--Agent:AgentId"], vec![]),
            (
                &["--Agent:AgentId", "--Agent:TokenFile=x"],
                one("Agent:AgentId", "--Agent:TokenFile=x"),
            ),
            (&["--Agent__AgentId=FD01"], one("Agent__AgentId", "FD01")),
            (&["--Agent:AgentId=a=b"], one("Agent:AgentId", "a=b")),
            (&["--=x"], one("", "x")),
            (
                &["--Agent:AgentId=A", "--agent:agentid=B"],
                one("Agent:AgentId", "B"),
            ),
            (&["---Agent:AgentId=A"], one("-Agent:AgentId", "A")),
            (&["=x"], one("", "x")),
        ];
        for (a, expected) in cases {
            assert_eq!(
                pairs(&parse_command_line(&args(a)).unwrap()),
                expected,
                "{a:?}"
            );
        }
        let err = parse_command_line(&args(&["-Agent:AgentId=FD01"])).unwrap_err();
        assert_eq!(
            err.to_string(),
            "System.FormatException: The short switch '-Agent:AgentId=FD01' is not defined in the switch mappings."
        );
        let c = parse_command_line(&args(&["--Agent:AgentId=A", "--agent:agentid=B"])).unwrap();
        assert_eq!(c.get("AGENT:AGENTID"), Some("B"));
    }

    #[test]
    fn environment_variables_map_double_underscore_and_prefix() {
        let env = vec![
            ("Agent__ProbeKey".to_owned(), "env1".to_owned()),
            ("DOTNET_Agent__ProbeKey2".to_owned(), "env2".to_owned()),
            ("Agent___Probe3".to_owned(), "env3".to_owned()),
        ];
        let all = environment_variables(&env, None);
        assert_eq!(all.get("Agent:ProbeKey"), Some("env1"));
        assert_eq!(all.get("DOTNET_Agent:ProbeKey2"), Some("env2"));
        assert_eq!(all.get("Agent:_Probe3"), Some("env3"));
        let host = environment_variables(&env, Some("DOTNET_"));
        assert_eq!(pairs(&host), one("Agent:ProbeKey2", "env2"));
        let lower = environment_variables(
            &[("dotnet_environment".to_owned(), "Development".to_owned())],
            Some("DOTNET_"),
        );
        assert_eq!(lower.get("environment"), Some("Development"));
    }

    /// The cases measured with `AddJsonStream` on .NET 10.
    #[test]
    fn json_settings_match_dotnet() {
        let c = parse_json_settings(
            br#"{"Agent":{"AgentId":null,"RunOnce":null,"Lock":{},"TokenFile":"t"}}"#,
        )
        .unwrap();
        assert_eq!(
            pairs(&c),
            vec![
                ("Agent:AgentId".to_owned(), None),
                ("Agent:Lock".to_owned(), None),
                ("Agent:RunOnce".to_owned(), None),
                ("Agent:TokenFile".to_owned(), Some("t".to_owned())),
            ]
        );
        let c = parse_json_settings(b"// c\n{\"Agent\":{/* x */\"AgentId\":\"A\",},}").unwrap();
        assert_eq!(pairs(&c), one("Agent:AgentId", "A"));
        let c = parse_json_settings(
            br#"{"Agent":{"PollInterval":1,"RunOnce":true,"Lock":{"MfLockComPort":3.50}}}"#,
        )
        .unwrap();
        assert_eq!(c.get("Agent:PollInterval"), Some("1"));
        assert_eq!(c.get("Agent:RunOnce"), Some("True"));
        assert_eq!(c.get("Agent:Lock:MfLockComPort"), Some("3.50"));
        let err = parse_json_settings(br#"{"Agent":{"AgentId":"A","agentid":"B"}}"#).unwrap_err();
        assert_eq!(
            err.to_string(),
            "System.FormatException: A duplicate key 'Agent:agentid' was found."
        );
        let c = parse_json_settings(br#"{"Agent":{"AgentId":["A","B"],"x":[]}}"#).unwrap();
        assert_eq!(
            pairs(&c),
            vec![
                ("Agent:AgentId:0".to_owned(), Some("A".to_owned())),
                ("Agent:AgentId:1".to_owned(), Some("B".to_owned())),
                ("Agent:x".to_owned(), None),
            ]
        );
        let err = parse_json_settings(b"[1]").unwrap_err();
        assert_eq!(
            err.to_string(),
            "System.FormatException: Top-level JSON element must be an object. Instead, 'Array' was found."
        );
        assert_eq!(
            parse_json_settings(br#"{"Agent":"#).unwrap_err().message,
            "Could not parse the JSON file."
        );
        assert_eq!(
            parse_json_settings(b"").unwrap_err().message,
            "Could not parse the JSON file."
        );
        assert_eq!(
            parse_json_settings(br#"{"Agent":"x"}"#)
                .unwrap()
                .get("agent"),
            Some("x")
        );
        assert_eq!(
            parse_json_settings(br#"{"Agent":{"Agent\u0049d":"Z"}}"#)
                .unwrap()
                .get("Agent:AgentId"),
            Some("Z")
        );
        assert_eq!(
            parse_json_settings(br#"{"Agent:AgentId":"Q"}"#)
                .unwrap()
                .get("Agent:AgentId"),
            Some("Q")
        );
        // Comment markers inside strings are data.
        assert_eq!(
            parse_json_settings(br#"{"a":"http://x/*y*/",}"#)
                .unwrap()
                .get("a"),
            Some("http://x/*y*/")
        );
        assert_eq!(
            parse_json_settings("\u{feff}{\"a\":\"b\"}".as_bytes())
                .unwrap()
                .get("a"),
            Some("b")
        );
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("device-agent-cfg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_applies_the_dotnet_precedence_and_content_root() {
        let root = temp_dir("load");
        std::fs::write(
            root.join("appsettings.json"),
            r#"{"Agent":{"AgentId":"json","TokenFile":"json","Adapter":"none","RunOnce":false}}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("appsettings.Development.json"),
            r#"{"Agent":{"AgentId":"dev","Adapter":"simulated-lock"}}"#,
        )
        .unwrap();
        let inputs = HostInputs {
            args: args(&["--Agent:AgentId=cmd"]),
            env: vec![
                ("DOTNET_ENVIRONMENT".to_owned(), "Development".to_owned()),
                ("DOTNET_Agent__RunOnce".to_owned(), "true".to_owned()),
                ("Agent__TokenFile".to_owned(), "env".to_owned()),
                (
                    "DOTNET_Agent__PollInterval".to_owned(),
                    "00:00:05".to_owned(),
                ),
            ],
            current_dir: root.clone(),
            base_dir: PathBuf::from("/nowhere"),
            system_dir: PathBuf::from("/system32"),
        };
        let loaded = load(&inputs).unwrap();
        assert_eq!(loaded.environment, "Development");
        assert_eq!(loaded.content_root, root);
        let c = &loaded.configuration;
        assert_eq!(c.get("Agent:AgentId"), Some("cmd"));
        assert_eq!(c.get("Agent:TokenFile"), Some("env"));
        assert_eq!(c.get("Agent:Adapter"), Some("simulated-lock"));
        // DOTNET_-prefixed variables are host settings, below the JSON files.
        assert_eq!(c.get("Agent:RunOnce"), Some("False"));
        assert_eq!(c.get("Agent:PollInterval"), Some("00:00:05"));

        // A service starts in the system directory: the content root is the exe directory.
        let service = HostInputs {
            current_dir: PathBuf::from("/SYSTEM32"),
            system_dir: PathBuf::from("/system32"),
            base_dir: root.clone(),
            args: vec![],
            env: vec![],
        };
        let loaded = load(&service).unwrap();
        assert_eq!(
            (loaded.content_root.as_path(), loaded.environment.as_str()),
            (root.as_path(), "Production")
        );
        assert_eq!(loaded.configuration.get("Agent:AgentId"), Some("json"));
        assert!(
            loaded
                .content_root
                .to_string_lossy()
                .ends_with(std::path::MAIN_SEPARATOR)
        );

        // --contentRoot relative to the exe directory.
        let other = HostInputs {
            args: args(&["--contentRoot", "sub"]),
            current_dir: PathBuf::from("/elsewhere"),
            base_dir: root.clone(),
            ..HostInputs::default()
        };
        assert_eq!(load(&other).unwrap().content_root, root.join("sub"));

        std::fs::write(root.join("appsettings.json"), "{bad").unwrap();
        let err = load(&service).unwrap_err();
        assert_eq!(err.type_name, "System.IO.InvalidDataException");
        assert!(
            err.message
                .starts_with("Failed to load configuration from file '")
                && err.message.ends_with("appsettings.json'.")
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
