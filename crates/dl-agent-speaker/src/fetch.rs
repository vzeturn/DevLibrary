//! Reads each RVC's sound mode from the PDA cloud (port of `FetchMode` and the
//! two-minute refresh in the Go `main.go`).
//!
//! `GET {cloud}/api/v1/speaker/sound-mode/{rvc}` with the bearer token answers the
//! `{result, data: [{key, value}]}` envelope with `guest.speakerSoundMode`. A failed
//! fetch keeps the last good mode of that RVC (never overwritten by a default), so a
//! network blip never changes what plays.

use std::time::Duration;

use crate::hub::TokenSource;
use crate::mode::{Cache, Mode, mode_from_envelope};

/// How often the modes are read again.
pub const REFRESH: Duration = Duration::from_secs(120);
/// Limit for one fetch (Go `httpClient.Timeout`).
pub const TIMEOUT: Duration = Duration::from_secs(15);

/// Why a fetch failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// The cloud answered with this status (401/403: the token is not accepted there).
    Status(u16),
    /// No answer, or one that does not decode.
    Other(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Status(s) => write!(f, "sound-mode fetch returned status {s}"),
            FetchError::Other(e) => f.write_str(e),
        }
    }
}

/// The speaker route of `rvc` under `api_base` (scheme and authority of the cloud).
pub fn sound_mode_url(api_base: &str, rvc: i64) -> String {
    format!(
        "{}/api/v1/speaker/sound-mode/{rvc}",
        api_base.trim_end_matches('/')
    )
}

/// `FetchMode` for one RVC.
///
/// # Errors
/// [`FetchError`]; the caller keeps the cached mode.
pub fn fetch_mode(
    http: &dl_http::Client,
    api_base: &str,
    token: &str,
    rvc: i64,
) -> Result<Mode, FetchError> {
    let url = dl_http::Url::parse(&sound_mode_url(api_base, rvc))
        .map_err(|e| FetchError::Other(format!("build sound-mode request: {e}")))?;
    let req = dl_http::Request::get(url)
        .with_header("Authorization", &format!("Bearer {token}"))
        .map_err(|e| FetchError::Other(format!("build sound-mode request: {e}")))?
        .with_timeout(TIMEOUT);
    let resp = http
        .request(&req)
        .map_err(|e| FetchError::Other(format!("fetch sound mode: {e}")))?;
    let status = resp.status.as_u16();
    if status != 200 {
        return Err(FetchError::Status(status));
    }
    let text = std::str::from_utf8(&resp.body)
        .map_err(|_| FetchError::Other("decode sound mode: invalid UTF-8".into()))?;
    mode_from_envelope(text).map_err(|e| FetchError::Other(format!("decode sound mode: {e}")))
}

/// Reads every RVC's mode into `cache`; returns the failures `(rvc, text)` (logged).
pub fn refresh(
    http: &dl_http::Client,
    api_base: &str,
    tokens: &dyn TokenSource,
    rvcs: &[i64],
    cache: &Cache,
) -> Vec<(i64, String)> {
    let token = match tokens.token() {
        Ok(t) => t,
        Err(e) => {
            dl_log::warn!("speaker: sound-mode refresh has no token"; error = %e);
            return rvcs
                .iter()
                .map(|r| (*r, format!("no token: {e}")))
                .collect();
        }
    };
    let mut failed = Vec::new();
    for &rvc in rvcs {
        match fetch_mode(http, api_base, &token, rvc) {
            Ok(mode) => cache.set(rvc, mode),
            Err(e) => {
                if e == FetchError::Status(401) {
                    tokens.invalidate();
                }
                dl_log::warn!("speaker: sound-mode fetch failed, keeping the last mode"; rvc = rvc, error = %e);
                failed.push((rvc, e.to_string()));
            }
        }
    }
    failed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;

    struct Fixed(Mutex<u32>);

    impl TokenSource for Fixed {
        fn token(&self) -> Result<String, String> {
            Ok("tok".into())
        }
        fn invalidate(&self) {
            *self.0.lock().unwrap() += 1;
        }
    }

    /// Answers each request with the next `(status, body)` and records the heads.
    fn server(answers: Vec<(u16, &'static str)>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let mut heads = Vec::new();
            for (status, body) in answers {
                let (mut s, _) = l.accept().unwrap();
                let mut head = Vec::new();
                let mut b = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    s.read_exact(&mut b).unwrap();
                    head.push(b[0]);
                }
                heads.push(String::from_utf8_lossy(&head).into_owned());
                write!(
                    s,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
            heads
        });
        (base, h)
    }

    #[test]
    fn refresh_fills_the_cache_and_keeps_the_last_mode_on_failure() {
        let ok = r#"{"result":0,"data":[{"key":"guest.speakerSoundMode","value":"{\"tone\":\"gong\",\"voice\":false}"}]}"#;
        let (base, h) = server(vec![(200, ok), (401, "no"), (200, "{bad")]);
        let cache = Cache::new();
        cache.set(2, Mode::new("bell", true));
        cache.set(3, Mode::new("harp", true));
        let tokens = Fixed(Mutex::new(0));
        let http = dl_http::Client::new();
        let failed = refresh(&http, &base, &tokens, &[1, 2, 3], &cache);
        assert_eq!(cache.get(1), Mode::new("gong", false));
        assert_eq!(cache.get(2), Mode::new("bell", true));
        assert_eq!(cache.get(3), Mode::new("harp", true));
        assert_eq!(failed.iter().map(|f| f.0).collect::<Vec<_>>(), [2, 3]);
        assert_eq!(*tokens.0.lock().unwrap(), 1);
        let heads = h.join().unwrap();
        assert!(
            heads[0].starts_with("GET /api/v1/speaker/sound-mode/1 "),
            "{}",
            heads[0]
        );
        assert!(
            heads[0]
                .to_ascii_lowercase()
                .contains("authorization: bearer tok")
        );
    }

    #[test]
    fn route_url() {
        assert_eq!(
            sound_mode_url("https://pda.example.com/", 7),
            "https://pda.example.com/api/v1/speaker/sound-mode/7"
        );
    }
}
