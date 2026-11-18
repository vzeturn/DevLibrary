//! `AgentOptions` (`Agent:*`) and `LockOptions` (`Agent:Lock:*`) with the C# defaults.

use std::fmt;
use std::path::PathBuf;

use super::TimeSpan;

/// `AgentOptions.Section`: the configuration section the options are bound from.
pub const SECTION: &str = "Agent";

/// `Agent:*` settings (`Agent/AgentOptions.cs`).
#[derive(Debug, Clone, PartialEq)]
pub struct AgentOptions {
    /// `ApiBaseUrl`, default `https://localhost:3001`.
    pub api_base_url: String,
    /// `AgentId`, default empty (the validator then refuses to start).
    pub agent_id: String,
    /// `TokenFile`: path of the file holding the agent's bearer token.
    pub token_file: String,
    /// `PollInterval`, default 1 s.
    pub poll_interval: TimeSpan,
    /// `CommandTimeout`, default 30 s.
    pub command_timeout: TimeSpan,
    /// `RunOnce`: stop when the queue is empty (smoke runs).
    pub run_once: bool,
    /// `Adapter`: `none` (default), `simulated-lock` or `smile-lock`.
    pub adapter: String,
    /// `Lock`: door-lock settings for `Adapter = smile-lock`.
    pub lock: LockOptions,
    /// `DataDirectory`: holds the result outbox; must survive restarts. Default
    /// `%ProgramData%\SmilePms\DeviceAgent` (`/usr/share/SmilePms/DeviceAgent` off Windows,
    /// where .NET maps `CommonApplicationData` to `/usr/share`).
    pub data_directory: String,
}

impl Default for AgentOptions {
    fn default() -> Self {
        AgentOptions {
            api_base_url: "https://localhost:3001".to_owned(),
            agent_id: String::new(),
            token_file: String::new(),
            poll_interval: TimeSpan::from_secs(1),
            command_timeout: TimeSpan::from_secs(30),
            run_once: false,
            adapter: crate::adapters::NOT_CONNECTED.to_owned(),
            lock: LockOptions::default(),
            data_directory: default_data_directory(),
        }
    }
}

impl AgentOptions {
    /// `OutboxDirectory`: `DataDirectory/outbox`.
    pub fn outbox_directory(&self) -> PathBuf {
        PathBuf::from(&self.data_directory).join("outbox")
    }
}

/// `Path.Combine(Environment.GetFolderPath(CommonApplicationData), "SmilePms", "DeviceAgent")`.
fn default_data_directory() -> String {
    let common = common_application_data();
    PathBuf::from(common)
        .join("SmilePms")
        .join("DeviceAgent")
        .to_string_lossy()
        .into_owned()
}

/// `Environment.SpecialFolder.CommonApplicationData`: the ProgramData folder on Windows.
#[cfg(windows)]
fn common_application_data() -> String {
    std::env::var("ProgramData")
        .or_else(|_| std::env::var("ALLUSERSPROFILE"))
        .ok()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| r"C:\ProgramData".to_owned())
}

/// `Environment.SpecialFolder.CommonApplicationData` on Unix (.NET maps it to `/usr/share`).
#[cfg(not(windows))]
fn common_application_data() -> String {
    "/usr/share".to_owned()
}

/// `Agent:Lock:*` (`Adapters/Lock/LockOptions.cs`): the workstation's SMILE_FO lock settings.
/// The payload's `vendor` wins over `lock_key_type` and `mf_lock_name`. `Debug` hides
/// `tt_lock_client_secret`.
#[derive(Clone, PartialEq, Eq)]
pub struct LockOptions {
    /// `LockKeyType`: WSParameter LockKeyType fallback (MFCARD, HUNE2013, ORBITA, TTLOCK, MULTI, ...).
    pub lock_key_type: String,
    /// `MfLockName`: WSParameter MFLockName fallback (hls, goodum, ezibee, adel, ...).
    pub mf_lock_name: String,
    /// `MfLockServer`: WSParameter MFLockServer (the Adel server).
    pub mf_lock_server: String,
    /// `MfLockComPort`: WSParameter MFLockComPort, default 0.
    pub mf_lock_com_port: i32,
    /// `MfLockComBaud`: WSParameter MFLockComBaud, default 19200.
    pub mf_lock_com_baud: i32,
    /// `MfLockMore`: WSParameter MFLockMore (vendor extras, passed through as P5).
    pub mf_lock_more: String,
    /// `MfLockDoorType`: LockSDK TP_Configuration door type for goodum / ezibee, default 5.
    pub mf_lock_door_type: i32,
    /// `HuneComPort`: WSParameter HUNEComPort, default 1.
    pub hune_com_port: i32,
    /// `HuneCardCode`: GlobalParameter HUNECardCode (the HSmart CardPass).
    pub hune_card_code: String,
    /// `HuneSystemCode`: GlobalParameter HUNESystemCode.
    pub hune_system_code: String,
    /// `HuneHotelCode`: GlobalParameter HUNEHotelCode.
    pub hune_hotel_code: String,
    /// `OrbitaVersion`: GlobalParameter OrbitaCLock, default 3.
    pub orbita_version: i32,
    /// `TtLockClientId`: GlobalParameter ttLockClientID.
    pub tt_lock_client_id: String,
    /// `TtLockClientSecret`: GlobalParameter ttLockClientSec. Secret: never log or echo it.
    pub tt_lock_client_secret: String,
    /// `TtLockCardType`: the second ttLockInterface argument, default 1.
    pub tt_lock_card_type: i32,
}

impl Default for LockOptions {
    fn default() -> Self {
        LockOptions {
            lock_key_type: String::new(),
            mf_lock_name: String::new(),
            mf_lock_server: String::new(),
            mf_lock_com_port: 0,
            mf_lock_com_baud: 19200,
            mf_lock_more: String::new(),
            mf_lock_door_type: 5,
            hune_com_port: 1,
            hune_card_code: String::new(),
            hune_system_code: String::new(),
            hune_hotel_code: String::new(),
            orbita_version: 3,
            tt_lock_client_id: String::new(),
            tt_lock_client_secret: String::new(),
            tt_lock_card_type: 1,
        }
    }
}

impl fmt::Debug for LockOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LockOptions")
            .field("lock_key_type", &self.lock_key_type)
            .field("mf_lock_name", &self.mf_lock_name)
            .field("mf_lock_server", &self.mf_lock_server)
            .field("mf_lock_com_port", &self.mf_lock_com_port)
            .field("mf_lock_com_baud", &self.mf_lock_com_baud)
            .field("mf_lock_more", &self.mf_lock_more)
            .field("mf_lock_door_type", &self.mf_lock_door_type)
            .field("hune_com_port", &self.hune_com_port)
            .field("hune_card_code", &self.hune_card_code)
            .field("hune_system_code", &self.hune_system_code)
            .field("hune_hotel_code", &self.hune_hotel_code)
            .field("orbita_version", &self.orbita_version)
            .field("tt_lock_client_id", &self.tt_lock_client_id)
            .field(
                "tt_lock_client_secret",
                &if self.tt_lock_client_secret.is_empty() {
                    ""
                } else {
                    "***"
                },
            )
            .field("tt_lock_card_type", &self.tt_lock_card_type)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i1_o1_the_default_adapter_is_none() {
        assert_eq!(AgentOptions::default().adapter, "none");
    }

    #[test]
    fn defaults_match_the_csharp_initialisers() {
        let o = AgentOptions::default();
        assert_eq!(o.api_base_url, "https://localhost:3001");
        assert_eq!(
            (o.poll_interval, o.command_timeout, o.run_once),
            (TimeSpan::from_secs(1), TimeSpan::from_secs(30), false)
        );
        assert!(
            o.data_directory
                .ends_with(&format!("SmilePms{}DeviceAgent", std::path::MAIN_SEPARATOR))
        );
        assert_eq!(
            o.outbox_directory(),
            PathBuf::from(&o.data_directory).join("outbox")
        );
        let l = LockOptions::default();
        assert_eq!(
            (
                l.mf_lock_com_port,
                l.mf_lock_com_baud,
                l.mf_lock_door_type,
                l.hune_com_port
            ),
            (0, 19200, 5, 1)
        );
        assert_eq!((l.orbita_version, l.tt_lock_card_type), (3, 1));
    }

    #[test]
    fn debug_never_shows_the_ttlock_secret() {
        let l = LockOptions {
            tt_lock_client_secret: "s3cr3t".into(),
            ..LockOptions::default()
        };
        let shown = format!("{l:?}");
        assert!(!shown.contains("s3cr3t") && shown.contains("***"));
    }
}
