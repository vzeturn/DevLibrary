//! `SpeakerAdapter.cs`: the payment speaker (POS-HW-06, PDAOrder "announce payment"),
//! text-to-speech through Windows SAPI (`SAPI.SpVoice`, COM). Without SAPI every call refuses
//! with DEVICE_UNAVAILABLE instead of reporting a silent success.

use std::sync::Arc;

use dl_json::{Value, json};

use super::{failures, target_of, trimmed_endpoint};
use crate::adapters::Cx;
use crate::contracts::{AdapterError, PosSpeakRequest};

/// Says a sentence on the workstation speaker. Faked in tests.
pub trait SpeechEngine: Send + Sync {
    /// Blocks until the sentence is spoken. `voice_hint` is the trimmed target endpoint
    /// (`default` or part of a voice description).
    fn speak(
        &self,
        voice_hint: &str,
        text: &str,
        volume: i32,
        rate: i32,
    ) -> Result<(), AdapterError>;
}

/// The SAPI speaker.
pub struct SpeakerAdapter {
    engine: Option<Arc<dyn SpeechEngine>>,
}

impl SpeakerAdapter {
    /// Longest text, in UTF-16 units.
    pub const MAX_TEXT_LENGTH: usize = 200;

    /// A speaker over `engine`; `None` is a host without SAPI.
    pub fn new(engine: Option<Arc<dyn SpeechEngine>>) -> SpeakerAdapter {
        SpeakerAdapter { engine }
    }

    /// SAPI when `SAPI.SpVoice` is registered (Windows); none otherwise.
    pub fn create() -> SpeakerAdapter {
        SpeakerAdapter::new(
            SapiSpeechEngine::try_create().map(|e| Arc::new(e) as Arc<dyn SpeechEngine>),
        )
    }

    /// pos.speak: validates, then says the trimmed text; answers
    /// `{"spoken":true,"characters":n}`.
    pub fn speak(&self, request: &PosSpeakRequest, cx: &Cx) -> Result<Value, AdapterError> {
        let text = request
            .text
            .as_deref()
            .ok_or_else(failures::null_reference)?
            .trim();
        let length = text.encode_utf16().count();
        if length == 0 || length > Self::MAX_TEXT_LENGTH {
            return Err(failures::bad_payload(format!(
                "speak text must be 1-{} characters",
                Self::MAX_TEXT_LENGTH
            )));
        }
        if !(0..=100).contains(&request.volume) {
            return Err(failures::bad_payload(format!(
                "volume must be 0-100, got {}",
                request.volume
            )));
        }
        if !(-10..=10).contains(&request.rate) {
            return Err(failures::bad_payload(format!(
                "rate must be -10..10, got {}",
                request.rate
            )));
        }
        let target = target_of(&request.target)?;
        let Some(engine) = &self.engine else {
            return Err(failures::unavailable(
                target,
                "Windows speech (SAPI) is not available on this agent",
            ));
        };
        // Task.Run(..., ct): a cancelled call says nothing.
        cx.check()?;
        let voice = trimmed_endpoint(target)?;
        engine.speak(voice, text, request.volume, request.rate)?;
        Ok(json!({"spoken": true, "characters": length}))
    }
}

/// SAPI 5 SpVoice through late-bound COM (dl-winos `IDispatch`), on one dedicated
/// single-threaded-apartment thread.
pub struct SapiSpeechEngine {
    #[cfg(windows)]
    thread: super::sta::StaThread,
}

impl SapiSpeechEngine {
    /// The SAPI ProgID.
    pub const PROG_ID: &'static str = "SAPI.SpVoice";

    /// `None` when the SAPI runtime is not registered on this host (C#
    /// `Type.GetTypeFromProgID` is null), and always off Windows.
    pub fn try_create() -> Option<SapiSpeechEngine> {
        #[cfg(windows)]
        {
            use dl_winos::registry::{Hive, Key, View};
            let registered = Key::open(
                Hive::ClassesRoot,
                &format!(r"{}\CLSID", Self::PROG_ID),
                View::Default,
            )
            .is_ok();
            registered.then(|| SapiSpeechEngine {
                thread: super::sta::StaThread::new("sapi-speaker"),
            })
        }
        #[cfg(not(windows))]
        {
            None
        }
    }
}

impl SpeechEngine for SapiSpeechEngine {
    #[cfg(windows)]
    fn speak(
        &self,
        voice_hint: &str,
        text: &str,
        volume: i32,
        rate: i32,
    ) -> Result<(), AdapterError> {
        let (hint, text) = (voice_hint.to_owned(), text.to_owned());
        self.thread
            .run(move || sapi::speak(&hint, &text, volume, rate))
            .unwrap_or_else(|| {
                Err(crate::adapters::adapter_failed(
                    "InvalidOperationException",
                    "the SAPI speech thread stopped",
                ))
            })
    }

    #[cfg(not(windows))]
    fn speak(&self, _: &str, _: &str, _: i32, _: i32) -> Result<(), AdapterError> {
        Err(crate::adapters::adapter_failed(
            "PlatformNotSupportedException",
            "SAPI runs only on Windows",
        ))
    }
}

#[cfg(windows)]
mod sapi {
    use dl_winos::com::{Object, Variant};

    use super::super::contains_ignore_case;
    use crate::adapters::adapter_failed;
    use crate::contracts::{AdapterError, codes};

    /// `SapiSpeechEngine.Speak`: create the voice, pick it, set volume and rate, speak
    /// synchronously (SVSFDefault).
    pub(super) fn speak(
        hint: &str,
        text: &str,
        volume: i32,
        rate: i32,
    ) -> Result<(), AdapterError> {
        // Activator.CreateInstance is outside the C# try: its failure is the catch-all.
        let voice = Object::create(super::SapiSpeechEngine::PROG_ID)
            .map_err(|e| adapter_failed("COMException", &e.to_string()))?;
        if !hint.is_empty() && !hint.eq_ignore_ascii_case("default") {
            pick_voice(&voice, hint)?;
        }
        voice.set("Volume", volume).map_err(refused)?;
        voice.set("Rate", rate).map_err(refused)?;
        voice
            .call("Speak", &[Variant::from(text), Variant::I32(0)])
            .map_err(refused)?;
        Ok(())
    }

    /// The first installed voice whose description contains the hint (e.g. "An" for
    /// Microsoft An, Vietnamese).
    fn pick_voice(voice: &Object, hint: &str) -> Result<(), AdapterError> {
        let voices: Object = voice
            .call("GetVoices", &[])
            .and_then(Object::try_from)
            .map_err(refused)?;
        let count: i32 = voices
            .get("Count")
            .and_then(i32::try_from)
            .map_err(refused)?;
        for i in 0..count {
            let token: Object = voices
                .call("Item", &[Variant::I32(i)])
                .and_then(Object::try_from)
                .map_err(refused)?;
            let name: String = token
                .call("GetDescription", &[])
                .and_then(String::try_from)
                .map_err(refused)?;
            if contains_ignore_case(&name, hint) {
                return voice.set_ref("Voice", &token).map_err(refused);
            }
        }
        Err(AdapterError::new(
            codes::UNAVAILABLE,
            format!("no installed SAPI voice matches '{hint}'"),
        ))
    }

    /// A COMException from the voice: DEVICE_ERROR `"SAPI refused: 0x{HResult:X8}"`; any
    /// other failure is the catch-all (C# would throw InvalidCastException).
    fn refused(e: dl_winos::Error) -> AdapterError {
        match e.hresult() {
            Some(h) => AdapterError::new(codes::ERROR, format!("SAPI refused: 0x{h:08X}")),
            None => adapter_failed("InvalidCastException", &e.to_string()),
        }
    }
}
