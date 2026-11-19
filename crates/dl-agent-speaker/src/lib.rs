#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

/// The enrolled `pda-sync` identity as the speaker's credential.
pub mod auth;
/// Guest events and the per-RVC tone/speech decision.
pub mod decision;
/// Sound-mode fetch from the PDA cloud.
pub mod fetch;
/// The order hub connection (`JoinRvc`, reconnect).
pub mod hub;
/// The `guest.speakerSoundMode` setting and its per-RVC cache.
pub mod mode;
/// The dl-agent module and its hub commands.
pub mod module;
/// The output queue and the system speaker.
pub mod output;
/// The spoken phrases (vi/en).
pub mod phrase;
/// Windows SAPI speech and tone playback.
#[cfg(windows)]
pub mod sapi;
/// The embedded alert tones.
pub mod tones;

pub use module::{SpeakerModule, speaker_module};
