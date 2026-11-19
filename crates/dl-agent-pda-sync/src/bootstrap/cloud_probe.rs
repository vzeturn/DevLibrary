//! Checks, without redeeming anything, that this machine reaches the cloud and that its
//! clock agrees (port of `Bootstrap/CloudProbe.cs`): DNS, TLS and HTTP reachability of
//! `GET {url}/health/ready` through the agent's HTTP settings, and the clock skew
//! against the response `Date` header.

use super::preflight::PreflightFinding;
use crate::config::codes;
use crate::config::time::{SharedClock, SystemClock};
use crate::contracts::time::TICKS_PER_SECOND;
use crate::reporting::site_event_store::age_text;
use crate::transport::http::client_options;
use crate::util::ctx::Ctx;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

/// The probed route.
pub const READY_PATH: &str = "/health/ready";
/// Over this the probe warns (2 min, in ticks).
pub const WARN_SKEW_TICKS: i64 = 120 * TICKS_PER_SECOND;
/// Over this the probe fails (5 min, in ticks): signed requests are refused beyond it.
pub const MAX_SKEW_TICKS: i64 = 300 * TICKS_PER_SECOND;
/// The request timeout.
pub const TIMEOUT: Duration = Duration::from_secs(15);

/// The result of `GET {url}/health/ready` (`CloudProbeResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudProbeResult {
    /// What was found.
    pub findings: Vec<PreflightFinding>,
    /// The HTTP status when one came back.
    pub http_status: Option<u16>,
    /// Local clock minus cloud clock, in ticks (100 ns), when the answer had a `Date`.
    pub clock_skew: Option<i64>,
}

impl CloudProbeResult {
    /// No blocking finding.
    pub fn ok(&self) -> bool {
        self.findings.iter().all(|f| !f.is_error())
    }
}

/// Probes the cloud (`ICloudProbe`).
pub trait CloudProbe: Send + Sync {
    /// Probes the absolute http(s) `base_url`.
    fn probe(&self, base_url: &str, ctx: &Ctx) -> CloudProbeResult;
}

/// What came back from the ready route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResponse {
    /// The status code.
    pub status: u16,
    /// The `Date` header, if any.
    pub date: Option<String>,
}

/// Why the request got no answer (the C# `HttpRequestError` classes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeFailure {
    /// The name did not resolve (`NameResolutionError`).
    NameResolution(String),
    /// The TLS handshake failed (`SecureConnectionError`).
    SecureConnection(String),
    /// The timeout passed (`TaskCanceledException`).
    Timeout(String),
    /// Anything else (`ConnectionError`, ...).
    Connection(String),
}

/// Sends `GET url` (an absolute URL).
pub type Sender = Arc<dyn Fn(&str, &Ctx) -> Result<ProbeResponse, ProbeFailure> + Send + Sync>;
/// Resolves a host name: `Err` with the reason when it fails.
pub type Resolver = Arc<dyn Fn(&str) -> Result<Vec<IpAddr>, String> + Send + Sync>;

/// The real probe (`HttpCloudProbe`).
pub struct HttpCloudProbe {
    send: Sender,
    resolve: Resolver,
    clock: SharedClock,
}

impl std::fmt::Debug for HttpCloudProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpCloudProbe").finish_non_exhaustive()
    }
}

/// Whether `e` is a failed name lookup (Winsock `WSAHOST_NOT_FOUND`, `WSATRY_AGAIN`,
/// `WSANO_DATA`, or a `getaddrinfo` failure).
fn is_dns_error(e: &std::io::Error) -> bool {
    matches!(e.raw_os_error(), Some(11001..=11004))
        || e.to_string().contains("failed to lookup address")
}

/// `GET` through the agent's HTTP settings (`SyncHttpClient.CreateHandler`, 15 s).
pub fn default_sender() -> Sender {
    let client = Arc::new(dl_http::Client::with_options(client_options(TIMEOUT)));
    Arc::new(move |url, ctx| {
        let url = dl_http::Url::parse(url).map_err(|e| ProbeFailure::Connection(e.to_string()))?;
        let mut req = dl_http::Request::new(dl_http::Method::Get, url);
        if let Some(left) = ctx.remaining() {
            req.timeout = Some(left.min(TIMEOUT));
        }
        match client.request(&req) {
            Ok(resp) => Ok(ProbeResponse {
                status: resp.status.as_u16(),
                date: resp.headers.get("date").map(str::to_owned),
            }),
            Err(dl_http::Error::Timeout) => Err(ProbeFailure::Timeout(format!(
                "The request was canceled due to the configured HttpClient.Timeout of {} seconds elapsing.",
                TIMEOUT.as_secs()
            ))),
            Err(dl_http::Error::Tls(e)) => Err(ProbeFailure::SecureConnection(e.to_string())),
            Err(dl_http::Error::Io(e)) if is_dns_error(&e) => {
                Err(ProbeFailure::NameResolution(e.to_string()))
            }
            Err(e) => Err(ProbeFailure::Connection(e.to_string())),
        }
    })
}

/// `Dns.GetHostAddressesAsync` through the OS resolver.
pub fn default_resolver() -> Resolver {
    Arc::new(|host| {
        (host, 0)
            .to_socket_addrs()
            .map(|a| a.map(|s| s.ip()).collect())
            .map_err(|e| e.to_string())
    })
}

fn dns_failed(host: &str, reason: &str) -> PreflightFinding {
    PreflightFinding::error(
        codes::CLOUD_DNS_FAILED,
        format!(
            "The cloud host name {host} does not resolve ({reason}). Check the DNS settings of this machine."
        ),
    )
}

/// `HttpCloudProbe.Skew`: local clock minus cloud clock at the midpoint of the request
/// (unix nanoseconds in, ticks out).
pub fn skew(cloud_date: Option<i128>, sent: i128, received: i128) -> Option<i64> {
    let date = cloud_date?;
    let mid = sent + (received - sent) / 2;
    i64::try_from((mid - date) / 100).ok()
}

/// `HttpCloudProbe.SkewFindings`: unknown is a warning, over 2 min a warning, over 5 min
/// an error.
pub fn skew_findings(skew: Option<i64>) -> Vec<PreflightFinding> {
    let Some(s) = skew else {
        return vec![PreflightFinding::warn(
            codes::CLOCK_SKEW_UNKNOWN,
            "The cloud answer had no Date header; the clock could not be compared.",
        )];
    };
    let abs = s.saturating_abs();
    let text = format!(
        "This machine's clock is {} {} the cloud.",
        age_text(abs),
        if s > 0 { "ahead of" } else { "behind" }
    );
    let max_min = MAX_SKEW_TICKS / TICKS_PER_SECOND / 60;
    if abs > MAX_SKEW_TICKS {
        return vec![PreflightFinding::error(
            codes::CLOCK_SKEW_TOO_LARGE,
            format!(
                "{text} Beyond {max_min} min the cloud refuses the agent's signed requests. Fix the Windows time (w32tm /resync) and retry."
            ),
        )];
    }
    if abs > WARN_SKEW_TICKS {
        return vec![PreflightFinding::warn(
            codes::CLOCK_SKEW,
            format!(
                "{text} It still works, but the cloud refuses requests beyond {max_min} min. Fix the Windows time (w32tm /resync)."
            ),
        )];
    }
    Vec::new()
}

impl HttpCloudProbe {
    /// A probe over its parts (tests inject the sender, the resolver and the clock).
    pub fn new(send: Sender, resolve: Resolver, clock: SharedClock) -> HttpCloudProbe {
        HttpCloudProbe {
            send,
            resolve,
            clock,
        }
    }

    /// The agent's HTTP settings, the OS resolver and the system clock
    /// (`CreateDefault`).
    pub fn create_default() -> HttpCloudProbe {
        HttpCloudProbe::new(default_sender(), default_resolver(), SystemClock::shared())
    }

    fn resolve_host(&self, url: &dl_http::Url) -> Option<PreflightFinding> {
        let host = url.host();
        if url.is_ipv6() || host.parse::<IpAddr>().is_ok() {
            return None;
        }
        match (self.resolve)(host) {
            Ok(a) if !a.is_empty() => None,
            Ok(_) => Some(dns_failed(host, "no address returned")),
            Err(reason) => Some(dns_failed(host, &reason)),
        }
    }

    fn classify(failure: &ProbeFailure, host: &str) -> PreflightFinding {
        match failure {
            ProbeFailure::SecureConnection(m) => PreflightFinding::error(
                codes::CLOUD_TLS_FAILED,
                format!(
                    "The TLS handshake with {host} failed ({m}). Check the machine clock, the Windows root certificates, \
                     or a proxy that inspects TLS."
                ),
            ),
            ProbeFailure::NameResolution(m) => dns_failed(host, m),
            ProbeFailure::Timeout(m) => PreflightFinding::error(
                codes::CLOUD_UNREACHABLE,
                format!(
                    "{host} did not answer {READY_PATH} within {} s ({m}). Check the network, firewall or proxy.",
                    TIMEOUT.as_secs()
                ),
            ),
            ProbeFailure::Connection(m) => PreflightFinding::error(
                codes::CLOUD_UNREACHABLE,
                format!(
                    "Cannot reach {host} ({m}). Check the network, firewall or proxy of this machine."
                ),
            ),
        }
    }
}

impl CloudProbe for HttpCloudProbe {
    fn probe(&self, base_url: &str, ctx: &Ctx) -> CloudProbeResult {
        let mut findings = Vec::new();
        let (url, ready) = match dl_http::Url::parse(base_url)
            .and_then(|u| u.join(READY_PATH).map(|r| (u, r)))
        {
            Ok(v) => v,
            Err(e) => {
                findings.push(PreflightFinding::error(
                    codes::CLOUD_UNREACHABLE,
                    format!("Cannot reach {base_url} ({e}). Check the network, firewall or proxy of this machine."),
                ));
                return CloudProbeResult {
                    findings,
                    http_status: None,
                    clock_skew: None,
                };
            }
        };
        let host = url.host().to_owned();
        let dns = self.resolve_host(&url);
        let ready_text = ready.to_string();
        let t0 = self.clock.now().unix_nanos();
        let resp = match (self.send)(&ready_text, ctx) {
            Ok(r) => r,
            Err(f) => {
                // A timeout cut short by the caller's own cancellation is still a failed probe here.
                findings.push(dns.unwrap_or_else(|| HttpCloudProbe::classify(&f, &host)));
                return CloudProbeResult {
                    findings,
                    http_status: None,
                    clock_skew: None,
                };
            }
        };
        let t1 = self.clock.now().unix_nanos();
        // The name did not resolve here, yet the request went through: a proxy resolves it.
        if let Some(d) = dns {
            findings.push(PreflightFinding::warn(
                &d.code,
                format!("{} The request still succeeded through a proxy.", d.message),
            ));
        }
        if !(200..300).contains(&resp.status) {
            findings.push(PreflightFinding::error(
                codes::CLOUD_NOT_READY,
                format!(
                    "{ready_text} answered HTTP {}; the cloud is not ready or the URL is wrong.",
                    resp.status
                ),
            ));
        }
        let date = resp
            .date
            .as_deref()
            .and_then(|d| dl_time::OffsetDateTime::parse_http_date(d.trim()).ok())
            .map(dl_time::OffsetDateTime::unix_nanos);
        let s = skew(date, t0, t1);
        findings.extend(skew_findings(s));
        CloudProbeResult {
            findings,
            http_status: Some(resp.status),
            clock_skew: s,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_skew_is_measured_at_the_midpoint_of_the_request() {
        let s = 1_000_000_000_000i128;
        assert_eq!(
            skew(Some(s), s, s + 4_000_000_000),
            Some(2 * TICKS_PER_SECOND)
        );
        assert_eq!(skew(None, s, s), None);
        let unknown = skew_findings(None);
        assert_eq!(unknown.len(), 1);
        assert_eq!(unknown[0].code, codes::CLOCK_SKEW_UNKNOWN);
        assert!(!unknown[0].is_error());
        let behind = skew_findings(Some(-360 * TICKS_PER_SECOND));
        assert!(
            behind[0].message.contains("behind"),
            "{}",
            behind[0].message
        );
        assert_eq!(
            behind[0].message,
            "This machine's clock is 6m 0s behind the cloud. Beyond 5 min the cloud refuses the agent's signed requests. Fix the Windows time (w32tm /resync) and retry."
        );
    }
}
