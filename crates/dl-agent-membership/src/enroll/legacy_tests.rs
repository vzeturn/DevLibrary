//! Every format the .NET agent keeps its identity in, as plaintext samples: the
//! `config.dpapi` JSON exactly as `JsonSerializer.SerializeToUtf8Bytes(AgentConfig)` writes
//! it (sealed here with the test sealer; real DPAPI only runs on Windows), the seed
//! configuration (`appsettings*.json` + `Agent__*`) and the portable store.

use std::sync::Arc;

use super::*;
use crate::config::{ConfigStore, FILE_NAME, PlainSealer, Sealer};

pub(crate) fn scratch(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let d = std::env::temp_dir().join(format!("dl-mb-legacy-{tag}-{nanos}"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

const HOTEL: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";

/// `JsonSerializer.SerializeToUtf8Bytes(new AgentConfig(...))` with the default options:
/// declaration order, compact, `+ & '` and non-ASCII escaped, `\` doubled, `/` kept.
const STJ: &str = concat!(
    r#"{"HubBaseUrl":"https://membership.victoryhotel.vn/api","#,
    r#""AgentToken":"mbat_9xQ\u002B/abc==","#,
    r#""HotelId":"0f8fad5b-d9cb-469f-a165-70867728950e","#,
    r#""SmileConnectionString":"Data Source=SMILESRV\\SQL2008;Initial Catalog=SMILE_FO;User ID=smile;Password=P\u0026ss\u0027w\u0110;","#,
    r#""OriginCode":"Membership"}"#
);

fn stj_config() -> AgentConfig {
    AgentConfig {
        hub_base_url: "https://membership.victoryhotel.vn/api".into(),
        agent_token: "mbat_9xQ+/abc==".into(),
        hotel_id: HOTEL.parse().unwrap(),
        smile_connection_string:
            "Data Source=SMILESRV\\SQL2008;Initial Catalog=SMILE_FO;User ID=smile;Password=P&ss'wĐ;"
                .into(),
        origin_code: "Membership".into(),
    }
}

fn sealed(dir: &Path, plain: &str) -> ConfigStore {
    std::fs::write(
        dir.join(FILE_NAME),
        PlainSealer.seal(plain.as_bytes()).unwrap(),
    )
    .unwrap();
    ConfigStore::new(dir, Arc::new(PlainSealer))
}

#[test]
fn reads_and_writes_the_dotnet_blob_byte_for_byte() {
    let dir = scratch("stj");
    let store = sealed(&dir, STJ);
    assert_eq!(store.load().unwrap(), Some(stj_config()));
    assert_eq!(stj_config().to_json(), STJ);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn reads_a_blob_with_other_order_extra_properties_and_no_origin() {
    let dir = scratch("order");
    let plain = concat!(
        "\u{feff}{\"Extra\":{\"Nested\":[1,2]},\"HotelId\":\"0F8FAD5B-D9CB-469F-A165-70867728950E\",",
        "\"SmileConnectionString\":\"Server=.;Database=SMILE_FO\",\"Unknown\":null,",
        "\"AgentToken\":\"t\",\"HubBaseUrl\":\"https://hub.example.vn\"}"
    );
    let cfg = sealed(&dir, plain).load().unwrap().unwrap();
    assert_eq!(cfg.hotel_id.to_string(), HOTEL);
    assert_eq!(cfg.hub_base_url, "https://hub.example.vn");
    assert_eq!(cfg.smile_connection_string, "Server=.;Database=SMILE_FO");
    assert_eq!(cfg.origin_code, "");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The packaged `appsettings.json` of the .NET agent (its `Agent` section, trimmed).
const PACKAGED: &str = r#"{
  "Serilog": { "__comment": "log ra FILE // khong phai comment", "MinimumLevel": { "Default": "Information" } },
  "Worker": { "MaxJobsPerPull": 20, "IdlePollSeconds": 5, "HeartbeatSeconds": 30 },
  "Agent": {
    "HubBaseUrl": "",
    "AgentToken": "",
    "HotelId": "",
    "SmileConnectionString": "",
    "OriginCode": "Membership"
  }
}"#;

fn places(dir: &Path, portable: bool) -> Places {
    Places {
        install_dirs: vec![dir.join("missing"), dir.to_path_buf()],
        environment: "Production".into(),
        portable,
    }
}

fn no_env(_: &str) -> Option<String> {
    None
}

#[test]
fn the_packaged_settings_alone_are_not_a_configuration() {
    let dir = scratch("packaged");
    std::fs::write(dir.join("appsettings.json"), PACKAGED).unwrap();
    let seed = read_seed(&places(&dir, false), &no_env).unwrap().unwrap();
    assert_eq!(seed.files, vec![dir.join("appsettings.json")]);
    assert_eq!(seed.section.config().unwrap(), None);
    assert_eq!(seed.section.smile(), None);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_hotel_file_with_comments_and_any_case_seeds_like_agent_config_reader() {
    let dir = scratch("hotel");
    std::fs::write(dir.join("appsettings.json"), PACKAGED).unwrap();
    std::fs::write(
        dir.join("appsettings.Production.json"),
        r#"{"Agent":{"OriginCode":"Prod"}}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("appsettings.hotel.json"),
        r#"{
  // Victory Sai Gon
  "agent": {
    "hubBaseUrl": "https://membership.victoryhotel.vn//", /* trailing '/' trimmed */
    "agentToken": "  tok-1  ",
    "hotelId": "0f8fad5b-d9cb-469f-a165-70867728950e",
    "SmileConnectionString": "Server=SMILESRV;Database=SMILE_FO;User Id=sa;Password=a\"b//c;",
  },
}"#,
    )
    .unwrap();
    let seed = read_seed(&places(&dir, false), &no_env).unwrap().unwrap();
    assert_eq!(seed.files.len(), 3);
    assert!(!seed.portable);
    let cfg = seed.section.config().unwrap().unwrap();
    assert_eq!(cfg.hub_base_url, "https://membership.victoryhotel.vn");
    assert_eq!(cfg.agent_token, "tok-1");
    assert_eq!(cfg.hotel_id.to_string(), HOTEL);
    assert_eq!(
        cfg.smile_connection_string,
        "Server=SMILESRV;Database=SMILE_FO;User Id=sa;Password=a\"b//c;"
    );
    assert_eq!(cfg.origin_code, "Prod");
    let shown = format!("{:?}", seed.section);
    assert!(
        !shown.contains("tok-1") && !shown.contains("Password"),
        "{shown}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn smile_fo_is_used_only_when_the_key_is_absent_and_the_environment_wins() {
    let dir = scratch("smilefo");
    std::fs::write(
        dir.join("appsettings.hotel.json"),
        r#"{"ConnectionStrings":{"SmileFo":"Server=fo"},"Agent":{"HubBaseUrl":"https://a.vn","AgentToken":"t","HotelId":"0f8fad5b-d9cb-469f-a165-70867728950e"}}"#,
    )
    .unwrap();
    let seed = read_seed(&places(&dir, false), &no_env).unwrap().unwrap();
    let cfg = seed.section.config().unwrap().unwrap();
    assert_eq!(cfg.smile_connection_string, "Server=fo");
    assert_eq!(cfg.origin_code, DEFAULT_ORIGIN);

    std::fs::write(dir.join("appsettings.json"), PACKAGED).unwrap();
    let seed = read_seed(&places(&dir, false), &no_env).unwrap().unwrap();
    assert_eq!(seed.section.config().unwrap(), None, "\"\" hides SmileFo");

    let env = |k: &str| match k {
        "Agent__SmileConnectionString" => Some("Server=env".to_owned()),
        "Agent__HubBaseUrl" => Some("https://env.vn/".to_owned()),
        _ => None,
    };
    let cfg = read_seed(&places(&dir, false), &env)
        .unwrap()
        .unwrap()
        .section
        .config()
        .unwrap()
        .unwrap();
    assert_eq!(cfg.smile_connection_string, "Server=env");
    assert_eq!(cfg.hub_base_url, "https://env.vn");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_bad_hotel_id_or_bad_json_is_an_error_without_the_value() {
    let dir = scratch("bad");
    std::fs::write(
        dir.join("appsettings.hotel.json"),
        r#"{"Agent":{"HubBaseUrl":"https://a.vn","AgentToken":"t","HotelId":"hotel-42","SmileConnectionString":"s"}}"#,
    )
    .unwrap();
    let seed = read_seed(&places(&dir, false), &no_env).unwrap().unwrap();
    let e = seed.section.config().unwrap_err();
    assert!(e.contains("HotelId") && !e.contains("hotel-42"), "{e}");

    std::fs::write(
        dir.join("appsettings.json"),
        "{\"Agent\": {\"AgentToken\": \"sec",
    )
    .unwrap();
    let e = read_seed(&places(&dir, false), &no_env).unwrap_err();
    assert!(
        e.contains("appsettings.json") && !e.contains("sec\""),
        "{e}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn nothing_anywhere_is_none() {
    let dir = scratch("none");
    assert_eq!(read_seed(&places(&dir, false), &no_env).unwrap(), None);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn reads_the_portable_store_alone_as_written_by_portable_agent_config_store() {
    let dir = scratch("portable");
    std::fs::write(dir.join("appsettings.json"), PACKAGED).unwrap();
    let file = dir.join("secret").join("config.json");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    // `JsonSerializer.Serialize(new { Agent = new { ... } }, WriteIndented = true)`.
    std::fs::write(
        &file,
        "{\n  \"Agent\": {\n    \"HubBaseUrl\": \"https://hub.example.vn\",\n    \"AgentToken\": \"tok\\u002B\",\n    \"HotelId\": \"0f8fad5b-d9cb-469f-a165-70867728950e\",\n    \"SmileConnectionString\": \"Server=x\",\n    \"OriginCode\": \"AZ\"\n  }\n}",
    )
    .unwrap();
    let path = file.display().to_string();
    let env = move |k: &str| (k == "Agent__SecretStorePath").then(|| path.clone());
    let seed = read_seed(&places(&dir, true), &env).unwrap().unwrap();
    assert!(seed.portable);
    assert_eq!(seed.files, vec![file.clone()]);
    let cfg = seed.section.config().unwrap().unwrap();
    assert_eq!(
        (cfg.agent_token.as_str(), cfg.origin_code.as_str()),
        ("tok+", "AZ")
    );

    let seed = read_seed(&places(&dir, false), &env).unwrap().unwrap();
    assert!(!seed.portable, "no portable store on Windows");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn strips_comments_and_trailing_commas_outside_strings_only() {
    assert_eq!(
        strip_jsonc("{\"a\":\"x//y /* z */ \\\"q\\\",\",// c\n\"b\":[1,2,],/* d */}"),
        "{\"a\":\"x//y /* z */ \\\"q\\\",\",\n\"b\":[1,2]}"
    );
}

#[test]
fn finds_the_install_folder_of_a_service_command_line() {
    assert_eq!(
        exe_dir(r#""C:\Program Files\MM.Membership.Agent\MM.Membership.Agent.exe" --x"#),
        Some(PathBuf::from(r"C:\Program Files\MM.Membership.Agent"))
    );
    assert_eq!(
        exe_dir(r"D:\Agents\MM\MM.Membership.Agent.exe"),
        Some(PathBuf::from(r"D:\Agents\MM"))
    );
    assert_eq!(exe_dir("agent.exe"), None);
}
