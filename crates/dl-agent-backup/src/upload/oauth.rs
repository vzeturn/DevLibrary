//! OAuth2 refresh-token access for the Google Drive and OneDrive uploaders, plus the request
//! helper they share.
//!
//! Like Go's `oauth2.TokenSource`: the access token the hub sent (`accessToken`,
//! `tokenExpiresAt`) is used until shortly before it expires, then a new one is fetched with
//! the refresh token (`clientId`, `clientSecret`, `refreshToken`) and cached for the life of
//! the uploader. A 401 forces one refresh and one retry. A refresh token the provider rotates
//! is kept in memory only, as in Go: nothing is written back to the hub, so the next job
//! starts again from the hub's token (Microsoft keeps the previous refresh token valid).
//!
//! Tokens, the client secret and upload-session URLs never appear in an error or a log line.

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dl_http::{Method, Request, Response, Url, encode_query_component};
use dl_json::Value;
use dl_tds::CancelToken;

use super::text;
use crate::Error;
use crate::schedule::cron::days_from_civil;

/// A cached access token is renewed this long before it expires.
const EXPIRY_MARGIN: Duration = Duration::from_secs(60);
/// Throttled (429 / 503) answers followed before the call fails.
const MAX_THROTTLED: u32 = 5;

/// How long the helper waits; tests shrink it.
#[derive(Debug, Clone, Copy)]
pub struct Pace {
    /// Longest `Retry-After` honoured.
    pub throttle_cap: Duration,
    /// Base of the exponential wait when there is no usable `Retry-After`.
    pub backoff: Duration,
}

impl Default for Pace {
    fn default() -> Pace {
        Pace {
            throttle_cap: Duration::from_secs(60),
            backoff: Duration::from_secs(1),
        }
    }
}

impl Pace {
    /// The wait before retry number `attempt` (0-based) of a throttled or failed call.
    pub fn wait_for(&self, resp: Option<&Response>, attempt: u32) -> Duration {
        let hinted = resp
            .and_then(|r| r.headers.get("retry-after"))
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        hinted
            .unwrap_or_else(|| self.backoff * 2u32.saturating_pow(attempt.min(6)))
            .min(self.throttle_cap)
    }

    /// Sleeps `d` in short slices so a cancel is noticed.
    pub fn sleep(&self, d: Duration, cancel: &CancelToken) -> Result<(), Error> {
        let until = Instant::now() + d;
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(());
            }
            std::thread::sleep(left.min(Duration::from_millis(100)));
        }
    }
}

struct State {
    access: Option<(String, Instant)>,
    refresh: String,
}

/// A provider account: the refresh-token flow and an HTTP client.
pub struct OAuth {
    label: &'static str,
    http: dl_http::Client,
    token_url: String,
    client_id: String,
    client_secret: String,
    state: Mutex<State>,
    pace: Pace,
}

impl std::fmt::Debug for OAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuth")
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

/// Seconds since the epoch of an RFC 3339 time (`2026-11-19T10:00:00Z`, fractions and
/// numeric offsets allowed).
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, rest) = s.split_once(['T', 't', ' '])?;
    let mut d = date.split('-');
    let (y, m, day) = (
        d.next()?.parse::<i64>().ok()?,
        d.next()?.parse::<u32>().ok()?,
        d.next()?.parse::<u32>().ok()?,
    );
    if d.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&day) {
        return None;
    }
    let split = rest.find(['Z', 'z', '+', '-']).unwrap_or(rest.len());
    let (clock, zone) = rest.split_at(split);
    let mut c = clock.split(':');
    let h = c.next()?.parse::<i64>().ok()?;
    let mi = c.next()?.parse::<i64>().ok()?;
    let sec = c.next()?.split('.').next()?.parse::<i64>().ok()?;
    let offset = match zone {
        "" | "Z" | "z" => 0,
        z => {
            let sign = if z.starts_with('-') { -1 } else { 1 };
            let (zh, zm) = z[1..].split_once(':')?;
            sign * (zh.parse::<i64>().ok()? * 3600 + zm.parse::<i64>().ok()? * 60)
        }
    };
    Some(days_from_civil(y, m, day) * 86_400 + h * 3600 + mi * 60 + sec - offset)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl OAuth {
    /// Reads `clientId`, `clientSecret`, `refreshToken` and the optional `accessToken` /
    /// `tokenExpiresAt` from the destination's tokens.
    pub fn from_tokens(
        label: &'static str,
        tokens: &Value,
        token_url: &str,
        pace: Pace,
    ) -> Result<OAuth, Error> {
        let (client_id, client_secret, refresh) = (
            text(tokens, "clientId"),
            text(tokens, "clientSecret"),
            text(tokens, "refreshToken"),
        );
        if client_id.is_empty() || client_secret.is_empty() || refresh.is_empty() {
            return Err(Error::Upload(format!(
                "{label} credentials incomplete (need clientId, clientSecret, refreshToken)"
            )));
        }
        // A token without a readable expiry is not trusted: refresh at once.
        let access = Some(text(tokens, "accessToken"))
            .filter(|t| !t.is_empty())
            .zip(parse_rfc3339(&text(tokens, "tokenExpiresAt")))
            .and_then(|(t, exp)| {
                let left = u64::try_from(exp - now_secs()).ok()?;
                let left = Duration::from_secs(left).checked_sub(EXPIRY_MARGIN)?;
                Some((t, Instant::now() + left))
            });
        Ok(OAuth {
            label,
            http: super::http_client(),
            token_url: token_url.to_owned(),
            client_id,
            client_secret,
            state: Mutex::new(State { access, refresh }),
            pace,
        })
    }

    /// The underlying HTTP client.
    pub fn http(&self) -> &dl_http::Client {
        &self.http
    }

    /// The pacing rules.
    pub fn pace(&self) -> Pace {
        self.pace
    }

    /// A valid access token, refreshing when the cached one is (nearly) expired.
    pub fn token(&self, cancel: &CancelToken) -> Result<String, Error> {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((t, until)) = &st.access
            && Instant::now() < *until
        {
            return Ok(t.clone());
        }
        self.refresh(&mut st, cancel)
    }

    /// Refreshes after a 401 for `rejected`, unless another thread already did.
    fn renew(&self, rejected: &str, cancel: &CancelToken) -> Result<String, Error> {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((t, _)) = &st.access
            && t != rejected
        {
            return Ok(t.clone());
        }
        self.refresh(&mut st, cancel)
    }

    fn refresh(&self, st: &mut State, cancel: &CancelToken) -> Result<String, Error> {
        let form = format!(
            "grant_type=refresh_token&client_id={}&client_secret={}&refresh_token={}",
            encode_query_component(&self.client_id),
            encode_query_component(&self.client_secret),
            encode_query_component(&st.refresh),
        );
        let label = self.label;
        let mut req = Request::new(Method::Post, parse_url(&self.token_url)?).with_body(form);
        for (k, v) in [
            ("content-type", "application/x-www-form-urlencoded"),
            ("accept", "application/json"),
        ] {
            req.headers
                .insert(k, v)
                .map_err(|e| Error::Upload(format!("{label} token request: {e}")))?;
        }
        let resp = self.send_with(req, None, cancel, &format!("{label} token refresh"))?;
        let body = dl_json::from_str::<Value>(&String::from_utf8_lossy(&resp.body)).ok();
        let field = |k: &str| {
            body.as_ref()
                .and_then(|b| b.get(k))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        if !resp.is_success() {
            let code = field("error").unwrap_or_default();
            let msg = format!(
                "{label} token refresh: HTTP {} {code}",
                resp.status.as_u16()
            );
            return Err(match code.as_str() {
                "invalid_grant" | "invalid_client" | "unauthorized_client" => {
                    Error::Credential(format!("{} (re-authorize the destination)", msg.trim()))
                }
                _ => Error::Upload(msg.trim().to_owned()),
            });
        }
        let access = field("access_token")
            .filter(|t| !t.is_empty())
            .ok_or_else(|| {
                Error::Upload(format!(
                    "{label} token refresh: no access_token in the answer"
                ))
            })?;
        let lifetime = body
            .as_ref()
            .and_then(|b| b.get("expires_in"))
            .and_then(Value::as_u64)
            .unwrap_or(3600);
        let valid = Duration::from_secs(lifetime).saturating_sub(EXPIRY_MARGIN);
        if let Some(rotated) = field("refresh_token").filter(|t| !t.is_empty()) {
            st.refresh = rotated;
        }
        st.access = Some((access.clone(), Instant::now() + valid));
        Ok(access)
    }

    /// Sends `req` with a bearer token: one refresh and retry on 401, bounded waits on
    /// 429 / 503. Any other status is returned for the caller to judge.
    pub fn send(&self, req: Request, cancel: &CancelToken, what: &str) -> Result<Response, Error> {
        let mut token = self.token(cancel)?;
        let mut renewed = false;
        loop {
            let resp = self.send_with(req.clone(), Some(&token), cancel, what)?;
            if resp.status.as_u16() == 401 && !renewed {
                renewed = true;
                token = self.renew(&token, cancel)?;
                continue;
            }
            return Ok(resp);
        }
    }

    /// Sends `req` as it is (a pre-authenticated URL), with the bounded throttle waits.
    pub fn send_plain(
        &self,
        req: Request,
        cancel: &CancelToken,
        what: &str,
    ) -> Result<Response, Error> {
        self.send_with(req, None, cancel, what)
    }

    fn send_with(
        &self,
        req: Request,
        bearer: Option<&str>,
        cancel: &CancelToken,
        what: &str,
    ) -> Result<Response, Error> {
        let mut throttled = 0;
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let mut r = req.clone();
            if let Some(t) = bearer {
                r.headers
                    .insert("authorization", &format!("Bearer {t}"))
                    .map_err(|e| Error::Upload(format!("{what}: {e}")))?;
            }
            let resp = self
                .http
                .request(&r)
                .map_err(|e| Error::Upload(format!("{what}: {e}")))?;
            if !matches!(resp.status.as_u16(), 429 | 503) {
                return Ok(resp);
            }
            if throttled >= MAX_THROTTLED {
                return Err(Error::Upload(format!(
                    "{what}: still throttled (HTTP {}) after {MAX_THROTTLED} retries",
                    resp.status.as_u16()
                )));
            }
            self.pace
                .sleep(self.pace.wait_for(Some(&resp), throttled), cancel)?;
            throttled += 1;
        }
    }
}

/// Parses a URL the uploader built itself.
pub fn parse_url(s: &str) -> Result<Url, Error> {
    Url::parse(s).map_err(|e| Error::Upload(format!("invalid request URL: {e}")))
}

/// A request with headers.
pub fn request(
    method: Method,
    url: &str,
    headers: &[(&str, &str)],
    body: impl Into<Vec<u8>>,
) -> Result<Request, Error> {
    let mut req = Request::new(method, parse_url(url)?).with_body(body);
    for (k, v) in headers {
        req.headers
            .insert(k, v)
            .map_err(|e| Error::Upload(format!("request header {k}: {e}")))?;
    }
    Ok(req)
}

/// An error line for a failed provider call: status plus the JSON error code and message
/// (`{"error":{"code":..,"message":..}}`, Google's and Graph's shape), never the request.
pub fn json_error(what: &str, resp: &Response) -> Error {
    let body = dl_json::from_str::<Value>(&String::from_utf8_lossy(&resp.body)).ok();
    let err = body.as_ref().and_then(|b| b.get("error"));
    let pick = |k: &str| {
        err.and_then(|e| e.get(k))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect::<String>()
    };
    let (code, msg) = (pick("code"), pick("message"));
    Error::Upload(
        format!("{what}: HTTP {} {code} {msg}", resp.status.as_u16())
            .trim_end()
            .to_owned(),
    )
}

/// Reads `len` bytes of `f` at `offset` (fewer only at end of file).
pub fn read_at(f: &mut std::fs::File, offset: u64, len: usize) -> Result<Vec<u8>, Error> {
    use std::io::{Seek, SeekFrom};
    f.seek(SeekFrom::Start(offset))
        .map_err(|e| Error::Upload(format!("seek backup file: {e}")))?;
    let mut buf = vec![0u8; len];
    let n = super::read_full(f, &mut buf)?;
    buf.truncate(n);
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2026-11-19T10:00:00Z"), Some(1_795_082_400));
        assert_eq!(
            parse_rfc3339("2026-11-19T17:00:00.123+07:00"),
            Some(1_795_082_400)
        );
        assert_eq!(
            parse_rfc3339("2026-11-19T03:00:00-07:00"),
            Some(1_795_082_400)
        );
        assert_eq!(parse_rfc3339("yesterday"), None);
        assert_eq!(parse_rfc3339(""), None);
    }

    #[test]
    fn waits_follow_retry_after_and_cap() {
        let p = Pace::default();
        let mut r = dl_http::Response {
            version: dl_http::Version::Http11,
            status: dl_http::StatusCode::new(429).unwrap(),
            reason: String::new(),
            headers: dl_http::Headers::new(),
            body: Vec::new(),
            trailers: dl_http::Headers::new(),
            reused: false,
        };
        r.headers.insert("retry-after", "7").unwrap();
        assert_eq!(p.wait_for(Some(&r), 0), Duration::from_secs(7));
        r.headers.insert("retry-after", "9999").unwrap();
        assert_eq!(p.wait_for(Some(&r), 0), p.throttle_cap);
        assert_eq!(p.wait_for(None, 2), Duration::from_secs(4));
    }
}
