//! `ExchangeToken`: swaps a one-time enroll token for the device-bound API key.

use std::time::Duration;

use dl_http::{ClientOptions, Request, Url};
use dl_json::Value;
use dl_uuid::Uuid;

use crate::gojson::Obj;

const ENROLL_TIMEOUT: Duration = Duration::from_secs(30);

/// `POST <platform>/agent/enroll`; returns `(api_key, tenant_id)`. `agent_version` goes in
/// the `X-Agent-Version` header (the body keeps the Go agent's exact fields, which the
/// platform decodes strictly). Error texts never contain the token or the key.
pub fn exchange_token(
    platform_url: &str,
    token: &str,
    host: &str,
    device_id: Uuid,
    agent_version: &str,
) -> Result<(String, String), String> {
    let url = format!("{}/agent/enroll", platform_url.trim_end_matches('/'));
    // Go marshals a map[string]string: keys in sorted order.
    let mut body = String::new();
    Obj::new(&mut body)
        .str("device_id", &device_id.to_string())
        .str("device_name", host)
        .str("enroll_token", token)
        .end();
    let url = Url::parse(&url).map_err(|e| format!("agent: enroll request: {e}"))?;
    let req = Request::post(url, body.into_bytes())
        .with_header("Content-Type", "application/json")
        .and_then(|r| r.with_header("X-Agent-Host", host))
        .and_then(|r| r.with_header("X-Agent-Version", agent_version))
        .map_err(|e| format!("agent: enroll request: {e}"))?
        .with_timeout(ENROLL_TIMEOUT);
    let http = dl_http::Client::with_options(ClientOptions {
        max_body: 8 << 10,
        ..Default::default()
    });
    let resp = http
        .request(&req)
        .map_err(|e| format!("agent: enroll exchange: {e}"))?;
    if resp.status.as_u16() != 200 {
        return Err(format!(
            "agent: enroll rejected (status {})",
            resp.status.as_u16()
        ));
    }
    let v = dl_json::parse_slice(&resp.body)
        .map_err(|_| "agent: enroll response malformed".to_owned())?;
    let field = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let key = field("api_key");
    if key.is_empty() {
        return Err("agent: enroll response malformed".into());
    }
    Ok((key, field("tenant_id")))
}
