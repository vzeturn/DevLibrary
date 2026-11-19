//! Windows SAPI 5 through late-bound COM (`dl-winos`): the spoken phrase
//! (`SAPI.SpVoice.Speak`) and the tone (`SAPI.SpFileStream` + `SpeakStream`, which
//! plays a WAV file). The Go agent spoke through PowerShell `System.Speech` (a SAPI
//! wrapper) and played tones with `winmm`; this keeps both in-process with no `unsafe`
//! code in this crate.
//!
//! COM objects are not `Send`, so every call runs on one dedicated single-threaded
//! apartment thread that creates, uses and drops its objects.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, PoisonError};

use dl_winos::com::{Object, Variant};

/// `SpeechVoiceSpeakFlags.SVSFDefault`: synchronous.
const SVSF_DEFAULT: i32 = 0;
/// `SpeechStreamFileMode.SSFMOpenForRead`.
const SSFM_OPEN_FOR_READ: i32 = 0;

type Job = Box<dyn FnOnce() + Send>;

/// The SAPI speaker on its STA thread (started on first use).
#[derive(Debug, Default)]
pub struct Sapi {
    jobs: Mutex<Option<Sender<Job>>>,
}

impl Sapi {
    /// A speaker; the thread starts with the first call.
    pub fn new() -> Sapi {
        Sapi::default()
    }

    /// Says `text` with the first installed voice of `lang` (`vi`: LCID 042A; `en`:
    /// 0409, then 0809), or the default voice when none is installed (as the Go agent).
    ///
    /// # Errors
    /// A text naming the SAPI failure.
    pub fn speak(&self, text: &str, lang: &str) -> Result<(), String> {
        let (text, lang) = (text.to_owned(), lang.to_owned());
        self.run(move || {
            let voice = Object::create("SAPI.SpVoice").map_err(|e| format!("SAPI: {e}"))?;
            pick_voice(&voice, &lang);
            voice
                .call(
                    "Speak",
                    &[Variant::from(text.as_str()), Variant::I32(SVSF_DEFAULT)],
                )
                .map_err(|e| format!("SAPI Speak: {e}"))?;
            Ok(())
        })
    }

    /// Plays the WAV file at `path` and returns when it has played.
    ///
    /// # Errors
    /// A text naming the SAPI failure.
    pub fn play_file(&self, path: &Path) -> Result<(), String> {
        let path = path.display().to_string();
        self.run(move || {
            let voice = Object::create("SAPI.SpVoice").map_err(|e| format!("SAPI: {e}"))?;
            let stream =
                Object::create("SAPI.SpFileStream").map_err(|e| format!("SAPI stream: {e}"))?;
            stream
                .call(
                    "Open",
                    &[
                        Variant::from(path.as_str()),
                        Variant::I32(SSFM_OPEN_FOR_READ),
                        Variant::Bool(false),
                    ],
                )
                .map_err(|e| format!("SAPI open {path}: {e}"))?;
            let played = voice
                .call(
                    "SpeakStream",
                    &[Variant::Object(stream.clone()), Variant::I32(SVSF_DEFAULT)],
                )
                .map_err(|e| format!("SAPI SpeakStream: {e}"));
            if let Err(e) = stream.call("Close", &[]) {
                dl_log::warn!("speaker: SAPI stream close failed"; error = %e);
            }
            played.map(drop)
        })
    }

    /// Runs `job` on the STA thread and waits for its result.
    fn run(&self, job: impl FnOnce() -> Result<(), String> + Send + 'static) -> Result<(), String> {
        let (done, result) = mpsc::channel();
        let job: Job = Box::new(move || {
            // The caller may have gone (it never does: it waits); nothing to report then.
            let _ = done.send(job());
        });
        {
            let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
            if jobs.is_none() {
                *jobs = Some(spawn().map_err(|e| format!("start the SAPI thread: {e}"))?);
            }
            let sender = jobs.as_ref().ok_or("the SAPI thread is not running")?;
            if sender.send(job).is_err() {
                // The thread ended; the next call starts a new one.
                *jobs = None;
                return Err("the SAPI thread stopped".into());
            }
        }
        result
            .recv()
            .unwrap_or_else(|_| Err("the SAPI call panicked".into()))
    }
}

fn spawn() -> std::io::Result<Sender<Job>> {
    let (tx, rx) = mpsc::channel::<Job>();
    std::thread::Builder::new()
        .name("speaker-sapi".into())
        .spawn(move || {
            // Kept for the thread's life; when it fails, Object::create reports why.
            let apartment = dl_winos::com::init();
            if let Err(e) = &apartment {
                dl_log::error!("speaker: COM init failed"; error = %e);
            }
            for job in rx {
                // A panicking job drops its result sender: the caller gets an error.
                if catch_unwind(AssertUnwindSafe(job)).is_err() {
                    dl_log::error!("speaker: a SAPI call panicked");
                }
            }
        })?;
    Ok(tx)
}

/// Selects the first installed voice for `lang`; keeps the default voice otherwise.
fn pick_voice(voice: &Object, lang: &str) {
    let lcids: &[&str] = if lang == crate::phrase::LANG_EN {
        &["409", "809"]
    } else {
        &["42A"]
    };
    for lcid in lcids {
        let found = (|| -> Result<Option<Object>, dl_winos::Error> {
            let voices: Object = voice
                .call(
                    "GetVoices",
                    &[
                        Variant::from(format!("Language={lcid}").as_str()),
                        Variant::from(""),
                    ],
                )
                .and_then(Object::try_from)?;
            let count: i32 = voices.get("Count").and_then(i32::try_from)?;
            if count == 0 {
                return Ok(None);
            }
            voices
                .call("Item", &[Variant::I32(0)])
                .and_then(Object::try_from)
                .map(Some)
        })();
        match found {
            Ok(Some(token)) => match voice.set_ref("Voice", &token) {
                Ok(()) => return,
                Err(e) => {
                    dl_log::warn!("speaker: SAPI voice not selectable"; lang = lang, error = %e)
                }
            },
            Ok(None) => {}
            Err(e) => dl_log::warn!("speaker: SAPI voice lookup failed"; lang = lang, error = %e),
        }
    }
    dl_log::warn!("speaker: no SAPI voice installed for the language; the default voice speaks"; lang = lang);
}

#[cfg(test)]
mod tests {
    use crate::output::{Output, SystemOutput};

    /// Run by hand on a Windows machine with speakers:
    /// `cargo test -p dl-agent-speaker -- --ignored`. The phrase holds a typographic
    /// quote: it must be read, never interpreted.
    #[test]
    #[ignore = "plays audio"]
    fn plays_a_tone_and_speaks_both_languages() {
        let dir = std::env::temp_dir().join("dl-agent-speaker-tones");
        let out = SystemOutput::new(dir);
        out.play("chime", crate::tones::tone("chime").unwrap())
            .unwrap();
        out.speak("Bàn 5’; exit 3; ’ đặt món", "vi").unwrap();
        out.speak("Table 7 requests the bill", "en").unwrap();
    }
}
