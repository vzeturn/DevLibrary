//! The hub's `modules.speaker` section:
//!
//! ```text
//! {"modules": {"speaker": {"enabled": true, "rvcList": [1, 2],
//!                          "soundRefreshSeconds": 120}}}
//! ```
//!
//! `rvcList` (required, the outlets this machine announces) keeps the Go agent's
//! `config.json` name. The cloud URL and the credential come from the `pda-sync`
//! enrolment, so there is no `apiUrl` or `token` here.

use std::time::Duration;

use dl_json::Value;

/// The smallest accepted refresh interval.
pub const MIN_REFRESH: Duration = Duration::from_secs(30);

/// Bound module settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The RVCs to join and announce (deduplicated, in order).
    pub rvcs: Vec<i64>,
    /// How often the per-RVC sound modes are read again.
    pub sound_refresh: Duration,
}

/// Binds the section.
///
/// # Errors
/// A text naming the setting that is missing or of the wrong type.
pub fn parse(config: &Value) -> Result<Settings, String> {
    let list = match config.get("rvcList") {
        None | Some(Value::Null) => {
            return Err("modules.speaker.rvcList is required (the RVC numbers to announce)".into());
        }
        Some(Value::Array(a)) => a,
        Some(other) => {
            return Err(format!(
                "modules.speaker.rvcList must be an array of RVC numbers, not {}",
                other.type_name()
            ));
        }
    };
    let mut rvcs = Vec::new();
    for v in list {
        let rvc = v
            .as_i64()
            .filter(|r| *r > 0)
            .ok_or_else(|| format!("modules.speaker.rvcList: {v} is not a positive RVC number"))?;
        if !rvcs.contains(&rvc) {
            rvcs.push(rvc);
        }
    }
    if rvcs.is_empty() {
        return Err("modules.speaker.rvcList is empty".into());
    }
    let sound_refresh = match config.get("soundRefreshSeconds") {
        None | Some(Value::Null) => crate::fetch::REFRESH,
        Some(v) => {
            let s = v.as_u64().ok_or_else(|| {
                format!("modules.speaker.soundRefreshSeconds must be a number of seconds, not {v}")
            })?;
            Duration::from_secs(s).max(MIN_REFRESH)
        }
    };
    Ok(Settings {
        rvcs,
        sound_refresh,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dl_json::json;

    #[test]
    fn binds_the_rvc_list_and_the_refresh() {
        let s = parse(&json!({"enabled": true, "rvcList": [2, 1, 2]})).unwrap();
        assert_eq!(s.rvcs, [2, 1]);
        assert_eq!(s.sound_refresh, Duration::from_secs(120));
        let s = parse(&json!({"rvcList": [1], "soundRefreshSeconds": 5})).unwrap();
        assert_eq!(s.sound_refresh, MIN_REFRESH);
        let s = parse(&json!({"rvcList": [1], "soundRefreshSeconds": 600})).unwrap();
        assert_eq!(s.sound_refresh, Duration::from_secs(600));
    }

    #[test]
    fn refuses_what_does_not_bind() {
        for (cfg, needle) in [
            (json!({"enabled": true}), "required"),
            (Value::Null, "required"),
            (json!({"rvcList": 1}), "array"),
            (json!({"rvcList": []}), "empty"),
            (json!({"rvcList": ["1"]}), "positive"),
            (json!({"rvcList": [0]}), "positive"),
            (
                json!({"rvcList": [1], "soundRefreshSeconds": "x"}),
                "seconds",
            ),
        ] {
            let e = parse(&cfg).unwrap_err();
            assert!(e.contains(needle), "{cfg}: {e}");
        }
    }
}
