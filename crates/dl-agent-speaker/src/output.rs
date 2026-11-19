//! Playing the decisions: a queue and one worker thread, so announcements play one after
//! the other in arrival order and never block the hub connection's reader (the Go agent
//! played inside its read loop).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};

use crate::decision::{Announcement, GuestEvent, decide};
use crate::mode::Cache;
use crate::tones;

/// Announcements waiting beyond this are dropped (and counted): a burst of guest calls
/// must not leave the speaker talking for minutes.
pub const QUEUE_LIMIT: usize = 32;

/// The speaker hardware: a tone and a spoken phrase. Faked in tests.
pub trait Output: Send + Sync {
    /// Plays the tone `name` (`wav` is its embedded WAV image) and returns when done.
    ///
    /// # Errors
    /// A text to log; the announcement goes on with the speech.
    fn play(&self, name: &str, wav: &'static [u8]) -> Result<(), String>;

    /// Says `text` in `lang` (`vi` or `en`) and returns when done.
    ///
    /// # Errors
    /// A text to log.
    fn speak(&self, text: &str, lang: &str) -> Result<(), String>;
}

/// Counters for the module metrics.
#[derive(Debug, Default)]
pub struct Stats {
    /// Guest events received.
    pub events: AtomicU64,
    /// Tones played.
    pub played: AtomicU64,
    /// Phrases spoken.
    pub spoken: AtomicU64,
    /// Tone or speech failures (logged).
    pub failures: AtomicU64,
    /// Announcements dropped because the queue was full.
    pub dropped: AtomicU64,
}

impl Stats {
    /// `(name, value)` pairs for `ctx.set_metric`.
    pub fn metrics(&self) -> Vec<(&'static str, u64)> {
        vec![
            ("events", self.events.load(Ordering::Relaxed)),
            ("tones_played", self.played.load(Ordering::Relaxed)),
            ("phrases_spoken", self.spoken.load(Ordering::Relaxed)),
            ("output_failures", self.failures.load(Ordering::Relaxed)),
            ("dropped", self.dropped.load(Ordering::Relaxed)),
        ]
    }
}

/// Decides with the per-RVC [`Cache`] and plays through an [`Output`] on its own thread.
pub struct Announcer {
    cache: Arc<Cache>,
    queue: Mutex<Option<SyncSender<Announcement>>>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    stats: Arc<Stats>,
}

impl std::fmt::Debug for Announcer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Announcer")
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl Announcer {
    /// Starts the worker over `output`.
    ///
    /// # Errors
    /// When the thread cannot start.
    pub fn start(cache: Arc<Cache>, output: Arc<dyn Output>) -> std::io::Result<Arc<Announcer>> {
        let (tx, rx) = mpsc::sync_channel::<Announcement>(QUEUE_LIMIT);
        let stats = Arc::new(Stats::default());
        let worker_stats = stats.clone();
        let worker = std::thread::Builder::new()
            .name("speaker-output".into())
            .spawn(move || {
                for a in rx {
                    perform(&*output, &a, &worker_stats);
                }
            })?;
        Ok(Arc::new(Announcer {
            cache,
            queue: Mutex::new(Some(tx)),
            worker: Mutex::new(Some(worker)),
            stats,
        }))
    }

    /// The counters.
    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// Decides what plays for `event` with its RVC's mode and queues it.
    pub fn announce(&self, event: &GuestEvent) {
        self.stats.events.fetch_add(1, Ordering::Relaxed);
        let a = decide(event, &self.cache.get(event.rvc));
        dl_log::info!("speaker: guest event"; kind = event.kind, rvc = event.rvc, tone = a.tone.as_deref().unwrap_or("off"), voice = a.speech.is_some());
        self.enqueue(a);
    }

    /// Queues a decided announcement (also the `test` command).
    pub fn enqueue(&self, a: Announcement) {
        let queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(tx) = queue.as_ref() else {
            dl_log::warn!("speaker: announcement after stop dropped"; rvc = a.rvc);
            return;
        };
        match tx.try_send(a) {
            Ok(()) => {}
            Err(TrySendError::Full(a)) => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                dl_log::warn!("speaker: queue full, announcement dropped"; rvc = a.rvc, limit = QUEUE_LIMIT);
            }
            Err(TrySendError::Disconnected(a)) => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                dl_log::error!("speaker: output thread ended, announcement dropped"; rvc = a.rvc);
            }
        }
    }

    /// Stops taking announcements and waits until the queued ones have played.
    pub fn stop(&self) {
        drop(
            self.queue
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
        let worker = self
            .worker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(w) = worker
            && w.join().is_err()
        {
            dl_log::error!("speaker: output thread panicked");
        }
    }
}

/// Plays one announcement: the tone first, then the phrase (Go's order). A failure of
/// one is logged and does not stop the other.
fn perform(output: &dyn Output, a: &Announcement, stats: &Stats) {
    if let Some(name) = &a.tone {
        let played = match tones::tone(name) {
            Some(wav) => output.play(name, wav),
            None => Err(format!("unknown tone {name:?}")),
        };
        match played {
            Ok(()) => {
                stats.played.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                stats.failures.fetch_add(1, Ordering::Relaxed);
                dl_log::warn!("speaker: play tone failed"; rvc = a.rvc, tone = name.as_str(), error = %e);
            }
        }
    }
    if let Some(s) = &a.speech {
        match output.speak(&s.text, s.lang) {
            Ok(()) => {
                stats.spoken.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                stats.failures.fetch_add(1, Ordering::Relaxed);
                dl_log::warn!("speaker: speak failed"; rvc = a.rvc, lang = s.lang, error = %e);
            }
        }
    }
}

/// Writes the tone `name` to `<dir>/<name>.wav` unless an identical file is there, and
/// returns its path (SAPI plays WAV files, not memory images).
///
/// # Errors
/// The folder or file cannot be written.
pub fn tone_file(dir: &Path, name: &str, wav: &[u8]) -> std::io::Result<PathBuf> {
    let path = dir.join(format!("{name}.wav"));
    let same = std::fs::read(&path).map(|d| d == wav).unwrap_or(false);
    if !same {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{name}.wav.tmp"));
        std::fs::write(&tmp, wav)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(path)
}

/// The machine's speaker: Windows SAPI (`SAPI.SpVoice`) for the tone (a WAV file stream)
/// and the phrase; other systems only log what would play.
#[derive(Debug)]
pub struct SystemOutput {
    #[cfg_attr(not(windows), allow(dead_code))]
    tones_dir: PathBuf,
    #[cfg(windows)]
    sapi: crate::sapi::Sapi,
}

impl SystemOutput {
    /// An output that keeps the tone files in `tones_dir`.
    pub fn new(tones_dir: PathBuf) -> SystemOutput {
        SystemOutput {
            tones_dir,
            #[cfg(windows)]
            sapi: crate::sapi::Sapi::new(),
        }
    }
}

impl Output for SystemOutput {
    #[cfg(windows)]
    fn play(&self, name: &str, wav: &'static [u8]) -> Result<(), String> {
        let path =
            tone_file(&self.tones_dir, name, wav).map_err(|e| format!("write tone {name}: {e}"))?;
        self.sapi.play_file(&path)
    }

    #[cfg(not(windows))]
    fn play(&self, name: &str, wav: &'static [u8]) -> Result<(), String> {
        dl_log::info!("speaker: tone playback is Windows-only; not played"; tone = name, bytes = wav.len());
        Ok(())
    }

    #[cfg(windows)]
    fn speak(&self, text: &str, lang: &str) -> Result<(), String> {
        self.sapi.speak(text, lang)
    }

    #[cfg(not(windows))]
    fn speak(&self, text: &str, lang: &str) -> Result<(), String> {
        dl_log::info!("speaker: speech is Windows-only; not spoken"; lang = lang, chars = text.chars().count());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::Mode;

    #[derive(Default)]
    struct Fake(Mutex<Vec<String>>);

    impl Output for Fake {
        fn play(&self, name: &str, _: &'static [u8]) -> Result<(), String> {
            self.0.lock().unwrap().push(format!("play {name}"));
            Ok(())
        }
        fn speak(&self, text: &str, lang: &str) -> Result<(), String> {
            self.0.lock().unwrap().push(format!("speak {lang} {text}"));
            Err("no voice".into())
        }
    }

    #[test]
    fn plays_in_order_and_counts_failures() {
        let cache = Arc::new(Cache::new());
        cache.set(2, Mode::new("no-such-tone", true).with_lang("en"));
        let fake = Arc::new(Fake::default());
        let a = Announcer::start(cache, fake.clone()).unwrap();
        let ev = |rvc| GuestEvent {
            kind: "order",
            rvc,
            table: "4".into(),
        };
        a.announce(&ev(1));
        a.announce(&ev(2));
        a.stop();
        assert_eq!(
            *fake.0.lock().unwrap(),
            [
                "play guest",
                "speak vi Bàn 4 đặt món",
                "speak en Table 4 placed an order"
            ]
        );
        let m: std::collections::HashMap<_, _> = a.stats().metrics().into_iter().collect();
        assert_eq!(
            (m["events"], m["tones_played"], m["output_failures"]),
            (2, 1, 3)
        );
        // After stop nothing is queued.
        a.announce(&ev(1));
        assert_eq!(fake.0.lock().unwrap().len(), 3);
    }

    #[test]
    fn tone_files_are_written_once() {
        let dir = std::env::temp_dir().join(format!("speaker-tones-{}", std::process::id()));
        let wav = tones::tone("chime").unwrap();
        let p = tone_file(&dir, "chime", wav).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), wav);
        let t = std::fs::metadata(&p).unwrap().modified().unwrap();
        assert_eq!(tone_file(&dir, "chime", wav).unwrap(), p);
        assert_eq!(std::fs::metadata(&p).unwrap().modified().unwrap(), t);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
