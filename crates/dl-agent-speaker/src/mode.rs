//! The per-RVC `guest.speakerSoundMode` setting: which tone to play and whether to speak
//! (port of the Go `internal/soundconfig/soundconfig.go` and the Rust port's
//! `soundconfig.rs`).
//!
//! The web Settings page (`AlertSoundCard`) writes the value as JSON, for example
//! `{"tone":"bell","voice":true,"lang":"en"}`. Three tone states are distinct:
//! a tone name plays that tone, an explicit `"tone": null` (or a blank name) is "tone off"
//! (the voice may still speak), and a missing `tone` key means the default tone.

use std::collections::HashMap;
use std::sync::{PoisonError, RwLock};

use dl_json::Value;

/// `DEFAULT_MODE.tone` in `web/src/components/StaffAlerts.tsx`.
pub const DEFAULT_TONE: &str = "guest";

/// The config key the speaker route returns.
pub const SOUND_MODE_KEY: &str = "guest.speakerSoundMode";

/// The decoded setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mode {
    /// The tone to play; `None` is "off" (an explicit JSON `null`).
    pub tone: Option<String>,
    /// Whether to announce by voice.
    pub voice: bool,
    /// Announcement language as configured (`vi`, `en`, `auto`, ...); see
    /// [`crate::phrase::normalize_lang`].
    pub lang: String,
}

impl Default for Mode {
    /// `DEFAULT_MODE`: the guest tone and voice on.
    fn default() -> Mode {
        Mode {
            tone: Some(DEFAULT_TONE.to_owned()),
            voice: true,
            lang: String::new(),
        }
    }
}

impl Mode {
    /// A mode with a tone and no language (helper for callers and tests).
    pub fn new(tone: &str, voice: bool) -> Mode {
        Mode {
            tone: Some(tone.to_owned()),
            voice,
            lang: String::new(),
        }
    }

    /// The same mode with `lang`.
    pub fn with_lang(mut self, lang: &str) -> Mode {
        self.lang = lang.to_owned();
        self
    }

    /// The tone, or `""` when it is off.
    pub fn tone_or_empty(&self) -> &str {
        self.tone.as_deref().unwrap_or("")
    }

    /// Whether no tone should play (off or blank); the voice may still speak.
    pub fn tone_off(&self) -> bool {
        self.tone_or_empty().is_empty()
    }
}

/// `ParseMode`: never fails. A malformed or absent value gives [`Mode::default`], so one
/// bad config write never stops the alerts. An explicit `"tone": null` is kept as "off";
/// a missing `tone` key means the default tone. Keys match case-insensitively as Go's
/// struct decoding does, but only the exact key `tone` counts as "present".
pub fn parse_mode(value: &str) -> Mode {
    if value.is_empty() {
        return Mode::default();
    }
    let Ok(v) = dl_json::parse(value) else {
        return Mode::default();
    };
    let obj = match v {
        Value::Object(o) => o,
        // Go decodes `null` into a nil map and a zero Mode; the tone key is then absent.
        Value::Null => {
            return Mode {
                tone: Some(DEFAULT_TONE.to_owned()),
                voice: false,
                lang: String::new(),
            };
        }
        _ => return Mode::default(),
    };
    let mut mode = Mode {
        tone: None,
        voice: false,
        lang: String::new(),
    };
    for (k, val) in obj.iter() {
        if k.eq_ignore_ascii_case("tone") {
            match val {
                Value::Null => mode.tone = None,
                Value::String(s) => mode.tone = Some(s.clone()),
                _ => return Mode::default(),
            }
        } else if k.eq_ignore_ascii_case("voice") {
            match val {
                Value::Null => {}
                Value::Bool(b) => mode.voice = *b,
                _ => return Mode::default(),
            }
        } else if k.eq_ignore_ascii_case("lang") {
            match val {
                Value::Null => {}
                Value::String(s) => mode.lang = s.clone(),
                _ => return Mode::default(),
            }
        }
    }
    if !obj.contains_key("tone") {
        mode.tone = Some(DEFAULT_TONE.to_owned());
    }
    mode
}

/// Decodes the speaker route's `{result, data: [{key, value}]}` envelope and returns the
/// mode it carries ([`Mode::default`] when the key is not in it).
///
/// # Errors
/// A text naming the part that does not decode (the caller then keeps its last mode).
pub fn mode_from_envelope(text: &str) -> Result<Mode, String> {
    let v = dl_json::parse(text).map_err(|e| e.to_string())?;
    let obj = match &v {
        Value::Object(o) => o,
        Value::Null => return Ok(Mode::default()),
        other => {
            return Err(format!(
                "cannot unmarshal {} into envelope",
                other.type_name()
            ));
        }
    };
    match obj.get("result") {
        None | Some(Value::Null) => {}
        Some(r) if r.as_i64().is_some() => {}
        Some(r) => return Err(format!("cannot unmarshal {} into result", r.type_name())),
    }
    let entries = match obj.get("data") {
        None | Some(Value::Null) => return Ok(Mode::default()),
        Some(Value::Array(a)) => a,
        Some(other) => return Err(format!("cannot unmarshal {} into data", other.type_name())),
    };
    let mut found = None;
    for e in entries {
        let (key, value) = match e {
            Value::Null => (String::new(), String::new()),
            Value::Object(o) => (opt_string(o, "key")?, opt_string(o, "value")?),
            other => {
                return Err(format!(
                    "cannot unmarshal {} into ConfigDto",
                    other.type_name()
                ));
            }
        };
        // Go decodes every entry (a type error anywhere fails the fetch), then takes the
        // first matching key.
        if found.is_none() && key == SOUND_MODE_KEY {
            found = Some(value);
        }
    }
    Ok(found.map(|v| parse_mode(&v)).unwrap_or_default())
}

fn opt_string(o: &dl_json::Map, key: &str) -> Result<String, String> {
    match o.get(key) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(other) => Err(format!(
            "cannot unmarshal {} into ConfigDto.{key}",
            other.type_name()
        )),
    }
}

/// The last fetched mode per RVC; safe for the refresh thread (writer) and the
/// announcement path (reader).
#[derive(Debug, Default)]
pub struct Cache {
    by_rvc: RwLock<HashMap<i64, Mode>>,
}

impl Cache {
    /// An empty cache.
    pub fn new() -> Cache {
        Cache::default()
    }

    /// The cached mode, or the default when none was fetched yet.
    pub fn get(&self, rvc: i64) -> Mode {
        self.by_rvc
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&rvc)
            .cloned()
            .unwrap_or_default()
    }

    /// Stores a fetched mode.
    pub fn set(&self, rvc: i64, mode: Mode) {
        self.by_rvc
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(rvc, mode);
    }

    /// How many RVCs have a fetched mode.
    pub fn len(&self) -> usize {
        self.by_rvc
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Whether no mode was fetched yet.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mode_cases_like_go() {
        assert_eq!(
            parse_mode(r#"{"tone":"bell","voice":false}"#),
            Mode::new("bell", false)
        );
        assert_eq!(parse_mode(""), Mode::default());
        assert_eq!(parse_mode("{not json"), Mode::default());
        let off = parse_mode(r#"{"tone":null,"voice":true}"#);
        assert_eq!(off.tone, None);
        assert!(off.tone_off() && off.voice);
        assert_ne!(off, Mode::default());
        assert_eq!(parse_mode(r#"{"voice":true}"#), Mode::default());
        assert_eq!(
            parse_mode(r#"{"tone":"bell","voice":true,"lang":"en"}"#).lang,
            "en"
        );
        assert_eq!(
            parse_mode(r#"{"tone":"bell","voice":"yes"}"#),
            Mode::default()
        );
        assert_eq!(parse_mode("[1]"), Mode::default());
        assert_eq!(
            parse_mode("null"),
            Mode {
                tone: Some("guest".into()),
                voice: false,
                lang: String::new()
            }
        );
        // "Tone" matches the field but not the key-presence check.
        assert_eq!(parse_mode(r#"{"Tone":null,"voice":true}"#), Mode::default());
        assert!(parse_mode(r#"{"tone":"","voice":false}"#).tone_off());
    }

    #[test]
    fn envelope_cases() {
        let body = r#"{"result":0,"data":[{"key":"other.key","value":"x"},{"key":"guest.speakerSoundMode","value":"{\"tone\":\"bell\",\"voice\":false}"}]}"#;
        assert_eq!(mode_from_envelope(body).unwrap(), Mode::new("bell", false));
        let missing = r#"{"result":0,"data":[{"key":"other.key","value":"x"}]}"#;
        assert_eq!(mode_from_envelope(missing).unwrap(), Mode::default());
        assert!(mode_from_envelope("{not json").is_err());
        assert!(mode_from_envelope(r#"{"result":"0"}"#).is_err());
        assert!(mode_from_envelope(r#"{"data":[{"key":1}]}"#).is_err());
        assert!(mode_from_envelope("[]").is_err());
        assert_eq!(
            mode_from_envelope(r#"{"data":null}"#).unwrap(),
            Mode::default()
        );
    }

    #[test]
    fn cache_defaults_per_rvc() {
        let c = Cache::new();
        assert!(c.is_empty());
        assert_eq!(c.get(99), Mode::default());
        c.set(1, Mode::new("gong", false));
        assert_eq!(c.get(1), Mode::new("gong", false));
        assert_eq!(c.get(2), Mode::default());
        assert_eq!(c.len(), 1);
    }
}
